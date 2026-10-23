//! OAuth for `[[mcp]] auth = "oauth"` servers (ADR 0012).
//!
//! okbot is the client of the MCP specification's authorization flow:
//!
//! ```text
//! MCP server ──401 + resource_metadata──► protected resource metadata ──► authorization server
//!   metadata (RFC 8414 / OpenID) ──► client registration (RFC 7591, once per server) ──►
//!   authorize (PKCE S256, state, resource) ──browser──► /oauth/callback ──► token ──► refresh
//! ```
//!
//! Tokens and registrations are sealed with ChaCha20-Poly1305 before they reach the store, with
//! the kind, server and subject as associated data, so a sealed value only opens where it was
//! written. A refresh holds a lock per credential: providers rotate refresh tokens, and a second
//! refresh with the old one would fail or revoke the grant.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use jiff::Timestamp;
use okbot_core::{Clock, ConversationKey, ConversationStore, Secret};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{Credentials, ExtraMcp};

/// The subject of shared credentials.
pub const SHARED: &str = "shared";
/// How long a connect link works.
const PENDING_TTL: Duration = Duration::from_secs(600);
/// Links waiting at most (older ones are dropped first).
const MAX_PENDING: usize = 256;
/// Tokens this close to expiry are refreshed before use.
const REFRESH_MARGIN: i64 = 120;
/// Upper bound for one request to a provider.
const TIMEOUT: Duration = Duration::from_secs(20);
const KIND_TOKEN: &str = "oauth-token";
const KIND_CLIENT: &str = "oauth-client";
/// The registration of every server is stored under this subject.
const CLIENT_SUBJECT: &str = "client";

/// A failure of the flow or of a provider.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OAuthError {
    /// The server is not an OAuth one.
    #[error("`{0}` does not use OAuth")]
    NotOAuth(String),
    /// No redirect address is configured, so no browser can come back.
    #[error("set [http] listen (and public_url for other machines) to connect accounts")]
    NoRedirect,
    /// The provider's metadata could not be found or read.
    #[error("cannot find how `{server}` logs in: {reason}")]
    Discovery {
        /// The server.
        server: String,
        /// Why.
        reason: String,
    },
    /// The provider refused a request.
    #[error("the provider refused: {0}")]
    Refused(String),
    /// The provider no longer accepts the grant (`invalid_grant`): connect again.
    #[error("the provider no longer accepts this connection: {0}")]
    InvalidGrant(String),
    /// The link is unknown, used or expired.
    #[error("this link is unknown, already used or expired")]
    UnknownState,
    /// The person declined on the provider's page.
    #[error("access was not granted ({0})")]
    Denied(String),
    /// The provider could not be reached.
    #[error("the provider cannot be reached: {0}")]
    Http(String),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] okbot_core::StoreError),
}

fn http_error(error: reqwest::Error) -> OAuthError {
    // reqwest names the URL, which may carry a code or a key.
    OAuthError::Http(error.without_url().to_string())
}

/// The key that seals stored credentials (`state/secrets.key`).
#[derive(Clone)]
pub struct SecretKey(Arc<LessSafeKey>);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey([REDACTED])")
    }
}

impl SecretKey {
    /// Length of the key in bytes.
    pub const LEN: usize = 32;

    /// A key from its bytes.
    pub fn new(bytes: &Secret<[u8; 32]>) -> Self {
        let key = UnboundKey::new(&CHACHA20_POLY1305, bytes.expose())
            .expect("ChaCha20-Poly1305 takes 32-byte keys");
        Self(Arc::new(LessSafeKey::new(key)))
    }

    /// A new random key's bytes.
    pub fn generate() -> Secret<[u8; 32]> {
        Secret::new(rand::random())
    }

    fn seal(&self, aad: &str, plain: &[u8]) -> Vec<u8> {
        let nonce: [u8; 12] = rand::random();
        let mut data = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad.as_bytes()),
                &mut data,
            )
            .expect("sealing a short value cannot fail");
        let mut out = nonce.to_vec();
        out.extend(data);
        out
    }

    fn open(&self, aad: &str, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 12 {
            return None;
        }
        let (nonce, data) = sealed.split_at(12);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut data = data.to_vec();
        let plain = self
            .0
            .open_in_place(nonce, Aad::from(aad.as_bytes()), &mut data)
            .ok()?;
        Some(plain.to_vec())
    }
}

