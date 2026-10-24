//! Requests from other members: authentication, admission, and tasks handed to the runtime as
//! events of the `peer:<team>` channel.

use std::sync::Arc;

use okbot_core::{
    ConversationKey, InboundEvent, PeerCaller, PeerTaskRecord, PeerTaskState, PromptAnswer, UserRef,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::a2a::{self, Part, code};
use crate::node::{Inner, OpenPrompt, Running, SCHEME, Team, lock};
use crate::token::{self, Claims};

/// Longest task text accepted.
const MAX_TASK_CHARS: usize = 20_000;

/// A response to send back.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    /// The signature, once the caller is authenticated.
    pub(crate) signature: Option<String>,
}

impl Reply {
    fn plain(status: u16, text: &str) -> Self {
        Self {
            status,
            body: text.as_bytes().to_vec(),
            signature: None,
        }
    }
}

/// Handles one request for `team`: `GET` (the Agent Card) or `POST` (JSON-RPC).
pub(crate) async fn handle(
    inner: &Arc<Inner>,
    team: &str,
    method: &str,
    authorization: Option<&str>,
    version: Option<&str>,
    body: &[u8],
) -> Reply {
    let Some(team) = inner.teams.get(team) else {
        return Reply::plain(404, "no such team");
    };
    let authenticated = if team.config.id == crate::external::TEAM {
        bearer(inner, authorization)
    } else {
        authenticate(inner, team, authorization, body)
            .await
            .map(|claims| claims.iss)
    };
    let caller = match authenticated {
        Ok(caller) => caller,
        Err(reason) => {
            tracing::warn!(team = %team.config.id, %reason, "peer request refused");
            return Reply::plain(401, &reason);
        }
    };
    let response = match method {
        _ if version.is_some() && a2a::Version::parse(version).is_none() => a2a::error(
            &Value::Null,
            code::VERSION_NOT_SUPPORTED,
            "okbot speaks A2A 1.0 and 0.3",
        ),
        "GET" => card(team, &caller),
        "POST" => rpc(inner, team, &caller, body).await,
        _ => return Reply::plain(405, "GET or POST"),
    };
    let body = response.to_string().into_bytes();
    Reply {
        status: 200,
        signature: Some(inner.sign_response(team, &caller, &body)),
        body,
    }
}

/// An `[[a2a.client]]` by its bearer token.
fn bearer(inner: &Inner, authorization: Option<&str>) -> Result<String, String> {
    let token = authorization
        .and_then(|a| a.strip_prefix("Bearer "))
        .ok_or("a bearer token is required")?;
    inner
        .external
        .as_ref()
        .and_then(|e| e.client(token.trim()))
        .map(|c| c.name.clone())
        .ok_or_else(|| "unknown token".into())
}

/// Checks the token: a member of the team, signed by its key, for this bot, fresh and new.
async fn authenticate(
    inner: &Arc<Inner>,
    team: &Team,
    authorization: Option<&str>,
    body: &[u8],
) -> Result<Claims, String> {
    let token = authorization
        .and_then(|a| a.strip_prefix(SCHEME))
        .ok_or("an OkbotPeer token is required")?;
    let claims = token::peek(token).map_err(|e| e.to_string())?;
    if claims.team != team.config.id || claims.aud != team.config.member {
        return Err("the token is for another team or member".into());
    }
    let mut manifest = team.manifest();
    if claims.ver > manifest.team.version && team.may_force_refresh() {
        // The caller knows a newer manifest: read it before deciding.
        if let Err(error) = inner.refresh(team).await {
            tracing::warn!(team = %team.config.id, %error, "cannot refresh the team manifest");
        }
        manifest = team.manifest();
    }
    manifest
        .check_fresh(inner.now(), team.signed())
        .map_err(|e| e.to_string())?;
    let key = Inner::key_of(&manifest, &claims.iss).map_err(|e| e.to_string())?;
    let claims = token::verify(token, &key, body, inner.now()).map_err(|e| e.to_string())?;
    if !inner.replays.first_use(&claims, inner.now()) {
        return Err("a replayed token".into());
    }
    Ok(claims)
}

