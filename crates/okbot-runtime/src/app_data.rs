//! App API methods beyond tasks (ADR 0019): the schedule, memory, notes, knowledge,
//! automations and connected accounts. Each goes through the code its chat command uses, with
//! the requester's scope.

use axum::http::StatusCode;
use jiff::{SignedDuration, Timestamp};
use okbot_core::{AppPermission, MemoryError, MemoryScope};
use serde_json::{Value, json};

use crate::app_api::{ApiError, Call};

/// The permission a method of this module needs; `None` for methods it does not have.
pub(crate) fn permission_of(method: &str) -> Option<AppPermission> {
    Some(match method {
        "schedule.list" => AppPermission::ScheduleRead,
        "memory.list" => AppPermission::MemoryRead,
        "memory.forget" => AppPermission::MemoryWrite,
        "notes.list" | "notes.get" => AppPermission::NotesRead,
        "notes.add" => AppPermission::NotesWrite,
        "knowledge.search" | "knowledge.get" => AppPermission::KnowledgeRead,
        "automations.list" => AppPermission::AutomationsRead,
        "connections.list" => AppPermission::ConnectionsRead,
        _ => return None,
    })
}

/// Whether a method of this module changes data.
pub(crate) fn writes(method: &str) -> bool {
    matches!(method, "memory.forget" | "notes.add" | "mcp.call")
}

/// Firings listed at most, over the whole range.
const MAX_FIRINGS: usize = 500;
/// The furthest the schedule looks ahead.
const MAX_DAYS: i64 = 62;

/// Areas the knowledge app does not show: they have apps of their own.
const PRIVATE_AREAS: [&str; 3] = ["memory/", "notes/", "tasks/"];

fn not_found(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("no {what} here"),
    )
}

fn memory_error(call: &Call<'_>, error: MemoryError) -> ApiError {
    match error {
        MemoryError::NotFound(_) => not_found("such note"),
        MemoryError::Backend(error) => {
            tracing::error!(%error, "a memory call failed");
            ApiError::internal(call.shared)
        }
        other => ApiError::invalid(other.to_string()),
    }
}

fn knowledge_error(call: &Call<'_>, error: &okbot_core::KnowledgeError) -> ApiError {
    tracing::error!(%error, "a knowledge call failed");
    ApiError::internal(call.shared)
}

fn scope_name(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::User => "personal",
        MemoryScope::Room => "room",
        MemoryScope::Global => "everyone",
    }
}

fn doc_json(d: &okbot_core::DocumentInfo) -> Value {
    json!({
        "id": d.id,
        "title": d.title,
        "type": d.kind,
        "updated": d.updated,
        "tags": d.tags,
    })
}

/// Runs a method of this module.
pub(crate) async fn run(call: &Call<'_>, method: &str) -> Result<Value, ApiError> {
    match method {
        "schedule.list" => schedule(call).await,
        "memory.list" => memory_list(call).await,
        "memory.forget" => memory_forget(call).await,
        "notes.list" => documents(call, "notes/", 200).await,
        "notes.get" => document(call, Some("notes/")).await,
        "notes.add" => notes_add(call).await,
        "knowledge.search" => documents(call, "", 50).await,
        "knowledge.get" => document(call, None).await,
        "automations.list" => automations(call).await,
        "connections.list" => connections(call).await,
        "mcp.call" => mcp_call(call).await,
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no method `{method}`"),
        )),
    }
}

