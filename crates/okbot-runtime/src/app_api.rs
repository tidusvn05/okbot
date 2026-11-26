//! The App API (ADR 0019): `POST /apps/api/session` turns a launch ticket into a session
//! cookie, `POST /apps/api/logout` ends it, `POST /apps/api/v1/<method>` runs one method as the
//! signed-in person.
//!
//! Only okbot's shell calls these: apps run in a sandboxed frame with no network and reach the
//! API through the shell. Every call checks the session, the CSRF header, where the request
//! comes from, the app's granted permissions and the person's access now.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    AppPermission, AppSource, NewTask, Task, TaskChange, TaskError, TaskKind, TaskStatus,
};
use serde_json::{Value, json};

use crate::Shared;
use crate::apps::{self, Context, Session};

/// The largest request body.
const MAX_BODY: usize = 64 * 1024;

/// A failed call, as the shell sees it.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// More fields for the error object, for the shell.
    details: Option<serde_json::Map<String, Value>>,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", message)
    }

    fn signed_out() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "signed_out", "signed out")
    }

    pub(crate) fn internal(shared: &Shared) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed",
            shared.config.replies.error.clone(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = self.details.unwrap_or_default();
        error.insert("code".into(), self.code.into());
        error.insert("message".into(), self.message.into());
        json_response(self.status, &json!({ "error": error }))
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut response = (status, body.to_string()).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The session id in the request's cookie.
fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == apps::COOKIE && !value.is_empty()).then(|| value.to_string())
        })
}

/// The public origin (`https://bot.example.com`) of a base URL.
fn origin_of(base: &str) -> String {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or(rest);
    let scheme = base.split_once("://").map_or("https", |(s, _)| s);
    format!("{scheme}://{host}")
}

/// The path apps live under at the public URL (`/apps`, or `/bot/apps` behind a prefix).
fn cookie_path(shared: &Shared) -> String {
    let path = shared
        .config
        .apps
        .public_url
        .as_deref()
        .and_then(|u| u.split_once("://"))
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, p)| format!("/{}", p.trim_end_matches('/')))
        .unwrap_or_default();
    format!("{path}/apps")
}

/// Only the bot's own pages may call: the browser says so (`Sec-Fetch-Site`, `Origin`), and
/// neither can be set by a page of another site.
fn same_origin(shared: &Shared, headers: &HeaderMap) -> bool {
    if header_str(headers, "sec-fetch-site") == Some("same-origin") {
        return true;
    }
    let Some(origin) = header_str(headers, "origin") else {
        return false;
    };
    if let Some(public) = &shared.config.apps.public_url {
        return origin == origin_of(public);
    }
    // Loopback (the console): the browser may say localhost or the address.
    shared
        .apps
        .as_ref()
        .and_then(|a| a.loopback.get())
        .is_some_and(|local| {
            origin == local || origin == local.replacen("127.0.0.1", "localhost", 1)
        })
}