fn card(team: &Team, caller: &str) -> Value {
    let manifest = team.manifest();
    let me = &team.config.member;
    if !team.config.accept || !manifest.may_call(caller, me) {
        return json!({"error": "this member does not take tasks from you"});
    }
    let member = &manifest.members[me];
    a2a::agent_card(
        me,
        &member.description,
        member.url.as_deref().unwrap_or("unix:"),
        &member.skills,
    )
}

async fn rpc(inner: &Arc<Inner>, team: &Team, caller: &str, body: &[u8]) -> Value {
    let Ok(request) = serde_json::from_slice::<a2a::Request>(body) else {
        return a2a::error(&Value::Null, code::PARSE, "not a JSON-RPC request");
    };
    let id = request.id.clone();
    let Some((op, version)) = a2a::op_of(&request.method) else {
        return match crate::relay::handle(inner, team, caller, &request.method, request.params)
            .await
        {
            Some(Ok(result)) => a2a::result(&id, &result),
            Some(Err((code, message))) => a2a::error(&id, code, &message),
            None => a2a::error(&id, code::METHOD_NOT_FOUND, "not offered by okbot"),
        };
    };
    if op == a2a::Op::Send {
        let Some(message) = request
            .params
            .get("message")
            .and_then(a2a::Message::from_wire)
        else {
            return a2a::error(&id, code::INVALID_PARAMS, "invalid message parameters");
        };
        let outcome = match message.task_id.clone() {
            Some(task) => answer(inner, team, caller, &task, &message).await,
            None => submit(inner, team, caller, message).await,
        };
        return match outcome {
            Ok(task) => a2a::result(&id, &a2a::send_result(&task, version)),
            Err((code, message)) => a2a::error(&id, code, &message),
        };
    }
    let Some(task_id) = request.params.get("id").and_then(Value::as_str) else {
        return a2a::error(&id, code::INVALID_PARAMS, "a task id is required");
    };
    let record = match inner.store.peer_task(true, task_id).await {
        Ok(Some(r)) if r.team == team.config.id && r.peer == caller => r,
        Ok(_) => return a2a::error(&id, code::TASK_NOT_FOUND, "no such task"),
        Err(error) => {
            tracing::error!(%error, "cannot read a peer task");
            return a2a::error(&id, code::TASK_NOT_FOUND, "the task is unavailable");
        }
    };
    if op == a2a::Op::Cancel {
        if record.state.is_final() {
            return a2a::error(&id, code::NOT_CANCELABLE, "the task has ended");
        }
        let record = finish(inner, &record, PeerTaskState::Canceled, None).await;
        stop_turn(inner, &record, caller).await;
        return a2a::result(&id, &task_model(inner, &record).to_wire(version));
    }
    a2a::result(&id, &task_model(inner, &record).to_wire(version))
}

type Refusal = (i64, String);

fn refused(message: impl Into<String>) -> Refusal {
    (code::REFUSED, message.into())
}

/// The A2A context id as a safe room part: as is when short and plain, else hashed.
fn room_part(context: &str) -> String {
    let plain = !context.is_empty()
        && context.len() <= 64
        && context
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        context.to_string()
    } else {
        hex::encode(Sha256::digest(context.as_bytes()))[..32].to_string()
    }
}

/// Checks that `caller` may give this bot a task now; returns the delegation context.
fn admit(
    inner: &Inner,
    team: &Team,
    caller: &str,
    message: &a2a::Message,
) -> Result<okbot_core::DelegationContext, Refusal> {
    let manifest = team.manifest();
    let me = &team.config.member;
    if !team.config.accept {
        return Err(refused("this bot does not take tasks from its team"));
    }
    if !manifest.may_call(caller, me) {
        return Err(refused(format!(
            "no link of team `{}` lets `{caller}` call `{me}`",
            team.config.id
        )));
    }
    // A client's own context is not trusted: its task starts a chain of one.
    let client = inner
        .external
        .as_ref()
        .filter(|_| team.config.id == crate::external::TEAM)
        .and_then(|e| e.clients.iter().find(|c| c.name == caller));
    let context = match client {
        Some(client) => crate::external::context(client, inner.now(), team.config.task_timeout),
        None => message.context().ok_or((
            code::INVALID_PARAMS,
            "the okbot delegation context is missing".to_string(),
        ))?,
    };
    if context.chain.last().map(String::as_str) != Some(caller) {
        return Err(refused("the chain must end with the caller"));
    }
    if context.chain.iter().any(|m| m == me) {
        return Err(refused("the work already passed through this bot (a loop)"));
    }
    if context.chain.len() > team.config.max_depth as usize {
        return Err(refused(format!(
            "the chain is longer than this bot accepts ({})",
            team.config.max_depth
        )));
    }
    if context.deadline <= inner.now() {
        return Err(refused("the deadline has passed"));
    }
    if context.budget.turns == 0 {
        return Err(refused("the chain has no agent turns left"));
    }
    if context.budget.tokens == Some(0) {
        return Err(refused("the chain has used its token budget"));
    }
    Ok(context)
}