/// Where the provider's endpoints are, for one server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Endpoints {
    issuer: String,
    authorization: String,
    token: String,
    registration: Option<String>,
    /// The canonical URI of the MCP server (the `resource` parameter).
    resource: String,
    /// Scopes to ask for.
    scopes: Vec<String>,
}

/// A registered client.
#[derive(Clone, Serialize, Deserialize)]
struct Client {
    /// The `[[mcp]] url` it was found for: a changed URL means a new discovery.
    server_url: String,
    endpoints: Endpoints,
    id: String,
    #[serde(default)]
    secret: Option<String>,
    /// `none`, `client_secret_basic` or `client_secret_post`.
    auth_method: String,
    redirect_uris: Vec<String>,
    /// Registered by okbot (and so registered again when the redirect addresses change).
    dynamic: bool,
}

/// A stored token, with the client it was issued to (a refresh must come from that client,
/// even after okbot registers a new one for new connections).
#[derive(Clone, Serialize, Deserialize)]
struct Token {
    access: String,
    #[serde(default)]
    refresh: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    expires_at: Option<i64>,
    client: Client,
}

/// A connect link waiting for the browser to come back.
struct Pending {
    server: String,
    subject: String,
    verifier: String,
    redirect_uri: String,
    client: Client,
    created: std::time::Instant,
    /// Who asked, as shown on the callback page.
    label: String,
    /// The chat to tell when it is done.
    conversation: Option<ConversationKey>,
}

/// A finished connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// The server.
    pub server: String,
    /// Whose credentials they are.
    pub subject: String,
    /// Who asked, as given to [`Connector::begin`].
    pub label: String,
    /// The chat the link was asked for in.
    pub conversation: Option<ConversationKey>,
    /// Links asked for in a chat end with this code on the page: the account is kept only when
    /// the asker sends it in that chat ([`Connector::confirm`]), so a link someone else opened
    /// connects nothing.
    pub confirm_code: Option<String>,
}

/// A token waiting for the asker to send the code its browser showed.
struct Confirming {
    server: String,
    subject: String,
    conversation: ConversationKey,
    token: Token,
    created: std::time::Instant,
}

/// Wrong codes tolerated per asker before their waiting connections are dropped.
const MAX_CODE_FAILURES: u32 = 5;

/// Runs the flow, stores the results and hands out fresh access tokens.
pub struct Connector {
    store: Arc<dyn ConversationStore>,
    key: SecretKey,
    clock: Arc<dyn Clock>,
    http: reqwest::Client,
    /// Where links send the browser back to (the bot's `/oauth/callback`).
    link_redirect: Option<String>,
    /// Every address registered with providers: the link one and the terminal's.
    redirect_uris: Vec<String>,
    client_name: String,
    pending: Mutex<HashMap<String, Pending>>,
    /// Tokens waiting for their code, by code.
    confirming: Mutex<HashMap<String, Confirming>>,
    /// Wrong codes by (conversation, subject).
    code_failures: Mutex<HashMap<(String, String), u32>>,
    locks: Mutex<HashMap<(String, String), RefreshLock>>,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("redirect_uris", &self.redirect_uris)
            .finish_non_exhaustive()
    }
}

/// Held while one credential is refreshed.
type RefreshLock = Arc<tokio::sync::Mutex<()>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn aad(kind: &str, server: &str, subject: &str) -> String {
    format!("{kind}\n{server}\n{subject}")
}

/// The subject whose credentials `server` uses for `person` ([`okbot_core::person_key`]).
pub fn subject_for(server: &ExtraMcp, person: &str) -> String {
    match server.oauth.as_ref().map(|o| o.credentials) {
        Some(Credentials::Shared) => SHARED.to_string(),
        _ => person.to_string(),
    }
}

