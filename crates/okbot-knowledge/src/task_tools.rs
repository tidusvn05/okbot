//! The `task_*` tools of the `/bot` MCP server (design §5.6).

use jiff::Timestamp;
use jiff::tz::TimeZone;
use okbot_core::{
    ChannelId, MemoryScope, NewTask, Repeat, Requester, Task, TaskChange, TaskKind, TaskStatus,
    TaskStore, parse_time,
};
use std::fmt::Write as _;

use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::Knowledge;
use crate::bot_mcp::{scope, str_arg, tool};

/// The task tools' definitions.
pub(crate) fn tools() -> Vec<Tool> {
    vec![
        tool(
            "task_add",
            "Add a to-do or a reminder. The bot sends reminders by itself at remind_at.",
            json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["todo", "reminder"],
                             "description": "todo: stays open until done (may also have remind_at); reminder: a message at a time, done once sent."},
                    "title": {"type": "string", "description": "Short, in the user's language."},
                    "notes": {"type": "string"},
                    "remind_at": {"type": "string", "description": "Local time `YYYY-MM-DD HH:MM` when the bot sends the reminder."},
                    "due": {"type": "string", "description": "Local time `YYYY-MM-DD HH:MM` (or a date) when it should be done."},
                    "repeat": {"type": "string", "description": "`daily 08:00`, `weekdays 08:00`, `weekly mon,fri 09:00`, `monthly 1 09:00` or `every 3h`."},
                    "scope": {"type": "string", "enum": ["user", "room"],
                              "description": "user (default): the person's own; room: shared with this chat."},
                    "deliver_to": {"type": "string", "description": "Optional channel id of one of the user's accounts (`discord:main`); default: this chat."}
                },
                "required": ["title"]
            }),
        ),
        tool(
            "task_list",
            "List the person's and this chat's tasks with their ids.",
            json!({
                "type": "object",
                "properties": {"all": {"type": "boolean", "description": "Include done and cancelled tasks."}}
            }),
        ),
        tool(
            "task_update",
            "Change a task: snooze (new remind_at), reschedule, rename, pause or reopen. An empty string clears a time or the repeat.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "notes": {"type": "string"},
                    "remind_at": {"type": "string"},
                    "due": {"type": "string"},
                    "repeat": {"type": "string"},
                    "status": {"type": "string", "enum": ["open", "paused", "done", "cancelled"]}
                },
                "required": ["id"]
            }),
        ),
        tool(
            "task_done",
            "Mark a task as done.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        ),
        tool(
            "task_delete",
            "Delete a task.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        ),
    ]
}

/// A time as people read it: `2026-10-14 08:00 Wed`.
pub(crate) fn local(ts: Timestamp, tz: &TimeZone) -> String {
    ts.to_zoned(tz.clone())
        .strftime("%Y-%m-%d %H:%M %a")
        .to_string()
}

/// One line describing a task, with its short id.
pub(crate) fn describe(task: &Task, tz: &TimeZone) -> String {
    let short = task.id.rsplit('/').next().unwrap_or(&task.id);
    let mut out = format!(
        "{short} [{}, {}] {}",
        task.kind.as_str(),
        task.status.as_str(),
        task.title
    );
    if let Some(at) = task.remind_at {
        let _ = write!(out, " · remind {}", local(at, tz));
    }
    if let Some(repeat) = &task.repeat {
        let _ = write!(out, " · repeat {repeat}");
    }
    if let Some(due) = task.due {
        let _ = write!(out, " · due {}", local(due, tz));
    }
    if task.scope == MemoryScope::Room {
        out.push_str(" · this chat");
    }
    if let Some(channel) = &task.deliver_to {
        let _ = write!(out, " · to {channel}");
    }
    out
}

fn time_arg(
    args: &Map<String, Value>,
    key: &str,
    tz: &TimeZone,
) -> Result<Option<Timestamp>, String> {
    str_arg(args, key)
        .filter(|v| !v.trim().is_empty())
        .map(|v| parse_time(v, tz))
        .transpose()
}

/// `Some(None)` for an empty string (clear), `Some(Some(t))` for a time, `None` when absent.
#[expect(
    clippy::option_option,
    reason = "the shape of `TaskChange`: absent, clear or set"
)]
fn time_change(
    args: &Map<String, Value>,
    key: &str,
    tz: &TimeZone,
) -> Result<Option<Option<Timestamp>>, String> {
    match str_arg(args, key) {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(Some(None)),
        Some(v) => parse_time(v, tz).map(|t| Some(Some(t))),
    }
}

fn repeat_arg(value: Option<&str>) -> Result<Option<Repeat>, String> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(str::parse)
        .transpose()
}

/// A channel the requester can be reached on: the current one or one of their linked accounts.
fn deliver_to(who: &Requester, value: Option<&str>) -> Result<Option<ChannelId>, String> {
    let Some(value) = value
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "origin")
    else {
        return Ok(None);
    };
    let channel: ChannelId = value.parse().map_err(|e| format!("deliver_to: {e}"))?;
    let reachable = channel == who.conversation.channel
        || who
            .person
            .as_ref()
            .is_some_and(|p| p.accounts.iter().any(|(c, _)| *c == channel));
    if !reachable {
        return Err(format!(
            "deliver_to `{channel}` is not one of this person's accounts"
        ));
    }
    Ok(Some(channel))
}