/// A new task from `caller`.
async fn submit(
    inner: &Arc<Inner>,
    team: &Team,
    caller: &str,
    message: a2a::Message,
) -> Result<a2a::Task, Refusal> {
    let context = admit(inner, team, caller, &message)?;
    let text = message.text();
    if text.trim().is_empty() || text.chars().count() > MAX_TASK_CHARS {
        return Err((
            code::INVALID_PARAMS,
            format!("the task must be 1 to {MAX_TASK_CHARS} characters"),
        ));
    }
    if let Some(existing) = known(inner, team, caller, &message.message_id).await? {
        return Ok(task_model(inner, &existing));
    }
    let Some(sink) = inner.sink.get().cloned() else {
        return Err(refused("this bot is starting; try again in a moment"));
    };
    let context_id = message
        .context_id
        .clone()
        .unwrap_or_else(|| message.message_id.clone());
    let conversation = ConversationKey::room(
        okbot_core::ChannelId::new(okbot_core::PEER_PLATFORM, &team.config.id)
            .expect("team ids are slugs"),
        format!("{caller}/{}", room_part(&context_id)),
    );
    let now = inner.now();
    let record = PeerTaskRecord {
        id: hex::encode(rand::random::<[u8; 12]>()),
        inbound: true,
        team: team.config.id.clone(),
        peer: caller.to_string(),
        context_id,
        message_id: message.message_id.clone(),
        state: PeerTaskState::Working,
        conversation: Some(conversation.clone()),
        trace: context.trace.clone(),
        text: None,
        requester: None,
        direct: false,
        tokens: None,
        created_at: now,
        updated_at: now,
    };
    reserve(
        inner,
        team,
        caller,
        &conversation,
        &record.id,
        context.deadline,
    )?;
    if let Err(error) = inner.store.put_peer_task(record.clone()).await {
        release(inner, &conversation, &record.id);
        // The same message raced in: its task is the answer.
        if let Some(existing) = known(inner, team, caller, &message.message_id).await? {
            return Ok(task_model(inner, &existing));
        }
        tracing::error!(%error, "cannot store a peer task");
        return Err(refused("this bot cannot take tasks right now"));
    }
    let roles = team
        .manifest()
        .members
        .get(caller)
        .map(|m| m.roles.clone())
        .unwrap_or_default();
    let event = event_for(
        &record,
        &conversation,
        caller,
        &context.origin.name,
        text,
        now,
    );
    lock(&inner.callers).insert(
        conversation,
        PeerCaller {
            team: team.config.id.clone(),
            member: caller.to_string(),
            roles,
            context,
        },
    );
    tracing::info!(team = %team.config.id, %caller, task = %record.id, trace = %record.trace, "peer task accepted");
    if sink.deliver(event).await.is_err() {
        let record = finish(
            inner,
            &record,
            PeerTaskState::Failed,
            Some("the bot is stopping".into()),
        )
        .await;
        return Ok(task_model(inner, &record));
    }
    Ok(task_model(inner, &record))
}

/// The task an earlier copy of this message created.
async fn known(
    inner: &Inner,
    team: &Team,
    caller: &str,
    message_id: &str,
) -> Result<Option<PeerTaskRecord>, Refusal> {
    inner
        .store
        .peer_task_by_message(true, &team.config.id, caller, message_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "cannot read peer tasks");
            refused("this bot cannot take tasks right now")
        })
}

