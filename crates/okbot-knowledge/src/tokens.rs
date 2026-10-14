//! Short-lived run tokens: one per agent run, mapped to the requester's scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use okbase::Scope;
use okbase_mcp::{RequestInfo, ScopeProvider};

use crate::scope;

struct Entry {
    scope: Scope,
    context: RunContext,
    expires: Instant,
}

/// Who a run token belongs to.
#[derive(Debug, Clone)]
pub(crate) struct RunContext {
    /// The requester.
    pub(crate) who: okbot_core::Requester,
    /// The run's tool policy (`web` or `mcp-only`), recorded on notes it writes.
    pub(crate) policy: &'static str,
    /// Media sent with the messages being answered, viewable through `attachment_view`.
    pub(crate) attachments: Vec<okbot_core::Attachment>,
    /// The requester administers the bot ([`okbot_core::RunInfo::admin`]).
    pub(crate) admin: bool,
}

/// Issued tokens. Tokens are 256-bit random values, valid until revoked or expired.
#[derive(Default)]
pub(crate) struct RunTokens {
    entries: Mutex<HashMap<String, Entry>>,
}

impl RunTokens {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Issues a token for `scope`, valid for `ttl`. Dropping the guard revokes it.
    pub(crate) fn issue(
        self: &Arc<Self>,
        scope: Scope,
        context: RunContext,
        ttl: Duration,
    ) -> (String, TokenGuard) {
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut entries = self.lock();
        let now = Instant::now();
        entries.retain(|_, e| e.expires > now);
        entries.insert(
            token.clone(),
            Entry {
                scope,
                context,
                expires: now + ttl,
            },
        );
        (
            token.clone(),
            TokenGuard {
                tokens: self.clone(),
                token,
            },
        )
    }

    /// The scope of a valid token.
    pub(crate) fn scope(&self, token: &str) -> Option<Scope> {
        let entries = self.lock();
        let entry = entries.get(token)?;
        (entry.expires > Instant::now()).then(|| entry.scope.clone())
    }

    /// The requester of a valid token.
    pub(crate) fn context(&self, token: &str) -> Option<RunContext> {
        let entries = self.lock();
        let entry = entries.get(token)?;
        (entry.expires > Instant::now()).then(|| entry.context.clone())
    }

    pub(crate) fn is_valid(&self, token: &str) -> bool {
        self.scope(token).is_some()
    }

    fn revoke(&self, token: &str) {
        self.lock().remove(token);
    }
}

/// Revokes its token when dropped.
pub(crate) struct TokenGuard {
    tokens: Arc<RunTokens>,
    token: String,
}

impl Drop for TokenGuard {
    fn drop(&mut self) {
        self.tokens.revoke(&self.token);
    }
}

/// okbase asks this for the scope of every tool call.
pub(crate) struct TokenScopes(pub(crate) Arc<RunTokens>);

impl ScopeProvider for TokenScopes {
    fn scope(&self, request: &RequestInfo<'_>) -> Scope {
        match (request.headers, request.bearer_token()) {
            // okbase computes tool descriptions once, without a request: show shared knowledge.
            (None, _) => scope::shared(),
            (Some(_), Some(token)) => self.0.scope(token).unwrap_or_else(scope::nothing),
            (Some(_), None) => scope::nothing(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RunContext {
        RunContext {
            who: okbot_core::Requester {
                conversation: okbot_core::ConversationKey::room("x:y".parse().unwrap(), "r"),
                user: okbot_core::UserRef {
                    id: "u".into(),
                    display_name: "u".into(),
                    is_bot: false,
                },
                internal: false,
                person: None,
                personal: true,
            },
            policy: "web",
            attachments: Vec::new(),
            admin: false,
        }
    }

    #[test]
    fn issue_lookup_revoke_expire() {
        let tokens = Arc::new(RunTokens::default());
        let (t, guard) = tokens.issue(scope::shared(), context(), Duration::from_secs(60));
        assert_eq!(tokens.context(&t).unwrap().who.user.id.as_str(), "u");
        assert_eq!(t.len(), 64);
        assert!(tokens.is_valid(&t));
        drop(guard);
        assert!(!tokens.is_valid(&t));

        let (short, _guard) = tokens.issue(scope::shared(), context(), Duration::ZERO);
        assert!(!tokens.is_valid(&short), "expired");
        assert!(!tokens.is_valid("forged"));
    }
}