fn set_cookie(shared: &Shared, value: &str, max_age: SignedDuration) -> HeaderValue {
    let secure = if shared.config.apps.public_url.is_some() {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={value}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        apps::COOKIE,
        cookie_path(shared),
        max_age.as_secs().max(0)
    );
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// When a session used at `now` ends.
fn expiry(shared: &Shared, session: &Session, now: Timestamp) -> Timestamp {
    let idle =
        SignedDuration::try_from(shared.config.apps.session_idle).unwrap_or(SignedDuration::MAX);
    let max =
        SignedDuration::try_from(shared.config.apps.session_max).unwrap_or(SignedDuration::MAX);
    let by_use = now.checked_add(idle).unwrap_or(Timestamp::MAX);
    let by_age = session.created.checked_add(max).unwrap_or(Timestamp::MAX);
    by_use.min(by_age)
}

/// Keeps a session. A `fresh` one is stored; any other is only updated, so a request that was
/// in flight when the session was signed out or revoked cannot bring it back.
async fn save(
    shared: &Shared,
    handle: &str,
    session: &Session,
    now: Timestamp,
    fresh: bool,
) -> Result<Timestamp, ApiError> {
    let expires = expiry(shared, session, now);
    let event = apps::event_for(
        &session.user,
        &session.contexts[0].conversation,
        session.contexts[0].direct,
        now,
    );
    let data = serde_json::to_string(session).map_err(|_| ApiError::internal(shared))?;
    let kept = if fresh {
        let person = shared.person_key(&event).await;
        shared
            .store
            .put_app_session(handle, &person, data, expires)
            .await
            .map(|()| true)
    } else {
        shared.store.update_app_session(handle, data, expires).await
    };
    match kept {
        Ok(true) => Ok(expires),
        Ok(false) => Err(ApiError::signed_out()),
        Err(error) => {
            tracing::error!(%error, "cannot keep an app session");
            Err(ApiError::internal(shared))
        }
    }
}

async fn load(shared: &Shared, headers: &HeaderMap, now: Timestamp) -> Option<(String, Session)> {
    let id = cookie(headers)?;
    let handle = apps::hash(&id);
    let data = shared
        .store
        .app_session(&handle, now)
        .await
        .ok()
        .flatten()?;
    let mut session = serde_json::from_str::<Session>(&data).ok()?;
    // Rooms not pressed in lately drop out (apps::ROOM_CONTEXT_TTL).
    session.contexts.retain(|c| c.current(now));
    (!session.contexts.is_empty()).then_some((handle, session))
}

/// The apps `session` may open from context `i`.
async fn apps_for(shared: &Shared, session: &Session, context: &Context) -> Vec<Value> {
    let Some(state) = &shared.apps else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for info in state.directory.apps() {
        if let Ok(a) = apps::admit(
            shared,
            &session.user,
            &context.conversation,
            context.direct,
            &info.name,
        )
        .await
        {
            out.push(json!({
                "name": a.info.name,
                "title": a.info.title(session.locale.as_deref()),
                "view": a.info.default_view(),
                "unverified": a.info.source == AppSource::Local,
            }));
        }
    }
    out
}

/// A ticket's sign-in: the session it adds its place to (or a new one) and what to open.
struct SignIn {
    handle: String,
    session: Session,
    /// The place it opened ([`Context::id`]).
    current: String,
    open: Value,
    /// A new session's id, for the cookie.
    new_cookie: Option<String>,
}

/// The answer to a ticket for another account than the signed-in one: the ticket is put back,
/// and the shell asks the person whether to switch.
async fn other_account(
    shared: &Shared,
    secret: &str,
    data: String,
    ticket: &apps::Ticket,
    current: &Session,
) -> ApiError {
    if let Some(expires) = ticket.expires {
        let _ = shared
            .store
            .put_app_ticket(&apps::hash(secret), data, expires)
            .await;
    }
    let mut details = serde_json::Map::new();
    details.insert("name".into(), ticket.user.display_name.clone().into());
    details.insert(
        "platform".into(),
        ticket.conversation.channel.platform().into(),
    );
    details.insert("current".into(), current.user.display_name.clone().into());
    ApiError {
        details: Some(details),
        ..ApiError::new(
            StatusCode::CONFLICT,
            "other_account",
            "this link signs in another account",
        )
    }
}

async fn sign_in(
    shared: &Shared,
    secret: &str,
    replace: bool,
    existing: Option<(String, Session)>,
    now: Timestamp,
) -> Result<SignIn, ApiError> {
    let mut new_cookie = None;
    let taken = shared
        .store
        .take_app_ticket(&apps::hash(secret), now)
        .await
        .ok()
        .flatten();
    let Some((data, ticket)) = taken.and_then(|d| {
        let t = serde_json::from_str::<apps::Ticket>(&d).ok()?;
        Some((d, t))
    }) else {
        return Err(ApiError::new(
            StatusCode::GONE,
            "expired",
            "this link has expired or was used",
        ));
    };
    let same = |s: &Session| {
        s.user.id == ticket.user.id && s.channel() == Some(&ticket.conversation.channel)
    };
    // Someone else's link would sign this browser out of its own account and into theirs
    // (login CSRF): the person confirms first. The ticket stays usable until then.
    if let Some((_, current)) = existing.as_ref().filter(|(_, s)| !same(s))
        && !replace
    {
        return Err(other_account(shared, secret, data, &ticket, current).await);
    }
    // Access is checked again at the exchange: the lists may have changed since the press.
    if let Err(reason) = apps::admit(
        shared,
        &ticket.user,
        &ticket.conversation,
        ticket.direct,
        &ticket.app,
    )
    .await
    {
        return Err(ApiError::forbidden(reason));
    }
    let context = Context {
        conversation: ticket.conversation.clone(),
        direct: ticket.direct,
        proven: Some(now),
    };
    // The same account adds a place; anyone else signs in afresh (with a new cookie).
    let (handle, mut session) = match existing.filter(|(_, s)| same(s)) {
        Some(found) => found,
        None => {
            let id = apps::secret();
            let handle = apps::hash(&id);
            new_cookie = Some(id);
            (
                handle,
                Session {
                    user: ticket.user.clone(),
                    contexts: Vec::new(),
                    csrf: apps::secret(),
                    created: now,
                    touched: now,
                    locale: ticket.locale.clone(),
                },
            )
        }
    };
    session
        .user
        .display_name
        .clone_from(&ticket.user.display_name);
    let current = context.id();
    match session.contexts.iter_mut().find(|c| c.same_place(&context)) {
        Some(c) => c.proven = Some(now),
        None => {
            session.contexts.push(context);
            // A handful of places is plenty; the oldest goes first. Places are addressed by
            // id, so the others keep theirs.
            if session.contexts.len() > 8 {
                session.contexts.remove(0);
            }
        }
    }
    let open = json!({"app": ticket.app, "view": ticket.view, "params": ticket.params});
    tracing::info!(app = %ticket.app, user = %ticket.user.id, "app session signed in");
    Ok(SignIn {
        handle,
        session,
        current,
        open,
        new_cookie,
    })
}

/// `POST /apps/api/session`: a ticket (`{"ticket": "…"}`) signs in or adds where it came from
/// to the session; without one, the current session is described.
pub(crate) async fn session(shared: &Shared, headers: &HeaderMap, body: &[u8]) -> Response {
    if !same_origin(shared, headers) {
        return ApiError::forbidden("not from this bot's pages").into_response();
    }
    let body = serde_json::from_slice::<Value>(body).unwrap_or_default();
    let ticket = body
        .get("ticket")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|t| !t.is_empty() && t.len() <= 100);
    let replace = body.get("replace").and_then(Value::as_bool) == Some(true);
    let now = shared.clock.now();
    let existing = load(shared, headers, now).await;
    let mut new_cookie = None;
    let (handle, mut session, current, open) = if let Some(ticket) = ticket {
        match sign_in(shared, &ticket, replace, existing, now).await {
            Ok(done) => {
                new_cookie = done.new_cookie;
                (done.handle, done.session, done.current, done.open)
            }
            Err(error) => return error.into_response(),
        }
    } else if let Some((handle, session)) = existing {
        let first = session.contexts[0].id();
        (handle, session, first, Value::Null)
    } else {
        return ApiError::signed_out().into_response();
    };
    session.touched = now;
    let expires = match save(shared, &handle, &session, now, new_cookie.is_some()).await {
        Ok(e) => e,
        Err(e) => return e.into_response(),
    };
    let mut contexts = Vec::new();
    let mut apps_here = Value::Array(Vec::new());
    for c in &session.contexts {
        let id = c.id();
        let apps = Value::Array(apps_for(shared, &session, c).await);
        if id == current {
            apps_here = apps.clone();
        }
        contexts.push(json!({
            "id": id,
            "label": c.label(),
            "direct": c.direct,
            "apps": apps,
        }));
    }
    let body = json!({
        "bot": shared.config.bot_name,
        "account": {
            "name": session.user.display_name,
            "platform": session.contexts[0].conversation.channel.platform(),
            "id": format!("{}/{}", session.contexts[0].conversation.channel, session.user.id),
        },
        "contexts": contexts,
        "context": current,
        "apps": apps_here,
        "open": open,
        "csrf": session.csrf,
        "locale": session.locale,
        "expires": expires.to_string(),
    });
    let mut response = json_response(StatusCode::OK, &body);
    if let Some(id) = new_cookie {
        let max = SignedDuration::try_from(shared.config.apps.session_max)
            .unwrap_or(SignedDuration::from_hours(24 * 7));
        response
            .headers_mut()
            .insert(header::SET_COOKIE, set_cookie(shared, &id, max));
    }
    response
}

