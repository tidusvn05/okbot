//! `[[command]]`: chat commands that call one `[[mcp]]` tool directly, without the agent.
//!
//! `/logs freach channel-broker since=24h` → `company_logs {"company": "freach", "service":
//! "channel-broker", "since": "24h"}`. Values take the types the tool declares, so `limit=50` is
//! a number. The server sees the same identity an agent turn would give it (a signed token with
//! `command` set), so its own permission checks and prompts apply unchanged.
//!
//! A result may carry a view (ADR 0017): cards, links and buttons that run other `[[command]]`s,
//! shown natively where the platform can.

use okbot_core::InboundEvent;
use serde_json::{Map, Value, json};

use crate::config::{ArgKind, McpCommand};
use crate::mcp_client::{self, McpError};
use crate::permission::{self, Who};
use crate::ui::View;
use crate::{Shared, turn};

/// The `[[command]]` a message invokes, and the text after the command word.
pub(crate) fn find<'a>(shared: &'a Shared, text: &'a str) -> Option<(&'a McpCommand, &'a str)> {
    find_in(&shared.config.commands, text)
}

/// The command of `commands` that `text` invokes, and the text after the command word.
pub(crate) fn find_in<'a>(
    commands: &'a [McpCommand],
    text: &'a str,
) -> Option<(&'a McpCommand, &'a str)> {
    let text = text.trim();
    let word = text.split_whitespace().next()?;
    let name = word.strip_prefix('/')?;
    let name = name.split('@').next().unwrap_or(name).to_ascii_lowercase();
    let command = commands.iter().find(|c| c.name == name)?;
    Some((command, text[word.len()..].trim()))
}

/// What a command answers.
enum Reply {
    Text(String),
    View(Box<View>),
}

/// Runs a command and replies with the tool's view or text (nothing when it returns none, as a
/// tool that only shows a prompt may).
pub(crate) async fn run(shared: &Shared, command: &McpCommand, rest: &str, event: &InboundEvent) {
    let typing = turn::Typing::start(shared, event).await;
    let text = match execute(shared, command, rest, event).await {
        Reply::Text(text) => text,
        Reply::View(view) => {
            done(shared, event, typing.1.as_ref()).await;
            let to = crate::dispatch::target(event);
            if let Err(error) = crate::ui::deliver(shared, &to, event.is_direct, &view).await {
                tracing::error!(command = %command.name, %error, "cannot send a command's view");
            }
            return;
        }
    };
    if text.trim().is_empty() {
        done(shared, event, typing.1.as_ref()).await;
    } else {
        turn::reply_over(shared, event, &text, typing.1.as_ref()).await;
    }
    drop(typing);
}

