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

/// Where an answer goes.
pub(crate) enum Kind {
    /// To an `[[mcp]]` server's callback (ADR 0007).
    Callback { mcp: String, callback: String },
    /// To a tool call waiting in the gateway for approval (ADR 0011): `true` to allow.
    Confirm(Option<tokio::sync::oneshot::Sender<bool>>),
    /// To a delegation waiting for the origin person's choice (ADR 0014): the button id.
    Choice(Option<tokio::sync::oneshot::Sender<String>>),
}

pub(crate) struct Pending {
    pub(crate) kind: Kind,
    pub(crate) markdown: String,
    pub(crate) sent: SentRef,
    pub(crate) expires: Instant,
    pub(crate) busy: bool,
    /// Who may answer; anyone in the conversation when empty.
    pub(crate) allowed: Vec<UserId>,
    /// Button ids in order (`/answer <code> <n>` picks one).
    pub(crate) buttons: Vec<String>,
    /// Buttons that open a form: only the platform's own form can answer them, never a typed
    /// command (which would skip the form).
    pub(crate) forms: Vec<String>,
    /// Shown as text (the platform has no buttons): answered with commands.
    pub(crate) text: bool,
}

/// Prompts waiting for an answer, by id.
#[derive(Default)]
pub(crate) struct Prompts(Mutex<HashMap<String, Pending>>);

impl Prompts {
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
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
    let now = now_unix(shared);
    let claims = identity::verify(server.token.expose().as_bytes(), token, now)?;
    // Signed by the server itself, not issued for a turn: refused.
    shared
        .issued
        .contains(&claims, now)
        .then_some((claims, server))
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
        id: short_id(),
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
    let (sent, text) = match show(shared, &target, &prompt, &request.text).await {
        Ok(shown) => shown,
        Err(Shown::NoForms) => {
            return error(
                StatusCode::NOT_IMPLEMENTED,
                "this platform has no forms; use buttons without a form",
            );
        }
        Err(Shown::Failed(e)) => return error(StatusCode::BAD_GATEWAY, e),
    };
    let ttl = request
        .expires_in
        .map_or(DEFAULT_TTL, Duration::from_secs)
        .min(MAX_TTL);
    tracing::info!(prompt = %prompt.id, mcp = %server.server.name, user = %claims.user, "prompt shown");
    shared.prompts.lock().insert(
        prompt.id.clone(),
        Pending {
            kind: Kind::Callback {
                mcp: server.server.name.clone(),
                callback: request.callback,
            },
            markdown: request.text,
            sent,
            expires: Instant::now() + ttl,
            busy: false,
            allowed: prompt.allowed.clone(),
            buttons: prompt.buttons.iter().map(|b| b.id.clone()).collect(),
            forms: prompt
                .buttons
                .iter()
                .filter(|b| b.form.is_some())
                .map(|b| b.id.clone())
                .collect(),
            text,
        },
    );
    (StatusCode::OK, json!({"prompt": prompt.id}))
}

/// Why a prompt could not be shown.
pub(crate) enum Shown {
    /// The platform has no buttons, and a button needs a form.
    NoForms,
    /// Sending failed.
    Failed(String),
}

/// A short code for a prompt answered by command (`/approve 4f2a9c`).
pub(crate) fn short_id() -> String {
    hex::encode(rand::random::<[u8; 3]>())
}

/// The commands that answer a prompt shown as text.
pub(crate) fn text_choices(prompt: &Prompt) -> String {
    let id = &prompt.id;
    let ids: Vec<&str> = prompt.buttons.iter().map(|b| b.id.as_str()).collect();
    if ids == ["allow", "deny"] {
        return format!("Reply `/approve {id}` or `/deny {id}`.");
    }
    let choices: Vec<String> = prompt
        .buttons
        .iter()
        .enumerate()
        .map(|(i, b)| format!("`/answer {id} {}` for **{}**", i + 1, b.label))
        .collect();
    format!("Reply {}.", choices.join(", "))
}