impl Connector {
    /// A connector storing in `store`, sealing with `key`. Links send the browser back to
    /// `link_redirect` (the bot's `…/oauth/callback`); `other_redirects` (the terminal's) are
    /// registered too, for [`Connector::begin_with`]. `bot_name` names the client on the
    /// provider's consent page.
    pub fn new(
        store: Arc<dyn ConversationStore>,
        key: SecretKey,
        clock: Arc<dyn Clock>,
        link_redirect: Option<String>,
        other_redirects: Vec<String>,
        bot_name: &str,
    ) -> Self {
        let mut redirect_uris: Vec<String> = link_redirect.iter().cloned().collect();
        for uri in other_redirects {
            if !redirect_uris.contains(&uri) {
                redirect_uris.push(uri);
            }
        }
        let _ = rustls::crypto::ring::default_provider().install_default(); // see crates/okbot-cli/src/main.rs
        Self {
            store,
            key,
            clock,
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                // Metadata, registration and token endpoints answer directly; a redirect would
                // carry a code or a refresh token elsewhere.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            link_redirect,
            redirect_uris,
            client_name: format!("okbot ({bot_name})"),
            pending: Mutex::default(),
            confirming: Mutex::default(),
            code_failures: Mutex::default(),
            locks: Mutex::default(),
        }
    }

    /// The address links send the browser back to, if any.
    pub fn redirect_uri(&self) -> Option<&str> {
        self.link_redirect.as_deref()
    }

    async fn load<T: for<'de> Deserialize<'de>>(
        &self,
        kind: &str,
        server: &str,
        subject: &str,
    ) -> Result<Option<T>, OAuthError> {
        let Some(sealed) = self.store.sealed(kind, server, subject).await? else {
            return Ok(None);
        };
        let opened = self.key.open(&aad(kind, server, subject), &sealed);
        if opened.is_none() {
            // Another key (state/secrets.key replaced) or a moved value: unusable either way.
            tracing::warn!(%server, %kind, "a stored credential cannot be opened; connect again");
        }
        Ok(opened.and_then(|plain| serde_json::from_slice(&plain).ok()))
    }

    async fn save<T: Serialize>(
        &self,
        kind: &str,
        server: &str,
        subject: &str,
        value: &T,
    ) -> Result<(), OAuthError> {
        let plain = serde_json::to_vec(value).expect("credentials serialize");
        let sealed = self.key.seal(&aad(kind, server, subject), &plain);
        self.store.set_sealed(kind, server, subject, sealed).await?;
        Ok(())
    }

    /// Starts connecting `subject`'s account of `server`: the address of the provider's page.
    /// `label` names who asked on the callback page; `conversation` is told when it is done.
    pub async fn begin(
        &self,
        server: &ExtraMcp,
        subject: &str,
        label: &str,
        conversation: Option<ConversationKey>,
    ) -> Result<String, OAuthError> {
        self.begin_with(server, subject, label, conversation, None)
            .await
    }

