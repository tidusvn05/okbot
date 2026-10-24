//! The `peer:<team>` channel: inbound tasks as events, the runtime's replies as their results.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, LengthUnit,
    MarkdownDialect, Prompt, RenderedMessage, ReplyTarget, SentRef, WebhookHandler, WebhookRequest,
    WebhookResponse, async_trait,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::inbound;
use crate::node::{
    Inner, LocalRequest, LocalResponse, MAX_BODY, OpenPrompt, SIGNATURE_HEADER, SOCKET_FILE,
};

/// The longest Unix socket path (the platform limit, with room to spare).
const MAX_SOCKET_PATH: usize = 100;

/// One team's channel.
pub(crate) struct PeerChannel {
    pub(crate) id: ChannelId,
    pub(crate) team: String,
    pub(crate) caps: ChannelCaps,
    pub(crate) inner: Arc<Inner>,
}

impl PeerChannel {
    pub(crate) fn new(inner: Arc<Inner>, team: &str) -> Self {
        Self {
            id: ChannelId::new(okbot_core::PEER_PLATFORM, team).expect("team ids are slugs"),
            team: team.to_string(),
            caps: ChannelCaps {
                max_message_len: 100_000,
                length_unit: LengthUnit::Chars,
                dialect: MarkdownDialect::CommonMark,
                threads: false,
                edit: false,
                typing: false,
            },
            inner,
        }
    }
}

#[async_trait]
impl Channel for PeerChannel {
    fn id(&self) -> &ChannelId {
        &self.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let _ = self.inner.sink.set(sink);
        // The socket, the deadline sweep and manifest refreshes serve every team: the first
        // channel to run starts them.
        if !self.inner.local_server.swap(true, Ordering::SeqCst) {
            let inner = self.inner.clone();
            let stop = shutdown.clone();
            tokio::spawn(async move { serve_local(inner, stop).await });
            tokio::spawn(background(self.inner.clone(), shutdown.clone()));
            tokio::spawn(crate::relay::run(self.inner.clone(), shutdown.clone()));
        }
        shutdown.cancelled().await;
        Ok(())
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let task = to.in_reply_to.as_ref().map(okbot_core::MessageId::as_str);
        if !inbound::replied(&self.inner, &to.conversation, task, &message.text).await {
            tracing::debug!(team = %self.team, "a reply no task waits for");
        }
        Ok(SentRef {
            target: to.clone(),
            message_id: hex::encode(rand::random::<[u8; 8]>()).into(),
        })
    }

    async fn prompt(&self, to: &ReplyTarget, prompt: &Prompt) -> Result<SentRef, ChannelError> {
        if prompt.buttons.iter().any(|b| b.form.is_some()) {
            // Typed values may be secrets: they never travel between bots.
            return Err(ChannelError::Unsupported("forms"));
        }
        let sent = SentRef {
            target: to.clone(),
            message_id: prompt.id.as_str().into(),
        };
        let task = to.in_reply_to.as_ref().map(okbot_core::MessageId::as_str);
        let open = OpenPrompt {
            prompt: prompt.clone(),
            sent: sent.clone(),
        };
        if inbound::prompt_opened(&self.inner, &to.conversation, task, open).await {
            Ok(sent)
        } else {
            Err(ChannelError::Failed(
                "no task waits in this conversation".into(),
            ))
        }
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        _message: &RenderedMessage,
        _keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        inbound::prompt_closed(&self.inner, sent.message_id.as_str()).await;
        Ok(())
    }

    fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
        Some(Arc::new(Endpoint {
            inner: self.inner.clone(),
            team: self.team.clone(),
        }))
    }
}

/// `/a2a/<team>` on the bot's HTTP server.
struct Endpoint {
    inner: Arc<Inner>,
    team: String,
}

#[async_trait]
impl WebhookHandler for Endpoint {
    fn path(&self) -> String {
        format!("a2a/{}", self.team)
    }

    async fn handle(&self, request: WebhookRequest) -> WebhookResponse {
        let reply = inbound::handle(
            &self.inner,
            &self.team,
            &request.method,
            request.header("authorization"),
            &request.body,
        )
        .await;
        let mut response = WebhookResponse::status(reply.status);
        if reply.signature.is_some() {
            response.content_type = "application/json";
        }
        response.body = reply.body;
        match reply.signature {
            Some(signature) => response.with_header(SIGNATURE_HEADER, signature),
            None => response,
        }
    }
}

/// Serves members on this machine over `state/peer.sock` until `stop`.
async fn serve_local(inner: Arc<Inner>, stop: CancellationToken) {
    let path = inner.state_dir.join(SOCKET_FILE);
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        tracing::warn!(path = %path.display(), "the bot directory's path is too long for a socket; bots on this machine cannot reach it");
        return;
    }
    let _ = std::fs::remove_file(&path);
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "cannot open the peer socket");
            return;
        }
    };
    tracing::info!(path = %path.display(), "peer socket listening");
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => {
                    tracing::warn!(%error, "peer socket accept failed");
                    continue;
                }
            },
            () = stop.cancelled() => break,
        };
        let inner = inner.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read).take(MAX_BODY as u64 * 2);
            let mut line = String::new();
            if reader.read_line(&mut line).await.is_err() {
                return;
            }
            let response = match serde_json::from_str::<LocalRequest>(&line) {
                Ok(request) => {
                    let reply = inbound::handle(
                        &inner,
                        &request.team,
                        "POST",
                        Some(&request.authorization),
                        request.body.as_bytes(),
                    )
                    .await;
                    LocalResponse {
                        status: reply.status,
                        signature: reply.signature,
                        body: String::from_utf8_lossy(&reply.body).into_owned(),
                    }
                }
                Err(_) => LocalResponse {
                    status: 400,
                    signature: None,
                    body: "one JSON request per line".into(),
                },
            };
            let mut out = serde_json::to_vec(&response).expect("responses serialize");
            out.push(b'\n');
            let _ = write.write_all(&out).await;
        });
    }
    let _ = std::fs::remove_file(&path);
}

/// Deadlines of inbound tasks, and manifest refreshes.
async fn background(inner: Arc<Inner>, stop: CancellationToken) {
    let mut sweep = tokio::time::interval(Duration::from_secs(15));
    let mut refresh = tokio::time::interval(crate::node::REFRESH);
    refresh.tick().await; // `Node::start` just read them
    loop {
        tokio::select! {
            _ = sweep.tick() => inbound::expire(&inner).await,
            _ = refresh.tick() => {
                for team in inner.teams.values() {
                    if let Err(error) = inner.refresh(team).await {
                        tracing::warn!(team = %team.config.id, %error, "cannot refresh the team manifest");
                    }
                }
            }
            () = stop.cancelled() => break,
        }
    }
}
