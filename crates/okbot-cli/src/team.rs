//! `okbot peer` and `okbot team` (ADR 0014): a bot's peer key and the team manifests kept in
//! `~/.config/okbot/teams/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use okbot_peer::manifest::{self, Link, Manifest, Member, TeamPerson};
use okbot_peer::{PublicKey, SigningKey};

use crate::registry;

/// `okbot peer` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PeerCommand {
    /// Print this bot's public key (created on first use), to add it to a team.
    Id {
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// List tasks delegated to or by this bot, newest first.
    Tasks {
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// How many.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

/// `okbot team` subcommands. They edit the manifest in `~/.config/okbot/teams/<team>.toml`.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum TeamCommand {
    /// Start a team. `local` is the team of this machine's bots; other teams are signed with a
    /// team key (`--owner` creates it).
    Init {
        /// Team id: lowercase letters, digits, `-`, `_`.
        team: String,
        /// Create the team key here (keep this machine safe; bots do not need it).
        #[arg(long)]
        owner: bool,
        /// What the team is for.
        #[arg(long)]
        description: Option<String>,
    },
    /// Add or update a member.
    Add(AddArgs),
    /// Make a bot a member: writes `[teams.<team>]` in its okbot.toml and, for a team kept on
    /// this machine, adds the bot to the manifest.
    Join(JoinArgs),
    /// Remove a member.
    Remove {
        /// Team id.
        team: String,
        /// Member name.
        member: String,
    },
    /// Let members matching `--from` call members matching `--to` (a name, `role:<r>` or `*`).
    Link {
        /// Team id.
        team: String,
        /// Callers (repeatable).
        #[arg(long, required = true)]
        from: Vec<String>,
        /// Callees (repeatable).
        #[arg(long, required = true)]
        to: Vec<String>,
    },
    /// Remove a link by its number in `okbot team show`.
    Unlink {
        /// Team id.
        team: String,
        /// Link number (from 1).
        number: usize,
    },
    /// Name a person for the team, with their chat accounts, so other bots know who asked.
    Person {
        /// Team id.
        team: String,
        /// Person id (the same id as in each bot's `[people]` and `[roles]`).
        person: String,
        /// Accounts as `platform:instance/user-id` (repeatable).
        #[arg(long = "account")]
        accounts: Vec<String>,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// Remove the person instead.
        #[arg(long)]
        remove: bool,
    },
    /// Sign the manifest with the team key: a new version, valid for `--days`.
    Sign {
        /// Team id.
        team: String,
        /// Days until it expires (at most 90).
        #[arg(long, default_value_t = 30)]
        days: i64,
    },
    /// Show the manifest: members, links and people.
    Show {
        /// Team id.
        team: String,
    },
    /// Check a manifest file, and its signature with `--key`.
    Check {
        /// The manifest.
        file: PathBuf,
        /// The team's public key; the signature is read from `<file>.sig`.
        #[arg(long)]
        key: Option<String>,
    },
    /// Copy the signed manifest and its signature into a directory your HTTPS server serves.
    Publish {
        /// Team id.
        team: String,
        /// The directory (default: the current one).
        #[arg(long)]
        to: Option<PathBuf>,
    },
}

/// The bot's peer node for its `[teams]`, or `None` without teams. Problems that do not stop the
/// bot (a manifest not fetched yet, this bot missing from a team) are logged.
pub(crate) async fn node(
    config: &okbot_config::Config,
    store: std::sync::Arc<okbot_store::SqliteStore>,
) -> anyhow::Result<Option<okbot_peer::Node>> {
    if config.teams.is_empty() && config.a2a.agent.is_empty() && config.a2a.client.is_empty() {
        return Ok(None);
    }
    let node = node_unstarted(config, store)?;
    for problem in node.start().await {
        tracing::warn!("{problem}");
    }
    Ok(Some(node))
}

/// The peer node, before [`okbot_peer::Node::start`] (which changes stored tasks).
pub(crate) fn node_unstarted(
    config: &okbot_config::Config,
    store: std::sync::Arc<okbot_store::SqliteStore>,
) -> anyhow::Result<okbot_peer::Node> {
    let state = config.state_dir();
    let key = SigningKey::for_bot(&state)?;
    let default_member = slug(&config.bot.name);
    let mut teams = Vec::new();
    for (id, t) in &config.teams {
        let fetched =
            t.manifest.starts_with("https://") || t.manifest.starts_with("http://127.0.0.1");
        let source = if fetched {
            okbot_peer::ManifestSource::Url {
                url: t.manifest.clone(),
                team_key: t
                    .team_key
                    .parse()
                    .with_context(|| format!("[teams.{id}] team_key"))?,
            }
        } else if t.manifest.is_empty() {
            okbot_peer::ManifestSource::File(manifest_path(id)?)
        } else {
            okbot_peer::ManifestSource::File(config.dir.join(&t.manifest))
        };
        teams.push(okbot_peer::TeamConfig {
            id: id.clone(),
            member: if t.member.is_empty() {
                default_member.clone()
            } else {
                t.member.clone()
            },
            source,
            accept: t.accept,
            max_depth: t.max_depth,
            max_concurrent: t.max_concurrent,
            task_timeout: std::time::Duration::from_secs(t.task_timeout),
        });
    }
    let box_key = okbot_peer::BoxKey::for_bot(&state)?;
    let external = external(config)?.map(|e| (default_member.clone(), e));
    okbot_peer::Node::new(
        okbot_peer::NodeConfig {
            key,
            box_key,
            state_dir: state,
            sockets: okbot_peer::socket_dir(),
            teams,
            external,
        },
        store,
        std::sync::Arc::new(okbot_core::SystemClock),
    )
    .context("cannot start the bot's teams")
}

/// `[a2a]` agents and clients, with their tokens.
fn external(config: &okbot_config::Config) -> anyhow::Result<Option<okbot_peer::ExternalConfig>> {
    let a = &config.a2a;
    if a.agent.is_empty() && a.client.is_empty() {
        return Ok(None);
    }
    let mut agents = Vec::new();
    for agent in &a.agent {
        agents.push(okbot_peer::ExternalAgent {
            name: agent.name.clone(),
            url: agent.url.clone(),
            token: if agent.token_env.is_empty() {
                None
            } else {
                Some(
                    config
                        .secret(&agent.token_env)
                        .with_context(|| format!("[[a2a.agent]] `{}` token_env", agent.name))?,
                )
            },
            version: okbot_peer::a2a::Version::parse(Some(&agent.version))
                .unwrap_or(okbot_peer::a2a::Version::V10),
            description: agent.description.clone(),
            skills: agent.skills.clone(),
        });
    }
    let mut clients = Vec::new();
    for client in &a.client {
        clients.push(okbot_peer::ExternalClient {
            name: client.name.clone(),
            token: config
                .secret(&client.token_env)
                .with_context(|| format!("[[a2a.client]] `{}` token_env", client.name))?,
            person: client.person.clone(),
        });
    }
    Ok(Some(okbot_peer::ExternalConfig {
        agents,
        clients,
        max_concurrent: a.max_concurrent,
        task_timeout: std::time::Duration::from_secs(a.task_timeout),
    }))
}

/// A bot name as a member name: lowercase, other characters as `-`.
pub(crate) fn slug(name: &str) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

/// `okbot team join`.
#[derive(Debug, Default, clap::Args)]
pub(crate) struct JoinArgs {
    /// Team id.
    pub(crate) team: String,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    pub(crate) bot: Option<String>,
    /// Its member name; default: the bot's name as a slug.
    #[arg(long)]
    pub(crate) member: Option<String>,
    /// Other members may give this bot work.
    #[arg(long)]
    pub(crate) accept: bool,
    /// The caller's team role mapped to roles here: `leader=ops,support` (repeatable).
    #[arg(long = "map")]
    pub(crate) map: Vec<String>,
    /// Who here may make the agent delegate (default: admins).
    #[arg(long = "delegate-role")]
    pub(crate) delegate_roles: Vec<String>,
    /// The requester approves each delegation.
    #[arg(long)]
    pub(crate) confirm: bool,
    /// A team on several machines: the manifest's https:// URL.
    #[arg(long)]
    pub(crate) manifest: Option<String>,
    /// With `--manifest`: the team's public key.
    #[arg(long)]
    pub(crate) team_key: Option<String>,
    /// Team roles of this bot in a local manifest (repeatable).
    #[arg(long = "role")]
    pub(crate) roles: Vec<String>,
    /// Skills of this bot in a local manifest (repeatable).
    #[arg(long = "skill")]
    pub(crate) skills: Vec<String>,
    /// What this bot does, for the others.
    #[arg(long)]
    pub(crate) description: Option<String>,
}

/// `okbot team add`.
#[derive(Debug, clap::Args)]
pub(crate) struct AddArgs {
    /// Team id.
    team: String,
    /// Member name: lowercase letters, digits, `-`, `_`.
    member: String,
    /// Its public key (`okbot peer id` on its machine, first line).
    #[arg(long)]
    key: Option<String>,
    /// Its box key (`okbot peer id`, second line): needed when it is reached through a relay or
    /// calls a member that is.
    #[arg(long)]
    box_key: Option<String>,
    /// Its A2A endpoint: `https://<its public_url>/a2a/<team>`.
    #[arg(long)]
    url: Option<String>,
    /// The member that relays requests to it (for a bot behind NAT).
    #[arg(long)]
    relay: Option<String>,
    /// Its bot directory on this machine; default for a registered bot of the same name.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Team roles (repeatable).
    #[arg(long = "role")]
    roles: Vec<String>,
    /// Skills (repeatable).
    #[arg(long = "skill")]
    skills: Vec<String>,
    /// What it does, for the agents that delegate to it.
    #[arg(long)]
    description: Option<String>,
}

/// Where team manifests and keys live: `~/.config/okbot/teams/`.
pub(crate) fn teams_dir() -> anyhow::Result<PathBuf> {
    Ok(registry::home()?.join("teams"))
}

/// The manifest file of `team` in [`teams_dir`].
pub(crate) fn manifest_path(team: &str) -> anyhow::Result<PathBuf> {
    Ok(teams_dir()?.join(format!("{team}.toml")))
}

fn key_path(team: &str) -> anyhow::Result<PathBuf> {
    Ok(teams_dir()?.join(format!("{team}.key")))
}

fn sig_path(manifest: &Path) -> PathBuf {
    let mut name = manifest.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

fn load(team: &str) -> anyhow::Result<Manifest> {
    let path = manifest_path(team)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no team `{team}` here: `okbot team init {team}` starts one"))?;
    Manifest::parse(&text).with_context(|| format!("{} is invalid", path.display()))
}

/// Writes an edited manifest as a new version. An edit invalidates the old signature, so it is
/// removed: `okbot team sign` signs again.
fn save(mut manifest: Manifest, what: &str) -> anyhow::Result<()> {
    manifest.team.version += 1;
    manifest.check()?;
    let path = manifest_path(&manifest.team.id)?;
    std::fs::create_dir_all(teams_dir()?)?;
    std::fs::write(&path, manifest.to_toml())
        .with_context(|| format!("cannot write {}", path.display()))?;
    let sig = sig_path(&path);
    let signed = sig.exists();
    if signed {
        std::fs::remove_file(&sig)?;
    }
    println!(
        "{what} (team `{}`, version {})",
        manifest.team.id, manifest.team.version
    );
    if signed || key_path(&manifest.team.id)?.exists() {
        println!(
            "sign it for the other machines: okbot team sign {}",
            manifest.team.id
        );
    }
    Ok(())
}

pub(crate) async fn peer(command: PeerCommand) -> anyhow::Result<()> {
    match command {
        PeerCommand::Id { bot } => {
            let dir = registry::resolve(bot.as_deref())?;
            let key = SigningKey::for_bot(&dir.join("state"))?;
            let box_key = okbot_peer::BoxKey::for_bot(&dir.join("state"))?;
            println!("{}", key.public());
            println!("{}", box_key.public());
            eprintln!(
                "fingerprint {}; the second line is the box key (--box-key), for relays",
                key.public().fingerprint()
            );
            Ok(())
        }
        PeerCommand::Tasks { bot, limit } => tasks(bot.as_deref(), limit).await,
    }
}

async fn tasks(bot: Option<&str>, limit: usize) -> anyhow::Result<()> {
    use okbot_core::ConversationStore as _;
    let dir = registry::resolve(bot)?;
    let store = okbot_store::SqliteStore::open(&dir.join("state").join("okbot.db"))?;
    let tasks = store.peer_tasks(limit).await?;
    if tasks.is_empty() {
        println!("no peer tasks");
    }
    for t in tasks {
        println!(
            "{} {:<3} {}/{:<12} {:<14} {}  trace {}{}",
            t.updated_at.strftime("%Y-%m-%d %H:%M"),
            if t.inbound { "in" } else { "out" },
            t.team,
            t.peer,
            t.state.as_str(),
            t.id,
            t.trace,
            t.text
                .filter(|_| t.state != okbot_core::PeerTaskState::Completed)
                .map(|e| format!("  ({})", first_line(&e)))
                .unwrap_or_default(),
        );
    }
    Ok(())
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    line.chars().take(80).collect()
}

pub(crate) fn team(command: TeamCommand) -> anyhow::Result<()> {
    match command {
        TeamCommand::Init {
            team,
            owner,
            description,
        } => init(&team, owner, description),
        TeamCommand::Add(args) => add(args),
        TeamCommand::Join(args) => {
            let dir = registry::resolve(args.bot.as_deref())?;
            join(&dir, &args)
        }
        TeamCommand::Remove { team, member } => {
            let mut manifest = load(&team)?;
            if manifest.members.remove(&member).is_none() {
                bail!("no member `{member}` in team `{team}`");
            }
            manifest.links.retain_mut(|l| {
                l.from.retain(|s| *s != member);
                l.to.retain(|s| *s != member);
                !l.from.is_empty() && !l.to.is_empty()
            });
            save(manifest, &format!("member `{member}` removed"))
        }
        TeamCommand::Link { team, from, to } => {
            let mut manifest = load(&team)?;
            let link = Link { from, to };
            if manifest.links.contains(&link) {
                println!("that link exists");
                return Ok(());
            }
            manifest.links.push(link);
            save(manifest, "link added")
        }
        TeamCommand::Unlink { team, number } => {
            let mut manifest = load(&team)?;
            if number == 0 || number > manifest.links.len() {
                bail!("no link {number}; `okbot team show {team}` numbers them");
            }
            manifest.links.remove(number - 1);
            save(manifest, "link removed")
        }
        TeamCommand::Person {
            team,
            person,
            accounts,
            name,
            remove,
        } => {
            let mut manifest = load(&team)?;
            if remove {
                if manifest.people.remove(&person).is_none() {
                    bail!("no person `{person}` in team `{team}`");
                }
                return save(manifest, &format!("person `{person}` removed"));
            }
            let entry = manifest
                .people
                .entry(person.clone())
                .or_insert_with(TeamPerson::default);
            for account in accounts {
                if !entry.accounts.contains(&account) {
                    entry.accounts.push(account);
                }
            }
            if let Some(name) = name {
                entry.name = name;
            }
            save(manifest, &format!("person `{person}` saved"))
        }
        TeamCommand::Sign { team, days } => sign(&team, days),
        TeamCommand::Show { team } => {
            show(&load(&team)?);
            Ok(())
        }
        TeamCommand::Check { file, key } => check(&file, key.as_deref()),
        TeamCommand::Publish { team, to } => publish(&team, to.as_deref()),
    }
}

/// The `[teams.<team>]` section for `args`.
fn team_section(args: &JoinArgs) -> anyhow::Result<String> {
    use toml::Value;
    let mut t = toml::Table::new();
    if let Some(member) = &args.member {
        t.insert("member".into(), Value::String(member.clone()));
    }
    match (&args.manifest, &args.team_key) {
        (Some(url), Some(key)) => {
            key.parse::<PublicKey>()?;
            t.insert("manifest".into(), Value::String(url.clone()));
            t.insert("team_key".into(), Value::String(key.clone()));
        }
        (Some(_), None) => {
            bail!("--manifest needs --team-key (the key the manifest is signed with)")
        }
        (None, Some(_)) => bail!("--team-key goes with --manifest"),
        (None, None) => {}
    }
    if args.accept {
        t.insert("accept".into(), Value::Boolean(true));
    }
    if !args.map.is_empty() {
        let mut roles = toml::Table::new();
        for entry in &args.map {
            let Some((team_role, local)) = entry.split_once('=') else {
                bail!("--map `{entry}`: write `<team role>=<role>[,<role>…]`");
            };
            let local: Vec<Value> = local
                .split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(|r| Value::String(r.to_string()))
                .collect();
            roles.insert(team_role.trim().to_string(), Value::Array(local));
        }
        t.insert("roles".into(), Value::Table(roles));
    }
    if !args.delegate_roles.is_empty() {
        let roles = args
            .delegate_roles
            .iter()
            .cloned()
            .map(Value::String)
            .collect();
        t.insert("delegate_roles".into(), Value::Array(roles));
    }
    if args.confirm {
        t.insert("confirm".into(), Value::String("always".into()));
    }
    let mut teams = toml::Table::new();
    teams.insert(args.team.clone(), Value::Table(t));
    let mut root = toml::Table::new();
    root.insert("teams".into(), Value::Table(teams));
    Ok(toml::to_string(&root)?)
}

/// Writes `[teams.<team>]` into the bot in `dir`; adds it to a manifest kept on this machine.
pub(crate) fn join(dir: &Path, args: &JoinArgs) -> anyhow::Result<()> {
    let config = okbot_config::Config::load(dir)?;
    if config.teams.contains_key(&args.team) {
        bail!(
            "this bot is already in team `{}` ([teams.{}] in okbot.toml)",
            args.team,
            args.team
        );
    }
    let member = args
        .member
        .clone()
        .unwrap_or_else(|| slug(&config.bot.name));
    let mut editor = okbot_config::ConfigEditor::open(dir)?;
    editor.append(&format!("\n{}", team_section(args)?))?;
    editor.save()?;
    let key = SigningKey::for_bot(&dir.join("state"))?;
    println!(
        "joined team `{}` as `{member}` in {}",
        args.team,
        dir.display()
    );
    if args.manifest.is_some() {
        println!("ask the team's owner to add this bot:");
        println!(
            "  okbot team add {} {member} --key {} --box-key {} --url <public_url>/a2a/{}  (or --relay <member>)",
            args.team,
            key.public(),
            okbot_peer::BoxKey::for_bot(&dir.join("state"))?.public(),
            args.team
        );
        return Ok(());
    }
    let path = manifest_path(&args.team)?;
    let mut manifest = if path.exists() {
        load(&args.team)?
    } else if args.team == "local" {
        Manifest::new("local")
    } else {
        println!(
            "no manifest for `{}` here yet: `okbot team init {}`",
            args.team, args.team
        );
        return Ok(());
    };
    if manifest.members.contains_key(&member) {
        return Ok(());
    }
    if key_path(&args.team)?.exists() || sig_path(&path).exists() {
        println!(
            "team `{}` is signed: add this bot with `okbot team add {} {member} --key {} …`, then sign",
            args.team,
            args.team,
            key.public()
        );
        return Ok(());
    }
    manifest.members.insert(
        member.clone(),
        Member {
            dir: Some(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())),
            roles: args.roles.clone(),
            skills: args.skills.clone(),
            description: args.description.clone().unwrap_or_default(),
            ..Member::default()
        },
    );
    save(manifest, &format!("member `{member}` added"))?;
    println!(
        "link it to others: okbot team link {} --from <who> --to <whom>",
        args.team
    );
    Ok(())
}

fn add(args: AddArgs) -> anyhow::Result<()> {
    let AddArgs {
        team,
        member,
        key,
        box_key,
        url,
        relay,
        dir,
        roles,
        skills,
        description,
    } = args;
    let mut manifest = load(&team)?;
    let mut entry = manifest.members.get(&member).cloned().unwrap_or_default();
    let reach_given = url.is_some() || relay.is_some() || dir.is_some();
    let dir = match dir {
        Some(dir) => Some(std::fs::canonicalize(&dir).unwrap_or(dir)),
        None if !reach_given && entry == Member::default() => registry::resolve(Some(&member)).ok(),
        None => None,
    };
    if reach_given || dir.is_some() {
        entry.url = url;
        entry.relay = relay;
        entry.dir = dir;
    }
    if let Some(key) = key {
        entry.key = Some(key.parse::<PublicKey>()?);
    }
    if let Some(key) = box_key {
        entry.box_key = Some(key.parse::<okbot_peer::BoxPublic>()?);
    }
    if !roles.is_empty() {
        entry.roles = roles;
    }
    if !skills.is_empty() {
        entry.skills = skills;
    }
    if let Some(description) = description {
        entry.description = description;
    }
    if entry.url.is_none() && entry.relay.is_none() && entry.dir.is_none() {
        bail!(
            "say how to reach `{member}`: --url, --relay, or --dir (a bot directory on \
             this machine)"
        );
    }
    manifest.members.insert(member.clone(), entry);
    save(manifest, &format!("member `{member}` saved"))
}

fn init(team: &str, owner: bool, description: Option<String>) -> anyhow::Result<()> {
    let path = manifest_path(team)?;
    if path.exists() {
        bail!("team `{team}` exists: {}", path.display());
    }
    if owner && team == "local" {
        bail!("the local team is a file on this machine and is not signed");
    }
    let mut manifest = Manifest::new(team);
    manifest.team.description = description.unwrap_or_default();
    manifest.check()?;
    if owner {
        let (key, bytes) = SigningKey::generate()?;
        okbot_peer::key::write_new_private(&key_path(team)?, &bytes)?;
        println!("team key: {}", key.public());
        println!("members pin it as `team_key` in [teams.{team}]");
    }
    save(manifest, "team created")?;
    println!("manifest: {}", path.display());
    Ok(())
}

fn sign(team: &str, days: i64) -> anyhow::Result<()> {
    if !(1..=manifest::MAX_VALIDITY_DAYS).contains(&days) {
        bail!("--days must be 1 to {}", manifest::MAX_VALIDITY_DAYS);
    }
    let key_file = key_path(team)?;
    let bytes = std::fs::read(&key_file).with_context(|| {
        format!(
            "no team key at {}: only the owner's machine signs (`okbot team init {team} --owner`)",
            key_file.display()
        )
    })?;
    let key = SigningKey::from_pkcs8(&bytes)?;
    let mut manifest = load(team)?;
    manifest.team.version += 1;
    manifest.team.expires = Some(
        jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::from_hours(days * 24))
            .and_then(|t| t.round(jiff::Unit::Second))
            .context("date out of range")?,
    );
    manifest.check()?;
    let text = manifest.to_toml();
    let path = manifest_path(team)?;
    std::fs::write(&path, &text)?;
    std::fs::write(sig_path(&path), manifest::sign(&text, &key))?;
    println!(
        "signed team `{team}` version {}, valid until {}",
        manifest.team.version,
        manifest
            .team
            .expires
            .map(|e| e.to_string())
            .unwrap_or_default()
    );
    println!(
        "publish {} and its .sig where members fetch them",
        path.display()
    );
    Ok(())
}