/// `POST /apps/api/logout`.
pub(crate) async fn logout(shared: &Shared, headers: &HeaderMap) -> Response {
    if !same_origin(shared, headers) {
        return ApiError::forbidden("not from this bot's pages").into_response();
    }
    if let Some((handle, session)) = load(shared, headers, shared.clock.now()).await
        && header_str(headers, "x-okbot-csrf") == Some(session.csrf.as_str())
        && let Err(error) = shared.store.end_app_sessions(Some(&handle), None).await
    {
        tracing::error!(%error, "cannot end an app session");
    }
    let mut response = json_response(StatusCode::OK, &json!({"result": true}));
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(shared, "", SignedDuration::ZERO),
    );
    response
}

/// `POST /apps/api/v1/<method>`.
pub(crate) async fn call(
    shared: &Shared,
    method: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    match call_checked(shared, method, headers, body).await {
        Ok(result) => json_response(StatusCode::OK, &json!({ "result": result })),
        Err(error) => error.into_response(),
    }
}

async fn call_checked(
    shared: &Shared,
    method: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Value, ApiError> {
    if !same_origin(shared, headers) {
        return Err(ApiError::forbidden("not from this bot's pages"));
    }
    let now = shared.clock.now();
    let (handle, mut session) = load(shared, headers, now)
        .await
        .ok_or_else(ApiError::signed_out)?;
    if header_str(headers, "x-okbot-csrf") != Some(session.csrf.as_str()) {
        return Err(ApiError::forbidden("missing or wrong x-okbot-csrf"));
    }
    if !shared
        .apps
        .as_ref()
        .is_some_and(|a| a.count_call(&handle, now))
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many calls; wait a minute",
        ));
    }
    let app = header_str(headers, "x-okbot-app").unwrap_or_default();
    let context = header_str(headers, "x-okbot-context")
        .and_then(|id| session.contexts.iter().find(|c| c.id() == id))
        .cloned()
        .ok_or_else(|| ApiError::invalid("x-okbot-context names no place of this session"))?;
    let admitted = apps::admit(
        shared,
        &session.user,
        &context.conversation,
        context.direct,
        app,
    )
    .await
    .map_err(ApiError::forbidden)?;
    let params_early: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let needed = if method == "mcp.call" {
        crate::app_data::mcp_permission(&params_early)
            .ok_or_else(|| ApiError::invalid("mcp.call needs `server` and `tool`"))?
    } else {
        permission_of(method).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("no method `{method}`"),
            )
        })?
    };
    if !admitted.granted.contains(&needed) {
        return Err(ApiError::forbidden(format!(
            "{app} was not granted `{needed}`"
        )));
    }
    let params: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).map_err(|e| ApiError::invalid(format!("not JSON: {e}")))?
    };
    if !params.is_object() {
        return Err(ApiError::invalid("parameters are a JSON object"));
    }
    // Sliding expiry, written at most once a minute.
    if now.duration_since(session.touched) > SignedDuration::from_secs(60) {
        session.touched = now;
        save(shared, &handle, &session, now, false).await?;
    }
    let call = Call {
        shared,
        admitted: &admitted,
        context: &context,
        session: &session,
        params: &params,
    };
    let started = std::time::Instant::now();
    let result = call.run(method).await;
    if method_writes(method) {
        tracing::info!(app = %app, method, user = %session.user.id, ok = result.is_ok(), "app write");
        // Writes are audited like `[[command]]` tool calls, under okbot's own `bot` server
        // (a reserved `[[mcp]]` name) with the app as the source (ADR 0019).
        crate::gateway::audit(
            shared,
            okbot_core::ToolCallRecord {
                run_id: None,
                command: Some(format!("app:{app}")),
                person: shared.person_key(&admitted.event).await,
                conversation: admitted.event.conversation.clone(),
                server: "bot".to_string(),
                tool: method.to_string(),
                decision: okbot_core::ToolDecision::Allowed,
                error: result.as_ref().err().map(|e| e.message.clone()),
                duration: started.elapsed(),
                at: shared.clock.now(),
            },
        )
        .await;
    }
    result
}

