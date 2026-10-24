//! Team manifests (ADR 0014): who the members are, how to reach them, and which may call which.
//!
//! A manifest fetched over the network is signed by the team key: the `.sig` file next to it holds
//! one line per signature, `ed25519:<public key> <base64url signature>`, over the exact bytes of
//! the manifest. Two lines (old and new key) let the owner rotate the team key.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use jiff::Timestamp;
use okbot_core::PeerTarget;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::key::{PublicKey, SigningKey};

/// The longest a signed manifest may stay valid.
pub const MAX_VALIDITY_DAYS: i64 = 90;

/// A team manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The team itself.
    pub team: TeamInfo,
    /// Members by name.
    #[serde(default)]
    pub members: BTreeMap<String, Member>,
    /// Which members may call which.
    #[serde(default)]
    pub links: Vec<Link>,
    /// People the team knows, by person id.
    #[serde(default)]
    pub people: BTreeMap<String, TeamPerson>,
}

/// `[team]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamInfo {
    /// The team id (a lowercase slug), the `<team>` of `peer:<team>`.
    pub id: String,
    /// Grows with every change; a lower version is refused.
    pub version: u64,
    /// After this the manifest is not used (required when signed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    /// What the team is for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// `[members.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// Its public key (read from `dir` when that is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<PublicKey>,
    /// Its X25519 key, for what passes through a relay (read from `dir` when that is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_key: Option<crate::seal::BoxPublic>,
    /// Its A2A endpoint (`https://…/a2a/<team>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The member that relays requests to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// Its bot directory on this machine (local teams).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Team roles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Skills, for `skill:<name>` targets and the directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// What it does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// `[[links]]`: members matching `from` may call members matching `to`. Selectors are a member
/// name, `role:<name>` or `*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// Callers.
    pub from: Vec<String>,
    /// Callees.
    pub to: Vec<String>,
}

/// `[people.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamPerson {
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Chat accounts, `platform:instance/user-id`.
    #[serde(default)]
    pub accounts: Vec<String>,
}

fn slug(kind: &str, value: &str) -> Result<(), Error> {
    let ok = !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::Manifest(format!(
            "{kind} `{value}` may only contain a-z, 0-9, `-` and `_`"
        )))
    }
}

impl Manifest {
    /// An empty manifest for a new team.
    pub fn new(id: &str) -> Self {
        Self {
            team: TeamInfo {
                id: id.to_string(),
                version: 0,
                expires: None,
                description: String::new(),
            },
            members: BTreeMap::new(),
            links: Vec::new(),
            people: BTreeMap::new(),
        }
    }

