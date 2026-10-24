//! A bot's place in its teams: manifests, keys, and the transport to other members.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use okbot_core::{
    Clock, ConversationKey, ConversationStore, InboundSink, PeerCaller, PeerError, Prompt, SentRef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::key::{PublicKey, SigningKey, bot_public_key};
use crate::manifest::{self, Manifest, Member};
use crate::token::{self, Claims, Replays};
use crate::{Error, a2a};

/// The bot's socket for members on the same machine, in its `state/`.
pub const SOCKET_FILE: &str = "peer.sock";
/// Header carrying the response's signature.
pub const SIGNATURE_HEADER: &str = "okbot-peer-signature";
/// The authorization scheme.
pub(crate) const SCHEME: &str = "OkbotPeer ";
/// How often fetched manifests are refreshed.
pub(crate) const REFRESH: Duration = Duration::from_secs(600);
/// How long a member that did not answer is skipped.
const DOWN_FOR: Duration = Duration::from_secs(60);
/// Largest request or response body.
pub(crate) const MAX_BODY: usize = 4 << 20;
/// Time for one request to another member.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a team's manifest comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// A file on this machine (trusted as is).
    File(PathBuf),
    /// Built from `[a2a]` (the team `external`).
    Builtin,
    /// An `https://` URL, with `<url>.sig` signed by the team key.
    Url {
        /// The manifest URL.
        url: String,
        /// The pinned team key.
        team_key: PublicKey,
    },
}

/// This bot's settings for one team (`[teams.<id>]`).
#[derive(Debug, Clone)]
pub struct TeamConfig {
    /// Team id.
    pub id: String,
    /// This bot's member name.
    pub member: String,
    /// Where the manifest is.
    pub source: ManifestSource,
    /// Other members may delegate to this bot.
    pub accept: bool,
    /// Longest chain accepted.
    pub max_depth: u32,
    /// Inbound tasks at once per caller.
    pub max_concurrent: u32,
    /// The longest a task delegated by this bot may take.
    pub task_timeout: Duration,
}

/// Everything a [`Node`](crate::Node) needs besides its ports.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// This bot's key.
    pub key: SigningKey,
    /// This bot's X25519 key, for sealed messages through relays.
    pub box_key: crate::seal::BoxKey,
    /// This bot's `state/` directory (its socket lives there).
    pub state_dir: PathBuf,
    /// Teams.
    pub teams: Vec<TeamConfig>,
    /// A2A agents and clients that are not okbots, and this bot's name among them.
    pub external: Option<(String, crate::external::ExternalConfig)>,
}

/// One team as this bot knows it.
pub(crate) struct Team {
    pub(crate) config: TeamConfig,
    manifest: RwLock<Arc<Manifest>>,
    /// When a request last made this bot read the manifest again.
    forced: Mutex<Option<Instant>>,
}