/// The permission each method needs.
fn permission_of(method: &str) -> Option<AppPermission> {
    Some(match method {
        "me" => AppPermission::Me,
        "tasks.list" | "tasks.get" => AppPermission::TasksRead,
        "tasks.add" | "tasks.update" | "tasks.done" | "tasks.pause" | "tasks.resume"
        | "tasks.snooze" | "tasks.cancel" => AppPermission::TasksWrite,
        _ => return crate::app_data::permission_of(method),
    })
}

fn method_writes(method: &str) -> bool {
    method.starts_with("tasks.") && !matches!(method, "tasks.list" | "tasks.get")
        || crate::app_data::writes(method)
}

/// A field of a change: left alone, cleared or set.
enum Field<T> {
    Keep,
    Clear,
    Set(T),
}

impl<T> Field<T> {
    /// As a [`TaskChange`] field: `None` keeps, `Some(None)` clears.
    #[allow(clippy::option_option)] // the shape of `TaskChange`
    fn change(self) -> Option<Option<T>> {
        match self {
            Self::Keep => None,
            Self::Clear => Some(None),
            Self::Set(v) => Some(Some(v)),
        }
    }

    /// The value, when set.
    fn value(self) -> Option<T> {
        match self {
            Self::Set(v) => Some(v),
            _ => None,
        }
    }
}

