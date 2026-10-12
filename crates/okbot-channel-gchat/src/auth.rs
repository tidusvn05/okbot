//! Google service-account credentials and access tokens (OAuth 2.0 JWT bearer grant).

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use okbot_core::Secret;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::jwt;

/// Scopes okbot needs: posting as the Chat app and pulling Pub/Sub events.
pub const SCOPES: &str =
    "https://www.googleapis.com/auth/chat.bot https://www.googleapis.com/auth/pubsub";

/// A Google auth failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The credentials file is not a service-account key.
    #[error("invalid service-account credentials: {0}")]
    Credentials(String),
    /// Google refused the assertion.
    #[error("Google refused the service account: {0}")]
    Refused(String),
    /// Network or protocol failure.
    #[error("token request failed: {0}")]
    Transport(String),
}

#[derive(Deserialize)]
struct KeyFile {
    #[serde(rename = "type")]
    kind: String,
    client_email: String,
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
}

/// A service account from its JSON key file.
#[derive(Clone)]
pub struct ServiceAccount {
    /// `…@….iam.gserviceaccount.com`.
    pub client_email: String,
    /// Google Cloud project id.
    pub project_id: Option<String>,
    key_der: Secret<Vec<u8>>,
    key_id: Option<String>,
    token_uri: String,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

impl ServiceAccount {
    /// Parses a key file's JSON.
    pub fn from_json(text: &str) -> Result<Self, AuthError> {
        let file: KeyFile =
            serde_json::from_str(text).map_err(|e| AuthError::Credentials(e.to_string()))?;
        if file.kind != "service_account" {
            return Err(AuthError::Credentials(format!(
                "type is `{}`, expected `service_account`",
                file.kind
            )));
        }
        let key_der = jwt::pem_to_der(&file.private_key)
            .map_err(|e| AuthError::Credentials(e.to_string()))?;
        Ok(Self {
            client_email: file.client_email,
            project_id: file.project_id,
            key_der: Secret::new(key_der),
            key_id: file.private_key_id,
            token_uri: file
                .token_uri
                .unwrap_or_else(|| "https://oauth2.googleapis.com/token".into()),
        })
    }

    /// Reads a key file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, AuthError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AuthError::Credentials(format!("{}: {e}", path.display())))?;
        Self::from_json(&text)
    }

    /// Overrides the token endpoint (tests).
    pub fn with_token_uri(mut self, uri: &str) -> Self {
        self.token_uri = uri.to_string();
        self
    }

    fn assertion(&self, now: Timestamp) -> Result<String, AuthError> {
        let iat = now.as_second();
        let claims = json!({
            "iss": self.client_email,
            "scope": SCOPES,
            "aud": self.token_uri,
            "iat": iat,
            "exp": iat + 3600,
        });
        jwt::sign(&claims, self.key_der.expose(), self.key_id.as_deref())
            .map_err(|e| AuthError::Credentials(e.to_string()))
    }
}

/// A cached access token and when to refresh it.
type Cached = Arc<Mutex<Option<(Secret<String>, Timestamp)>>>;

/// Access tokens for a service account, refreshed a minute before they expire.
#[derive(Clone)]
pub struct TokenSource {
    account: ServiceAccount,
    http: reqwest::Client,
    cached: Cached,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSource")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl TokenSource {
    /// Tokens for `account`.
    pub fn new(account: ServiceAccount, http: reqwest::Client) -> Self {
        Self {
            account,
            http,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    /// The service account.
    pub fn account(&self) -> &ServiceAccount {
        &self.account
    }

    /// A valid access token.
    pub async fn token(&self) -> Result<Secret<String>, AuthError> {
        let mut cached = self.cached.lock().await;
        let now = Timestamp::now();
        if let Some((token, expires)) = cached.as_ref()
            && *expires > now
        {
            return Ok(token.clone());
        }
        let assertion = self.account.assertion(now)?;
        let response = self
            .http
            .post(&self.account.token_uri)
            .timeout(Duration::from_secs(20))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.without_url().to_string()))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(AuthError::Refused(
                body["error_description"]
                    .as_str()
                    .or(body["error"].as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            ));
        }
        let token = body["access_token"]
            .as_str()
            .ok_or_else(|| AuthError::Transport("no access_token".into()))?;
        let lifetime = body["expires_in"].as_i64().unwrap_or(3600).max(120);
        let expires = now
            .checked_add(jiff::SignedDuration::from_secs(lifetime - 60))
            .unwrap_or(now);
        let token = Secret::new(token.to_string());
        *cached = Some((token.clone(), expires));
        Ok(token)
    }
}