/// Reminders and routine firings in the next `days` (default 14), repeats expanded, and the
/// times of scheduled automations for administrators.
async fn schedule(call: &Call<'_>) -> Result<Value, ApiError> {
    let days = call
        .params
        .get("days")
        .and_then(Value::as_i64)
        .unwrap_or(14)
        .clamp(1, MAX_DAYS);
    let now = call.shared.clock.now();
    let until = now
        .checked_add(SignedDuration::from_hours(24 * days))
        .unwrap_or(Timestamp::MAX);
    let tz = call.shared.config.time_zone();
    let mut items: Vec<(Timestamp, Value)> = Vec::new();
    if let Some(store) = &call.shared.tasks {
        let who = call.requester().await;
        let tasks = store.list(&who).await.map_err(|e| call.task_error(e))?;
        for t in tasks
            .iter()
            .filter(|t| t.status == okbot_core::TaskStatus::Open)
        {
            let Some(mut at) = t.remind_at else { continue };
            while at <= until && items.len() < MAX_FIRINGS {
                items.push((
                    at,
                    json!({
                        "kind": t.kind.as_str(),
                        "task": t.id,
                        "title": t.title,
                        "at": okbot_core::format_time(at, &tz),
                        "repeat": t.repeat.as_ref().map(ToString::to_string),
                    }),
                ));
                match t.repeat.as_ref().and_then(|r| r.next_after(at, &tz)) {
                    Some(next) if next > at => at = next,
                    _ => break,
                }
            }
        }
    }
    let admin = call.shared.is_admin(&call.admitted.event).await;
    if admin {
        for a in &call.shared.config.automations {
            let crate::config::Trigger::Schedule(rule) = &a.trigger else {
                continue;
            };
            if a.paused {
                continue;
            }
            let mut at = now;
            while let Some(next) = rule.next_after(at, &tz) {
                if next > until || next <= at || items.len() >= MAX_FIRINGS {
                    break;
                }
                items.push((
                    next,
                    json!({
                        "kind": "automation",
                        "title": a.name,
                        "at": okbot_core::format_time(next, &tz),
                        "repeat": rule.to_string(),
                    }),
                ));
                at = next;
            }
        }
    }
    items.sort_by_key(|(at, _)| *at);
    let items: Vec<Value> = items.into_iter().map(|(_, v)| v).collect();
    Ok(json!({
        "items": items,
        "days": days,
        "timezone": call.shared.config.timezone.clone().unwrap_or_else(|| "UTC".into()),
    }))
}

async fn memory_list(call: &Call<'_>) -> Result<Value, ApiError> {
    let memory = call
        .shared
        .memory
        .as_ref()
        .ok_or_else(|| not_found("memory"))?;
    let who = call.requester().await;
    let entries = memory.list(&who).await.map_err(|e| memory_error(call, e))?;
    let items: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "scope": scope_name(e.scope),
                "title": e.title,
                "text": e.text,
                "updated": e.updated,
                "mine": e.author.as_deref()
                    == Some(&format!("{}/{}", who.conversation.channel, who.user.id)),
            })
        })
        .collect();
    Ok(json!({"items": items, "personal": who.personal}))
}

async fn memory_forget(call: &Call<'_>) -> Result<Value, ApiError> {
    let memory = call
        .shared
        .memory
        .as_ref()
        .ok_or_else(|| not_found("memory"))?;
    let id = call.required("id")?;
    let who = call.requester().await;
    let entry = memory
        .forget(&who, id)
        .await
        .map_err(|e| memory_error(call, e))?;
    Ok(json!({"id": entry.id, "title": entry.title}))
}

/// Documents under `path` (all visible when empty, less the areas with apps of their own).
async fn documents(call: &Call<'_>, path: &str, limit: usize) -> Result<Value, ApiError> {
    let knowledge = call
        .shared
        .knowledge
        .as_ref()
        .ok_or_else(|| not_found("knowledge base"))?;
    let who = call.requester().await;
    let text = call.str("query");
    let docs = knowledge
        .documents(&who, path, text, limit)
        .await
        .map_err(|e| knowledge_error(call, &e))?;
    let items: Vec<Value> = docs
        .iter()
        .filter(|d| !path.is_empty() || !PRIVATE_AREAS.iter().any(|a| d.id.starts_with(a)))
        .map(doc_json)
        .collect();
    Ok(json!({"items": items, "personal": who.personal}))
}

/// One document; under `within` when given.
async fn document(call: &Call<'_>, within: Option<&str>) -> Result<Value, ApiError> {
    let knowledge = call
        .shared
        .knowledge
        .as_ref()
        .ok_or_else(|| not_found("knowledge base"))?;
    let id = call.required("id")?;
    let allowed = match within {
        Some(area) => id.starts_with(area),
        None => !PRIVATE_AREAS.iter().any(|a| id.starts_with(a)),
    };
    if !allowed {
        return Err(not_found("such document"));
    }
    let who = call.requester().await;
    let doc = knowledge
        .document(&who, id)
        .await
        .map_err(|e| knowledge_error(call, &e))?
        .ok_or_else(|| not_found("such document"))?;
    Ok(json!({
        "info": doc_json(&doc.info),
        "text": doc.text,
        "truncated": doc.truncated,
    }))
}