pub(crate) struct Call<'a> {
    pub(crate) shared: &'a Shared,
    pub(crate) admitted: &'a apps::Admitted,
    pub(crate) context: &'a Context,
    pub(crate) session: &'a Session,
    pub(crate) params: &'a Value,
}

impl Call<'_> {
    pub(crate) fn str(&self, key: &str) -> Option<&str> {
        self.params
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub(crate) fn required(&self, key: &str) -> Result<&str, ApiError> {
        self.str(key)
            .ok_or_else(|| ApiError::invalid(format!("`{key}` is required")))
    }

    async fn run(&self, method: &str) -> Result<Value, ApiError> {
        match method {
            "me" => Ok(self.me().await),
            "tasks.list" => self.tasks_list().await,
            "tasks.get" => {
                let id = self.required("id")?;
                let task = self.find(id).await?;
                Ok(self.task_json(&task))
            }
            "tasks.add" => self.tasks_add().await,
            "tasks.update" => self.tasks_update().await,
            "tasks.done" => self.set_status(TaskStatus::Done).await,
            "tasks.cancel" => self.set_status(TaskStatus::Cancelled).await,
            "tasks.pause" => self.set_status(TaskStatus::Paused).await,
            "tasks.resume" => self.set_status(TaskStatus::Open).await,
            "tasks.snooze" => self.snooze().await,
            _ => crate::app_data::run(self, method).await,
        }
    }

    async fn me(&self) -> Value {
        let event = &self.admitted.event;
        json!({
            "name": self.session.user.display_name,
            "platform": event.conversation.channel.platform(),
            "account": format!("{}/{}", event.conversation.channel, self.session.user.id),
            "context": {"label": self.context.label(), "direct": self.context.direct},
            "locale": self.session.locale,
            "timezone": self.shared.config.timezone.clone().unwrap_or_else(|| "UTC".into()),
            "admin": self.shared.is_admin(event).await,
            "bot": self.shared.config.bot_name,
            "app": self.admitted.info.name,
            "permissions": self.admitted.granted.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }

    fn store(&self) -> Result<&Arc<dyn okbot_core::TaskStore>, ApiError> {
        self.shared.tasks.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "tasks are not enabled for this bot",
            )
        })
    }

    pub(crate) async fn requester(&self) -> okbot_core::Requester {
        crate::command::requester(self.shared, &self.admitted.event).await
    }

    pub(crate) fn task_error(&self, error: TaskError) -> ApiError {
        match error {
            TaskError::NotFound(id) => ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("no task `{id}` here"),
            ),
            TaskError::Forbidden(m) => ApiError::forbidden(m),
            TaskError::Invalid(m) => ApiError::invalid(m),
            TaskError::Backend(error) => {
                tracing::error!(%error, "a task call failed");
                ApiError::internal(self.shared)
            }
            other => ApiError::invalid(other.to_string()),
        }
    }

    fn task_json(&self, t: &Task) -> Value {
        let tz = self.shared.config.time_zone();
        let now = self.shared.clock.now();
        let time = |ts: Option<Timestamp>| ts.map(|ts| okbot_core::format_time(ts, &tz));
        json!({
            "id": t.id,
            "short": t.id.rsplit('/').next().unwrap_or(&t.id),
            "kind": t.kind.as_str(),
            "status": t.status.as_str(),
            "title": t.title,
            "notes": t.notes,
            "scope": if t.scope == okbot_core::MemoryScope::Room { "room" } else { "personal" },
            "due": time(t.due),
            "remind_at": time(t.remind_at),
            "repeat": t.repeat.as_ref().map(ToString::to_string),
            "overdue": t.status.is_active() && t.due.is_some_and(|d| d < now),
            "created": okbot_core::format_time(t.created, &tz),
        })
    }

    async fn find(&self, id: &str) -> Result<Task, ApiError> {
        let who = self.requester().await;
        let tasks = self
            .store()?
            .list(&who)
            .await
            .map_err(|e| self.task_error(e))?;
        tasks
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such task here"))
    }

    async fn tasks_list(&self) -> Result<Value, ApiError> {
        let who = self.requester().await;
        let mut tasks = self
            .store()?
            .list(&who)
            .await
            .map_err(|e| self.task_error(e))?;
        let status = self.str("status").unwrap_or("active");
        tasks.retain(|t| match status {
            "all" => true,
            "done" => !t.status.is_active(),
            _ => t.status.is_active(),
        });
        if let Some(kind) = self.str("kind") {
            let kind: TaskKind = kind.parse().map_err(ApiError::invalid)?;
            tasks.retain(|t| t.kind == kind);
        }
        if let Some(q) = self.str("query") {
            let q = q.to_lowercase();
            tasks.retain(|t| {
                t.title.to_lowercase().contains(&q) || t.notes.to_lowercase().contains(&q)
            });
        }
        let total = tasks.len();
        let start = match self.str("cursor") {
            Some(c) => c
                .parse::<usize>()
                .map_err(|_| ApiError::invalid("bad cursor"))?,
            None => 0,
        };
        let limit = self
            .params
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(50, |l| usize::try_from(l.clamp(1, 100)).unwrap_or(50));
        let items: Vec<Value> = tasks
            .iter()
            .skip(start)
            .take(limit)
            .map(|t| self.task_json(t))
            .collect();
        let next = (start + items.len() < total).then(|| (start + items.len()).to_string());
        Ok(json!({"items": items, "total": total, "next": next}))
    }

    fn time(&self, key: &str) -> Result<Field<Timestamp>, ApiError> {
        match self.params.get(key) {
            None => Ok(Field::Keep),
            Some(Value::Null) => Ok(Field::Clear),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(Field::Clear),
            Some(Value::String(s)) => okbot_core::parse_time(s, &self.shared.config.time_zone())
                .map(Field::Set)
                .map_err(|e| ApiError::invalid(format!("{key}: {e}"))),
            Some(_) => Err(ApiError::invalid(format!("{key}: a time as text"))),
        }
    }

    pub(crate) fn text(&self, key: &str, max: usize) -> Result<Option<String>, ApiError> {
        match self.params.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s.trim().to_string())),
            Some(Value::String(_)) => Err(ApiError::invalid(format!(
                "{key}: at most {max} characters"
            ))),
            Some(_) => Err(ApiError::invalid(format!("{key}: text"))),
        }
    }

    async fn tasks_add(&self) -> Result<Value, ApiError> {
        let title = self
            .text("title", 200)?
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ApiError::invalid("`title` is required"))?;
        let kind = match self.str("kind") {
            Some(k) => k.parse::<TaskKind>().map_err(ApiError::invalid)?,
            None => TaskKind::Todo,
        };
        // Routines run the agent unattended: they are made in chat, where they start paused.
        if kind == TaskKind::Routine {
            return Err(ApiError::forbidden(
                "routines are created in chat, where they start paused",
            ));
        }
        let repeat = match self.str("repeat") {
            Some(r) => Some(r.parse::<okbot_core::Repeat>().map_err(ApiError::invalid)?),
            None => None,
        };
        let remind_at = self.time("remind_at")?.value();
        if kind == TaskKind::Reminder && remind_at.is_none() && repeat.is_none() {
            return Err(ApiError::invalid(
                "a reminder needs `remind_at` or `repeat`",
            ));
        }
        let who = self.requester().await;
        let task = NewTask {
            kind,
            title,
            notes: self.text("notes", 2000)?.unwrap_or_default(),
            // Personal only where personal data is reachable, as for `/note`.
            scope: if who.personal {
                okbot_core::MemoryScope::User
            } else {
                okbot_core::MemoryScope::Room
            },
            due: self.time("due")?.value(),
            remind_at,
            repeat,
            deliver_to: None,
            paused: false,
        };
        let task = self
            .store()?
            .add(&who, task)
            .await
            .map_err(|e| self.task_error(e))?;
        Ok(self.task_json(&task))
    }

    async fn tasks_update(&self) -> Result<Value, ApiError> {
        let id = self.required("id")?;
        let current = self.find(id).await?;
        let repeat = match self.params.get("repeat") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) if s.trim().is_empty() => Some(None),
            Some(Value::String(s)) => Some(Some(
                s.parse::<okbot_core::Repeat>().map_err(ApiError::invalid)?,
            )),
            Some(_) => return Err(ApiError::invalid("repeat: text")),
        };
        if current.kind == TaskKind::Routine
            && (repeat.is_some() || self.params.get("notes").is_some())
        {
            // A routine's notes are its prompt: changing what runs unattended happens in chat.
            return Err(ApiError::forbidden(
                "a routine's prompt and schedule are changed in chat",
            ));
        }
        let change = TaskChange {
            title: self.text("title", 200)?.filter(|t| !t.is_empty()),
            notes: self.text("notes", 2000)?,
            status: None,
            due: self.time("due")?.change(),
            remind_at: self.time("remind_at")?.change(),
            repeat,
        };
        let who = self.requester().await;
        let task = self
            .store()?
            .update(&who, id, change)
            .await
            .map_err(|e| self.task_error(e))?;
        Ok(self.task_json(&task))
    }

    async fn set_status(&self, status: TaskStatus) -> Result<Value, ApiError> {
        let id = self.required("id")?;
        let current = self.find(id).await?;
        let mut change = TaskChange {
            status: Some(status),
            ..TaskChange::default()
        };
        if status == TaskStatus::Open {
            // A routine runs the agent unattended: only an administrator starts one (as `/resume`).
            if current.kind == TaskKind::Routine
                && !self.shared.is_admin(&self.admitted.event).await
            {
                return Err(ApiError::forbidden(
                    "only the owner of this assistant can start a routine",
                ));
            }
            if current.repeat.is_some()
                && current
                    .remind_at
                    .is_none_or(|at| at <= self.shared.clock.now())
            {
                change.remind_at = Some(None);
            }
        }
        let who = self.requester().await;
        let task = self
            .store()?
            .update(&who, id, change)
            .await
            .map_err(|e| self.task_error(e))?;
        Ok(self.task_json(&task))
    }

    async fn snooze(&self) -> Result<Value, ApiError> {
        let id = self.required("id")?;
        let current = self.find(id).await?;
        if current.kind == TaskKind::Routine {
            return Err(ApiError::invalid("routines are paused, not snoozed"));
        }
        let tz = self.shared.config.time_zone();
        let until = crate::command::snooze_until(
            self.str("until").unwrap_or_default(),
            self.shared.clock.now(),
            &tz,
        )
        .ok_or_else(|| ApiError::invalid("until: `10m`, `2h`, `15:30` or a date-time"))?;
        let change = TaskChange {
            status: Some(TaskStatus::Open),
            remind_at: Some(Some(until)),
            ..TaskChange::default()
        };
        let who = self.requester().await;
        let task = self
            .store()?
            .update(&who, id, change)
            .await
            .map_err(|e| self.task_error(e))?;
        Ok(self.task_json(&task))
    }
}