    /// Parses and checks a manifest.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let manifest: Self = toml::from_str(text).map_err(|e| Error::Manifest(e.to_string()))?;
        manifest.check()?;
        Ok(manifest)
    }

    /// The manifest as TOML.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("a manifest serializes")
    }

    /// Checks names, reachability and references.
    pub fn check(&self) -> Result<(), Error> {
        slug("team id", &self.team.id)?;
        for (name, member) in &self.members {
            slug("member name", name)?;
            for role in &member.roles {
                slug("role", role)?;
            }
            let ways = usize::from(member.url.is_some())
                + usize::from(member.relay.is_some())
                + usize::from(member.dir.is_some());
            if ways != 1 {
                return Err(Error::Manifest(format!(
                    "member `{name}` needs exactly one of `url`, `relay` and `dir`"
                )));
            }
            if member.dir.is_none() && member.key.is_none() {
                return Err(Error::Manifest(format!("member `{name}` needs a `key`")));
            }
            if let Some(url) = &member.url
                && !(url.starts_with("https://") || url.starts_with("http://127.0.0.1"))
            {
                return Err(Error::Manifest(format!(
                    "member `{name}`: `url` must be https:// (or http://127.0.0.1 for tests)"
                )));
            }
            if let Some(relay) = &member.relay {
                if member.box_key.is_none() {
                    return Err(Error::Manifest(format!(
                        "member `{name}` is reached through a relay and needs a `box_key` \
                         (`okbot peer id` prints it), so the relay cannot read its messages"
                    )));
                }
                let via = self.members.get(relay);
                if relay == name || via.is_none_or(|m| m.url.is_none()) {
                    return Err(Error::Manifest(format!(
                        "member `{name}`: relay `{relay}` must be another member with a `url`"
                    )));
                }
            }
        }
        let mut keys = std::collections::HashSet::new();
        for (name, member) in &self.members {
            if let Some(key) = &member.key
                && !keys.insert(key)
            {
                return Err(Error::Manifest(format!(
                    "member `{name}` has the key of another member"
                )));
            }
        }
        for link in &self.links {
            for selector in link.from.iter().chain(&link.to) {
                if selector != "*"
                    && !selector.starts_with("role:")
                    && !self.members.contains_key(selector)
                {
                    return Err(Error::Manifest(format!(
                        "link selector `{selector}` is not a member, `role:<name>` or `*`"
                    )));
                }
            }
        }
        for (id, person) in &self.people {
            slug("person id", id)?;
            for account in &person.accounts {
                let valid = account.split_once('/').is_some_and(|(channel, user)| {
                    !user.is_empty() && channel.parse::<okbot_core::ChannelId>().is_ok()
                });
                if !valid {
                    return Err(Error::Manifest(format!(
                        "person `{id}`: `{account}` is not `platform:instance/user-id`"
                    )));
                }
            }
        }
        Ok(())
    }

    fn selects(&self, selector: &str, member: &str) -> bool {
        match selector.strip_prefix("role:") {
            _ if selector == "*" => true,
            Some(role) => self
                .members
                .get(member)
                .is_some_and(|m| m.roles.iter().any(|r| r == role)),
            None => selector == member,
        }
    }

    /// `from` may call `to` (both members, different, and a link allows it).
    pub fn may_call(&self, from: &str, to: &str) -> bool {
        from != to
            && self.members.contains_key(from)
            && self.members.contains_key(to)
            && self.links.iter().any(|l| {
                l.from.iter().any(|s| self.selects(s, from))
                    && l.to.iter().any(|s| self.selects(s, to))
            })
    }

    /// Members `from` may call, in name order.
    pub fn callees(&self, from: &str) -> Vec<&str> {
        self.members
            .keys()
            .map(String::as_str)
            .filter(|to| self.may_call(from, to))
            .collect()
    }

    /// Members matching `target` that `from` may call, in name order.
    pub fn route(&self, from: &str, target: &PeerTarget) -> Vec<&str> {
        self.callees(from)
            .into_iter()
            .filter(|name| {
                let member = &self.members[*name];
                match target {
                    PeerTarget::Member(n) => n == name,
                    PeerTarget::Role(r) => member.roles.contains(r),
                    PeerTarget::Skill(s) => member.skills.contains(s),
                }
            })
            .collect()
    }

    /// The person whose accounts include `account`.
    pub fn person_of(&self, account: &str) -> Option<&str> {
        self.people
            .iter()
            .find(|(_, p)| p.accounts.iter().any(|a| a == account))
            .map(|(id, _)| id.as_str())
    }

    /// The member whose key is `key`.
    pub fn member_by_key(&self, key: &PublicKey) -> Option<&str> {
        self.members
            .iter()
            .find(|(_, m)| m.key.as_ref() == Some(key))
            .map(|(name, _)| name.as_str())
    }

    /// Whether this manifest may replace `current`: a higher version, or the same one unchanged.
    pub fn may_replace(&self, current: &Self) -> Result<(), Error> {
        if self.team.id != current.team.id {
            return Err(Error::Manifest("the manifest is for another team".into()));
        }
        if self.team.version < current.team.version
            || (self.team.version == current.team.version && self != current)
        {
            return Err(Error::Manifest(format!(
                "version {} does not follow version {}",
                self.team.version, current.team.version
            )));
        }
        Ok(())
    }

    /// The manifest is not expired at `now`. Signed manifests must say when they expire.
    pub fn check_fresh(&self, now: Timestamp, signed: bool) -> Result<(), Error> {
        match self.team.expires {
            Some(expires) if expires <= now => Err(Error::Manifest(format!(
                "the manifest of team `{}` expired at {expires}; the owner must sign it again",
                self.team.id
            ))),
            None if signed => Err(Error::Manifest("a signed manifest needs `expires`".into())),
            _ => Ok(()),
        }
    }
}

/// Signs manifest text: one `.sig` line.
pub fn sign(text: &str, key: &SigningKey) -> String {
    format!(
        "{} {}\n",
        key.public(),
        B64.encode(key.sign(text.as_bytes()))
    )
}

/// Checks that one line of `signatures` is `team_key`'s signature of `text`, then parses it.
pub fn verify(text: &str, signatures: &str, team_key: &PublicKey) -> Result<Manifest, Error> {
    let signed = signatures.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let (Some(key), Some(signature)) = (parts.next(), parts.next()) else {
            return false;
        };
        key.parse::<PublicKey>().ok().as_ref() == Some(team_key)
            && B64
                .decode(signature)
                .is_ok_and(|s| team_key.verify(text.as_bytes(), &s))
    });
    if !signed {
        return Err(Error::Manifest(
            "the manifest is not signed by the team key".into(),
        ));
    }
    Manifest::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
