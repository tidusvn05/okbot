//! Who may use which `[[mcp]]` server and tool, where (ADR 0011).
//!
//! Every agent call through the gateway and every `[[command]]` asks these functions; nothing else
//! decides. Roles come from [`Shared::roles_of`](crate::Shared::roles_of).

use std::collections::BTreeSet;

use okbot_core::{ChannelId, InboundEvent};
use serde_json::Value;

use crate::config::{Confirm, ExtraMcp, McpCommand, McpRules, ROLE_ADMINS};

/// The person asking and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Who {
    /// Their roles, built-in ones included.
    pub(crate) roles: BTreeSet<String>,
    /// The room of the conversation.
    pub(crate) room: (ChannelId, String),
}

impl Who {
    pub(crate) fn new(roles: BTreeSet<String>, event: &InboundEvent) -> Self {
        Self {
            roles,
            room: (
                event.conversation.channel.clone(),
                event.conversation.room.clone(),
            ),
        }
    }

    fn any_of(&self, roles: &[String]) -> bool {
        roles.iter().any(|r| self.roles.contains(r))
    }

    pub(crate) fn admin(&self) -> bool {
        self.roles.contains(ROLE_ADMINS)
    }
}

/// `pattern` matches `name`; `*` stands for any characters.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

fn any_matches(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| matches(p, name))
}

/// The server may be used by `who` here.
pub(crate) fn may_use(server: &ExtraMcp, who: &Who) -> bool {
    let rules = &server.rules;
    (!server.admins_only || who.admin())
        && (rules.roles.is_empty() || who.any_of(&rules.roles))
        && (rules.rooms.is_empty() || rules.rooms.contains(&who.room))
}

/// The tool is offered by the server's lists, whoever asks.
fn offered(rules: &McpRules, tool: &str) -> bool {
    (rules.tools.is_empty() || any_matches(&rules.tools, tool))
        && !any_matches(&rules.deny_tools, tool)
}

/// `who` may call `tool` of `server` here.
pub(crate) fn may_call(server: &ExtraMcp, who: &Who, tool: &str) -> bool {
    may_use(server, who)
        && offered(&server.rules, tool)
        && server
            .rules
            .restrict
            .iter()
            .filter(|r| any_matches(&r.tools, tool))
            .all(|r| who.any_of(&r.roles))
}

/// `who` may run `command` (it calls a tool of `server`).
pub(crate) fn may_run(command: &McpCommand, server: &ExtraMcp, who: &Who) -> bool {
    (!command.admins_only || who.admin())
        && (command.roles.is_empty() || who.any_of(&command.roles))
        && may_call(server, who, &command.tool)
}

