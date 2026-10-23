//! Accounts of `[[mcp]] auth = "oauth"` servers (ADR 0012): `/connect`, `/disconnect`,
//! `/connections`, the `/oauth/callback` page, and the credential each server gets in a turn.

use okbot_core::{InboundEvent, ReplyTarget, Secret};

use crate::Shared;
use crate::config::ExtraMcp;
use crate::oauth::{self, OAuthError};
use crate::permission::{self, Who};

/// What a server gets as its bearer token for one run.
pub(crate) enum Upstream {
    /// This token.
    Token(Secret<String>),
    /// The person (or, for shared credentials, an administrator) has not connected an account.
    NotConnected,
    /// Personal credentials outside a direct chat or `personal` room.
    NotHere,
    /// OAuth is not set up, or the provider failed.
    Unavailable,
}

/// The credential of `server` for a run on behalf of `event`'s author. `personal`: the answer is
/// only read by the author (a direct chat or a `personal` room).
pub(crate) async fn upstream(
    shared: &Shared,
    server: &ExtraMcp,
    event: &InboundEvent,
    person: &str,
    personal: bool,
    admin: bool,
    command: Option<&str>,
) -> Upstream {
    if server.oauth.is_none() {
        return Upstream::Token(crate::identity::token_for(
            shared, server, event, admin, command,
        ));
    }
    if server.personal_credentials() && !personal {
        return Upstream::NotHere;
    }
    let subject = oauth::subject_for(server, person);
    token_of(shared, server, &subject).await
}

/// The access token of `subject` for an OAuth server.
pub(crate) async fn token_of(shared: &Shared, server: &ExtraMcp, subject: &str) -> Upstream {
    let Some(connector) = &shared.oauth else {
        return Upstream::Unavailable;
    };
    match connector
        .access_token(server, subject, shared.config.agent_timeout)
        .await
    {
        Ok(Some(token)) => Upstream::Token(token),
        Ok(None) => Upstream::NotConnected,
        Err(error) => {
            tracing::warn!(mcp = %server.server.name, %error, "no OAuth token");
            Upstream::Unavailable
        }
    }
}

fn oauth_servers(shared: &Shared) -> impl Iterator<Item = &ExtraMcp> {
    shared.config.mcp.iter().filter(|m| m.oauth.is_some())
}

/// `who` may use `server` somewhere (its rooms are not checked: accounts are connected in a
/// direct chat).
fn may_connect(server: &ExtraMcp, who: &Who) -> bool {
    permission::may_use_somewhere(server, who)
}

/// `/connect [server]`.
pub(crate) async fn connect(shared: &Shared, name: &str, event: &InboundEvent) -> String {
    let name = name.trim().trim_start_matches('/');
    if name.is_empty() {
        return connections(shared, event).await;
    }
    if name.len() == 6 && name.bytes().all(|b| b.is_ascii_digit()) {
        return confirm(shared, name, event).await;
    }
    let who = Who::new(shared.roles_of(event).await, event);
    let Some(server) = oauth_servers(shared)
        .find(|m| m.server.name == name)
        .filter(|m| may_connect(m, &who))
    else {
        return format!(
            "`{name}` is not an account you can connect here. {}",
            available(shared, &who)
        );
    };
    let shared_account = !server.personal_credentials();
    if shared_account && !who.admin() {
        return format!("Only administrators can connect the shared `{name}` account.");
    }
    if !event.is_direct {
        return format!(
            "Send `/connect {name}` in a direct chat with me: the link connects whoever opens it."
        );
    }
    let Some(connector) = &shared.oauth else {
        return "Connecting accounts is not set up on this bot.".into();
    };
    let person = shared.person_key(event).await;
    let subject = oauth::subject_for(server, &person);
    let label = format!(
        "{} ({}/{})",
        event.author.display_name, event.conversation.channel, event.author.id
    );
    let whose = if shared_account {
        "the bot (shared by everyone who may use it)"
    } else {
        "you"
    };
    match connector
        .begin(server, &subject, &label, Some(event.conversation.clone()))
        .await
    {
        Ok(url) => format!(
            "Open this link to connect your **{name}** account to {whose}. It works once, for 10 \
             minutes:\n{url}\n-# Do not share it: whoever opens it connects their account here."
        ),
        Err(OAuthError::NoRedirect) => format!(
            "This bot cannot receive the provider's answer: the operator sets `[http] public_url` \
             or runs `okbot mcp login {name}`."
        ),
        Err(error) => {
            tracing::warn!(mcp = %name, %error, "cannot start connecting");
            format!("⚠️ Cannot connect `{name}` now: {error}")
        }
    }
}

/// `/connect <code>`: keeps the account whose page showed the code.
async fn confirm(shared: &Shared, code: &str, event: &InboundEvent) -> String {
    let Some(connector) = &shared.oauth else {
        return "Connecting accounts is not set up on this bot.".into();
    };
    let person = shared.person_key(event).await;
    // The code is bound to the subject the link was for: the person, or `shared`.
    let mut subjects = vec![person];
    if shared.is_admin(event).await {
        subjects.push(oauth::SHARED.to_string());
    }
    for subject in subjects {
        match connector.confirm(code, &event.conversation, &subject).await {
            Ok(Some(server)) => return format!("✓ Connected **{server}**."),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, "cannot keep the connected account");
                return shared.config.replies.error.clone();
            }
        }
    }
    "That code is not waiting here. Codes work once, for 10 minutes, in the chat where you asked \
     for the link: send `/connect <name>` again."
        .into()
}