/// Replaces the "thinking…" placeholder, if any, with a check mark: it must not stay.
async fn done(shared: &Shared, event: &InboundEvent, placeholder: Option<&okbot_core::SentRef>) {
    if let (Some(sent), Some(channel)) = (
        placeholder,
        shared.channels.get(&event.conversation.channel),
    ) {
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
}

/// How a session with the command's server ended.
enum Call {
    Done(mcp_client::ToolOutput),
    Usage(String),
    /// A press must be confirmed first (`[[mcp]] confirm`).
    Confirm,
}

async fn execute(shared: &Shared, command: &McpCommand, rest: &str, event: &InboundEvent) -> Reply {
    let text = Reply::Text;
    let Some(extra) = shared
        .config
        .mcp
        .iter()
        .find(|m| m.server.name == command.mcp)
    else {
        return text(format!("`/{}` is not available.", command.name));
    };
    let who = Who::new(shared.roles_of(event).await, event);
    let admin = who.admin();
    if !permission::may_run(command, extra, &who) {
        if command.admins_only || extra.admins_only {
            return text(format!("Only administrators can use `/{}`.", command.name));
        }
        return text(format!("`/{}` is not available to you here.", command.name));
    }
    if matches!(rest, "help" | "?" | "-h" | "--help") {
        return text(describe(command, event.locale.as_deref()));
    }
    let person = shared.person_key(event).await;
    let token = match token(shared, command, extra, event, &person, admin).await {
        Ok(token) => token,
        Err(refusal) => return text(refusal),
    };
    // A button press runs what the presser saw only as a label: ask as an agent's call would.
    let pressed = crate::ui::pressed(event);
    let http = reqwest::Client::new();
    let started = std::time::Instant::now();
    let result = mcp_client::with_session(
        http,
        &extra.server.url,
        token.expose(),
        shared.config.agent_timeout,
        async |session| {
            let Some(tool) = session.tool(&command.tool).await? else {
                return Err(McpError::Rpc(format!(
                    "`/{}` is not available to you.",
                    command.name
                )));
            };
            if pressed && permission::needs_confirmation(&extra.rules, tool.get("annotations")) {
                return Ok(Call::Confirm);
            }
            let schema = tool.get("inputSchema").cloned().unwrap_or(json!({}));
            match arguments(command, rest, &schema, event.locale.as_deref()) {
                Ok(args) => session.call(&command.tool, args).await.map(Call::Done),
                Err(usage) => Ok(Call::Usage(usage)),
            }
        },
    )
    .await;
    if matches!(result, Ok(Call::Confirm)) {
        return confirmation(shared, command, extra, event);
    }
    tracing::info!(
        command = %command.name,
        user = %event.author.id,
        ok = matches!(result, Ok(Call::Done(ref out)) if !out.is_error),
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "command"
    );
    let error = match &result {
        Ok(Call::Done(out)) if out.is_error => Some("the tool reported an error".to_string()),
        Ok(Call::Done(_) | Call::Confirm) => None,
        Ok(Call::Usage(_)) => Some("usage".to_string()),
        Err(e) => Some(e.to_string()),
    };
    // A usage message never reached the tool.
    if error.as_deref() != Some("usage") {
        crate::gateway::audit(
            shared,
            okbot_core::ToolCallRecord {
                run_id: None,
                command: Some(command.name.clone()),
                person,
                conversation: event.conversation.clone(),
                server: extra.server.name.clone(),
                tool: command.tool.clone(),
                decision: okbot_core::ToolDecision::Allowed,
                error,
                duration: started.elapsed(),
                at: shared.clock.now(),
            },
        )
        .await;
    }
    text(match result {
        Ok(Call::Done(out)) => return answer(shared, command, out),
        Ok(Call::Usage(usage)) => usage,
        Ok(Call::Confirm) => String::new(),
        Err(McpError::Rpc(message)) => format!("⚠️ {message}"),
        Err(error) => {
            tracing::warn!(command = %command.name, %error, "command failed");
            format!("⚠️ `/{}` failed: {error}", command.name)
        }
    })
}

/// The tool's view, or its text when it carries none (or one okbot cannot show).
fn answer(shared: &Shared, command: &McpCommand, out: mcp_client::ToolOutput) -> Reply {
    // Its buttons may run only the commands of the same server: a server cannot make people run
    // another server's tools.
    let same_server: Vec<McpCommand> = shared
        .config
        .commands
        .iter()
        .filter(|c| c.mcp == command.mcp)
        .cloned()
        .collect();
    match crate::ui::carried(out.structured.as_ref(), &same_server) {
        Some(Ok(view)) => return Reply::View(Box::new(view)),
        Some(Err(error)) => {
            // The log only: the server's text is not repeated in the chat.
            tracing::warn!(command = %command.name, %error, "the tool's view cannot be shown");
            if out.text.trim().is_empty() {
                return Reply::Text(format!(
                    "⚠️ `/{}` answered with a view that cannot be shown.",
                    command.name
                ));
            }
        }
        None => {}
    }
    Reply::Text(if out.is_error && starts_with_icon(&out.text) {
        // A server that leads with its own icon (❌, 🔑…) keeps it.
        out.text
    } else if out.is_error {
        format!("⚠️ {}", out.text)
    } else {
        out.text
    })
}

/// Asks the approvers `[[mcp]] confirm_by` names to run the pressed command line.
fn confirmation(
    shared: &Shared,
    command: &McpCommand,
    extra: &crate::ExtraMcp,
    event: &InboundEvent,
) -> Reply {
    let by = extra.rules.confirm_by;
    let allowed = crate::confirm::approvers(shared, event, by);
    if allowed.is_empty() {
        tracing::warn!(command = %command.name, "no one on this channel can approve the command");
        return Reply::Text(format!(
            "⚠️ `/{}` needs an approval that no one here can give.",
            command.name
        ));
    }
    let line = event.text.trim();
    let presser = crate::interact::literal(&event.author.display_name);
    let text = match by {
        crate::config::ConfirmBy::Requester => format!(
            "🔐 {presser}, the button runs `{}`, which can change things. Run it?",
            line.replace('`', "'")
        ),
        crate::config::ConfirmBy::Admins => format!(
            "🔐 {presser} pressed a button that runs `{}`, which can change things. Run it \
             as {presser}? (an administrator decides)",
            line.replace('`', "'")
        ),
    };
    tracing::info!(command = %command.name, "a pressed command waits for approval");
    let approval = crate::ui::Approval {
        run_as: event.author.clone(),
        admins: by == crate::config::ConfirmBy::Admins,
    };
    Reply::View(Box::new(View::confirmation(text, line, approval, allowed)))
}

/// The credential `extra` gets for `command`, or why there is none.
async fn token(
    shared: &Shared,
    command: &McpCommand,
    extra: &crate::ExtraMcp,
    event: &InboundEvent,
    person: &str,
    admin: bool,
) -> Result<okbot_core::Secret<String>, String> {
    use crate::connect::Upstream;
    let room = shared
        .config
        .room(&event.conversation.channel, &event.conversation.room);
    let personal = event.is_direct || room.personal;
    let (name, server) = (&command.name, &extra.server.name);
    match crate::connect::upstream(shared, extra, event, person, personal, admin, Some(name)).await
    {
        Upstream::Token(token) => Ok(token),
        Upstream::NotConnected if extra.personal_credentials() => Err(format!(
            "`/{name}` uses your `{server}` account: send `/connect {server}` in a direct chat \
             with me first."
        )),
        Upstream::NotConnected => Err(format!(
            "`/{name}` needs the bot's `{server}` account, which an administrator has not \
             connected yet."
        )),
        Upstream::NotHere => Err(format!(
            "`/{name}` uses your own `{server}` account, so it works only in a direct chat with me."
        )),
        Upstream::Unavailable => Err(format!("⚠️ `/{name}` cannot reach `{server}` now.")),
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

/// The commands `who` may run here, for `/help`.
pub(crate) fn help(shared: &Shared, who: &Who, locale: Option<&str>) -> String {
    let lines: Vec<String> = shared
        .config
        .commands
        .iter()
        .filter(|c| {
            shared
                .config
                .mcp
                .iter()
                .find(|m| m.server.name == c.mcp)
                .is_some_and(|m| permission::may_run(c, m, who))
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
            roles: Vec::new(),
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