fn show(manifest: &Manifest) {
    println!(
        "team {} version {}{}",
        manifest.team.id,
        manifest.team.version,
        manifest
            .team
            .expires
            .map(|e| format!(", expires {e}"))
            .unwrap_or_default()
    );
    if !manifest.team.description.is_empty() {
        println!("{}", manifest.team.description);
    }
    println!("\nmembers:");
    for (name, m) in &manifest.members {
        let reach = match (&m.url, &m.relay, &m.dir) {
            (Some(url), _, _) => url.clone(),
            (_, Some(relay), _) => format!("through {relay}"),
            (_, _, Some(dir)) => dir.display().to_string(),
            _ => String::new(),
        };
        let key = match (&m.key, &m.dir) {
            (Some(k), _) => k.fingerprint(),
            (None, Some(dir)) => okbot_peer::key::bot_public_key(dir)
                .map_or_else(|_| "no key yet".into(), |k| k.fingerprint()),
            _ => String::new(),
        };
        println!(
            "  {name:<12} roles [{}] skills [{}]  {reach}  key {key}",
            m.roles.join(", "),
            m.skills.join(", ")
        );
        for to in manifest.callees(name) {
            println!("    may call {to}");
        }
    }
    println!("\nlinks:");
    for (i, l) in manifest.links.iter().enumerate() {
        println!("  {}. {} -> {}", i + 1, l.from.join(", "), l.to.join(", "));
    }
    if !manifest.people.is_empty() {
        println!("\npeople:");
        for (id, p) in &manifest.people {
            println!("  {id:<12} {}  {}", p.name, p.accounts.join(", "));
        }
    }
}