/// `/disconnect <server>`.
pub(crate) async fn disconnect(shared: &Shared, name: &str, event: &InboundEvent) -> String {
    let name = name.trim();
    let who = Who::new(shared.roles_of(event).await, event);
    let Some(server) = oauth_servers(shared).find(|m| m.server.name == name) else {
        return format!("Use `/disconnect <name>`. {}", available(shared, &who));
    };
    if !server.personal_credentials() && !who.admin() {
        return format!("Only administrators can disconnect the shared `{name}` account.");
    }
    let Some(connector) = &shared.oauth else {
        return "Connecting accounts is not set up on this bot.".into();
    };
    let subject = oauth::subject_for(server, &shared.person_key(event).await);
    match connector.disconnect(name, &subject).await {
        Ok(true) => format!(
            "Disconnected `{name}`. I no longer use that account; the provider may still list \
             okbot under connected apps, where you can remove it."
        ),
        Ok(false) => format!("`{name}` was not connected."),
        Err(error) => {
            tracing::error!(%error, "cannot disconnect");
            shared.config.replies.error.clone()
        }
    }
}

fn available(shared: &Shared, who: &Who) -> String {
    let names: Vec<String> = oauth_servers(shared)
        .filter(|m| may_connect(m, who))
        .map(|m| format!("`{}`", m.server.name))
        .collect();
    if names.is_empty() {
        "This bot has no accounts to connect.".into()
    } else {
        format!("Accounts: {}.", names.join(", "))
    }
}

/// `/connections`: the accounts the asker may connect and whether they are.
pub(crate) async fn connections(shared: &Shared, event: &InboundEvent) -> String {
    let who = Who::new(shared.roles_of(event).await, event);
    let person = shared.person_key(event).await;
    let servers: Vec<&ExtraMcp> = oauth_servers(shared)
        .filter(|m| may_connect(m, &who))
        .collect();
    if servers.is_empty() {
        return "This bot has no accounts to connect.".into();
    }
    let Some(connector) = &shared.oauth else {
        return "Connecting accounts is not set up on this bot.".into();
    };
    let mut lines = Vec::new();
    for server in servers {
        let name = &server.server.name;
        let subject = oauth::subject_for(server, &person);
        let connected = connector.connected(server, &subject).await.unwrap_or(false);
        let line = match (server.personal_credentials(), connected) {
            (true, true) => format!("- **{name}**: connected (`/disconnect {name}`)"),
            (true, false) => format!("- **{name}**: not connected (`/connect {name}`)"),
            (false, true) => format!("- **{name}**: the bot's shared account is connected"),
            (false, false) => {
                format!("- **{name}**: shared account not connected (an administrator connects it)")
            }
        };
        lines.push(line);
    }
    format!("Accounts:\n{}", lines.join("\n"))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>body{{font:16px \
         system-ui,sans-serif;max-width:36em;margin:3em auto;padding:0 1em;line-height:1.5}}\
         </style></head><body><h1>{t}</h1><p>{body}</p></body></html>",
        t = escape(title)
    )
}

/// `GET /oauth/callback`: finishes a connect link and tells the chat. Returns status and HTML.
pub(crate) async fn callback(shared: &Shared, query: &[(String, String)]) -> (u16, String) {
    let get = |k: &str| {
        query
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let Some(connector) = &shared.oauth else {
        return (404, page("Not found", "Nothing is waiting here."));
    };
    let state = get("state").unwrap_or_default();
    if !connector.knows(state) {
        return (
            400,
            page(
                "Link unknown or expired",
                "This connection link was already used, has expired, or was started from the \
                 terminal. If you ran <code>okbot mcp login</code>, paste this page's address \
                 there. Otherwise send <code>/connect</code> again.",
            ),
        );
    }
    match connector.complete(state, get("code"), get("error")).await {
        Ok(done) => {
            let Some(code) = &done.confirm_code else {
                return (
                    200,
                    page(
                        "Connected",
                        &format!(
                            "Your <b>{}</b> account is connected to {}. You can close this page.",
                            escape(&done.server),
                            escape(&shared.config.bot_name),
                        ),
                    ),
                );
            };
            if let Some(conversation) = &done.conversation {
                let to = ReplyTarget {
                    conversation: conversation.clone(),
                    in_reply_to: None,
                };
                let text = format!(
                    "Your browser shows a code: send `/connect <code>` here to finish connecting \
                     **{}**.",
                    done.server
                );
                if let Err(error) = crate::turn::send_to(shared, &to, &text).await {
                    tracing::warn!(%error, "cannot ask for the code in the chat");
                }
            }
            (
                200,
                page(
                    "Almost done",
                    &format!(
                        "To finish connecting your <b>{}</b> account to {} for {}, send this in \
                         that chat within 10 minutes:</p><p><code>/connect {}</code></p><p>If \
                         someone else sent you this link, close this page and do not share the \
                         code: nothing is connected until it is sent.",
                        escape(&done.server),
                        escape(&shared.config.bot_name),
                        escape(&done.label),
                        escape(code),
                    ),
                ),
            )
        }
        Err(error) => {
            tracing::warn!(%error, "connecting an account failed");
            (
                400,
                page(
                    "Not connected",
                    &format!(
                        "{}. Send <code>/connect</code> again to retry.",
                        escape(&error.to_string())
                    ),
                ),
            )
        }
    }
}