[team]
id = "acme"
version = 3
expires = "2030-01-01T00:00:00Z"

[members.lead]
key = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
url = "https://lead.example.com/a2a/acme"
roles = ["leader"]

[members.research]
key = "ed25519:AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
box_key = "x25519:AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
relay = "lead"
roles = ["member"]
skills = ["web-research"]

[members.ops]
key = "ed25519:AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
url = "https://ops.example.com/a2a/acme"
roles = ["member"]
skills = ["infrastructure"]

[[links]]
from = ["role:leader"]
to = ["*"]

[[links]]
from = ["*"]
to = ["role:leader"]

[people.lan]
name = "Lan"
accounts = ["zalo:shop/123"]
"#;

    #[test]
    fn links_decide_who_may_call_whom() {
        let m = Manifest::parse(TEXT).unwrap();
        assert!(m.may_call("lead", "ops"));
        assert!(m.may_call("research", "lead"));
        assert!(
            !m.may_call("research", "ops"),
            "members do not call each other"
        );
        assert!(!m.may_call("lead", "lead"), "never itself");
        assert!(!m.may_call("lead", "stranger"));
        assert_eq!(m.callees("lead"), ["ops", "research"]);
        assert_eq!(
            m.route("lead", &PeerTarget::Skill("web-research".into())),
            ["research"]
        );
        assert_eq!(
            m.route("lead", &PeerTarget::Role("member".into())),
            ["ops", "research"]
        );
        assert_eq!(
            m.route("ops", &PeerTarget::Member("research".into())),
            [] as [&str; 0]
        );
        assert_eq!(m.person_of("zalo:shop/123"), Some("lan"));
        assert_eq!(m.person_of("zalo:shop/9"), None);
    }

    #[test]
    fn invalid_manifests_are_refused() {
        let broken = |from: &str, to: &str| TEXT.replacen(from, to, 1);
        for text in [
            broken("relay = \"lead\"", "relay = \"research\""),
            broken("relay = \"lead\"", "url = \"https://x\"\nrelay = \"lead\""),
            broken(
                "url = \"https://lead.example.com/a2a/acme\"",
                "url = \"http://lead\"",
            ),
            broken("to = [\"*\"]", "to = [\"nobody\"]"),
            broken("id = \"acme\"", "id = \"Acme\""),
            broken("zalo:shop/123", "zalo"),
            broken("version = 3", "version = 3\nowner = \"x\""),
            broken(
                "box_key = \"x25519:AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"\n",
                "",
            ),
            broken(
                "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
        ] {
            assert!(Manifest::parse(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn signatures_and_versions() {
        let (team, _) = SigningKey::generate().unwrap();
        let sig = sign(TEXT, &team);
        let m = verify(TEXT, &sig, &team.public()).unwrap();
        assert!(
            verify(
                &TEXT.replace("version = 3", "version = 4"),
                &sig,
                &team.public()
            )
            .is_err()
        );
        let (other, _) = SigningKey::generate().unwrap();
        assert!(verify(TEXT, &sig, &other.public()).is_err());
        // Rotation: signed by the old and the new key, both pins accept it.
        let both = format!("{}{}", sign(TEXT, &other), sign(TEXT, &team));
        assert!(verify(TEXT, &both, &team.public()).is_ok());
        assert!(verify(TEXT, &both, &other.public()).is_ok());

        let mut newer = m.clone();
        newer.team.version = 4;
        assert!(newer.may_replace(&m).is_ok());
        assert!(m.may_replace(&newer).is_err(), "no rollback");
        assert!(m.may_replace(&m).is_ok());
        let mut changed = m.clone();
        changed.links.clear();
        assert!(
            changed.may_replace(&m).is_err(),
            "same version, other content"
        );

        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert!(m.check_fresh(at("2029-12-31T00:00:00Z"), true).is_ok());
        assert!(m.check_fresh(at("2030-01-01T00:00:00Z"), true).is_err());
        let mut forever = m;
        forever.team.expires = None;
        assert!(
            forever
                .check_fresh(at("2029-01-01T00:00:00Z"), true)
                .is_err()
        );
        assert!(
            forever
                .check_fresh(at("2029-01-01T00:00:00Z"), false)
                .is_ok()
        );
    }

    #[test]
    fn round_trips_through_toml() {
        let m = Manifest::parse(TEXT).unwrap();
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
    }
}