/// Adds the App API routes to `router`.
pub(crate) fn routes(router: Router, shared: &Arc<Shared>) -> Router {
    let limit = axum::extract::DefaultBodyLimit::max(MAX_BODY);
    let s1 = shared.clone();
    let s2 = shared.clone();
    let s3 = shared.clone();
    router
        .route(
            "/apps/api/session",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                let s = s1.clone();
                async move { session(&s, &headers, &body).await }
            })
            .layer(limit),
        )
        .route(
            "/apps/api/logout",
            axum::routing::post(move |headers: HeaderMap| {
                let s = s2.clone();
                async move { logout(&s, &headers).await }
            }),
        )
        .route(
            "/apps/api/v1/{method}",
            axum::routing::post(
                move |axum::extract::Path(method): axum::extract::Path<String>,
                      headers: HeaderMap,
                      body: Bytes| {
                    let s = s3.clone();
                    async move { call(&s, &method, &headers, &body).await }
                },
            )
            .layer(limit),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_cookies() {
        assert_eq!(
            origin_of("https://bot.example.com"),
            "https://bot.example.com"
        );
        assert_eq!(
            origin_of("https://bot.example.com/shop"),
            "https://bot.example.com"
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; okbot_app=xyz; b=2"),
        );
        assert_eq!(cookie(&headers).as_deref(), Some("xyz"));
        headers.insert(header::COOKIE, HeaderValue::from_static("okbot_app="));
        assert_eq!(cookie(&headers), None);
    }

    #[test]
    fn every_method_needs_a_permission() {
        for m in [
            "me",
            "tasks.list",
            "tasks.get",
            "tasks.add",
            "tasks.update",
            "tasks.done",
            "tasks.pause",
            "tasks.resume",
            "tasks.snooze",
            "tasks.cancel",
        ] {
            assert!(permission_of(m).is_some(), "{m}");
        }
        assert!(permission_of("shell.exec").is_none());
        assert_eq!(permission_of("tasks.done"), Some(AppPermission::TasksWrite));
    }
}