impl Team {
    pub(crate) fn manifest(&self) -> Arc<Manifest> {
        self.manifest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn signed(&self) -> bool {
        matches!(self.config.source, ManifestSource::Url { .. })
    }

    /// Whether a request naming a newer manifest may make this bot read it now: at most every
    /// [`FORCED_REFRESH`], since the claim is read before the signature can be checked.
    pub(crate) fn may_force_refresh(&self) -> bool {
        let mut last = lock(&self.forced);
        if last.is_some_and(|at| at.elapsed() < FORCED_REFRESH) {
            return false;
        }
        *last = Some(Instant::now());
        true
    }
}

/// The shortest time between two reads of a manifest asked for by requests.
const FORCED_REFRESH: Duration = Duration::from_secs(30);

/// An inbound task waiting for the runtime's answer.
#[derive(Debug, Clone)]
pub(crate) struct Running {
    pub(crate) team: String,
    pub(crate) caller: String,
    pub(crate) task: String,
    pub(crate) deadline: Timestamp,
}

/// A prompt the runtime showed in a peer conversation.
#[derive(Debug, Clone)]
pub(crate) struct OpenPrompt {
    pub(crate) prompt: Prompt,
    pub(crate) sent: SentRef,
}

/// Shared state of a node.
pub(crate) struct Inner {
    pub(crate) key: SigningKey,
    pub(crate) box_key: crate::seal::BoxKey,
    pub(crate) state_dir: PathBuf,
    pub(crate) teams: BTreeMap<String, Team>,
    pub(crate) store: Arc<dyn ConversationStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) replays: Replays,
    pub(crate) http: reqwest::Client,
    pub(crate) sink: OnceLock<Arc<dyn InboundSink>>,
    pub(crate) local_server: std::sync::atomic::AtomicBool,
    /// Inbound tasks by conversation, oldest first.
    pub(crate) inbound: Mutex<HashMap<ConversationKey, VecDeque<Running>>>,
    pub(crate) callers: Mutex<HashMap<ConversationKey, PeerCaller>>,
    /// Open prompts by inbound task id.
    pub(crate) prompts: Mutex<HashMap<String, OpenPrompt>>,
    /// Members that did not answer, until when.
    down: Mutex<HashMap<(String, String), Instant>>,
    /// Requests for members this bot relays.
    pub(crate) relay: crate::relay::Relay,
    /// Tokens reported for inbound tasks, until they end.
    pub(crate) usage: Mutex<HashMap<String, u64>>,
    /// `[a2a]` agents and clients.
    pub(crate) external: Option<crate::external::ExternalConfig>,
    /// What the turn of each working inbound task is doing.
    pub(crate) progress: Mutex<HashMap<String, String>>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Inner {
    pub(crate) fn new(
        config: NodeConfig,
        store: Arc<dyn ConversationStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Error> {
        let mut teams = BTreeMap::new();
        for team in config.teams {
            let manifest = match &team.source {
                ManifestSource::File(path) => load_file(path, &team.id)?,
                // Fetched before the node serves (see `Node::start`); empty until then.
                ManifestSource::Url { .. } | ManifestSource::Builtin => Manifest::new(&team.id),
            };
            teams.insert(
                team.id.clone(),
                Team {
                    config: team,
                    manifest: RwLock::new(Arc::new(manifest)),
                    forced: Mutex::new(None),
                },
            );
        }
        let external = config.external.map(|(me, external)| {
            teams.insert(
                crate::external::TEAM.to_string(),
                Team {
                    config: TeamConfig {
                        id: crate::external::TEAM.into(),
                        member: me.clone(),
                        source: ManifestSource::Builtin,
                        accept: !external.clients.is_empty(),
                        max_depth: 1,
                        max_concurrent: external.max_concurrent,
                        task_timeout: external.task_timeout,
                    },
                    manifest: RwLock::new(Arc::new(external.manifest(&me))),
                    forced: Mutex::new(None),
                },
            );
            external
        });
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            key: config.key,
            box_key: config.box_key,
            state_dir: config.state_dir,
            teams,
            store,
            clock,
            replays: Replays::default(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|e| Error::Key(format!("HTTP client: {e}")))?,
            sink: OnceLock::new(),
            local_server: std::sync::atomic::AtomicBool::new(false),
            inbound: Mutex::default(),
            callers: Mutex::default(),
            prompts: Mutex::default(),
            down: Mutex::default(),
            relay: crate::relay::Relay::default(),
            usage: Mutex::default(),
            external,
            progress: Mutex::default(),
        })
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// Checks that this bot is a member of each file-based team under its own key.
    pub(crate) fn check_membership(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for team in self.teams.values() {
            let manifest = team.manifest();
            if (team.signed() && manifest.team.version == 0)
                || team.config.source == ManifestSource::Builtin
            {
                continue;
            }
            match manifest.members.get(&team.config.member) {
                None => problems.push(format!(
                    "team `{}`: this bot is not a member named `{}`",
                    team.config.id, team.config.member
                )),
                Some(m) => {
                    if member_key(m).ok().as_ref() != Some(&self.key.public()) {
                        problems.push(format!(
                            "team `{}`: member `{}` has another key than this bot",
                            team.config.id, team.config.member
                        ));
                    }
                }
            }
        }
        problems
    }

    /// Replaces a team's manifest with a newer one.
    pub(crate) fn install(&self, team: &Team, manifest: Manifest) -> Result<bool, Error> {
        manifest.check_fresh(self.now(), team.signed())?;
        let current = team.manifest();
        if current.team.version > 0 {
            manifest.may_replace(&current)?;
        }
        if manifest == *current {
            return Ok(false);
        }
        tracing::info!(team = %team.config.id, version = manifest.team.version, "team manifest installed");
        *team
            .manifest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(manifest);
        Ok(true)
    }

    /// Reads a team's manifest again (from its file or URL).
    pub(crate) async fn refresh(&self, team: &Team) -> Result<bool, Error> {
        let manifest = match &team.config.source {
            ManifestSource::Builtin => return Ok(false),
            ManifestSource::File(path) => load_file(path, &team.config.id)?,
            ManifestSource::Url { url, team_key } => {
                let get = |u: String| async move {
                    let reply = self
                        .http
                        .get(&u)
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                        .map_err(|e| Error::Manifest(format!("cannot fetch {u}: {e}")))?;
                    reply
                        .text()
                        .await
                        .map_err(|e| Error::Manifest(format!("cannot read {u}: {e}")))
                };
                let text = get(url.clone()).await?;
                let sig = get(format!("{url}.sig")).await?;
                let manifest = manifest::verify(&text, &sig, team_key)?;
                if manifest.team.id != team.config.id {
                    return Err(Error::Manifest(format!(
                        "{url} is the manifest of team `{}`",
                        manifest.team.id
                    )));
                }
                manifest
            }
        };
        self.install(team, manifest)
    }

    /// The public key of a member.
    pub(crate) fn key_of(manifest: &Manifest, name: &str) -> Result<PublicKey, Error> {
        manifest
            .members
            .get(name)
            .ok_or_else(|| Error::Token(format!("`{name}` is not a member")))
            .and_then(member_key)
    }

    /// The A2A version `member` of `team` speaks: 1.0 for okbots, as configured for agents.
    pub(crate) fn version_of(&self, team: &Team, member: &str) -> a2a::Version {
        if team.config.source == ManifestSource::Builtin {
            return self
                .external
                .as_ref()
                .and_then(|e| e.agent(member))
                .map_or(a2a::Version::V10, |a| a.version);
        }
        a2a::Version::V10
    }

    pub(crate) fn is_down(&self, team: &str, member: &str) -> bool {
        lock(&self.down)
            .get(&(team.to_string(), member.to_string()))
            .is_some_and(|until| *until > Instant::now())
    }

    fn mark(&self, team: &str, member: &str, up: bool) {
        let key = (team.to_string(), member.to_string());
        let mut down = lock(&self.down);
        if up {
            down.remove(&key);
        } else {
            down.insert(key, Instant::now() + DOWN_FOR);
        }
    }

    /// Sends one JSON-RPC request to a member and returns its verified response.
    pub(crate) async fn call(
        &self,
        team: &Team,
        member: &str,
        request: &a2a::Request,
    ) -> Result<Value, PeerError> {
        let result = self.exchange(team, member, request).await;
        self.mark(
            &team.config.id,
            member,
            !matches!(result, Err(PeerError::Unreachable(_))),
        );
        let response = result?;
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("refused")
                .to_string();
            return Err(PeerError::Refused(message));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| PeerError::Refused("an empty response".into()))
    }