/// Takes a place for a task of `caller`, unless it already runs as many as allowed. Counting and
/// taking happen under one lock, so concurrent requests cannot both take the last place.
fn reserve(
    inner: &Inner,
    team: &Team,
    caller: &str,
    conversation: &ConversationKey,
    task: &str,
    deadline: jiff::Timestamp,
) -> Result<(), Refusal> {
    let mut inbound = lock(&inner.inbound);
    let running = inbound
        .values()
        .flatten()
        .filter(|r| r.team == team.config.id && r.caller == caller)
        .count();
    if running >= team.config.max_concurrent as usize {
        return Err(refused(
            "this bot is busy with your other tasks; try again later",
        ));
    }
    inbound
        .entry(conversation.clone())
        .or_default()
        .push_back(Running {
            team: team.config.id.clone(),
            caller: caller.to_string(),
            task: task.to_string(),
            deadline,
        });
    Ok(())
}

/// Frees a place taken for a task that did not start.
fn release(inner: &Inner, conversation: &ConversationKey, task: &str) {
    let mut inbound = lock(&inner.inbound);
    if let Some(queue) = inbound.get_mut(conversation) {
        queue.retain(|r| r.task != task);
        if queue.is_empty() {
            inbound.remove(conversation);
        }
    }
}

/// The event a task becomes: written by the caller, for the origin person.
fn event_for(
    record: &PeerTaskRecord,
    conversation: &ConversationKey,
    caller: &str,
    origin_name: &str,
    text: String,
    now: jiff::Timestamp,
) -> InboundEvent {
    InboundEvent {
        event_id: format!("peer:{}", record.id),
        conversation: conversation.clone(),
        author: UserRef {
            id: caller.into(),
            display_name: if origin_name.is_empty() {
                caller.to_string()
            } else {
                format!("{caller} (for {origin_name})")
            },
            is_bot: false,
        },
        text,
        attachments: Vec::new(),
        mentions_bot: true,
        is_direct: true,
        restricted: false,
        // Replies name the task they answer.
        message_id: Some(record.id.as_str().into()),
        sent_at: now,
        locale: None,
    }
}

/// An answer to a task's question (`input-required`).
async fn answer(
    inner: &Arc<Inner>,
    team: &Team,
    caller: &str,
    task: &str,
    message: &a2a::Message,
) -> Result<a2a::Task, Refusal> {
    let record = match inner.store.peer_task(true, task).await {
        Ok(Some(r)) if r.team == team.config.id && r.peer == caller => r,
        _ => return Err((code::TASK_NOT_FOUND, "no such task".into())),
    };
    let open = lock(&inner.prompts).get(task).cloned();
    let (Some(open), Some(data)) = (open, message.data()) else {
        return Err(refused("the task is not waiting for an answer"));
    };
    let prompt_id = data
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let button_id = data
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let button = open.prompt.buttons.iter().find(|b| b.id == button_id);
    if prompt_id != open.prompt.id || button.is_none_or(|b| b.form.is_some()) {
        return Err((code::INVALID_PARAMS, "no such prompt or button".into()));
    }
    let Some(sink) = inner.sink.get().cloned() else {
        return Err(refused("this bot is stopping"));
    };
    let Some(conversation) = record.conversation.clone() else {
        return Err(refused("the task has no conversation"));
    };
    let answer = PromptAnswer {
        prompt: open.prompt.id.clone(),
        button: button_id.to_string(),
        values: std::collections::BTreeMap::new(),
        user: UserRef {
            id: caller.into(),
            display_name: caller.to_string(),
            is_bot: false,
        },
        conversation,
        sent: open.sent.clone(),
    };
    if sink.answer(answer).await.is_err() {
        return Err(refused("this bot is stopping"));
    }
    Ok(task_model(inner, &record))
}

