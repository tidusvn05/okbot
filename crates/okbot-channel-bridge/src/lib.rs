//! Runs an okbot channel adapter written in any language as a child process.
//!
//! The child speaks JSON-RPC 2.0 over stdin/stdout, one JSON object per line (`docs/bridge.md`):
//!
//! - okbot → child requests: `initialize`, `send`, `edit`, `typing`, `shutdown`;
//! - child → okbot notifications: `inbound` (a message for the bot) and `log`.
//!
//! The child is restarted with backoff when it exits. Its stderr is forwarded to okbot's log.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundEvent, InboundSink,
    MessageId, RenderedMessage, ReplyTarget, Secret, SentRef, async_trait,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

/// The protocol version okbot speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// How to start the adapter.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Channel id, `<platform>:<instance>` (the platform is the adapter's name).
    pub id: ChannelId,
    /// Executable (an `okbot-channel-<name>` on `PATH`, or a path).
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Environment for the child (its secrets, resolved by okbot).
    pub env: Vec<(String, Secret<String>)>,
    /// Inherited variables to remove (other channels' secrets).
    pub env_remove: Vec<String>,
    /// Adapter settings, passed in `initialize`.
    pub settings: Value,
    /// What the platform supports (from `okbot.toml`; the runtime needs it before the child runs).
    pub caps: ChannelCaps,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A running child: where to write requests.
struct Child {
    stdin: AsyncMutex<ChildStdin>,
}

/// An adapter process as an okbot [`Channel`].
pub struct BridgeChannel {
    config: BridgeConfig,
    caps: ChannelCaps,
    child: AsyncMutex<Option<Arc<Child>>>,
    pending: Pending,
    next_id: AtomicU64,
}

impl std::fmt::Debug for BridgeChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeChannel")
            .field("id", &self.config.id)
            .field("command", &self.config.command)
            .finish_non_exhaustive()
    }
}

fn failed(message: impl Into<String>) -> ChannelError {
    ChannelError::Failed(message.into().into())
}

impl BridgeChannel {
    /// Creates the channel; the child starts in [`Channel::run`].
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            caps: config.caps.clone(),
            config,
            child: AsyncMutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        }
    }

    /// Sends a request and waits for its response.
    async fn request(&self, method: &str, params: Value) -> Result<Value, ChannelError> {
        let child = self
            .child
            .lock()
            .await
            .clone()
            .ok_or_else(|| ChannelError::Transient("the adapter is not running".into()))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string()
            + "\n";
        {
            let mut stdin = child.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| ChannelError::Transient(Box::new(e)))?;
            stdin
                .flush()
                .await
                .map_err(|e| ChannelError::Transient(Box::new(e)))?;
        }
        match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(failed(message)),
            Ok(Err(_)) => Err(ChannelError::Transient("the adapter exited".into())),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                Err(ChannelError::Transient(
                    format!("{method}: no answer within 60 s").into(),
                ))
            }
        }
    }

    /// Starts the child and serves it until it exits or `shutdown`. Returns `Ok(true)` to stop.
    async fn session(
        &self,
        sink: &Arc<dyn InboundSink>,
        shutdown: &CancellationToken,
    ) -> Result<bool, ChannelError> {
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.config.env_remove {
            command.env_remove(name);
        }
        for (name, value) in &self.config.env {
            command.env(name, value.expose());
        }
        let mut process = command
            .spawn()
            .map_err(|e| failed(format!("cannot start {}: {e}", self.config.command)))?;
        let stdin = process.stdin.take().expect("piped stdin");
        let stdout = process.stdout.take().expect("piped stdout");
        let stderr = process.stderr.take().expect("piped stderr");
        let id = self.config.id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(channel = %id, "{line}");
            }
        });
        *self.child.lock().await = Some(Arc::new(Child {
            stdin: AsyncMutex::new(stdin),
        }));

        let reader = {
            let pending = self.pending.clone();
            let sink = sink.clone();
            let id = self.config.id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    handle_line(&id, &line, &pending, &sink).await;
                }
            })
        };

        let init = self
            .request(
                "initialize",
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "channel": self.config.id.to_string(),
                    "settings": self.config.settings,
                }),
            )
            .await;
        match init {
            Ok(result) => {
                if result["protocol"].as_u64() != Some(u64::from(PROTOCOL_VERSION)) {
                    return Err(failed(format!(
                        "the adapter speaks protocol {}, okbot speaks {PROTOCOL_VERSION}",
                        result["protocol"]
                    )));
                }
                tracing::info!(channel = %self.config.id, adapter = %result["name"], "bridge adapter ready");
            }
            Err(error) => return Err(error),
        }

        let stop = tokio::select! {
            () = shutdown.cancelled() => {
                let _ = tokio::time::timeout(Duration::from_secs(5), self.request("shutdown", json!({}))).await;
                true
            }
            status = process.wait() => {
                tracing::warn!(channel = %self.config.id, ?status, "bridge adapter exited");
                false
            }
        };
        *self.child.lock().await = None;
        reader.abort();
        // Fail requests still waiting for the old process.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        Ok(stop)
    }
}

/// Handles one line from the child: a response or a notification.
async fn handle_line(id: &ChannelId, line: &str, pending: &Pending, sink: &Arc<dyn InboundSink>) {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        tracing::warn!(channel = %id, "adapter wrote a non-JSON line");
        return;
    };
    if let Some(request_id) = message["id"].as_u64()
        && message.get("method").is_none()
    {
        let tx = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&request_id);
        if let Some(tx) = tx {
            let result = match message.get("error") {
                Some(error) => Err(error["message"]
                    .as_str()
                    .unwrap_or("adapter error")
                    .to_string()),
                None => Ok(message["result"].clone()),
            };
            let _ = tx.send(result);
        }
        return;
    }
    match message["method"].as_str() {
        Some("inbound") => {
            match serde_json::from_value::<InboundEvent>(message["params"]["event"].clone()) {
                Ok(event) if event.conversation.channel == *id => {
                    let _ = sink.deliver(event).await;
                }
                Ok(event) => {
                    tracing::warn!(channel = %id, other = %event.conversation.channel, "adapter sent an event for another channel");
                }
                Err(error) => {
                    tracing::warn!(channel = %id, %error, "adapter sent an invalid event");
                }
            }
        }
        Some("log") => {
            tracing::info!(channel = %id, "{}", message["params"]["message"].as_str().unwrap_or(""));
        }
        _ => {}
    }
}

#[async_trait]
impl Channel for BridgeChannel {
    fn id(&self) -> &ChannelId {
        &self.config.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let mut failures: u32 = 0;
        loop {
            match self.session(&sink, &shutdown).await {
                Ok(true) => return Ok(()),
                Ok(false) => failures = failures.saturating_add(1),
                Err(error) => {
                    failures = failures.saturating_add(1);
                    tracing::error!(channel = %self.config.id, %error, "bridge adapter failed");
                }
            }
            let wait = Duration::from_secs(1u64 << failures.min(6));
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let result = self
            .request("send", json!({"target": to, "text": message.text}))
            .await?;
        let message_id = result["message_id"]
            .as_str()
            .ok_or_else(|| failed("send: the adapter returned no message_id"))?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(message_id),
        })
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        if !self.caps.edit {
            return Err(ChannelError::Unsupported("edit"));
        }
        self.request(
            "edit",
            json!({"target": sent.target, "message_id": sent.message_id, "text": message.text}),
        )
        .await
        .map(drop)
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        if !self.caps.typing {
            return Ok(());
        }
        self.request("typing", json!({"target": to}))
            .await
            .map(drop)
    }
}

#[cfg(test)]
mod tests;
