//! Minimal RS256 JSON Web Tokens on `ring`: signing service-account assertions and verifying
//! Google-signed tokens against a JWK set. Only what Google Chat needs.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, RsaKeyPair};
use serde_json::{Value, json};

/// A JWT problem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum JwtError {
    /// Not three base64url parts, or not JSON.
    #[error("malformed token")]
    Malformed,
    /// Not RS256.
    #[error("unsupported algorithm {0}")]
    Algorithm(String),
    /// No key with this `kid`.
    #[error("unknown signing key {0}")]
    UnknownKey(String),
    /// The signature does not match.
    #[error("bad signature")]
    Signature,
    /// The private key could not be read.
    #[error("invalid private key: {0}")]
    Key(String),
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(text: &str) -> Result<Vec<u8>, JwtError> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| JwtError::Malformed)
}

/// The DER body of a PEM block (`-----BEGIN PRIVATE KEY-----`).
pub(crate) fn pem_to_der(pem: &str) -> Result<Vec<u8>, JwtError> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .flat_map(|l| l.trim().chars())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| JwtError::Key(e.to_string()))
}

/// Signs `claims` with a PKCS#8 RSA key (RS256).
pub(crate) fn sign(
    claims: &Value,
    pkcs8_der: &[u8],
    kid: Option<&str>,
) -> Result<String, JwtError> {
    let key = RsaKeyPair::from_pkcs8(pkcs8_der).map_err(|e| JwtError::Key(e.to_string()))?;
    let mut header = json!({"alg": "RS256", "typ": "JWT"});
    if let Some(kid) = kid {
        header["kid"] = Value::String(kid.to_string());
    }
    let signing_input = format!(
        "{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes())
    );
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signing_input.as_bytes(),
        &mut sig,
    )
    .map_err(|_| JwtError::Signature)?;
    Ok(format!("{signing_input}.{}", b64(&sig)))
}

/// Verifies an RS256 token against a JWK set (`{"keys": [...]}`) and returns its claims.
/// Claims (audience, issuer, expiry) are checked by the caller.
pub(crate) fn verify(token: &str, jwks: &Value) -> Result<Value, JwtError> {
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Value = serde_json::from_slice(&unb64(h)?).map_err(|_| JwtError::Malformed)?;
    let alg = header["alg"].as_str().unwrap_or_default();
    if alg != "RS256" {
        return Err(JwtError::Algorithm(alg.to_string()));
    }
    let kid = header["kid"].as_str().unwrap_or_default();
    let keys = jwks["keys"]
        .as_array()
        .ok_or(JwtError::UnknownKey(kid.into()))?;
    let key = keys
        .iter()
        .find(|k| k["kid"].as_str() == Some(kid) || (kid.is_empty() && keys.len() == 1))
        .ok_or_else(|| JwtError::UnknownKey(kid.into()))?;
    let n = unb64(key["n"].as_str().ok_or(JwtError::Malformed)?)?;
    let e = unb64(key["e"].as_str().ok_or(JwtError::Malformed)?)?;
    let public = signature::RsaPublicKeyComponents { n: &n, e: &e };
    public
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{h}.{c}").as_bytes(),
            &unb64(s)?,
        )
        .map_err(|_| JwtError::Signature)?;
    serde_json::from_slice(&unb64(c)?).map_err(|_| JwtError::Malformed)
}

/// Test helpers: an RSA key made with `openssl` at test time (no key is committed).
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// `(pkcs8_der, jwks)` for a fresh 2048-bit key with `kid`.
    pub(crate) fn key(kid: &str) -> (Vec<u8>, Value) {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("k.pem");
        let ok = std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out",
            ])
            .arg(&pem)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "openssl is needed for these tests");
        let der = pem_to_der(&std::fs::read_to_string(&pem).unwrap()).unwrap();
        let pair = RsaKeyPair::from_pkcs8(&der).unwrap();
        let public = pair.public().modulus_len();
        assert_eq!(public, 256);
        // n and e from ring's public key (DER RSAPublicKey: SEQUENCE { n, e }).
        let (n, e) = rsa_public_components(pair.public().as_ref());
        let jwks = json!({"keys": [{"kty": "RSA", "alg": "RS256", "kid": kid, "n": b64(&n), "e": b64(&e)}]});
        (der, jwks)
    }

    fn rsa_public_components(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
        fn read_len(d: &[u8], i: &mut usize) -> usize {
            let first = d[*i];
            *i += 1;
            if first < 0x80 {
                return usize::from(first);
            }
            let count = usize::from(first & 0x7f);
            let mut len = 0;
            for _ in 0..count {
                len = (len << 8) | usize::from(d[*i]);
                *i += 1;
            }
            len
        }
        fn read_int(d: &[u8], i: &mut usize) -> Vec<u8> {
            assert_eq!(d[*i], 0x02);
            *i += 1;
            let len = read_len(d, i);
            let mut v = d[*i..*i + len].to_vec();
            *i += len;
            while v.first() == Some(&0) && v.len() > 1 {
                v.remove(0);
            }
            v
        }
        let mut i = 0;
        assert_eq!(der[i], 0x30);
        i += 1;
        read_len(der, &mut i);
        let n = read_int(der, &mut i);
        let e = read_int(der, &mut i);
        (n, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trip() {
        let (der, jwks) = testing::key("k1");
        let token = sign(&json!({"aud": "x", "n": 1}), &der, Some("k1")).unwrap();
        assert_eq!(verify(&token, &jwks).unwrap()["aud"], "x");

        let (_, other) = testing::key("k1");
        assert_eq!(verify(&token, &other), Err(JwtError::Signature));
        let mut tampered = token.clone();
        tampered.insert(tampered.find('.').unwrap() + 2, 'A');
        assert!(verify(&tampered, &jwks).is_err());
        let unknown = sign(&json!({}), &der, Some("k2")).unwrap();
        assert!(matches!(
            verify(&unknown, &jwks),
            Err(JwtError::UnknownKey(_))
        ));
        let none_alg = format!("{}.{}.", b64(br#"{"alg":"none"}"#), b64(b"{}"));
        assert!(matches!(
            verify(&none_alg, &jwks),
            Err(JwtError::Algorithm(_))
        ));
    }
}