pub(crate) async fn call(
    k: &Knowledge,
    who: &Requester,
    name: &str,
    args: &Map<String, Value>,
) -> Result<String, String> {
    let tz = k.timezone().clone();
    let id = || str_arg(args, "id").unwrap_or_default().to_string();
    match name {
        "task_add" => {
            let kind = match str_arg(args, "kind") {
                None => TaskKind::Todo,
                Some(v) => v.parse::<TaskKind>()?,
            };
            if kind == TaskKind::Routine {
                return Err("routines are not available yet; add a reminder instead".into());
            }
            let task = NewTask {
                kind,
                title: str_arg(args, "title").unwrap_or_default().to_string(),
                notes: str_arg(args, "notes").unwrap_or_default().to_string(),
                scope: match str_arg(args, "scope") {
                    None => MemoryScope::User,
                    Some(v) => scope(v)
                        .filter(|s| *s != MemoryScope::Global)
                        .ok_or("scope must be user or room")?,
                },
                due: time_arg(args, "due", &tz)?,
                remind_at: time_arg(args, "remind_at", &tz)?,
                repeat: repeat_arg(str_arg(args, "repeat"))?,
                deliver_to: deliver_to(who, str_arg(args, "deliver_to"))?,
                paused: false,
            };
            let task = TaskStore::add(k, who, task)
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "Added {} (time zone {}).",
                describe(&task, &tz),
                tz.iana_name().unwrap_or("UTC")
            ))
        }
        "task_list" => {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let tasks: Vec<Task> = TaskStore::list(k, who)
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|t| all || t.status.is_active())
                .collect();
            if tasks.is_empty() {
                return Ok("No tasks.".into());
            }
            Ok(tasks
                .iter()
                .map(|t| format!("- {}", describe(t, &tz)))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "task_update" => {
            let change = TaskChange {
                title: str_arg(args, "title").map(str::to_string),
                notes: str_arg(args, "notes").map(str::to_string),
                status: str_arg(args, "status")
                    .map(str::parse::<TaskStatus>)
                    .transpose()?,
                due: time_change(args, "due", &tz)?,
                remind_at: time_change(args, "remind_at", &tz)?,
                repeat: match str_arg(args, "repeat") {
                    None => None,
                    Some(v) => Some(repeat_arg(Some(v))?),
                },
            };
            let task = TaskStore::update(k, who, &id(), change)
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!("Updated {}.", describe(&task, &tz)))
        }
        "task_done" => {
            let change = TaskChange {
                status: Some(TaskStatus::Done),
                ..TaskChange::default()
            };
            let task = TaskStore::update(k, who, &id(), change)
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!("Done: {}.", task.title))
        }
        "task_delete" => {
            let task = TaskStore::delete(k, who, &id())
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!("Deleted: {}.", task.title))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{ConversationKey, Person, UserRef};

    fn args(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            unreachable!("test arguments are objects")
        };
        map
    }

    #[tokio::test]
    async fn add_list_snooze_done() {
        let dir = tempfile::tempdir().unwrap();
        crate::write_default_bundle(dir.path()).unwrap();
        let mut options = crate::KnowledgeOptions::new(dir.path());
        options.timezone = Some("Asia/Ho_Chi_Minh".into());
        let k = Knowledge::open(options).unwrap();
        let zalo: ChannelId = "zalo:me".parse().unwrap();
        let who = Requester {
            conversation: ConversationKey::room(zalo.clone(), "dm"),
            user: UserRef {
                id: "u1".into(),
                display_name: "Ana".into(),
                is_bot: false,
            },
            internal: false,
            person: Some(Person {
                id: "owner".into(),
                accounts: vec![
                    (zalo, "u1".into()),
                    ("discord:main".parse().unwrap(), "d1".into()),
                ],
            }),
        };
        let added = call(
            &k,
            &who,
            "task_add",
            &args(
                json!({"kind": "reminder", "title": "Gọi mẹ", "remind_at": "2099-01-02 20:00",
                         "deliver_to": "discord:main"}),
            ),
        )
        .await
        .unwrap();
        assert!(added.contains("remind 2099-01-02 20:00"), "{added}");
        assert!(added.contains("to discord:main"), "{added}");
        let id = added
            .trim_start_matches("Added ")
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();

        let bad = call(
            &k,
            &who,
            "task_add",
            &args(json!({"title": "x", "deliver_to": "slack:other"})),
        )
        .await
        .unwrap_err();
        assert!(bad.contains("not one of"), "{bad}");
        let routine = call(
            &k,
            &who,
            "task_add",
            &args(json!({"kind": "routine", "title": "x"})),
        )
        .await
        .unwrap_err();
        assert!(routine.contains("not available"), "{routine}");

        let snoozed = call(
            &k,
            &who,
            "task_update",
            &args(json!({"id": id, "remind_at": "2099-01-02 20:10"})),
        )
        .await
        .unwrap();
        assert!(snoozed.contains("remind 2099-01-02 20:10"), "{snoozed}");
        let list = call(&k, &who, "task_list", &Map::new()).await.unwrap();
        assert!(list.contains(&id), "{list}");
        call(&k, &who, "task_done", &args(json!({"id": id})))
            .await
            .unwrap();
        let list = call(&k, &who, "task_list", &Map::new()).await.unwrap();
        assert_eq!(list, "No tasks.");
        let list = call(&k, &who, "task_list", &args(json!({"all": true})))
            .await
            .unwrap();
        assert!(list.contains("done"), "{list}");
    }
}
