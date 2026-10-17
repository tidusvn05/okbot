//! `[[command]]`: chat commands that call one `[[mcp]]` tool directly, without the agent.
//!
//! `/logs freach channel-broker since=24h` → `company_logs {"company": "freach", "service":
//! "channel-broker", "since": "24h"}`. Values take the types the tool declares, so `limit=50` is
//! a number. The server sees the same identity an agent turn would give it (a signed token with
//! `command` set), so its own permission checks and prompts apply unchanged.

use okbot_core::InboundEvent;
use serde_json::{Map, Value};

use crate::config::{ArgKind, McpCommand};
use crate::mcp_client::{self, McpError};
use crate::{Shared, turn};

/// The `[[command]]` a message invokes, and the text after the command word.
pub(crate) fn find<'a>(shared: &'a Shared, text: &'a str) -> Option<(&'a McpCommand, &'a str)> {
    let text = text.trim();
    let word = text.split_whitespace().next()?;
    let name = word.strip_prefix('/')?;
    let name = name.split('@').next().unwrap_or(name).to_ascii_lowercase();
    let command = shared.config.commands.iter().find(|c| c.name == name)?;
    Some((command, text[word.len()..].trim()))
}

/// Runs a command and replies with the tool's text (nothing when it returns none, as a tool that
/// only shows a prompt may).
pub(crate) async fn run(shared: &Shared, command: &McpCommand, rest: &str, event: &InboundEvent) {
    let typing = turn::Typing::start(shared, event).await;
    let text = execute(shared, command, rest, event).await;
    if !text.trim().is_empty() {
        turn::reply_over(shared, event, &text, typing.1.as_ref()).await;
    } else if let (Some(sent), Some(channel)) =
        (&typing.1, shared.channels.get(&event.conversation.channel))
    {
        // Nothing to say: the "thinking…" placeholder must not stay.
        if let Some(part) = shared
            .renderer
            .render("✓", channel.caps())
            .into_iter()
            .next()
        {
            let _ = channel.edit(sent, &part).await;
        }
    }
    drop(typing);
}

async fn execute(
    shared: &Shared,
    command: &McpCommand,
    rest: &str,
    event: &InboundEvent,
) -> String {
    let Some(extra) = shared
        .config
        .mcp
        .iter()
        .find(|m| m.server.name == command.mcp)
    else {
        return format!("`/{}` is not available.", command.name);
    };
    let admin = shared.is_admin(event).await;
    if (command.admins_only || extra.admins_only) && !admin {
        return format!("Only administrators can use `/{}`.", command.name);
    }
    if matches!(rest, "help" | "?" | "-h" | "--help") {
        return describe(command, event.locale.as_deref());
    }
    let token = crate::identity::token_for(shared, extra, event, admin, Some(&command.name));
    let http = reqwest::Client::new();
    let started = std::time::Instant::now();
    let result = mcp_client::with_session(
        http,
        &extra.server.url,
        token.expose(),
        shared.config.agent_timeout,
        async |session| {
            let Some(schema) = session.tool_schema(&command.tool).await? else {
                return Err(McpError::Rpc(format!(
                    "`/{}` is not available to you.",
                    command.name
                )));
            };
            match arguments(command, rest, &schema, event.locale.as_deref()) {
                Ok(args) => session.call(&command.tool, args).await.map(Ok),
                Err(usage) => Ok(Err(usage)),
            }
        },
    )
    .await;
    tracing::info!(
        command = %command.name,
        user = %event.author.id,
        ok = matches!(result, Ok(Ok(ref out)) if !out.is_error),
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "command"
    );
    match result {
        // A server that leads with its own icon (❌, 🔑…) keeps it.
        Ok(Ok(out)) if out.is_error && starts_with_icon(&out.text) => out.text,
        Ok(Ok(out)) if out.is_error => format!("⚠️ {}", out.text),
        Ok(Ok(out)) => out.text,
        Ok(Err(usage)) => usage,
        Err(McpError::Rpc(message)) => format!("⚠️ {message}"),
        Err(error) => {
            tracing::warn!(command = %command.name, %error, "command failed");
            format!("⚠️ `/{}` failed: {error}", command.name)
        }
    }
}

/// The text starts with a symbol such as an emoji (not a letter, digit or Markdown).
fn starts_with_icon(text: &str) -> bool {
    text.trim_start()
        .chars()
        .next()
        .is_some_and(|c| !c.is_ascii() && !c.is_alphanumeric())
}