/// Shows `prompt` with buttons, or as text with answer commands on platforms without buttons.
/// Returns the message and whether it is text.
pub(crate) async fn show(
    shared: &Shared,
    target: &ReplyTarget,
    prompt: &Prompt,
    markdown: &str,
) -> Result<(SentRef, bool), Shown> {
    let Some(channel) = shared.channels.get(&target.conversation.channel) else {
        return Err(Shown::Failed(
            "the conversation's channel is not running".into(),
        ));
    };
    match channel.prompt(target, prompt).await {
        Ok(sent) => return Ok((sent, false)),
        Err(okbot_core::ChannelError::Unsupported(_)) => {}
        Err(e) => return Err(Shown::Failed(e.to_string())),
    }
    if prompt.buttons.iter().any(|b| b.form.is_some()) {
        return Err(Shown::NoForms);
    }
    let full = format!("{markdown}\n\n{}", text_choices(prompt));
    let mut sent = None;
    for part in shared.renderer.render(&full, channel.caps()) {
        match channel.send(target, &part).await {
            Ok(s) => {
                sent.get_or_insert(s);
            }
            Err(e) => return Err(Shown::Failed(e.to_string())),
        }
    }
    sent.map(|s| (s, true))
        .ok_or_else(|| Shown::Failed("nothing to send".into()))
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

/// Shows the prompt's new state: replaces its text (and buttons), or for a text prompt on a
/// platform that cannot edit, sends `outcome` as a new message.
pub(crate) async fn update(
    shared: &Shared,
    sent: &SentRef,
    text: bool,
    markdown: &str,
    outcome: &str,
    keep_buttons: bool,
) {
    let Some(channel) = shared.channels.get(&sent.target.conversation.channel) else {
        return;
    };
    let full = format!("{markdown}\n\n{outcome}");
    let result = if !text {
        match render(shared, sent, &full) {
            Some(message) => channel.close_prompt(sent, &message, keep_buttons).await,
            None => return,
        }
    } else if channel.caps().edit && !keep_buttons {
        match render(shared, sent, &full) {
            Some(message) => channel.edit(sent, &message).await,
            None => return,
        }
    } else {
        // A text prompt the platform cannot edit: the outcome follows as a message.
        crate::turn::send_to(shared, &sent.target, outcome).await
    };
    if let Err(error) = result {
        tracing::warn!(%error, "cannot update a prompt");
    }
}

#[derive(Debug, Default, Deserialize)]
struct CallbackReply {
    #[serde(default)]
    text: String,
    #[serde(default)]
    keep_open: bool,
    /// The next step of the same prompt (a form's values became a plan to confirm): shown as a new
    /// message with the same id and the same people allowed to answer, the old one closed with `text`.
    #[serde(default)]
    next: Option<NextStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NextStep {
    text: String,
    buttons: Vec<PromptButton>,
    #[serde(default)]
    expires_in: Option<u64>,
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
        .and_then(reqwest::Response::error_for_status)
        .map_err(reqwest::Error::without_url);
    match result {
        Ok(response) => response.json::<CallbackReply>().await.unwrap_or_default(),
        Err(error) => {
            // The error never contains the body (values) nor the URL (it may hold a key).
            tracing::warn!(prompt = %answer.prompt, %error, "prompt callback failed");
            CallbackReply {
                text: "⚠️ The service did not answer; try again.".into(),
                keep_open: true,
                next: None,
            }
        }
    }
}

/// Handles an answer: checks who answered, then releases a waiting tool call or sends the
/// answer to the server's callback, and shows the outcome under the prompt.
pub(crate) async fn answer(shared: Arc<Shared>, answer: PromptAnswer) {
    enum Next {
        Skip,
        Expired(Pending),
        NotYours(SentRef),
        Confirm(Pending),
        Go(String, String, String, SentRef, bool, Vec<UserId>),
    }
    let next = {
        let mut prompts = shared.prompts.lock();
        match prompts.get_mut(&answer.prompt) {
            None => Next::Skip,
            Some(p) if !same_room(&p.sent.target.conversation, &answer.conversation) => Next::Skip,
            Some(p) if !p.buttons.contains(&answer.button) => Next::Skip,
            Some(p) if !p.allowed.is_empty() && !p.allowed.contains(&answer.user.id) => {
                Next::NotYours(p.sent.clone())
            }
            Some(p) if p.busy => Next::Skip,
            Some(p) if p.expires <= Instant::now() => {
                Next::Expired(prompts.remove(&answer.prompt).expect("present"))
            }
            Some(p) => match &p.kind {
                Kind::Confirm(_) | Kind::Choice(_) => {
                    Next::Confirm(prompts.remove(&answer.prompt).expect("present"))
                }
                Kind::Callback { mcp, callback } => {
                    let go = Next::Go(
                        mcp.clone(),
                        callback.clone(),
                        p.markdown.clone(),
                        p.sent.clone(),
                        p.text,
                        p.allowed.clone(),
                    );
                    p.busy = true;
                    go
                }
            },
        }
    };
    let (mcp, callback, markdown, sent, text, allowed) = match next {
        Next::Skip => return,
        Next::NotYours(sent) => {
            // Only reached for text answers: buttons are checked by the platform adapter.
            crate::turn::send_to(&shared, &sent.target, "⛔ This is not yours to answer.")
                .await
                .ok();
            return;
        }
        Next::Expired(p) => {
            update(&shared, &p.sent, p.text, &p.markdown, "⌛", false).await;
            return;
        }
        Next::Confirm(p) => return release(&shared, p, &answer).await,
        Next::Go(mcp, callback, markdown, sent, text, allowed) => {
            (mcp, callback, markdown, sent, text, allowed)
        }
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
    if !text {
        update(
            &shared,
            &sent,
            false,
            &markdown,
            &format!("⏳ {}", answer.user.display_name),
            true,
        )
        .await;
    }
    let reply = post_callback(&shared, server, &callback, &answer, admin).await;
    let outcome = if reply.text.trim().is_empty() {
        "✅".to_string()
    } else {
        reply.text
    };
    if let Some(step) = reply.next {
        update(&shared, &sent, text, &markdown, &outcome, false).await;
        return go_on(&shared, &answer.prompt, step, &sent.target, allowed).await;
    }
    update(&shared, &sent, text, &markdown, &outcome, reply.keep_open).await;
    let mut prompts = shared.prompts.lock();
    if reply.keep_open {
        if let Some(p) = prompts.get_mut(&answer.prompt) {
            p.busy = false;
        }
    } else {
        prompts.remove(&answer.prompt);
    }
}

/// Goes on with prompt `id` as `step`, or closes it and says why it cannot.
async fn go_on(
    shared: &Shared,
    id: &str,
    step: NextStep,
    target: &ReplyTarget,
    allowed: Vec<UserId>,
) {
    if let Err(e) = show_next(shared, id, step, target, allowed).await {
        tracing::warn!(prompt = %id, error = %e, "cannot show the next step");
        shared.prompts.lock().remove(id);
        crate::turn::send_to(shared, target, &format!("⚠️ {e}"))
            .await
            .ok();
    }
}

/// Shows `step` as prompt `id` again, in a new message: its `Pending` keeps where answers go.
async fn show_next(
    shared: &Shared,
    id: &str,
    step: NextStep,
    target: &ReplyTarget,
    allowed: Vec<UserId>,
) -> Result<(), String> {
    let channel = shared
        .channels
        .get(&target.conversation.channel)
        .ok_or("the conversation's channel is not running")?;
    let rendered = shared.renderer.render(&step.text, channel.caps());
    let [message] = rendered.as_slice() else {
        return Err("the next step is too long for one message".into());
    };
    let prompt = Prompt {
        id: id.to_string(),
        text: message.text.clone(),
        buttons: step.buttons,
        allowed,
    };
    prompt.validate()?;
    let (sent, text) = match show(shared, target, &prompt, &step.text).await {
        Ok(shown) => shown,
        Err(Shown::NoForms) => return Err("this platform has no forms".into()),
        Err(Shown::Failed(e)) => return Err(e),
    };
    let ttl = step
        .expires_in
        .map_or(DEFAULT_TTL, Duration::from_secs)
        .min(MAX_TTL);
    let mut prompts = shared.prompts.lock();
    let p = prompts.get_mut(id).ok_or("the prompt is gone")?;
    p.markdown = step.text;
    p.sent = sent;
    p.text = text;
    p.expires = Instant::now() + ttl;
    p.busy = false;
    p.buttons = prompt.buttons.iter().map(|b| b.id.clone()).collect();
    p.forms = prompt
        .buttons
        .iter()
        .filter(|b| b.form.is_some())
        .map(|b| b.id.clone())
        .collect();
    tracing::info!(prompt = %id, "next step shown");
    Ok(())
}

/// The same chat: channel and room (adapters may fill the thread of an answer differently).
fn same_room(a: &okbot_core::ConversationKey, b: &okbot_core::ConversationKey) -> bool {
    a.channel == b.channel && a.room == b.room
}

/// Releases a tool call (or a delegation) waiting for an answer and shows who decided.
async fn release(shared: &Shared, p: Pending, answer: &PromptAnswer) {
    let allow = answer.button == "allow";
    let choice = matches!(p.kind, Kind::Choice(_));
    // The call may have stopped waiting (the agent gave up): then nothing was allowed.
    let waiting = match p.kind {
        Kind::Confirm(Some(tx)) => tx.send(allow).is_ok(),
        Kind::Choice(Some(tx)) => tx.send(answer.button.clone()).is_ok(),
        _ => false,
    };
    let outcome = match (waiting, allow) {
        (false, _) => "⌛ No longer waiting; nothing was done.".to_string(),
        (true, _) if choice => format!("➡️ {} chose `{}`", answer.user.display_name, answer.button),
        (true, true) => format!("✅ Allowed by {}", answer.user.display_name),
        (true, false) => format!("❌ Declined by {}", answer.user.display_name),
    };
    tracing::info!(prompt = %answer.prompt, user = %answer.user.id, allow, waiting, "tool call answered");
    update(shared, &p.sent, p.text, &p.markdown, &outcome, false).await;
}

/// `/approve <code>`, `/deny <code>` or `/answer <code> <n>` from `event`: the answer to a
/// prompt shown as text (or with buttons, typed instead).
pub(crate) async fn answer_text(
    shared: Arc<Shared>,
    event: &okbot_core::InboundEvent,
    code: &str,
    choice: Choice,
) {
    let found = shared.prompts.lock().get(code).map(|p| {
        let button = match &choice {
            Choice::Id(id) => Some(id.clone()),
            Choice::Number(n) => n.checked_sub(1).and_then(|i| p.buttons.get(i)).cloned(),
        };
        let form = button.as_ref().is_some_and(|b| p.forms.contains(b));
        (button, form, p.sent.clone())
    });
    if let Some((_, true, _)) = found {
        crate::turn::reply(&shared, event, "Answer this one with its button.").await;
        return;
    }
    let Some((Some(button), _, sent)) = found else {
        crate::turn::reply(
            &shared,
            event,
            "There is no such question waiting for an answer.",
        )
        .await;
        return;
    };
    answer(
        shared,
        PromptAnswer {
            prompt: code.to_string(),
            button,
            values: std::collections::BTreeMap::new(),
            user: event.author.clone(),
            conversation: event.conversation.clone(),
            sent,
        },
    )
    .await;
}

/// The button a text answer picks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    /// By id (`allow`, `deny`).
    Id(String),
    /// By position, from 1.
    Number(usize),
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
            // Approvals expire with their turn; one whose call was abandoned is closed here.
            let ids: Vec<String> = prompts
                .iter()
                .filter(|(_, p)| !p.busy && p.expires <= Instant::now())
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| prompts.remove(id)).collect()
        };
        for p in expired {
            update(&shared, &p.sent, p.text, &p.markdown, "⌛", false).await;
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
