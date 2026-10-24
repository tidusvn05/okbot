//! Sealing what passes through a relay (ADR 0014): the relay forwards bytes it cannot read.
//!
//! Each bot has an X25519 key (`state/peer.box`, its public half `x25519:<base64url>` in
//! `state/peer.box.pub` and in the team manifest as `box_key`). A request through a relay is sealed
//! for the target and the answer for the caller: a fresh X25519 key per message, HKDF-SHA256 over
//! the shared secret (salt: both public keys, info: `okbot-relay-v1`), ChaCha20-Poly1305. The
//! okp1 tokens still sign the plain bodies, so a relay can neither read nor alter them.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::Error;

/// The private key file in a bot's `state/`.
pub const BOX_FILE: &str = "peer.box";
/// Its public half, read by other bots of the machine.
pub const BOX_PUBLIC_FILE: &str = "peer.box.pub";

const PREFIX: &str = "x25519:";
const INFO: &[u8] = b"okbot-relay-v1";

/// A public X25519 key, written `x25519:<base64url>`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoxPublic([u8; 32]);

impl fmt::Display for BoxPublic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", B64.encode(self.0))
    }
}

impl fmt::Debug for BoxPublic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for BoxPublic {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || Error::Key(format!("`{s}` is not an `x25519:<base64url>` key"));
        let bytes = B64
            .decode(s.trim().strip_prefix(PREFIX).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        Ok(Self(bytes.try_into().map_err(|_| bad())?))
    }
}

impl Serialize for BoxPublic {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BoxPublic {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A bot's private X25519 key.
#[derive(Clone)]
pub struct BoxKey(StaticSecret);

impl fmt::Debug for BoxKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BoxKey({})", self.public())
    }
}

impl BoxKey {
    /// A new random key.
    pub fn generate() -> Self {
        Self(StaticSecret::from(rand::random::<[u8; 32]>()))
    }

    /// Reads a bot's key from its `state/` directory (creating it on first use) and writes its
    /// public half next to it.
    pub fn for_bot(state_dir: &Path) -> Result<Self, Error> {
        let path = state_dir.join(BOX_FILE);
        let key = match std::fs::read(&path) {
            Ok(bytes) => {
                let bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| Error::Key(format!("{} is not an X25519 key", path.display())))?;
                Self(StaticSecret::from(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate();
                crate::key::write_new_private(&path, &key.0.to_bytes())?;
                key
            }
            Err(e) => return Err(Error::Io(path.display().to_string(), e)),
        };
        let public = state_dir.join(BOX_PUBLIC_FILE);
        let text = format!("{}\n", key.public());
        if std::fs::read_to_string(&public).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&public, text)
                .map_err(|e| Error::Io(public.display().to_string(), e))?;
        }
        Ok(key)
    }

    /// The public half.
    pub fn public(&self) -> BoxPublic {
        BoxPublic(PublicKey::from(&self.0).to_bytes())
    }
}

/// Reads the box key a bot directory publishes (`<dir>/state/peer.box.pub`).
pub fn bot_box_key(bot_dir: &Path) -> Result<BoxPublic, Error> {
    let path = bot_dir.join("state").join(BOX_PUBLIC_FILE);
    std::fs::read_to_string(&path)
        .map_err(|e| Error::Io(path.display().to_string(), e))?
        .parse()
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u8,
    epk: String,
    nonce: String,
    ct: String,
}

fn cipher(shared: &[u8], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> LessSafeKey {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral);
    salt.extend_from_slice(recipient);
    let prk = Salt::new(HKDF_SHA256, &salt).extract(shared);
    let okm = prk
        .expand(&[INFO], &CHACHA20_POLY1305)
        .expect("32 bytes is a valid HKDF-SHA256 output");
    LessSafeKey::new(UnboundKey::from(okm))
}

/// Seals `plain` for the holder of `to`.
pub fn seal(to: &BoxPublic, plain: &[u8]) -> Vec<u8> {
    let ephemeral = StaticSecret::from(rand::random::<[u8; 32]>());
    let epk = PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&PublicKey::from(to.0));
    let key = cipher(shared.as_bytes(), &epk, &to.0);
    let nonce: [u8; 12] = rand::random();
    let mut data = plain.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .expect("ChaCha20-Poly1305 seals any length used here");
    serde_json::to_vec(&Envelope {
        v: 1,
        epk: B64.encode(epk),
        nonce: B64.encode(nonce),
        ct: B64.encode(data),
    })
    .expect("envelopes serialize")
}

/// Opens an envelope sealed for `key`.
pub fn open(key: &BoxKey, sealed: &[u8]) -> Result<Vec<u8>, Error> {
    let bad = || Error::Token("a sealed message that cannot be opened".into());
    let envelope: Envelope = serde_json::from_slice(sealed).map_err(|_| bad())?;
    if envelope.v != 1 {
        return Err(bad());
    }
    let epk: [u8; 32] = B64
        .decode(&envelope.epk)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(bad)?;
    let nonce: [u8; 12] = B64
        .decode(&envelope.nonce)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(bad)?;
    let mut data = B64.decode(&envelope.ct).map_err(|_| bad())?;
    let shared = key.0.diffie_hellman(&PublicKey::from(epk));
    let cipher = cipher(shared.as_bytes(), &epk, &key.public().0);
    let plain = cipher
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .map_err(|_| bad())?;
    Ok(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_for_one_key_only() {
        let alice = BoxKey::generate();
        let mallory = BoxKey::generate();
        let sealed = seal(&alice.public(), b"restart nginx");
        assert!(!String::from_utf8_lossy(&sealed).contains("nginx"));
        assert_eq!(open(&alice, &sealed).unwrap(), b"restart nginx");
        assert!(open(&mallory, &sealed).is_err());
        let mut tampered: serde_json::Value = serde_json::from_slice(&sealed).unwrap();
        let ct = tampered["ct"].as_str().unwrap().to_string();
        let flipped = if ct.starts_with('A') { "B" } else { "A" };
        tampered["ct"] = format!("{flipped}{}", &ct[1..]).into();
        assert!(open(&alice, &serde_json::to_vec(&tampered).unwrap()).is_err());
        assert_ne!(
            seal(&alice.public(), b"x"),
            seal(&alice.public(), b"x"),
            "fresh keys"
        );
    }

    #[test]
    fn keys_are_kept_and_published() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let first = BoxKey::for_bot(&state).unwrap();
        assert_eq!(BoxKey::for_bot(&state).unwrap().public(), first.public());
        assert_eq!(bot_box_key(dir.path()).unwrap(), first.public());
        let text = first.public().to_string();
        assert_eq!(text.parse::<BoxPublic>().unwrap(), first.public());
        assert!("ed25519:abc".parse::<BoxPublic>().is_err());
    }
}
