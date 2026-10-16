//! Prompts on behalf of `[[mcp]]` servers (ADR 0007): `POST /interact` shows buttons (and forms)
//! in the conversation of a signed run token; answers go to the server's callback, signed, and
//! never to the agent or the conversation history.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use okbot_core::{
    Prompt, PromptAnswer, PromptButton, RenderedMessage, ReplyTarget, SentRef, UserId,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Shared;
use crate::config::ExtraMcp;
use crate::identity;

/// Default and longest time a prompt waits for an answer.
const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);
/// The server's callback may run a long operation.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

struct Pending {
    mcp: String,
    callback: String,
    markdown: String,
    sent: SentRef,
    expires: Instant,
    busy: bool,
}

/// Prompts waiting for an answer, by id.
#[derive(Default)]
pub(crate) struct Prompts(Mutex<HashMap<String, Pending>>);

impl Prompts {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum AnswerBy {
    /// Only the person whose turn issued the token.
    #[default]
    Requester,
    /// `[bot] admins` on this channel.
    Admins,
    /// Anyone the bot answers in this conversation.
    Anyone,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    text: String,
    buttons: Vec<PromptButton>,
    callback: String,
    #[serde(default)]
    answer_by: AnswerBy,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn now_unix(shared: &Shared) -> i64 {
    shared.clock.now().as_second()
}

/// `scheme://host:port` of a URL, to keep callbacks on the server's own origin.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty() && !host.contains('@')).then(|| format!("{scheme}://{host}"))
}

/// The verified claims and the server they belong to.
fn authenticate<'a>(
    shared: &'a Shared,
    bearer: Option<&str>,
) -> Option<(identity::Claims, &'a ExtraMcp)> {
    let token = bearer?;
    let name = identity::claimed_server(token)?;
    let server = shared
        .config
        .mcp
        .iter()
        .find(|m| m.signed && m.server.name == name)?;
    let claims = identity::verify(server.token.expose().as_bytes(), token, now_unix(shared))?;
    Some((claims, server))
}

fn error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Value) {
    (status, json!({"error": message.into()}))
}

/// Who may answer, as channel-local user ids (empty: anyone).
fn allowed(shared: &Shared, claims: &identity::Claims, by: AnswerBy) -> Vec<UserId> {
    let channel = &claims.conversation.channel;
    match by {
        AnswerBy::Requester => claims
            .user
            .rsplit_once('/')
            .map(|(_, id)| vec![UserId::new(id)])
            .unwrap_or_default(),
        AnswerBy::Admins => shared
            .config
            .access
            .admins
            .iter()
            .filter(|(c, _)| c == channel)
            .map(|(_, u)| u.clone())
            .collect(),
        AnswerBy::Anyone => shared
            .config
            .access
            .allow_users
            .as_ref()
            .map(|users| {
                users
                    .iter()
                    .filter(|(c, _)| c == channel)
                    .map(|(_, u)| u.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// `POST /interact`: shows a prompt. Answers `{"prompt": id}`.
pub(crate) async fn create(
    shared: &Arc<Shared>,
    bearer: Option<&str>,
    body: &[u8],
) -> (StatusCode, Value) {
    let Some((claims, server)) = authenticate(shared, bearer) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "a valid signed run token is required",
        );
    };
    let request: Request = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    if origin(&request.callback).is_none()
        || origin(&request.callback) != origin(&server.server.url)
    {
        return error(
            StatusCode::BAD_REQUEST,
            "callback must be on the MCP server's own origin",
        );
    }
    let Some(channel) = shared.channels.get(&claims.conversation.channel) else {
        return error(
            StatusCode::NOT_FOUND,
            "the conversation's channel is not running",
        );
    };
    let rendered = shared.renderer.render(&request.text, channel.caps());
    let [text] = rendered.as_slice() else {
        return error(StatusCode::BAD_REQUEST, "text is too long for one message");
    };
    let prompt = Prompt {
        id: hex::encode(rand::random::<[u8; 8]>()),
        text: text.text.clone(),
        buttons: request.buttons,
        allowed: allowed(shared, &claims, request.answer_by),
    };
    if let Err(message) = prompt.validate() {
        return error(StatusCode::BAD_REQUEST, message);
    }
    if request.answer_by == AnswerBy::Admins && prompt.allowed.is_empty() {
        return error(StatusCode::BAD_REQUEST, "no [bot] admins on this channel");
    }
    let target = ReplyTarget {
        conversation: claims.conversation.clone(),
        in_reply_to: None,
    };
    let sent = match channel.prompt(&target, &prompt).await {
        Ok(sent) => sent,
        Err(okbot_core::ChannelError::Unsupported(_)) => {
            return error(StatusCode::NOT_IMPLEMENTED, "this platform has no buttons");
        }
        Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    let ttl = request
        .expires_in
        .map_or(DEFAULT_TTL, Duration::from_secs)
        .min(MAX_TTL);
    tracing::info!(prompt = %prompt.id, mcp = %server.server.name, user = %claims.user, "prompt shown");
    shared.prompts.lock().insert(
        prompt.id.clone(),
        Pending {
            mcp: server.server.name.clone(),
            callback: request.callback,
            markdown: request.text,
            sent,
            expires: Instant::now() + ttl,
            busy: false,
        },
    );
    (StatusCode::OK, json!({"prompt": prompt.id}))
}

/// The first rendered message of `markdown` for the channel of `sent`.
fn render(shared: &Shared, sent: &SentRef, markdown: &str) -> Option<RenderedMessage> {
    let channel = shared.channels.get(&sent.target.conversation.channel)?;
    shared
        .renderer
        .render(markdown, channel.caps())
        .into_iter()
        .next()
}

async fn update(shared: &Shared, sent: &SentRef, markdown: &str, keep_buttons: bool) {
    let (Some(channel), Some(message)) = (
        shared.channels.get(&sent.target.conversation.channel),
        render(shared, sent, markdown),
    ) else {
        return;
    };
    if let Err(error) = channel.close_prompt(sent, &message, keep_buttons).await {
        tracing::warn!(%error, "cannot update a prompt");
    }
}

#[derive(Debug, Default, Deserialize)]
struct CallbackReply {
    #[serde(default)]
    text: String,
    #[serde(default)]
    keep_open: bool,
}

/// POSTs the answer to the server's callback, signed; its reply, or a retry note on failure.
async fn post_callback(
    shared: &Shared,
    server: &ExtraMcp,
    callback: &str,
    answer: &PromptAnswer,
    admin: bool,
) -> CallbackReply {
    let channel = &answer.conversation.channel;
    let values: serde_json::Map<String, Value> = answer
        .values
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.expose().clone())))
        .collect();
    let body = json!({
        "prompt": answer.prompt,
        "button": answer.button,
        "values": values,
        "user": {"id": format!("{channel}/{}", answer.user.id), "name": answer.user.display_name, "admin": admin},
        "conversation": answer.conversation,
    })
    .to_string();
    let timestamp = now_unix(shared);
    let signature =
        identity::sign_body(server.token.expose().as_bytes(), timestamp, body.as_bytes());
    let _ = rustls::crypto::ring::default_provider().install_default(); // see crates/okbot-cli/src/main.rs
    let result = reqwest::Client::new()
        .post(callback)
        .timeout(CALLBACK_TIMEOUT)
        .header("content-type", "application/json")
        .header("x-okbot-timestamp", timestamp.to_string())
        .header("x-okbot-signature", signature)
        .body(body)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status);
    match result {
        Ok(response) => response.json::<CallbackReply>().await.unwrap_or_default(),
        Err(error) => {
            // The error never contains the body (values); the URL is the server's own.
            tracing::warn!(prompt = %answer.prompt, %error, "prompt callback failed");
            CallbackReply {
                text: "⚠️ The service did not answer; try again.".into(),
                keep_open: true,
            }
        }
    }
}