/// Asks the runtime to stop the turn working on a cancelled task.
async fn stop_turn(inner: &Inner, record: &PeerTaskRecord, caller: &str) {
    let (Some(sink), Some(conversation)) = (inner.sink.get(), record.conversation.clone()) else {
        return;
    };
    let event = InboundEvent {
        event_id: format!("{}{}", okbot_core::PEER_CANCEL_PREFIX, record.id),
        conversation,
        author: UserRef {
            id: caller.into(),
            display_name: caller.to_string(),
            is_bot: false,
        },
        text: String::new(),
        attachments: Vec::new(),
        mentions_bot: true,
        is_direct: true,
        restricted: false,
        message_id: Some(record.id.as_str().into()),
        sent_at: inner.now(),
        locale: None,
    };
    let _ = sink.deliver(event).await;
}

/// Longest progress text kept.
const MAX_PROGRESS_CHARS: usize = 500;

/// What the turn of `task` (or the oldest of `conversation`) is doing now.
pub(crate) fn progress(
    inner: &Inner,
    conversation: &ConversationKey,
    task: Option<&str>,
    text: &str,
) {
    if let Some(task) = task
        .map(str::to_string)
        .or_else(|| oldest(inner, conversation))
    {
        let count = text.chars().count();
        let text: String = text
            .chars()
            .skip(count.saturating_sub(MAX_PROGRESS_CHARS))
            .collect();
        lock(&inner.progress).insert(task, text);
    }
}

/// Tokens the runtime's turn used for `task` (or the oldest of `conversation`).
pub(crate) fn usage(
    inner: &Inner,
    conversation: &ConversationKey,
    task: Option<&str>,
    tokens: u64,
) {
    if let Some(task) = task
        .map(str::to_string)
        .or_else(|| oldest(inner, conversation))
    {
        *lock(&inner.usage).entry(task).or_default() += tokens;
    }
}

/// The runtime could not answer: the task (or the oldest of the conversation) fails, with the
/// older tasks the same turn was answering.
pub(crate) async fn fail(
    inner: &Arc<Inner>,
    conversation: &ConversationKey,
    task: Option<&str>,
    reason: &str,
) {
    end(inner, conversation, task, PeerTaskState::Failed, reason).await;
}

/// Ends an inbound task and frees its place.
pub(crate) async fn finish(
    inner: &Arc<Inner>,
    record: &PeerTaskRecord,
    state: PeerTaskState,
    text: Option<String>,
) -> PeerTaskRecord {
    if let Some(conversation) = &record.conversation {
        let mut inbound = lock(&inner.inbound);
        if let Some(queue) = inbound.get_mut(conversation) {
            queue.retain(|r| r.task != record.id);
            if queue.is_empty() {
                inbound.remove(conversation);
                lock(&inner.callers).remove(conversation);
            }
        }
    }
    lock(&inner.prompts).remove(&record.id);
    let tokens = lock(&inner.usage).remove(&record.id).or(record.tokens);
    lock(&inner.progress).remove(&record.id);
    let record = PeerTaskRecord {
        state,
        text,
        tokens,
        updated_at: inner.now(),
        ..record.clone()
    };
    if let Err(error) = inner.store.put_peer_task(record.clone()).await {
        tracing::error!(%error, "cannot store a peer task");
    }
    tracing::info!(team = %record.team, task = %record.id, state = state.as_str(), "peer task ended");
    record
}

/// A task as A2A sees it.
pub(crate) fn task_model(inner: &Inner, record: &PeerTaskRecord) -> a2a::Task {
    let mut parts = Vec::new();
    if let Some(text) = &record.text {
        parts.push(Part::Text(text.clone()));
    } else if record.state == PeerTaskState::Working
        && let Some(progress) = lock(&inner.progress).get(&record.id)
    {
        parts.push(Part::Text(progress.clone()));
    }
    if record.state == PeerTaskState::InputRequired
        && let Some(open) = lock(&inner.prompts).get(&record.id)
    {
        let buttons: Vec<Value> = open
            .prompt
            .buttons
            .iter()
            .filter(|b| b.form.is_none())
            .map(|b| json!({"id": b.id, "label": b.label}))
            .collect();
        parts.push(Part::Data(
            json!({"prompt": open.prompt.id, "buttons": buttons}),
        ));
    }
    let message = (!parts.is_empty() || record.tokens.is_some()).then(|| {
        let mut message = a2a::Message::agent(parts);
        if let Some(tokens) = record.tokens {
            message.metadata = Some(json!({a2a::USAGE_KEY: {"tokens": tokens}}));
        }
        message
    });
    a2a::Task {
        id: record.id.clone(),
        context_id: record.context_id.clone(),
        state: record.state,
        message,
        timestamp: Some(record.updated_at.to_string()),
    }
}

