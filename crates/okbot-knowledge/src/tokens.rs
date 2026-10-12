//! Short-lived run tokens: one per agent run, mapped to the requester's scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use okbase::Scope;
use okbase_mcp::{RequestInfo, ScopeProvider};

use crate::scope;

struct Entry {
    scope: Scope,
    expires: Instant,
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
    pub(crate) fn issue(self: &Arc<Self>, scope: Scope, ttl: Duration) -> (String, TokenGuard) {
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut entries = self.lock();
        let now = Instant::now();
        entries.retain(|_, e| e.expires > now);
        entries.insert(
            token.clone(),
            Entry {
                scope,
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

    #[test]
    fn issue_lookup_revoke_expire() {
        let tokens = Arc::new(RunTokens::default());
        let (t, guard) = tokens.issue(scope::shared(), Duration::from_secs(60));
        assert_eq!(t.len(), 64);
        assert!(tokens.is_valid(&t));
        drop(guard);
        assert!(!tokens.is_valid(&t));

        let (short, _guard) = tokens.issue(scope::shared(), Duration::ZERO);
        assert!(!tokens.is_valid(&short), "expired");
        assert!(!tokens.is_valid("forged"));
    }
}