    /// Like [`Connector::begin`], sending the browser back to `redirect_uri` (one of the
    /// registered addresses) instead of the link address.
    pub async fn begin_with(
        &self,
        server: &ExtraMcp,
        subject: &str,
        label: &str,
        conversation: Option<ConversationKey>,
        redirect_uri: Option<&str>,
    ) -> Result<String, OAuthError> {
        if server.oauth.is_none() {
            return Err(OAuthError::NotOAuth(server.server.name.clone()));
        }
        let redirect_uri = redirect_uri
            .filter(|r| self.redirect_uris.iter().any(|u| u == r))
            .or_else(|| self.redirect_uri())
            .ok_or(OAuthError::NoRedirect)?
            .to_string();
        let client = self.client(server).await?;
        let verifier = B64.encode(rand::random::<[u8; 32]>());
        let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
        let state = B64.encode(rand::random::<[u8; 24]>());
        let mut url = reqwest::Url::parse(&client.endpoints.authorization).map_err(|e| {
            OAuthError::Discovery {
                server: server.server.name.clone(),
                reason: format!("authorization endpoint: {e}"),
            }
        })?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &client.id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("resource", &client.endpoints.resource);
            if !client.endpoints.scopes.is_empty() {
                q.append_pair("scope", &client.endpoints.scopes.join(" "));
            }
        }
        let mut pending = lock(&self.pending);
        // One link per person and server: a new one replaces theirs, never someone else's.
        pending.retain(|_, p| {
            p.created.elapsed() < PENDING_TTL
                && !(p.server == server.server.name && p.subject == subject)
        });
        if pending.len() >= MAX_PENDING
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(k, _)| k.clone())
        {
            pending.remove(&oldest);
        }
        pending.insert(
            state,
            Pending {
                server: server.server.name.clone(),
                subject: subject.to_string(),
                verifier,
                redirect_uri,
                client,
                created: std::time::Instant::now(),
                label: label.to_string(),
                conversation,
            },
        );
        Ok(url.to_string())
    }

    /// Whether `state` belongs to a link this connector handed out and that still works.
    pub fn knows(&self, state: &str) -> bool {
        lock(&self.pending)
            .get(state)
            .is_some_and(|p| p.created.elapsed() < PENDING_TTL)
    }

    /// Finishes a link with the provider's answer (the callback's query parameters).
    pub async fn complete(
        &self,
        state: &str,
        code: Option<&str>,
        error: Option<&str>,
    ) -> Result<Connected, OAuthError> {
        let pending = lock(&self.pending)
            .remove(state)
            .filter(|p| p.created.elapsed() < PENDING_TTL)
            .ok_or(OAuthError::UnknownState)?;
        if let Some(error) = error {
            return Err(OAuthError::Denied(error.chars().take(100).collect()));
        }
        let code = code.ok_or_else(|| OAuthError::Refused("no code in the answer".into()))?;
        let form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", pending.redirect_uri.clone()),
            ("code_verifier", pending.verifier.clone()),
            ("resource", pending.client.endpoints.resource.clone()),
        ];
        let token = self.token_request(&pending.client, form).await?;
        let confirm_code = match &pending.conversation {
            Some(conversation) => {
                let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
                let mut waiting = lock(&self.confirming);
                waiting.retain(|_, c| c.created.elapsed() < PENDING_TTL);
                waiting.insert(
                    code.clone(),
                    Confirming {
                        server: pending.server.clone(),
                        subject: pending.subject.clone(),
                        conversation: conversation.clone(),
                        token,
                        created: std::time::Instant::now(),
                    },
                );
                Some(code)
            }
            None => {
                self.save(KIND_TOKEN, &pending.server, &pending.subject, &token)
                    .await?;
                tracing::info!(mcp = %pending.server, "account connected");
                None
            }
        };
        Ok(Connected {
            server: pending.server,
            subject: pending.subject,
            label: pending.label,
            conversation: pending.conversation,
            confirm_code,
        })
    }

    /// Keeps the account waiting for `code`, when `subject` sends it in `conversation`: the
    /// server's name. After five wrong codes the asker's waiting accounts are
    /// dropped, so codes cannot be guessed.
    pub async fn confirm(
        &self,
        code: &str,
        conversation: &ConversationKey,
        subject: &str,
    ) -> Result<Option<String>, OAuthError> {
        let found = {
            let mut waiting = lock(&self.confirming);
            waiting.retain(|_, c| c.created.elapsed() < PENDING_TTL);
            let matches = waiting
                .get(code)
                .is_some_and(|c| c.conversation == *conversation && c.subject == subject);
            if matches { waiting.remove(code) } else { None }
        };
        let key = (conversation.to_string(), subject.to_string());
        let Some(c) = found else {
            let mut failures = lock(&self.code_failures);
            let count = failures.entry(key).or_default();
            *count += 1;
            if *count >= MAX_CODE_FAILURES {
                *count = 0;
                lock(&self.confirming)
                    .retain(|_, c| !(c.conversation == *conversation && c.subject == subject));
            }
            return Ok(None);
        };
        lock(&self.code_failures).remove(&key);
        self.save(KIND_TOKEN, &c.server, &c.subject, &c.token)
            .await?;
        tracing::info!(mcp = %c.server, "account connected");
        Ok(Some(c.server))
    }

    /// Finishes a link from the address the browser was sent to (pasted into a terminal).
    pub async fn complete_url(&self, address: &str) -> Result<Connected, OAuthError> {
        let url = reqwest::Url::parse(address.trim()).map_err(|_| OAuthError::UnknownState)?;
        let get = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let state = get("state").ok_or(OAuthError::UnknownState)?;
        self.complete(&state, get("code").as_deref(), get("error").as_deref())
            .await
    }

    /// Whether `subject` has credentials for `server`.
    pub async fn connected(&self, server: &ExtraMcp, subject: &str) -> Result<bool, OAuthError> {
        Ok(self
            .load::<Token>(KIND_TOKEN, &server.server.name, subject)
            .await?
            .is_some())
    }

    /// Forgets `subject`'s credentials for `server`. Returns whether there were any.
    pub async fn disconnect(&self, server: &str, subject: &str) -> Result<bool, OAuthError> {
        Ok(self
            .store
            .delete_sealed(KIND_TOKEN, server, subject)
            .await?)
    }

    /// Every stored credential: `(server, subject, last change)`.
    pub async fn list(&self) -> Result<Vec<(String, String, Timestamp)>, OAuthError> {
        Ok(self
            .store
            .sealed_entries(KIND_TOKEN)
            .await?
            .into_iter()
            .map(|e| (e.server, e.subject, e.updated_at))
            .collect())
    }

    /// A usable access token of `subject` for `server`, refreshed when it expires within
    /// `margin` (plus two minutes); `None` when not connected or no longer valid.
    pub async fn access_token(
        &self,
        server: &ExtraMcp,
        subject: &str,
        margin: Duration,
    ) -> Result<Option<Secret<String>>, OAuthError> {
        let name = &server.server.name;
        let needed = self.clock.now().as_second()
            + i64::try_from(margin.as_secs()).unwrap_or(0)
            + REFRESH_MARGIN;
        let fresh = |t: &Token| t.expires_at.is_none_or(|exp| exp > needed);
        match self.load::<Token>(KIND_TOKEN, name, subject).await? {
            None => return Ok(None),
            Some(t) if fresh(&t) => return Ok(Some(Secret::new(t.access))),
            Some(_) => {}
        }
        let guard = lock(&self.locks)
            .entry((name.clone(), subject.to_string()))
            .or_default()
            .clone();
        let _held = guard.lock().await;
        // Another turn may have refreshed it while this one waited.
        let Some(token) = self.load::<Token>(KIND_TOKEN, name, subject).await? else {
            return Ok(None);
        };
        if fresh(&token) {
            return Ok(Some(Secret::new(token.access)));
        }
        let Some(refresh) = token.refresh.clone() else {
            tracing::info!(mcp = %name, "token expired and cannot be refreshed; connect again");
            return Ok(None);
        };
        let client = token.client;
        let form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh.clone()),
            ("resource", client.endpoints.resource.clone()),
        ];
        match self.token_request(&client, form).await {
            Ok(mut new) => {
                // Providers that do not rotate refresh tokens leave them out.
                if new.refresh.is_none() {
                    new.refresh = Some(refresh);
                }
                // Disconnected while the refresh was on its way: stay disconnected.
                if self
                    .store
                    .sealed(KIND_TOKEN, name, subject)
                    .await?
                    .is_none()
                {
                    return Ok(None);
                }
                self.save(KIND_TOKEN, name, subject, &new).await?;
                tracing::info!(mcp = %name, "token refreshed");
                Ok(Some(Secret::new(new.access)))
            }
            Err(OAuthError::InvalidGrant(reason)) => {
                // Another process (the console, `okbot mcp login`) may have rotated it meanwhile.
                if let Some(current) = self.load::<Token>(KIND_TOKEN, name, subject).await?
                    && current.refresh.as_deref() != Some(refresh.as_str())
                {
                    return Ok(fresh(&current).then(|| Secret::new(current.access)));
                }
                tracing::warn!(mcp = %name, %reason, "refresh refused; the account must be connected again");
                self.store.delete_sealed(KIND_TOKEN, name, subject).await?;
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }

    /// The registered client of `server`: stored, configured, or registered now.
    async fn client(&self, server: &ExtraMcp) -> Result<Client, OAuthError> {
        let name = &server.server.name;
        let settings = server
            .oauth
            .as_ref()
            .ok_or_else(|| OAuthError::NotOAuth(name.clone()))?;
        let stored: Option<Client> = self.load(KIND_CLIENT, name, CLIENT_SUBJECT).await?;
        if let Some(client) = stored.filter(|c| {
            let same_client = match &settings.client_id {
                Some(id) => !c.dynamic && c.id == *id,
                None => c.dynamic && c.redirect_uris == self.redirect_uris,
            };
            same_client && c.server_url == server.server.url
        }) {
            return Ok(client);
        }
        let endpoints = self.discover(server).await?;
        let client = match &settings.client_id {
            Some(id) => Client {
                server_url: server.server.url.clone(),
                endpoints,
                id: id.clone(),
                secret: settings.client_secret.as_ref().map(|s| s.expose().clone()),
                auth_method: if settings.client_secret.is_some() {
                    "client_secret_post".into()
                } else {
                    "none".into()
                },
                redirect_uris: self.redirect_uris.clone(),
                dynamic: false,
            },
            None => self.register(server, endpoints).await?,
        };
        self.save(KIND_CLIENT, name, CLIENT_SUBJECT, &client)
            .await?;
        Ok(client)
    }

    async fn get_json(&self, url: &str) -> Result<Option<Value>, OAuthError> {
        let reply = self
            .http
            .get(url)
            .header("accept", "application/json")
            .header("mcp-protocol-version", "2025-06-18")
            .send()
            .await
            .map_err(http_error)?;
        if !reply.status().is_success() {
            return Ok(None);
        }
        Ok(reply.json::<Value>().await.ok().filter(Value::is_object))
    }

    /// Finds the authorization server of `server` and its endpoints.
    async fn discover(&self, server: &ExtraMcp) -> Result<Endpoints, OAuthError> {
        let name = &server.server.name;
        let fail = |reason: String| OAuthError::Discovery {
            server: name.clone(),
            reason,
        };
        let mcp = reqwest::Url::parse(&server.server.url).map_err(|e| fail(e.to_string()))?;
        let origin = mcp.origin().ascii_serialization();
        let resource_meta = self.resource_metadata(&server.server.url, &mcp).await?;
        let resource = resource_meta
            .as_ref()
            .and_then(|m| m.get("resource"))
            .and_then(Value::as_str)
            .map_or_else(|| canonical(&server.server.url), str::to_string);
        // Servers without resource metadata (the 2025-03-26 protocol) are their own issuer.
        let issuer = resource_meta
            .as_ref()
            .and_then(|m| m.pointer("/authorization_servers/0"))
            .and_then(Value::as_str)
            .map_or_else(|| origin.clone(), |s| s.trim_end_matches('/').to_string());
        let scopes_from_resource: Vec<String> = resource_meta
            .as_ref()
            .and_then(|m| m.get("scopes_supported"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let (as_origin, as_meta) = self.server_metadata(&issuer).await.map_err(fail)?;
        let endpoint = |key: &str, default: &str| {
            as_meta
                .as_ref()
                .and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .map_or_else(|| format!("{as_origin}{default}"), str::to_string)
        };
        if let Some(methods) = as_meta
            .as_ref()
            .and_then(|m| m.get("code_challenge_methods_supported"))
            .and_then(Value::as_array)
            && !methods.iter().any(|m| m == "S256")
        {
            return Err(fail("the provider does not support PKCE with S256".into()));
        }
        let registration = as_meta
            .as_ref()
            .and_then(|m| m.get("registration_endpoint"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| as_meta.is_none().then(|| format!("{as_origin}/register")));
        let settings = server.oauth.as_ref();
        let scopes = match settings.map(|s| s.scopes.clone()) {
            Some(s) if !s.is_empty() => s,
            _ => scopes_from_resource,
        };
        let endpoints = Endpoints {
            issuer,
            authorization: endpoint("authorization_endpoint", "/authorize"),
            token: endpoint("token_endpoint", "/token"),
            registration,
            resource,
            scopes,
        };
        for url in [&endpoints.authorization, &endpoints.token] {
            let parsed = reqwest::Url::parse(url).map_err(|e| fail(format!("`{url}`: {e}")))?;
            let local = parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if parsed.scheme() != "https" && !local {
                return Err(fail(format!(
                    "endpoint `{url}` is not HTTPS (only loopback may be plain HTTP)"
                )));
            }
        }
        Ok(endpoints)
    }

    /// The protected resource metadata of the MCP server at `url`: the address its 401 challenge
    /// names, else the well-known addresses (with the path, then without).
    async fn resource_metadata(
        &self,
        url: &str,
        mcp: &reqwest::Url,
    ) -> Result<Option<Value>, OAuthError> {
        let mut candidates = Vec::new();
        if let Some(named) = self.challenge(url).await {
            candidates.push(named);
        }
        let origin = mcp.origin().ascii_serialization();
        let path = mcp.path().trim_end_matches('/');
        if !path.is_empty() {
            candidates.push(format!(
                "{origin}/.well-known/oauth-protected-resource{path}"
            ));
        }
        candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
        for candidate in candidates {
            if let Some(meta) = self.get_json(&candidate).await? {
                return Ok(Some(meta));
            }
        }
        Ok(None)
    }

    /// The authorization server's metadata (RFC 8414, then OpenID discovery).
    async fn server_metadata(&self, issuer: &str) -> Result<(String, Option<Value>), String> {
        let issuer_url = reqwest::Url::parse(issuer).map_err(|e| e.to_string())?;
        let origin = issuer_url.origin().ascii_serialization();
        let path = issuer_url.path().trim_end_matches('/');
        let urls = if path.is_empty() {
            vec![
                format!("{origin}/.well-known/oauth-authorization-server"),
                format!("{origin}/.well-known/openid-configuration"),
            ]
        } else {
            vec![
                format!("{origin}/.well-known/oauth-authorization-server{path}"),
                format!("{origin}/.well-known/openid-configuration{path}"),
                format!("{issuer}/.well-known/openid-configuration"),
            ]
        };
        for url in urls {
            if let Some(meta) = self.get_json(&url).await.map_err(|e| e.to_string())? {
                return Ok((origin, Some(meta)));
            }
        }
        Ok((origin, None))
    }

    /// The resource metadata address from the server's `WWW-Authenticate` challenge.
    async fn challenge(&self, url: &str) -> Option<String> {
        let reply = self
            .http
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "okbot", "version": env!("CARGO_PKG_VERSION")},
                },
            }))
            .send()
            .await
            .ok()?;
        let header = reply
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)?
            .to_str()
            .ok()?;
        resource_metadata(header)
    }

    async fn register(&self, mcp: &ExtraMcp, endpoints: Endpoints) -> Result<Client, OAuthError> {
        let server = &mcp.server.name;
        let Some(url) = endpoints.registration.clone() else {
            return Err(OAuthError::Discovery {
                server: server.clone(),
                reason: "the provider has no client registration; register a client and set \
                         [mcp.oauth] client_id"
                    .into(),
            });
        };
        if self.redirect_uris.is_empty() {
            return Err(OAuthError::NoRedirect);
        }
        let mut body = serde_json::json!({
            "client_name": self.client_name,
            "redirect_uris": self.redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        if !endpoints.scopes.is_empty() {
            body["scope"] = Value::String(endpoints.scopes.join(" "));
        }
        let reply = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(http_error)?;
        let status = reply.status();
        let answer: Value = reply.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(OAuthError::Refused(format!(
                "client registration: HTTP {} {}",
                status.as_u16(),
                describe_error(&answer)
            )));
        }
        let client_id = answer
            .get("client_id")
            .and_then(Value::as_str)
            .ok_or_else(|| OAuthError::Refused("client registration: no client_id".into()))?
            .to_string();
        let client_secret = answer
            .get("client_secret")
            .and_then(Value::as_str)
            .map(str::to_string);
        let auth_method = answer
            .get("token_endpoint_auth_method")
            .and_then(Value::as_str)
            .map_or_else(
                || {
                    if client_secret.is_some() {
                        "client_secret_basic".to_string()
                    } else {
                        "none".to_string()
                    }
                },
                str::to_string,
            );
        tracing::info!(mcp = %server, "registered okbot with the provider");
        Ok(Client {
            server_url: mcp.server.url.clone(),
            endpoints,
            id: client_id,
            secret: client_secret,
            auth_method,
            redirect_uris: self.redirect_uris.clone(),
            dynamic: true,
        })
    }

    async fn token_request(
        &self,
        client: &Client,
        mut form: Vec<(&'static str, String)>,
    ) -> Result<Token, OAuthError> {
        let mut request = self
            .http
            .post(&client.endpoints.token)
            .header("accept", "application/json");
        match (client.auth_method.as_str(), &client.secret) {
            ("client_secret_basic", Some(secret)) => {
                request = request.basic_auth(&client.id, Some(secret));
            }
            (_, Some(secret)) => {
                form.push(("client_id", client.id.clone()));
                form.push(("client_secret", secret.clone()));
            }
            (_, None) => form.push(("client_id", client.id.clone())),
        }
        let reply = request.form(&form).send().await.map_err(http_error)?;
        let status = reply.status();
        let answer: Value = reply.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            // `invalid_grant`: the code or refresh token is spent or revoked. Other 4xx: the
            // request or the client is not accepted. 5xx: try again later.
            let text = format!("HTTP {} {}", status.as_u16(), describe_error(&answer));
            let invalid_grant =
                answer.get("error").and_then(Value::as_str) == Some("invalid_grant");
            return Err(if invalid_grant {
                OAuthError::InvalidGrant(text)
            } else if status.is_client_error() {
                OAuthError::Refused(text)
            } else {
                OAuthError::Http(text)
            });
        }
        let access_token = answer
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| OAuthError::Refused("no access_token in the answer".into()))?
            .to_string();
        let expires_at = answer
            .get("expires_in")
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .map(|secs| self.clock.now().as_second() + secs);
        Ok(Token {
            access: access_token,
            refresh: answer
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at,
            client: client.clone(),
        })
    }
}