/// Usage and description.
pub(crate) fn describe(command: &McpCommand, locale: Option<&str>) -> String {
    let mut out = format!("`{}`", command.usage());
    let description = command.description_for(locale);
    if !description.is_empty() {
        out.push_str(" — ");
        out.push_str(description);
    }
    out
}

/// The commands `admin` may run, for `/help`.
pub(crate) fn help(shared: &Shared, admin: bool, locale: Option<&str>) -> String {
    let lines: Vec<String> = shared
        .config
        .commands
        .iter()
        .filter(|c| {
            admin
                || !(c.admins_only
                    || shared
                        .config
                        .mcp
                        .iter()
                        .any(|m| m.server.name == c.mcp && m.admins_only))
        })
        .map(|c| format!("- {}", describe(c, locale)))
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        format!("\n\nCommands of this bot:\n{}", lines.join("\n"))
    }
}

/// The command menu: the `[[command]]`s and the everyday built-ins (empty without `[[command]]`,
/// so bots that have none keep their platform menus as they are).
pub(crate) fn menu(config: &crate::RuntimeConfig) -> Vec<okbot_core::CommandInfo> {
    if config.commands.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<okbot_core::CommandInfo> = config
        .commands
        .iter()
        .map(|c| okbot_core::CommandInfo {
            name: c.name.clone(),
            description: if c.description.is_empty() {
                c.tool.replace('_', " ")
            } else {
                c.description.clone()
            },
            descriptions: c.descriptions.clone(),
            params: c
                .args
                .iter()
                .map(|a| okbot_core::CommandParam {
                    name: a.name.clone(),
                    required: a.kind != ArgKind::Optional,
                    rest: a.kind == ArgKind::Rest,
                })
                .collect(),
        })
        .collect();
    for (name, description) in [
        ("help", "What I can do"),
        ("reset", "Forget this conversation and start over"),
        ("stop", "Stop the answer being written"),
        ("whoami", "The ids I see for you and this chat"),
    ] {
        out.push(okbot_core::CommandInfo {
            name: name.into(),
            description: description.into(),
            descriptions: Vec::new(),
            params: Vec::new(),
        });
    }
    out
}

/// Words of `text` with the byte offset each starts at; `"…"` keeps spaces in one word.
fn words(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&(_, c)) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
            } else if c.is_whitespace() && !quoted {
                break;
            } else {
                word.push(c);
            }
            chars.next();
        }
        out.push((start, word));
    }
    out
}

fn unquote(text: &str) -> String {
    let t = text.trim();
    t.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(t)
        .to_string()
}

/// The tool arguments for `rest`, or the usage when it does not fit.
fn arguments(
    command: &McpCommand,
    rest: &str,
    schema: &Value,
    locale: Option<&str>,
) -> Result<Map<String, Value>, String> {
    let props = schema.get("properties").and_then(Value::as_object);
    let known = |k: &str| {
        props.is_some_and(|p| p.contains_key(k)) || command.args.iter().any(|a| a.name == k)
    };
    let usage = |why: String| format!("{why}\nUsage: {}", describe(command, locale));
    let mut out = command.with.clone();
    let mut positional = command.args.iter();
    let mut next = positional.next();
    for (start, word) in words(rest) {
        if let Some((key, value)) = word.split_once('=')
            && known(key)
            // A quoted word is a value, even with `=` in it.
            && !rest[start..].starts_with('"')
        {
            // `with` is the operator's: chat never changes it.
            if command.with.contains_key(key) {
                return Err(usage(format!("`{key}` is fixed for this command.")));
            }
            out.insert(key.to_string(), coerce(key, value, props)?);
            continue;
        }
        let Some(arg) = next else {
            return Err(usage(format!("Too many arguments (`{word}`).")));
        };
        if arg.kind == ArgKind::Rest {
            let value = unquote(&rest[start..]);
            out.insert(arg.name.clone(), coerce(&arg.name, &value, props)?);
            next = None;
            break;
        }
        out.insert(arg.name.clone(), coerce(&arg.name, &word, props)?);
        next = positional.next();
    }
    for arg in next.into_iter().chain(positional) {
        if arg.kind != ArgKind::Optional && !out.contains_key(&arg.name) {
            return Err(usage(format!("Missing `{}`.", arg.name)));
        }
    }
    let required = schema.get("required").and_then(Value::as_array);
    for name in required.into_iter().flatten().filter_map(Value::as_str) {
        if !out.contains_key(name) {
            return Err(usage(format!("Missing `{name}`.")));
        }
    }
    Ok(out)
}