/// Records a prompt the runtime showed for the task `task` (or the oldest of `conversation`).
pub(crate) async fn prompt_opened(
    inner: &Arc<Inner>,
    conversation: &ConversationKey,
    task: Option<&str>,
    open: OpenPrompt,
) -> bool {
    let Some(task) = task
        .map(str::to_string)
        .or_else(|| oldest(inner, conversation))
    else {
        return false;
    };
    let Ok(Some(record)) = inner.store.peer_task(true, &task).await else {
        return false;
    };
    if record.state.is_final() {
        return false;
    }
    let text = Some(open.prompt.text.clone());
    lock(&inner.prompts).insert(task, open);
    let record = PeerTaskRecord {
        state: PeerTaskState::InputRequired,
        text,
        updated_at: inner.now(),
        ..record
    };
    inner.store.put_peer_task(record).await.is_ok()
}

/// The prompt was answered, cancelled or expired: the task works again.
pub(crate) async fn prompt_closed(inner: &Arc<Inner>, prompt: &str) {
    let task = {
        let mut prompts = lock(&inner.prompts);
        let task = prompts
            .iter()
            .find(|(_, p)| p.prompt.id == prompt)
            .map(|(t, _)| t.clone());
        if let Some(task) = &task {
            prompts.remove(task);
        }
        task
    };
    if let Some(task) = task
        && let Ok(Some(record)) = inner.store.peer_task(true, &task).await
        && record.state == PeerTaskState::InputRequired
    {
        let record = PeerTaskRecord {
            state: PeerTaskState::Working,
            text: None,
            updated_at: inner.now(),
            ..record
        };
        let _ = inner.store.put_peer_task(record).await;
    }
}

/// The runtime's reply to task `task` completes it, and the older tasks of its conversation that
/// the same turn answered (messages of one conversation are answered together). Without a task,
/// the oldest is meant. Returns whether a task took the reply.
pub(crate) async fn replied(
    inner: &Arc<Inner>,
    conversation: &ConversationKey,
    task: Option<&str>,
    text: &str,
) -> bool {
    end(inner, conversation, task, PeerTaskState::Completed, text).await
}

/// Ends task `task` (or the oldest of `conversation`) and the older ones of the conversation.
async fn end(
    inner: &Arc<Inner>,
    conversation: &ConversationKey,
    task: Option<&str>,
    state: PeerTaskState,
    text: &str,
) -> bool {
    let ended: Vec<String> = {
        let inbound = lock(&inner.inbound);
        let Some(queue) = inbound.get(conversation) else {
            return false;
        };
        match task {
            Some(task) => match queue.iter().position(|r| r.task == task) {
                Some(at) => queue.iter().take(at + 1).map(|r| r.task.clone()).collect(),
                None => return false,
            },
            None => queue.front().map(|r| r.task.clone()).into_iter().collect(),
        }
    };
    let mut any = false;
    for id in ended {
        if let Ok(Some(record)) = inner.store.peer_task(true, &id).await
            && !record.state.is_final()
        {
            finish(inner, &record, state, Some(text.to_string())).await;
            any = true;
        }
    }
    any
}

fn oldest(inner: &Inner, conversation: &ConversationKey) -> Option<String> {
    lock(&inner.inbound)
        .get(conversation)
        .and_then(|q| q.front())
        .map(|r| r.task.clone())
}

/// Fails tasks past their deadline (the runtime never answered them).
pub(crate) async fn expire(inner: &Arc<Inner>) {
    let now = inner.now();
    let late: Vec<String> = lock(&inner.inbound)
        .values()
        .flatten()
        .filter(|r| r.deadline <= now)
        .map(|r| r.task.clone())
        .collect();
    for task in late {
        if let Ok(Some(record)) = inner.store.peer_task(true, &task).await {
            finish(
                inner,
                &record,
                PeerTaskState::Failed,
                Some("the deadline passed".into()),
            )
            .await;
        }
    }
}