async fn notes_add(call: &Call<'_>) -> Result<Value, ApiError> {
    let memory = call
        .shared
        .memory
        .as_ref()
        .ok_or_else(|| not_found("journal"))?;
    let text = call
        .text("text", 2000)?
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiError::invalid("`text` is required"))?;
    let who = call.requester().await;
    // Personal only where personal data is reachable, as for `/note`.
    let scope = if who.personal {
        MemoryScope::User
    } else {
        MemoryScope::Room
    };
    let id = memory
        .note(&who, scope, &text)
        .await
        .map_err(|e| memory_error(call, e))?;
    Ok(json!({"id": id, "scope": scope_name(scope)}))
}

/// Every automation with its trigger and latest runs: administrators only (ADR 0013).
async fn automations(call: &Call<'_>) -> Result<Value, ApiError> {
    if !call.shared.is_admin(&call.admitted.event).await {
        return Err(ApiError::forbidden("automations are for administrators"));
    }
    let tz = call.shared.config.time_zone();
    let day_ago = call
        .shared
        .clock
        .now()
        .checked_sub(SignedDuration::from_hours(24))
        .unwrap_or(Timestamp::UNIX_EPOCH);
    let mut items = Vec::new();
    for a in &call.shared.config.automations {
        let trigger = match &a.trigger {
            crate::config::Trigger::Every(d) => format!("every {}s", d.as_secs()),
            crate::config::Trigger::Schedule(rule) => rule.to_string(),
            crate::config::Trigger::Webhook(_) => format!("POST /automation/{}", a.name),
        };
        let action = match &a.action {
            crate::config::AutomationAction::Text { .. } => "text",
            crate::config::AutomationAction::Prompt { .. } => "prompt",
        };
        let runs = call
            .shared
            .store
            .automation_runs(Some(&a.name), Timestamp::UNIX_EPOCH, 10)
            .await
            .unwrap_or_default();
        let today = runs
            .iter()
            .filter(|r| r.at >= day_ago && r.items > 0)
            .count();
        let runs: Vec<Value> = runs
            .iter()
            .map(|r| {
                json!({
                    "at": okbot_core::format_time(r.at, &tz),
                    "items": r.items,
                    "action": r.action,
                    "error": r.error,
                })
            })
            .collect();
        items.push(json!({
            "name": a.name,
            "trigger": trigger,
            "source": a.source.as_ref().map(|s| format!("{} {}", s.mcp, s.tool)),
            "action": action,
            "paused": a.paused,
            "max_runs_per_day": a.max_runs_per_day,
            "acted_last_24h": today,
            "runs": runs,
        }));
    }
    Ok(json!({"items": items}))
}

/// The person's connected accounts: only from a direct chat, where they are theirs to see.
async fn connections(call: &Call<'_>) -> Result<Value, ApiError> {
    if !call.context.direct {
        return Err(ApiError::forbidden(
            "connected accounts are shown from a direct chat only",
        ));
    }
    let Some(list) = crate::connect::connection_list(call.shared, &call.admitted.event).await
    else {
        return Ok(json!({"items": [], "enabled": false}));
    };
    let items: Vec<Value> = list
        .iter()
        .map(|c| {
            json!({
                "server": c.server,
                "personal": c.personal,
                "connected": c.connected,
                "connect": format!("/connect {}", c.server),
                "disconnect": format!("/disconnect {}", c.server),
            })
        })
        .collect();
    Ok(json!({"items": items, "enabled": true}))
}

/// The tool `mcp.call` names, as the permission it needs (`mcp:<server>/<tool>`).
pub(crate) fn mcp_permission(params: &Value) -> Option<AppPermission> {
    let server = params.get("server")?.as_str()?;
    let tool = params.get("tool")?.as_str()?;
    format!("mcp:{server}/{tool}").parse().ok()
}

