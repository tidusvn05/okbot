//! Signed request and response tokens (`okp1`, ADR 0014).
//!
//! `okp1.<base64url(claims JSON)>.<base64url(Ed25519 signature of "okp1." + claims part)>`. The
//! `body` claim binds the token to one request or response body.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::key::{PublicKey, SigningKey};

const VERSION: &str = "okp1";
/// The longest a token may live.
pub const MAX_LIFETIME_SECS: i64 = 300;
/// Clock difference tolerated between bots.
pub const SKEW_SECS: i64 = 60;

/// What a token says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The team.
    pub team: String,
    /// The signing member.
    pub iss: String,
    /// The member it is meant for.
    pub aud: String,
    /// Issued at (Unix seconds).
    pub iat: i64,
    /// Expires at (Unix seconds).
    pub exp: i64,
    /// A unique id (replays are refused).
    pub jti: String,
    /// Hex SHA-256 of the body.
    pub body: String,
    /// The signer's manifest version.
    pub ver: u64,
}

/// Hex SHA-256 of `body`.
pub fn body_hash(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

impl Claims {
    /// Claims for a body sent now from `iss` to `aud`, valid for `MAX_LIFETIME_SECS`.
    pub fn new(team: &str, iss: &str, aud: &str, ver: u64, body: &[u8], now: Timestamp) -> Self {
        let iat = now.as_second();
        Self {
            team: team.to_string(),
            iss: iss.to_string(),
            aud: aud.to_string(),
            iat,
            exp: iat + MAX_LIFETIME_SECS,
            jti: hex::encode(rand::random::<[u8; 16]>()),
            body: body_hash(body),
            ver,
        }
    }
}

/// Signs `claims`.
pub fn sign(key: &SigningKey, claims: &Claims) -> String {
    let payload = B64.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signed = format!("{VERSION}.{payload}");
    let signature = B64.encode(key.sign(signed.as_bytes()));
    format!("{signed}.{signature}")
}

/// The claims of a token, before its signature is checked: they name the signer to look up.
pub fn peek(token: &str) -> Result<Claims, Error> {
    let bad = || Error::Token("malformed token".into());
    let mut parts = token.trim().split('.');
    let (Some(VERSION), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    serde_json::from_slice(&B64.decode(payload).map_err(|_| bad())?).map_err(|_| bad())
}

/// Checks a token's signature by `signer`, its times and its body; returns its claims. Replays
/// are checked separately ([`Replays`]), after the signer is known to be a member.
pub fn verify(
    token: &str,
    signer: &PublicKey,
    body: &[u8],
    now: Timestamp,
) -> Result<Claims, Error> {
    let claims = peek(token)?;
    let token = token.trim();
    let (signed, signature) = token
        .rsplit_once('.')
        .ok_or_else(|| Error::Token("malformed token".into()))?;
    let signature = B64
        .decode(signature)
        .map_err(|_| Error::Token("malformed signature".into()))?;
    if !signer.verify(signed.as_bytes(), &signature) {
        return Err(Error::Token("bad signature".into()));
    }
    let now = now.as_second();
    if claims.exp <= claims.iat || claims.exp - claims.iat > MAX_LIFETIME_SECS {
        return Err(Error::Token("invalid lifetime".into()));
    }
    if claims.iat > now + SKEW_SECS || claims.exp + SKEW_SECS < now {
        return Err(Error::Token(
            "expired or not yet valid (check the clocks)".into(),
        ));
    }
    if claims.body != body_hash(body) {
        return Err(Error::Token("the body does not match the token".into()));
    }
    Ok(claims)
}

/// Token ids seen until they expire, to refuse replays.
#[derive(Debug, Default)]
pub struct Replays(Mutex<HashMap<String, i64>>);

impl Replays {
    /// Records the token's id; `false` when it was seen before.
    pub fn first_use(&self, claims: &Claims, now: Timestamp) -> bool {
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = now.as_second();
        seen.retain(|_, exp| *exp + SKEW_SECS >= now);
        let key = format!("{}/{}/{}", claims.team, claims.iss, claims.jti);
        seen.insert(key, claims.exp).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    #[test]
    fn signed_tokens_verify_and_bind_the_body() {
        let (key, _) = SigningKey::generate().unwrap();
        let claims = Claims::new("acme", "lead", "ops", 3, b"{}", now());
        let token = sign(&key, &claims);
        assert_eq!(peek(&token).unwrap(), claims);
        assert_eq!(verify(&token, &key.public(), b"{}", now()).unwrap(), claims);
        assert!(
            verify(&token, &key.public(), b"{ }", now()).is_err(),
            "body"
        );
        let (other, _) = SigningKey::generate().unwrap();
        assert!(
            verify(&token, &other.public(), b"{}", now()).is_err(),
            "signer"
        );
    }

    #[test]
    fn times_are_checked_with_skew() {
        let (key, _) = SigningKey::generate().unwrap();
        let token = sign(&key, &Claims::new("t", "a", "b", 1, b"", now()));
        let at = |secs: i64| {
            now()
                .checked_add(jiff::SignedDuration::from_secs(secs))
                .unwrap()
        };
        assert!(verify(&token, &key.public(), b"", at(-SKEW_SECS)).is_ok());
        assert!(verify(&token, &key.public(), b"", at(-SKEW_SECS - 1)).is_err());
        assert!(
            verify(
                &token,
                &key.public(),
                b"",
                at(MAX_LIFETIME_SECS + SKEW_SECS)
            )
            .is_ok()
        );
        assert!(
            verify(
                &token,
                &key.public(),
                b"",
                at(MAX_LIFETIME_SECS + SKEW_SECS + 1)
            )
            .is_err()
        );
        let mut long = Claims::new("t", "a", "b", 1, b"", now());
        long.exp = long.iat + MAX_LIFETIME_SECS + 1;
        assert!(verify(&sign(&key, &long), &key.public(), b"", now()).is_err());
    }

    #[test]
    fn altered_tokens_are_refused() {
        let (key, _) = SigningKey::generate().unwrap();
        let token = sign(&key, &Claims::new("t", "a", "b", 1, b"", now()));
        let mut forged = Claims::new("t", "a", "admin", 1, b"", now());
        forged.jti = peek(&token).unwrap().jti;
        let payload = B64.encode(serde_json::to_vec(&forged).unwrap());
        let signature = token.rsplit_once('.').unwrap().1;
        let altered = format!("okp1.{payload}.{signature}");
        assert!(verify(&altered, &key.public(), b"", now()).is_err());
        assert!(peek("okp2.a.b").is_err());
        assert!(peek("okp1.a").is_err());
    }

    #[test]
    fn replays_are_refused() {
        let claims = Claims::new("t", "a", "b", 1, b"", now());
        let replays = Replays::default();
        assert!(replays.first_use(&claims, now()));
        assert!(!replays.first_use(&claims, now()));
        let later = now()
            .checked_add(jiff::SignedDuration::from_secs(
                MAX_LIFETIME_SECS + SKEW_SECS + 1,
            ))
            .unwrap();
        assert!(replays.first_use(&Claims::new("t", "a", "b", 1, b"", later), later));
    }
}