/// Sends an answer to the server's callback and shows its reply under the prompt.
pub(crate) async fn answer(shared: Arc<Shared>, answer: PromptAnswer) {
    enum Next {
        Skip,
        Expired(Pending),
        Go(String, String, String, SentRef),
    }
    let next = {
        let mut prompts = shared.prompts.lock();
        match prompts.get_mut(&answer.prompt) {
            None => Next::Skip,
            Some(p) if p.busy => Next::Skip,
            Some(p) if p.expires <= Instant::now() => {
                Next::Expired(prompts.remove(&answer.prompt).expect("present"))
            }
            Some(p) => {
                p.busy = true;
                Next::Go(
                    p.mcp.clone(),
                    p.callback.clone(),
                    p.markdown.clone(),
                    p.sent.clone(),
                )
            }
        }
    };
    let (mcp, callback, markdown, sent) = match next {
        Next::Skip => return,
        Next::Expired(p) => {
            update(&shared, &p.sent, &format!("{}\n\n⌛", p.markdown), false).await;
            return;
        }
        Next::Go(mcp, callback, markdown, sent) => (mcp, callback, markdown, sent),
    };
    let Some(server) = shared.config.mcp.iter().find(|m| m.server.name == mcp) else {
        return;
    };
    let channel = &answer.conversation.channel;
    let admin = shared
        .config
        .access
        .admins
        .contains(&(channel.clone(), answer.user.id.clone()));
    update(
        &shared,
        &sent,
        &format!("{markdown}\n\n⏳ {}", answer.user.display_name),
        true,
    )
    .await;
    let reply = post_callback(&shared, server, &callback, &answer, admin).await;
    let text = if reply.text.trim().is_empty() {
        "✅".to_string()
    } else {
        reply.text
    };
    update(
        &shared,
        &sent,
        &format!("{markdown}\n\n{text}"),
        reply.keep_open,
    )
    .await;
    let mut prompts = shared.prompts.lock();
    if reply.keep_open {
        if let Some(p) = prompts.get_mut(&answer.prompt) {
            p.busy = false;
        }
    } else {
        prompts.remove(&answer.prompt);
    }
}

/// Closes expired prompts (their buttons disappear). Runs until `stop`.
pub(crate) async fn sweep(shared: Arc<Shared>, stop: okbot_core::CancellationToken) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
        let expired: Vec<Pending> = {
            let mut prompts = shared.prompts.lock();
            let ids: Vec<String> = prompts
                .iter()
                .filter(|(_, p)| !p.busy && p.expires <= Instant::now())
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| prompts.remove(id)).collect()
        };
        for p in expired {
            update(&shared, &p.sent, &format!("{}\n\n⌛", p.markdown), false).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callbacks_stay_on_the_server_origin() {
        assert_eq!(
            origin("http://127.0.0.1:8790/mcp").as_deref(),
            Some("http://127.0.0.1:8790")
        );
        assert_eq!(
            origin("http://127.0.0.1:8790/answers?x=1"),
            origin("http://127.0.0.1:8790/mcp")
        );
        assert_ne!(
            origin("http://127.0.0.1:9999/a"),
            origin("http://127.0.0.1:8790/mcp")
        );
        assert_eq!(origin("http://user@evil/"), None);
        assert_eq!(origin("not a url"), None);
    }
}