/// `mcp.call {server, tool, arguments}` (ADR 0019, 0011): one `[[mcp]]` tool, called as the
/// signed-in person with their rights and credentials. A tool that `[[mcp]] confirm` guards
/// answers `{"confirm": "<question>"}` first; the shell asks the person in its own dialog and
/// calls again with `confirmed`, which apps cannot set (the shell removes it).
pub(crate) async fn mcp_call(call: &Call<'_>) -> Result<Value, ApiError> {
    use crate::mcp_client::McpError;
    let server = call.required("server")?.to_string();
    let tool = call.required("tool")?.to_string();
    let arguments = match call.params.get("arguments") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(ApiError::invalid("arguments: a JSON object")),
    };
    let confirmed = call.params.get("confirmed").and_then(Value::as_bool) == Some(true);
    let shared = call.shared;
    let event = &call.admitted.event;
    let Some(extra) = shared.config.mcp.iter().find(|m| m.server.name == server) else {
        return Err(not_found("such server"));
    };
    let who = crate::permission::Who::new(shared.roles_of(event).await, event);
    if !crate::permission::may_call(extra, &who, &tool) {
        return Err(ApiError::forbidden(format!(
            "`{tool}` of `{server}` is not available to you here"
        )));
    }
    let person = shared.person_key(event).await;
    let token = mcp_token(call, extra, &who, &person, &server).await?;
    let started = std::time::Instant::now();
    let result = crate::mcp_client::with_session(
        reqwest::Client::new(),
        &extra.server.url,
        token.expose(),
        shared.config.agent_timeout,
        async |session| {
            let Some(described) = session.tool(&tool).await? else {
                return Err(McpError::Rpc(format!("`{tool}` is not available")));
            };
            if !confirmed
                && crate::permission::needs_confirmation(&extra.rules, described.get("annotations"))
            {
                return Ok(Outcome::Confirm);
            }
            session
                .call(&tool, arguments.clone())
                .await
                .map(Outcome::Done)
        },
    )
    .await;
    if matches!(result, Ok(Outcome::Confirm)) {
        return Ok(json!({
            "confirm": format!(
                "{} wants to run `{tool}` of `{server}` as you. It can change things. Run it?",
                call.admitted.info.title(None)
            ),
        }));
    }
    let error = match &result {
        Ok(Outcome::Done(out)) if out.is_error => Some("the tool reported an error".to_string()),
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };
    crate::gateway::audit(
        shared,
        okbot_core::ToolCallRecord {
            run_id: None,
            command: Some(format!("app:{}", call.admitted.info.name)),
            person,
            conversation: event.conversation.clone(),
            server: server.clone(),
            tool: tool.clone(),
            decision: okbot_core::ToolDecision::Allowed,
            error,
            duration: started.elapsed(),
            at: shared.clock.now(),
        },
    )
    .await;
    match result {
        Ok(Outcome::Done(out)) => Ok(json!({
            "text": out.text,
            "is_error": out.is_error,
            "structured": out.structured,
        })),
        Ok(Outcome::Confirm) => Err(ApiError::internal(shared)),
        Err(error) => {
            tracing::warn!(%server, %tool, %error, "an app's tool call failed");
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "failed",
                format!("`{tool}` failed"),
            ))
        }
    }
}

/// What a tool call came to.
enum Outcome {
    Confirm,
    Done(crate::mcp_client::ToolOutput),
}

/// The credential the person's call of `extra` goes with, or why there is none.
async fn mcp_token(
    call: &Call<'_>,
    extra: &crate::ExtraMcp,
    who: &crate::permission::Who,
    person: &str,
    server: &str,
) -> Result<okbot_core::Secret<String>, ApiError> {
    let (shared, event) = (call.shared, &call.admitted.event);
    let room = shared
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    let personal = event.is_direct || room.personal;
    match crate::connect::upstream(shared, extra, event, person, personal, who.admin(), None).await
    {
        crate::connect::Upstream::Token(token) => Ok(token),
        crate::connect::Upstream::NotConnected => Err(ApiError::forbidden(format!(
            "connect your `{server}` account first: send `/connect {server}` to the bot in a direct chat"
        ))),
        crate::connect::Upstream::NotHere => Err(ApiError::forbidden(format!(
            "`{server}` uses your own account: open this app from a direct chat"
        ))),
        crate::connect::Upstream::Unavailable => Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "unavailable",
            format!("`{server}` cannot be reached now"),
        )),
    }
}