    async fn exchange(
        &self,
        team: &Team,
        member: &str,
        request: &a2a::Request,
    ) -> Result<Value, PeerError> {
        if team.config.source == ManifestSource::Builtin {
            let agent = self
                .external
                .as_ref()
                .and_then(|e| e.agent(member))
                .ok_or_else(|| PeerError::NoRoute(member.to_string()))?;
            return crate::external::call(&self.http, agent, request).await;
        }
        let manifest = team.manifest();
        let target = manifest
            .members
            .get(member)
            .ok_or_else(|| PeerError::NoRoute(member.to_string()))?;
        let callee_key = member_key(target).map_err(|e| PeerError::Unreachable(e.to_string()))?;
        let body = serde_json::to_vec(request).expect("requests serialize");
        let claims = Claims::new(
            &team.config.id,
            &team.config.member,
            member,
            manifest.team.version,
            &body,
            self.now(),
        );
        let authorization = format!("{SCHEME}{}", token::sign(&self.key, &claims));
        let (status, signature, reply) = match (&target.dir, &target.url, &target.relay) {
            (Some(dir), _, _) => {
                let socket = dir.join("state").join(SOCKET_FILE);
                local_request(&socket, &team.config.id, &authorization, &body).await?
            }
            (_, Some(url), _) => self.http_request(url, &authorization, body).await?,
            (_, _, Some(relay)) => {
                Box::pin(crate::relay::send(
                    self,
                    team,
                    relay,
                    member,
                    &authorization,
                    &body,
                ))
                .await?
            }
            _ => return Err(PeerError::NoRoute(member.to_string())),
        };
        let Some(signature) = signature else {
            return Err(PeerError::Refused(format!(
                "`{member}` answered HTTP {status} without a signature"
            )));
        };
        let claims = token::verify(&signature, &callee_key, &reply, self.now())
            .map_err(|e| PeerError::Refused(format!("`{member}`'s answer: {e}")))?;
        if claims.iss != member || claims.aud != team.config.member || claims.team != team.config.id
        {
            return Err(PeerError::Refused(format!(
                "`{member}`'s answer is signed for someone else"
            )));
        }
        serde_json::from_slice(&reply)
            .map_err(|_| PeerError::Refused(format!("`{member}` sent invalid JSON")))
    }