/// `value` as the type the tool declares for `key` (text when it declares none).
fn coerce(key: &str, value: &str, props: Option<&Map<String, Value>>) -> Result<Value, String> {
    let declared = props
        .and_then(|p| p.get(key))
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("string");
    let bad = || format!("`{key}` must be a {declared}, not `{value}`.");
    Ok(match declared {
        "integer" => Value::from(value.parse::<i64>().map_err(|_| bad())?),
        "number" => serde_json::Number::from_f64(value.parse::<f64>().map_err(|_| bad())?)
            .map(Value::Number)
            .ok_or_else(bad)?,
        "boolean" => match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            _ => return Err(bad()),
        },
        "array" => Value::Array(
            value
                .split(',')
                .map(|v| Value::String(v.trim().to_string()))
                .filter(|v| v != "")
                .collect(),
        ),
        "object" => serde_json::from_str(value).map_err(|_| bad())?,
        _ => Value::String(value.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CommandArg;
    use serde_json::json;

    #[test]
    fn icons_lead_error_texts() {
        assert!(starts_with_icon("❌ down") && starts_with_icon("🔑 key"));
        assert!(!starts_with_icon("no such company") && !starts_with_icon("**x**"));
        assert!(!starts_with_icon("Đã hỏng") && !starts_with_icon("会社がありません"));
    }

    fn command(args: &[&str]) -> McpCommand {
        McpCommand {
            name: "logs".into(),
            description: "Log events".into(),
            descriptions: vec![("ja".into(), "ログ".into())],
            mcp: "ops".into(),
            tool: "company_logs".into(),
            args: args.iter().map(|a| CommandArg::parse(a)).collect(),
            with: json!({"level": "warn"}).as_object().unwrap().clone(),
            admins_only: false,
        }
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "company": {"type": "string"},
                "service": {"type": "string"},
                "limit": {"type": "integer"},
                "deep": {"type": "boolean"},
                "level": {"type": "string"},
                "note": {"type": "string"}
            },
            "required": ["company"]
        })
    }

    #[test]
    fn positional_named_and_typed() {
        let c = command(&["company", "service?"]);
        let args = arguments(&c, "freach agent-sales limit=50 deep=yes", &schema(), None).unwrap();
        assert_eq!(
            Value::Object(args),
            json!({"company": "freach", "service": "agent-sales", "limit": 50, "deep": true, "level": "warn"})
        );
        // `with` is fixed.
        assert!(
            arguments(&c, "freach level=error", &schema(), None)
                .unwrap_err()
                .starts_with("`level` is fixed")
        );
        let args = arguments(&c, "freach", &schema(), None).unwrap();
        assert_eq!(
            Value::Object(args),
            json!({"company": "freach", "level": "warn"})
        );
    }

    #[test]
    fn rest_takes_the_line() {
        let c = command(&["company", "note..."]);
        let args = arguments(&c, "acme  Lan (sales) a=b ", &schema(), None).unwrap();
        assert_eq!(args["note"], "Lan (sales) a=b");
        let args = arguments(&c, "acme \"quoted text\"", &schema(), None).unwrap();
        assert_eq!(args["note"], "quoted text");
        // Named arguments may come before the rest; a quoted one is text.
        let args = arguments(&c, "acme deep=yes Lan", &schema(), None).unwrap();
        assert_eq!(
            (args["deep"].clone(), args["note"].clone()),
            (json!(true), json!("Lan"))
        );
        let args = arguments(&c, "acme \"deep=yes Lan\"", &schema(), None).unwrap();
        assert_eq!(args["note"], "deep=yes Lan");
    }

    #[test]
    fn usage_errors() {
        let c = command(&["company", "service?"]);
        assert!(
            arguments(&c, "", &schema(), None)
                .unwrap_err()
                .starts_with("Missing `company`")
        );
        assert!(
            arguments(&c, "a b c", &schema(), None)
                .unwrap_err()
                .contains("Usage: `/logs <company> [service]`")
        );
        assert!(
            arguments(&c, "a limit=x", &schema(), None)
                .unwrap_err()
                .contains("integer")
        );
        // A key the tool does not know is an ordinary word.
        assert_eq!(
            arguments(&c, "a x=1", &schema(), None).unwrap()["service"],
            "x=1"
        );
        let c = command(&[]);
        assert!(
            arguments(&c, "", &schema(), None)
                .unwrap_err()
                .contains("Missing `company`"),
            "schema requires it"
        );
    }

    #[test]
    fn quoted_words() {
        assert_eq!(
            words(r#"a "b c" d"#),
            vec![
                (0, "a".to_string()),
                (2, "b c".to_string()),
                (8, "d".to_string())
            ]
        );
    }
}