fn check(file: &Path, key: Option<&str>) -> anyhow::Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let manifest = match key {
        Some(key) => {
            let sig = sig_path(file);
            let signatures = std::fs::read_to_string(&sig)
                .with_context(|| format!("cannot read {}", sig.display()))?;
            let manifest = manifest::verify(&text, &signatures, &key.parse()?)?;
            manifest.check_fresh(jiff::Timestamp::now(), true)?;
            println!("signature: ok");
            manifest
        }
        None => Manifest::parse(&text)?,
    };
    println!(
        "team {} version {}: {} members, {} links",
        manifest.team.id,
        manifest.team.version,
        manifest.members.len(),
        manifest.links.len()
    );
    Ok(())
}

fn publish(team: &str, to: Option<&Path>) -> anyhow::Result<()> {
    let path = manifest_path(team)?;
    let sig = sig_path(&path);
    if !sig.exists() {
        bail!("team `{team}` is not signed: okbot team sign {team}");
    }
    let dir = match to {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()?,
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(&path, dir.join(format!("{team}.toml")))?;
    std::fs::copy(&sig, dir.join(format!("{team}.toml.sig")))?;
    println!(
        "copied {team}.toml and {team}.toml.sig to {}; members fetch `https://<host>/<path>/{team}.toml`",
        dir.display()
    );
    println!("anyone who can fetch it reads the members, their URLs and the people listed");
    Ok(())
}