    async fn http_request(
        &self,
        url: &str,
        authorization: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Option<String>, Vec<u8>), PeerError> {
        let reply = self
            .http
            .post(url)
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .header(
                crate::a2a::VERSION_HEADER,
                crate::a2a::Version::V10.header(),
            )
            .header(crate::a2a::EXTENSIONS_HEADER, crate::a2a::EXTENSION)
            .body(body)
            .send()
            .await
            .map_err(|e| PeerError::Unreachable(without_url(e)))?;
        let status = reply.status().as_u16();
        let signature = reply
            .headers()
            .get(SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = reply
            .bytes()
            .await
            .map_err(|e| PeerError::Unreachable(without_url(e)))?;
        if bytes.len() > MAX_BODY {
            return Err(PeerError::Refused("the answer is too large".into()));
        }
        Ok((status, signature, bytes.to_vec()))
    }

    /// Signs a response body for `caller`.
    pub(crate) fn sign_response(&self, team: &Team, caller: &str, body: &[u8]) -> String {
        let claims = Claims::new(
            &team.config.id,
            &team.config.member,
            caller,
            team.manifest().team.version,
            body,
            self.now(),
        );
        token::sign(&self.key, &claims)
    }
}

fn without_url(error: reqwest::Error) -> String {
    if error.is_timeout() {
        return "timed out".into();
    }
    error.without_url().to_string()
}

/// A member's X25519 key: from the manifest, or from its bot directory.
pub(crate) fn member_box_key(member: &Member) -> Result<crate::seal::BoxPublic, Error> {
    match (&member.box_key, &member.dir) {
        (Some(key), _) => Ok(*key),
        (None, Some(dir)) => crate::seal::bot_box_key(dir),
        (None, None) => Err(Error::Key(
            "the member has no `box_key` (`okbot peer id` prints it)".into(),
        )),
    }
}

/// A member's key: from the manifest, or from its bot directory.
pub(crate) fn member_key(member: &Member) -> Result<PublicKey, Error> {
    match (&member.key, &member.dir) {
        (Some(key), _) => Ok(key.clone()),
        (None, Some(dir)) => bot_public_key(dir),
        (None, None) => Err(Error::Key("the member has no key".into())),
    }
}

fn load_file(path: &Path, team: &str) -> Result<Manifest, Error> {
    let text =
        std::fs::read_to_string(path).map_err(|e| Error::Io(path.display().to_string(), e))?;
    let manifest = Manifest::parse(&text)?;
    if manifest.team.id != team {
        return Err(Error::Manifest(format!(
            "{} is the manifest of team `{}`, not `{team}`",
            path.display(),
            manifest.team.id
        )));
    }
    Ok(manifest)
}

/// One request over a Unix socket: a JSON line each way.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LocalRequest {
    pub(crate) team: String,
    pub(crate) authorization: String,
    pub(crate) body: String,
}

/// The answer to a [`LocalRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LocalResponse {
    pub(crate) status: u16,
    #[serde(default)]
    pub(crate) signature: Option<String>,
    pub(crate) body: String,
}

async fn local_request(
    socket: &Path,
    team: &str,
    authorization: &str,
    body: &[u8],
) -> Result<(u16, Option<String>, Vec<u8>), PeerError> {
    let unreachable = |e: std::io::Error| {
        PeerError::Unreachable(format!("{}: {e} (is that bot running?)", socket.display()))
    };
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(unreachable)?;
        let request = LocalRequest {
            team: team.to_string(),
            authorization: authorization.to_string(),
            body: String::from_utf8_lossy(body).into_owned(),
        };
        let mut line = serde_json::to_vec(&request).expect("requests serialize");
        line.push(b'\n');
        stream.write_all(&line).await.map_err(unreachable)?;
        let mut reader = BufReader::new(stream).take(MAX_BODY as u64 * 2);
        let mut answer = String::new();
        reader.read_line(&mut answer).await.map_err(unreachable)?;
        let response: LocalResponse = serde_json::from_str(&answer)
            .map_err(|_| PeerError::Refused("an invalid local answer".into()))?;
        Ok((
            response.status,
            response.signature,
            response.body.into_bytes(),
        ))
    };
    tokio::time::timeout(REQUEST_TIMEOUT, exchange)
        .await
        .map_err(|_| PeerError::Unreachable(format!("{}: timed out", socket.display())))?
}
