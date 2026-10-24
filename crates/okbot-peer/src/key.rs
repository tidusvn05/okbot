//! Ed25519 keys: a bot's own key (`state/peer.key`) and the public keys of members and teams.

use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};

use crate::Error;

/// The private key file in a bot's `state/`.
pub const KEY_FILE: &str = "peer.key";
/// The public key file next to it, read by other bots of the machine.
pub const PUBLIC_FILE: &str = "peer.pub";

const PREFIX: &str = "ed25519:";

/// A public key, written `ed25519:<base64url>`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// Whether `signature` is this key's signature of `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, &self.0)
            .verify(message, signature)
            .is_ok()
    }

    /// A short form for people to compare (the first 8 bytes, hex).
    pub fn fingerprint(&self) -> String {
        hex::encode(&self.0[..8])
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", B64.encode(self.0))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for PublicKey {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || Error::Key(format!("`{s}` is not an `ed25519:<base64url>` public key"));
        let bytes = B64
            .decode(s.trim().strip_prefix(PREFIX).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        Ok(Self(bytes.try_into().map_err(|_| bad())?))
    }
}

impl serde::Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A private signing key (a bot's, or a team owner's).
#[derive(Clone)]
pub struct SigningKey(Arc<Ed25519KeyPair>);

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey({})", self.public())
    }
}

impl SigningKey {
    /// A new random key and its PKCS#8 bytes (what is saved).
    pub fn generate() -> Result<(Self, Vec<u8>), Error> {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| Error::Key("cannot generate a key".into()))?;
        let bytes = pkcs8.as_ref().to_vec();
        Ok((Self::from_pkcs8(&bytes)?, bytes))
    }

    /// A key from PKCS#8 bytes.
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, Error> {
        Ed25519KeyPair::from_pkcs8_maybe_unchecked(bytes)
            .map(|k| Self(Arc::new(k)))
            .map_err(|_| Error::Key("the key file is not an Ed25519 PKCS#8 key".into()))
    }

    /// Reads the key at `path`, or creates it (mode 0600) when there is none.
    pub fn load_or_create(path: &Path) -> Result<Self, Error> {
        match std::fs::read(path) {
            Ok(bytes) => Self::from_pkcs8(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (key, bytes) = Self::generate()?;
                write_new_private(path, &bytes)?;
                tracing::info!(path = %path.display(), "created a peer key");
                Ok(key)
            }
            Err(e) => Err(Error::Io(path.display().to_string(), e)),
        }
    }

    /// Reads a bot's key from its `state/` directory (creating it on first use) and writes its
    /// public key next to it, for the other bots of the machine.
    pub fn for_bot(state_dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| Error::Io(state_dir.display().to_string(), e))?;
        let key = Self::load_or_create(&state_dir.join(KEY_FILE))?;
        let public = state_dir.join(PUBLIC_FILE);
        let text = format!("{}\n", key.public());
        if std::fs::read_to_string(&public).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&public, text)
                .map_err(|e| Error::Io(public.display().to_string(), e))?;
        }
        Ok(key)
    }

    /// The public key.
    pub fn public(&self) -> PublicKey {
        let bytes: [u8; 32] = self
            .0
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public keys are 32 bytes");
        PublicKey(bytes)
    }

    /// Signs `message`.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.0.sign(message).as_ref().to_vec()
    }
}

/// Reads the public key a bot directory publishes (`<dir>/state/peer.pub`).
pub fn bot_public_key(bot_dir: &Path) -> Result<PublicKey, Error> {
    let path = bot_dir.join("state").join(PUBLIC_FILE);
    std::fs::read_to_string(&path)
        .map_err(|e| Error::Io(path.display().to_string(), e))?
        .parse()
}

/// Writes `bytes` to a new file only its owner can read.
pub fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let io = |e| Error::Io(path.display().to_string(), e);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    file.write_all(bytes).map_err(io)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let (key, _) = SigningKey::generate().unwrap();
        let signature = key.sign(b"hello");
        assert!(key.public().verify(b"hello", &signature));
        assert!(!key.public().verify(b"hullo", &signature));
        let (other, _) = SigningKey::generate().unwrap();
        assert!(!other.public().verify(b"hello", &signature));
    }

    #[test]
    fn public_key_text_round_trips() {
        let (key, _) = SigningKey::generate().unwrap();
        let text = key.public().to_string();
        assert!(text.starts_with("ed25519:"));
        assert_eq!(text.parse::<PublicKey>().unwrap(), key.public());
        assert!("ed25519:abc".parse::<PublicKey>().is_err());
        assert!("rsa:abc".parse::<PublicKey>().is_err());
    }

    #[test]
    fn bot_key_is_created_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let first = SigningKey::for_bot(&state).unwrap();
        let again = SigningKey::for_bot(&state).unwrap();
        assert_eq!(first.public(), again.public());
        let mode = std::fs::metadata(state.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(bot_public_key(dir.path()).unwrap(), first.public());
    }
}