/// A call of a tool with these annotations (`tools/list`, MCP 2025-06-18) needs approval. Unknown
/// annotations count as the protocol's defaults: not read-only, destructive.
pub(crate) fn needs_confirmation(rules: &McpRules, annotations: Option<&Value>) -> bool {
    let hint = |name: &str| {
        annotations
            .and_then(|a| a.get(name))
            .and_then(Value::as_bool)
    };
    match rules.confirm {
        Confirm::Never => false,
        Confirm::Always => true,
        Confirm::Destructive => {
            hint("readOnlyHint") != Some(true) && hint("destructiveHint") != Some(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::Restrict;

    fn who(roles: &[&str], room: &str) -> Who {
        Who {
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
            room: ("discord:main".parse().unwrap(), room.into()),
        }
    }

    fn server(rules: McpRules) -> ExtraMcp {
        ExtraMcp {
            server: okbot_core::McpServer {
                name: "ops".into(),
                url: "http://127.0.0.1:9/mcp".into(),
                bearer_token_env: "OPS_TOKEN".into(),
            },
            token: okbot_core::Secret::new("s".into()),
            admins_only: false,
            signed: false,
            rules,
            stdio: None,
        }
    }

    #[test]
    fn patterns() {
        assert!(matches("status", "status"));
        assert!(!matches("status", "status2"));
        assert!(matches("logs_*", "logs_tail"));
        assert!(matches("*", "anything"));
        assert!(matches("*_delete", "user_delete"));
        assert!(matches("a*b*c", "a-x-b-y-c"));
        assert!(!matches("a*b*c", "a-x-c"));
        assert!(!matches("deploy*", "redeploy"));
        assert!(matches("*deploy*", "redeploy_now"));
    }

    #[test]
    fn servers_by_role_and_room() {
        let open = server(McpRules::default());
        assert!(may_use(&open, &who(&["everyone"], "r1")));

        let mut admins = server(McpRules::default());
        admins.admins_only = true;
        assert!(!may_use(&admins, &who(&["everyone", "ops"], "r1")));
        assert!(may_use(&admins, &who(&["everyone", "admins"], "r1")));

        let ops = server(McpRules {
            roles: vec!["ops".into()],
            rooms: vec![("discord:main".parse().unwrap(), "ops-room".into())],
            ..McpRules::default()
        });
        assert!(may_use(&ops, &who(&["everyone", "ops"], "ops-room")));
        assert!(!may_use(&ops, &who(&["everyone", "ops"], "general")));
        assert!(!may_use(&ops, &who(&["everyone", "admins"], "ops-room")));
    }

    #[test]
    fn tools_by_lists_and_restrictions() {
        let s = server(McpRules {
            tools: vec!["status".into(), "logs_*".into(), "deploy*".into()],
            deny_tools: vec!["logs_purge".into()],
            restrict: vec![Restrict {
                tools: vec!["deploy*".into()],
                roles: vec!["ops".into(), "admins".into()],
            }],
            ..McpRules::default()
        });
        let member = who(&["everyone"], "r");
        let ops = who(&["everyone", "ops"], "r");
        assert!(may_call(&s, &member, "status"));
        assert!(may_call(&s, &member, "logs_tail"));
        assert!(!may_call(&s, &member, "logs_purge"), "deny wins");
        assert!(!may_call(&s, &ops, "logs_purge"));
        assert!(!may_call(&s, &member, "drop_db"), "not in tools");
        assert!(!may_call(&s, &member, "deploy_prod"), "restricted");
        assert!(may_call(&s, &ops, "deploy_prod"));
        assert!(may_call(&s, &who(&["admins"], "r"), "deploy"));
    }

    #[test]
    fn commands_follow_their_tool() {
        let s = server(McpRules {
            deny_tools: vec!["drop".into()],
            ..McpRules::default()
        });
        let command = |tool: &str, roles: &[&str], admins_only| McpCommand {
            name: "x".into(),
            description: String::new(),
            descriptions: Vec::new(),
            mcp: "ops".into(),
            tool: tool.into(),
            args: Vec::new(),
            with: serde_json::Map::new(),
            admins_only,
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
        };
        let member = who(&["everyone"], "r");
        assert!(may_run(&command("status", &[], false), &s, &member));
        assert!(!may_run(&command("drop", &[], false), &s, &member));
        assert!(!may_run(&command("status", &["ops"], false), &s, &member));
        assert!(!may_run(&command("status", &[], true), &s, &member));
        assert!(may_run(
            &command("status", &["ops"], false),
            &s,
            &who(&["everyone", "ops"], "r")
        ));
    }

    #[test]
    fn confirmation_from_annotations() {
        let rules = |confirm| McpRules {
            confirm,
            ..McpRules::default()
        };
        let destructive = rules(Confirm::Destructive);
        assert!(
            needs_confirmation(&destructive, None),
            "defaults: destructive"
        );
        assert!(needs_confirmation(&destructive, Some(&json!({}))));
        assert!(!needs_confirmation(
            &destructive,
            Some(&json!({"readOnlyHint": true}))
        ));
        assert!(!needs_confirmation(
            &destructive,
            Some(&json!({"destructiveHint": false}))
        ));
        assert!(needs_confirmation(
            &destructive,
            Some(&json!({"readOnlyHint": false, "destructiveHint": true}))
        ));
        assert!(!needs_confirmation(&rules(Confirm::Never), None));
        assert!(needs_confirmation(
            &rules(Confirm::Always),
            Some(&json!({"readOnlyHint": true}))
        ));
    }
}