/// `error` and `error_description` of a provider's answer, short and without anything else.
fn describe_error(answer: &Value) -> String {
    let field = |k: &str| answer.get(k).and_then(Value::as_str).unwrap_or_default();
    let text = format!("{} {}", field("error"), field("error_description"));
    text.trim().chars().take(200).collect()
}

/// The canonical URI of an MCP server for the `resource` parameter: no fragment, no trailing
/// slash.
fn canonical(url: &str) -> String {
    let url = url.split('#').next().unwrap_or(url);
    url.trim_end_matches('/').to_string()
}

/// The `resource_metadata` parameter of a `WWW-Authenticate: Bearer …` header.
fn resource_metadata(header: &str) -> Option<String> {
    let at = header.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &header[at..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    (value.starts_with("https://") || value.starts_with("http://")).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_open_only_where_they_were_written() {
        let key = SecretKey::new(&SecretKey::generate());
        let sealed = key.seal("oauth-token\nnotion\nperson:lan", b"secret");
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(
            key.open("oauth-token\nnotion\nperson:lan", &sealed)
                .as_deref(),
            Some(&b"secret"[..])
        );
        assert_eq!(key.open("oauth-token\nnotion\nperson:bob", &sealed), None);
        let other = SecretKey::new(&SecretKey::generate());
        assert_eq!(other.open("oauth-token\nnotion\nperson:lan", &sealed), None);
        assert_eq!(key.open("x", b"short"), None);
    }

    #[test]
    fn challenge_header_names_the_metadata() {
        assert_eq!(
            resource_metadata(
                r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#
            )
            .as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            resource_metadata("Bearer resource_metadata=https://a.example/m, scope=x").as_deref(),
            Some("https://a.example/m")
        );
        assert_eq!(resource_metadata("Bearer realm=\"x\""), None);
        assert_eq!(
            resource_metadata("Bearer resource_metadata=\"file:///x\""),
            None
        );
    }

    #[test]
    fn resources_are_canonical() {
        assert_eq!(
            canonical("https://mcp.notion.com/mcp/"),
            "https://mcp.notion.com/mcp"
        );
        assert_eq!(
            canonical("http://127.0.0.1:8000/mcp#x"),
            "http://127.0.0.1:8000/mcp"
        );
    }
}
