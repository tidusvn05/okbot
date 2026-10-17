//! Signed run identity for `[[mcp]] auth = "signed"` servers (ADR 0007).
//!
//! Instead of the configured secret, the agent gets a short-lived token that says who asked and
//! where, signed with that secret, so the server can authorize per person:
//!
//! ```text
//! okbot1.<base64url(JSON claims)>.<base64url(HMAC-SHA256(secret, "okbot1." + base64url(claims)))>
//! ```
//!
//! The agent cannot forge or widen it. The same token authenticates the server's calls back to
//! okbot (`POST /interact`), which can therefore only reach that conversation.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{KeyInit, Mac};
use okbot_core::ConversationKey;
use serde::{Deserialize, Serialize};

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

const PREFIX: &str = "okbot1.";

/// What a signed token says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// `[bot] name`.
    pub bot: String,
    /// The `[[mcp]]` server the token is for.
    pub mcp: String,
    /// The person, as `platform:instance/user-id` (the form `[bot] admins` uses).
    pub user: String,
    /// Display name.
    pub name: String,
    /// Where the turn runs.
    pub conversation: ConversationKey,
    /// Listed in `[bot] admins` (or the owner of a personal bot).
    pub admin: bool,
    /// A one-to-one chat.
    pub direct: bool,
    /// okbot's endpoint for prompts, when `[http]` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interact: Option<String>,
    /// Set when a person ran a `[[command]]` (its name): the result is shown to them as is,
    /// without the agent, so the server should answer in readable text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The person's language as their platform reports it (`en-US`, `ja`), when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Expiry, Unix seconds.
    pub exp: i64,
    /// Random id of this token.
    pub jti: String,
}

fn mac(key: &[u8]) -> HmacSha256 {
    <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes keys of any length")
}

/// The signed token for `claims`.
pub fn sign(key: &[u8], claims: &Claims) -> String {
    let body = B64.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signed = format!("{PREFIX}{body}");
    let mut m = mac(key);
    m.update(signed.as_bytes());
    format!("{signed}.{}", B64.encode(m.finalize().into_bytes()))
}

/// What `[[mcp]] extra` gets for a run on behalf of `event`'s author: a signed token naming them,
/// or the configured secret itself. `command` is set for `[[command]]` runs.
pub(crate) fn token_for(
    shared: &crate::Shared,
    extra: &crate::ExtraMcp,
    event: &okbot_core::InboundEvent,
    admin: bool,
    command: Option<&str>,
) -> okbot_core::Secret<String> {
    if !extra.signed {
        return extra.token.clone();
    }
    let claims = Claims {
        bot: shared.config.bot_name.clone(),
        mcp: extra.server.name.clone(),
        user: format!("{}/{}", event.conversation.channel, event.author.id),
        name: event.author.display_name.clone(),
        conversation: event.conversation.clone(),
        admin,
        direct: event.is_direct,
        interact: shared.interact_url.get().cloned(),
        command: command.map(str::to_string),
        locale: event.locale.clone(),
        exp: shared.clock.now().as_second()
            + i64::try_from(shared.config.agent_timeout.as_secs()).unwrap_or(3600)
            + 60,
        jti: hex::encode(rand::random::<[u8; 8]>()),
    };
    okbot_core::Secret::new(sign(extra.token.expose().as_bytes(), &claims))
}

/// The server name a token claims, read before its signature is checked (to pick the key).
pub fn claimed_server(token: &str) -> Option<String> {
    let body = token.strip_prefix(PREFIX)?.split('.').next()?;
    let claims: Claims = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
    Some(claims.mcp)
}

/// The claims of a token signed with `key` and not expired at `now` (Unix seconds).
pub fn verify(key: &[u8], token: &str, now: i64) -> Option<Claims> {
    let (signed, signature) = token.rsplit_once('.')?;
    let body = signed.strip_prefix(PREFIX)?;
    let mut m = mac(key);
    m.update(signed.as_bytes());
    m.verify_slice(&B64.decode(signature).ok()?).ok()?;
    let claims: Claims = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
    (claims.exp > now).then_some(claims)
}

/// The signature of a callback body: `base64url(HMAC-SHA256(key, "<timestamp>.<body>"))`.
pub fn sign_body(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut m = mac(key);
    m.update(timestamp.to_string().as_bytes());
    m.update(b".");
    m.update(body);
    B64.encode(m.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: i64) -> Claims {
        Claims {
            bot: "ops".into(),
            mcp: "ops".into(),
            user: "discord:main/42".into(),
            name: "Lan".into(),
            conversation: okbot_testkit::conversation("discord:main", "500"),
            admin: true,
            direct: false,
            interact: Some("http://127.0.0.1:8788/interact".into()),
            command: None,
            locale: None,
            exp,
            jti: "abc".into(),
        }
    }

    #[test]
    fn round_trip_and_rejections() {
        let token = sign(b"k", &claims(100));
        assert!(token.starts_with("okbot1."));
        assert_eq!(claimed_server(&token).as_deref(), Some("ops"));
        assert_eq!(verify(b"k", &token, 99), Some(claims(100)));
        assert_eq!(verify(b"k", &token, 100), None, "expired");
        assert_eq!(verify(b"other", &token, 0), None, "wrong key");
        let forged = sign(
            b"k",
            &Claims {
                admin: false,
                ..claims(100)
            },
        );
        let (head, _) = forged.rsplit_once('.').unwrap();
        let (_, sig) = token.rsplit_once('.').unwrap();
        assert_eq!(
            verify(b"k", &format!("{head}.{sig}"), 0),
            None,
            "claims swapped"
        );
        assert_eq!(verify(b"k", "okbot1.x.y", 0), None);
    }

    #[test]
    fn body_signature_is_stable() {
        assert_eq!(sign_body(b"k", 1, b"{}"), sign_body(b"k", 1, b"{}"));
        assert_ne!(sign_body(b"k", 1, b"{}"), sign_body(b"k", 2, b"{}"));
    }
}
