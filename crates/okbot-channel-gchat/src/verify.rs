//! Verifying that a webhook request comes from Google Chat
//! (<https://developers.google.com/workspace/chat/verify-requests-from-chat>).

use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::jwt;

/// The service account that signs Chat's requests.
pub const CHAT_ISSUER: &str = "chat@system.gserviceaccount.com";
/// Google's OpenID keys (tokens whose audience is the endpoint URL).
pub(crate) const GOOGLE_JWKS: &str = "https://www.googleapis.com/oauth2/v3/certs";
/// Chat's own keys (tokens whose audience is the project number).
pub(crate) const CHAT_JWKS: &str =
    "https://www.googleapis.com/service_accounts/v1/jwk/chat@system.gserviceaccount.com";

/// What the Chat app's "Authentication audience" setting is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    /// "HTTP endpoint URL": an ID token issued by Google for `chat@system.gserviceaccount.com`.
    EndpointUrl(String),
    /// "Project number": a JWT signed by `chat@system.gserviceaccount.com`.
    ProjectNumber(String),
}

/// Checks bearer tokens; keys are cached for an hour.
#[derive(Clone)]
pub struct Verifier {
    audience: Audience,
    jwks_url: String,
    http: reqwest::Client,
    keys: Arc<Mutex<Option<(Value, Instant)>>>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier")
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// A verifier for `audience`, reading keys from Google (or `jwks_url` in tests).
    pub fn new(audience: Audience, jwks_url: Option<String>, http: reqwest::Client) -> Self {
        let default = match audience {
            Audience::EndpointUrl(_) => GOOGLE_JWKS,
            Audience::ProjectNumber(_) => CHAT_JWKS,
        };
        Self {
            audience,
            jwks_url: jwks_url.unwrap_or_else(|| default.to_string()),
            http,
            keys: Arc::new(Mutex::new(None)),
        }
    }

    async fn keys(&self, refresh: bool) -> Result<Value, String> {
        let mut cached = self.keys.lock().await;
        if !refresh
            && let Some((keys, at)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(3600)
        {
            return Ok(keys.clone());
        }
        let keys: Value = self
            .http
            .get(&self.jwks_url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("cannot fetch Google keys: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bad Google keys: {e}"))?;
        *cached = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    /// Accepts the request when `authorization` carries a valid token for this app.
    pub async fn check(&self, authorization: Option<&str>) -> Result<(), String> {
        let token = authorization
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or("no bearer token")?;
        let claims = match jwt::verify(token, &self.keys(false).await?) {
            // Google rotates keys: refetch once when the key is unknown.
            Err(jwt::JwtError::UnknownKey(_)) => jwt::verify(token, &self.keys(true).await?),
            other => other,
        }
        .map_err(|e| e.to_string())?;
        check_claims(&claims, &self.audience, Timestamp::now())
    }
}

fn check_claims(claims: &Value, audience: &Audience, now: Timestamp) -> Result<(), String> {
    let leeway = 60;
    let now = now.as_second();
    let exp = claims["exp"].as_i64().ok_or("no exp")?;
    if exp + leeway < now {
        return Err("token expired".into());
    }
    if claims["iat"].as_i64().is_some_and(|iat| iat - leeway > now) {
        return Err("token issued in the future".into());
    }
    let aud_ok = |want: &str| match &claims["aud"] {
        Value::String(a) => a == want,
        Value::Array(list) => list.iter().any(|a| a == want),
        _ => false,
    };
    match audience {
        Audience::EndpointUrl(url) => {
            let iss = claims["iss"].as_str().unwrap_or_default();
            if iss != "https://accounts.google.com" && iss != "accounts.google.com" {
                return Err(format!("unexpected issuer {iss}"));
            }
            if claims["email"].as_str() != Some(CHAT_ISSUER)
                || claims["email_verified"] == Value::Bool(false)
            {
                return Err("token is not from Google Chat".into());
            }
            if !aud_ok(url) {
                return Err("token audience is not this endpoint".into());
            }
        }
        Audience::ProjectNumber(number) => {
            if claims["iss"].as_str() != Some(CHAT_ISSUER) {
                return Err("token is not from Google Chat".into());
            }
            if !aud_ok(number) {
                return Err("token audience is not this project".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    #[test]
    fn claims_for_endpoint_audience() {
        let aud = Audience::EndpointUrl("https://bot.example.com/gchat/corp".into());
        let good = json!({"iss": "https://accounts.google.com", "email": CHAT_ISSUER, "email_verified": true,
                          "aud": "https://bot.example.com/gchat/corp", "exp": 1_800_000_100_i64});
        assert_eq!(check_claims(&good, &aud, now()), Ok(()));
        let mut wrong_aud = good.clone();
        wrong_aud["aud"] = json!("https://evil.example.com");
        assert!(check_claims(&wrong_aud, &aud, now()).is_err());
        let mut other_sender = good.clone();
        other_sender["email"] = json!("someone@gmail.com");
        assert!(check_claims(&other_sender, &aud, now()).is_err());
        let mut expired = good;
        expired["exp"] = json!(1_799_999_000_i64);
        assert!(check_claims(&expired, &aud, now()).is_err());
    }

    #[test]
    fn claims_for_project_audience() {
        let aud = Audience::ProjectNumber("123456".into());
        let good = json!({"iss": CHAT_ISSUER, "aud": "123456", "exp": 1_800_000_100_i64});
        assert_eq!(check_claims(&good, &aud, now()), Ok(()));
        let mut forged = good;
        forged["iss"] = json!("attacker@example.iam.gserviceaccount.com");
        assert!(check_claims(&forged, &aud, now()).is_err());
    }
}
