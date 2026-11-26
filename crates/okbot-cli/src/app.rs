//! `okbot app` (ADR 0019): the apps of a bot and who is signed in to them.

use anyhow::{Context, bail};
use okbot_config::Config;
use okbot_core::{AppDirectory as _, ConversationStore as _};

/// `okbot app` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum AppCommand {
    /// The apps of a bot: built-in and installed, their permissions and whether chat offers them.
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// One app: what it shows, what it may do and the pages chat can open.
    Info {
        /// The app.
        app: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Apps in the registry (ADR 0020), whose index is checked against its signature.
    Search {
        /// Words in the name, title or description.
        query: Option<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Installs an app: from the registry by name, from a folder (`./my-app`), or from a git
    /// commit (`--git <url> --rev <commit>`). Folder and git apps are marked unverified.
    Add {
        /// A registry app name, or a folder.
        app: Option<String>,
        /// A git repository to take the app from.
        #[arg(long, requires = "rev")]
        git: Option<String>,
        /// The exact commit (a moving branch is refused).
        #[arg(long)]
        rev: Option<String>,
        /// The app's folder inside the repository.
        #[arg(long, default_value = ".")]
        path: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Grant its permissions without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Updates registry apps (one, or all): shows new permissions and asks first; reports
    /// versions the registry yanked.
    Update {
        /// One app; default: every registry app.
        app: Option<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Accept new permissions without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Removes an installed app (built-in apps are turned off with `[apps.<name>] disabled`).
    Remove {
        /// The app.
        app: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Serves an app folder while you write it: copied into the bot again whenever it changes.
    Dev {
        /// The app's folder.
        dir: std::path::PathBuf,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// For registry maintainers: the index of the apps under `<dir>/<app>/`, pinned to a commit.
    Index {
        /// The folder of app folders.
        dir: std::path::PathBuf,
        /// The commit the files are at.
        #[arg(long)]
        commit: String,
        /// Where the files are served, the commit in it
        /// (`https://raw.githubusercontent.com/<owner>/okbot-apps/<commit>/apps`).
        #[arg(long)]
        base: String,
        /// Versions to mark yanked, `name@version=reason`.
        #[arg(long = "yank", value_name = "NAME@VERSION=REASON")]
        yanked: Vec<String>,
        /// Where to write it (`index.json`).
        #[arg(long, default_value = "index.json")]
        out: std::path::PathBuf,
    },
    /// For registry maintainers: signs an index with an Ed25519 key (PKCS#8 file, or the
    /// `OKBOT_APPS_SIGNING_KEY` variable as base64), writing `<index>.sig`.
    Sign {
        /// The index file.
        index: std::path::PathBuf,
        /// The key file; default: `OKBOT_APPS_SIGNING_KEY`.
        #[arg(long)]
        key: Option<std::path::PathBuf>,
    },
    /// For registry maintainers: makes a signing key (keep the file offline) and prints its
    /// public key for `[apps] registry_keys` or okbot's built-in keys.
    Keygen {
        /// Where to write the secret key (refused if it exists).
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Browsers signed in to the bot's apps; `--revoke` or `--all` signs them out.
    Sessions {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
        /// Sign out every browser of this person (`person:<id>`) or account
        /// (`platform:instance/user-id`).
        #[arg(long, value_name = "PERSON")]
        revoke: Option<String>,
        /// Sign out every browser of everyone.
        #[arg(long, conflicts_with = "revoke")]
        all: bool,
    },
}

fn load(name: Option<&str>) -> anyhow::Result<Config> {
    let dir = crate::registry::resolve(name)?;
    Ok(Config::load(&dir)?)
}

/// Where people's browsers would open the bot's apps, or why they cannot.
fn address(config: &Config) -> String {
    match (&config.http.public_url, config.apps.enabled) {
        (_, false) => "apps are off ([apps] enabled = false)".into(),
        (Some(url), true) => format!("{url}/apps/"),
        (None, true) => {
            "no [http] public_url: only `okbot chat` opens apps, on this machine".into()
        }
    }
}

pub(crate) async fn run(command: AppCommand) -> anyhow::Result<()> {
    match command {
        AppCommand::List { name } => {
            list(&load(name.as_deref())?);
            Ok(())
        }
        AppCommand::Info { app, bot } => info(&load(bot.as_deref())?, &app),
        AppCommand::Sessions { name, revoke, all } => {
            sessions(&load(name.as_deref())?, revoke.as_deref(), all).await
        }
        AppCommand::Search { query, bot } => search(&load(bot.as_deref())?, query.as_deref()).await,
        AppCommand::Add {
            app,
            git,
            rev,
            path,
            bot,
            yes,
        } => {
            let config = load(bot.as_deref())?;
            match (app, git, rev) {
                (_, Some(url), Some(rev)) => add_git(&config, &url, &rev, &path, yes),
                (Some(app), None, _) if looks_like_folder(&app) => {
                    add_local(&config, std::path::Path::new(&app), yes)
                }
                (Some(app), None, _) => add_registry(&config, &app, yes).await,
                _ => bail!("name a registry app, a folder, or --git <url> --rev <commit>"),
            }
        }
        AppCommand::Update { app, bot, yes } => {
            update(&load(bot.as_deref())?, app.as_deref(), yes).await
        }
        AppCommand::Remove { app, bot } => {
            let config = load(bot.as_deref())?;
            if okbot_apps::builtin()
                .iter()
                .any(|a| a.manifest.info.name == app)
            {
                bail!("`{app}` is built in: turn it off with [apps.{app}] disabled = true");
            }
            if okbot_apps::registry::remove(&apps_dir(&config), &app)? {
                println!("Removed {app}. The bot stops serving it within seconds.");
            } else {
                bail!("`{app}` is not installed");
            }
            Ok(())
        }
        AppCommand::Dev { dir, bot } => dev(&load(bot.as_deref())?, &dir).await,
        AppCommand::Index {
            dir,
            commit,
            base,
            yanked,
            out,
        } => index(&dir, &commit, &base, &yanked, &out),
        AppCommand::Sign { index, key } => sign(&index, key.as_deref()),
        AppCommand::Keygen { out } => keygen(&out),
    }
}

fn apps_dir(config: &Config) -> std::path::PathBuf {
    config.dir.join("apps")
}

fn looks_like_folder(app: &str) -> bool {
    app.contains('/') || app.starts_with('.') || std::path::Path::new(app).is_dir()
}

/// The keys the bot trusts for its registry: okbot's own and `[apps] registry_keys`.
fn keys(config: &Config) -> anyhow::Result<Vec<Vec<u8>>> {
    okbot_apps::registry::OFFICIAL_KEYS
        .iter()
        .map(|k| (*k).to_string())
        .chain(config.apps.registry_keys.iter().cloned())
        .map(|k| okbot_apps::registry::parse_key(&k).map_err(anyhow::Error::from))
        .collect()
}

fn registry_url(config: &Config) -> &str {
    config
        .apps
        .registry
        .as_deref()
        .unwrap_or(okbot_apps::registry::OFFICIAL_INDEX)
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("okbot/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

async fn fetch_index(config: &Config) -> anyhow::Result<okbot_apps::registry::Index> {
    let url = registry_url(config);
    okbot_apps::registry::fetch_index(&client()?, url, &keys(config)?)
        .await
        .with_context(|| format!("cannot use the registry {url}"))
}

/// Shows what an app may do and asks to grant it (or says why not when `yes`).
fn grant(
    name: &str,
    source: &str,
    permissions: &[okbot_core::AppPermission],
    yes: bool,
) -> anyhow::Result<bool> {
    println!("{name} ({source}) may:");
    for p in permissions {
        println!("  - {} ({p})", p.describe());
    }
    if yes {
        return Ok(true);
    }
    Ok(
        inquire::Confirm::new(&format!("Install {name} with these permissions?"))
            .with_default(false)
            .prompt()?,
    )
}

fn permissions_of(list: &[String]) -> Vec<okbot_core::AppPermission> {
    list.iter().filter_map(|p| p.parse().ok()).collect()
}

async fn search(config: &Config, query: Option<&str>) -> anyhow::Result<()> {
    let index = fetch_index(config).await?;
    let query = query.map(str::to_lowercase);
    let matches = |a: &okbot_apps::registry::IndexApp| {
        query.as_ref().is_none_or(|q| {
            a.name.contains(q.as_str())
                || a.title
                    .values()
                    .any(|t| t.to_lowercase().contains(q.as_str()))
                || a.description
                    .values()
                    .any(|d| d.to_lowercase().contains(q.as_str()))
        })
    };
    let found: Vec<_> = index.apps.iter().filter(|a| matches(a)).collect();
    if found.is_empty() {
        println!("No app matches in {}.", registry_url(config));
    }
    for a in found {
        let note = match okbot_apps::registry::installable(a) {
            Ok(()) => String::new(),
            Err(why) => format!("  ({why})"),
        };
        let title = okbot_core::localized(&a.title, None, &a.name);
        println!("  {:<16} {:<10} {title}{note}", a.name, a.version);
        if let Some(d) = a
            .description
            .get("en")
            .or_else(|| a.description.values().next())
        {
            println!("  {:<16} {d}", "");
        }
    }
    Ok(())
}

async fn add_registry(config: &Config, name: &str, yes: bool) -> anyhow::Result<()> {
    let index = fetch_index(config).await?;
    let Some(app) = index.apps.iter().find(|a| a.name == name) else {
        bail!(
            "no app `{name}` in {}; `okbot app search` lists them",
            registry_url(config)
        );
    };
    okbot_apps::registry::installable(app).map_err(anyhow::Error::msg)?;
    if okbot_apps::builtin()
        .iter()
        .any(|a| a.manifest.info.name == name)
    {
        bail!("`{name}` is a built-in app");
    }
    if !grant(name, "registry", &permissions_of(&app.permissions), yes)? {
        bail!("not installed");
    }
    let manifest =
        okbot_apps::registry::install(&client()?, &index, app, &apps_dir(config)).await?;
    println!(
        "Installed {} {} from commit {}. The bot serves it within seconds: `/app {name}`.",
        name, manifest.info.version, index.commit
    );
    Ok(())
}

fn add_local(config: &Config, dir: &std::path::Path, yes: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(dir.join(okbot_apps::MANIFEST))
        .with_context(|| format!("no {} in {}", okbot_apps::MANIFEST, dir.display()))?;
    let manifest = okbot_apps::Manifest::parse(&text, okbot_core::AppSource::Local)?;
    let name = manifest.info.name.clone();
    if okbot_apps::builtin()
        .iter()
        .any(|a| a.manifest.info.name == name)
    {
        bail!("`{name}` is a built-in app; give yours another name");
    }
    println!("This app has not been reviewed: only install code you trust.");
    if !grant(&name, "local", &manifest.info.permissions, yes)? {
        bail!("not installed");
    }
    okbot_apps::registry::install_local(dir, &apps_dir(config))?;
    println!("Installed {name} (unverified). The bot serves it within seconds: `/app {name}`.");
    Ok(())
}

fn add_git(config: &Config, url: &str, rev: &str, path: &str, yes: bool) -> anyhow::Result<()> {
    if rev.len() < 7 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("--rev must be a commit id (a branch or tag can move)");
    }
    let work = tempfile::tempdir()?;
    let git = |args: &[&str]| -> anyhow::Result<()> {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(work.path())
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .context("cannot run git")?;
        if !status.success() {
            bail!("git {} failed", args.first().copied().unwrap_or_default());
        }
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&["remote", "add", "origin", url])?;
    git(&["fetch", "-q", "--depth", "1", "origin", rev])?;
    git(&["checkout", "-q", "FETCH_HEAD"])?;
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(work.path())
        .args(["rev-parse", "HEAD"])
        .output()?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !head.starts_with(&rev.to_ascii_lowercase()) {
        bail!("the repository gave commit {head}, not {rev}");
    }
    let dir = work.path().join(path);
    if !dir.starts_with(work.path()) || path.contains("..") {
        bail!("--path must stay inside the repository");
    }
    add_local(config, &dir, yes)
}

async fn update(config: &Config, only: Option<&str>, yes: bool) -> anyhow::Result<()> {
    let dir = apps_dir(config);
    let index = fetch_index(config).await?;
    let client = client()?;
    let mut any = false;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("No app is installed.");
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || only.is_some_and(|o| o != name) {
            continue;
        }
        let Some(installed) = okbot_apps::registry::installed(&entry.path()) else {
            if only.is_some() {
                bail!("`{name}` is not from the registry (local apps are added again)");
            }
            continue;
        };
        any = true;
        let Some(latest) = index.apps.iter().find(|a| a.name == name) else {
            println!("{name}: no longer in the registry; consider `okbot app remove {name}`");
            continue;
        };
        let yanked_here = index
            .apps
            .iter()
            .find(|a| a.name == name && Some(&a.version) == installed.version.as_ref())
            .and_then(|a| a.yanked.clone());
        let marker = entry.path().join(okbot_apps::YANKED_FILE);
        if let Some(reason) = &yanked_here {
            std::fs::write(&marker, reason)?;
            println!(
                "{name} {}: YANKED ({reason})",
                installed.version.as_deref().unwrap_or("?")
            );
        }
        if installed.version.as_ref() == Some(&latest.version)
            && installed.commit.as_ref() == Some(&index.commit)
        {
            println!("{name} {} is up to date", latest.version);
            continue;
        }
        if let Err(why) = okbot_apps::registry::installable(latest) {
            println!("{name}: {} cannot be installed ({why})", latest.version);
            continue;
        }
        let current = okbot_apps::check_folder(&entry.path(), okbot_core::AppSource::Registry)
            .map(|m| m.info.permissions)
            .unwrap_or_default();
        let new: Vec<okbot_core::AppPermission> = permissions_of(&latest.permissions)
            .into_iter()
            .filter(|p| !current.contains(p))
            .collect();
        if !new.is_empty() {
            println!("{name} {} asks for more:", latest.version);
            if !grant(&name, "registry", &new, yes)? {
                println!("{name}: kept the installed version");
                continue;
            }
        }
        okbot_apps::registry::install(&client, &index, latest, &dir).await?;
        println!("{name}: updated to {}", latest.version);
    }
    if !any {
        println!("No registry app is installed.");
    }
    Ok(())
}

async fn dev(config: &Config, dir: &std::path::Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(dir.join(okbot_apps::MANIFEST))
        .with_context(|| format!("no {} in {}", okbot_apps::MANIFEST, dir.display()))?;
    let name = okbot_apps::Manifest::parse(&text, okbot_core::AppSource::Local)?
        .info
        .name;
    println!(
        "Serving {} as `{name}` (unverified); edits are copied in. Ctrl-C stops.",
        dir.display()
    );
    let mut last = None;
    loop {
        let stamp = newest(dir);
        if stamp != last {
            last = stamp;
            match okbot_apps::registry::install_local(dir, &apps_dir(config)) {
                Ok(_) => println!("{}: copied", jiff::Zoned::now().strftime("%H:%M:%S")),
                Err(error) => println!("{}: {error}", jiff::Zoned::now().strftime("%H:%M:%S")),
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
    }
}

/// The newest change time under `dir`.
fn newest(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).ok()?.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            }
            if let Ok(t) = meta.modified() {
                newest = newest.max(Some(t));
            }
        }
    }
    newest
}

fn index(
    dir: &std::path::Path,
    commit: &str,
    base: &str,
    yanked: &[String],
    out: &std::path::Path,
) -> anyhow::Result<()> {
    let mut marks = std::collections::BTreeMap::new();
    for y in yanked {
        let (app, reason) = y
            .split_once('=')
            .with_context(|| format!("--yank {y}: write name@version=reason"))?;
        marks.insert(app.to_string(), reason.to_string());
    }
    let generated = jiff::Timestamp::now().to_string();
    let index = okbot_apps::registry::generate(dir, base, commit, &generated, &marks)?;
    let mut text = serde_json::to_string_pretty(&index)?;
    text.push('\n');
    std::fs::write(out, text)?;
    println!("Wrote {} with {} app(s).", out.display(), index.apps.len());
    Ok(())
}

fn sign(index: &std::path::Path, key: Option<&std::path::Path>) -> anyhow::Result<()> {
    use base64::Engine as _;
    let pkcs8 = match key {
        Some(path) => {
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?
        }
        None => {
            let value = std::env::var("OKBOT_APPS_SIGNING_KEY")
                .context("give --key or set OKBOT_APPS_SIGNING_KEY")?;
            base64::engine::general_purpose::STANDARD
                .decode(value.trim())
                .context("OKBOT_APPS_SIGNING_KEY is not base64")?
        }
    };
    let bytes = std::fs::read(index)?;
    // What would be published must parse, or installs would all fail.
    okbot_apps::registry::parse(&bytes)?;
    let signature = okbot_apps::registry::sign(&bytes, &pkcs8)?;
    let out = std::path::PathBuf::from(format!("{}.sig", index.display()));
    std::fs::write(&out, format!("{signature}\n"))?;
    println!(
        "Wrote {} (public key {}).",
        out.display(),
        okbot_apps::registry::public_of(&pkcs8)?
    );
    Ok(())
}

fn keygen(out: &std::path::Path) -> anyhow::Result<()> {
    use std::io::Write as _;
    let (pkcs8, public) = okbot_apps::registry::keygen()?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(out)
        .with_context(|| format!("cannot create {} (it must not exist)", out.display()))?;
    file.write_all(&pkcs8)?;
    println!(
        "Secret key: {} (keep it offline; back it up).",
        out.display()
    );
    println!("Public key: {public}");
    println!("For a GitHub Actions secret: base64 -w0 {}", out.display());
    Ok(())
}

fn list(config: &Config) {
    let catalog = okbot_apps::Catalog::new(Some(config.dir.join("apps")));
    println!("Apps of {} · {}", config.bot.name, address(config));
    for app in catalog.apps() {
        let rule = config.apps.rules.get(&app.name);
        let mut notes = Vec::new();
        if rule.is_some_and(|r| r.disabled) {
            notes.push("disabled".to_string());
        }
        if let Some(r) = rule.filter(|r| !r.roles.is_empty()) {
            notes.push(format!("roles: {}", r.roles.join(", ")));
        }
        if let Some(r) = rule.filter(|r| !r.rooms.is_empty()) {
            notes.push(format!("{} room(s)", r.rooms.len()));
        }
        let permissions: Vec<String> = app.permissions.iter().map(ToString::to_string).collect();
        println!(
            "  {:<16} {:<10} {:<20} {}{}",
            app.name,
            app.version,
            app.source.as_str(),
            permissions.join(" "),
            if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join("; "))
            }
        );
    }
}

fn info(config: &Config, name: &str) -> anyhow::Result<()> {
    let catalog = okbot_apps::Catalog::new(Some(config.dir.join("apps")));
    let Some(app) = catalog.get(name) else {
        bail!("no app `{name}`; `okbot app list` shows them");
    };
    let m = &app.manifest;
    println!(
        "{} ({}) {} · {}",
        m.info.title(None),
        m.info.name,
        m.info.version,
        m.info.source.as_str()
    );
    if let Some(d) = m
        .description
        .get("en")
        .or_else(|| m.description.values().next())
    {
        println!("{d}");
    }
    println!("\nIt may:");
    let granted = config
        .apps
        .rules
        .get(name)
        .and_then(|r| r.permissions.as_ref());
    for p in &m.info.permissions {
        let off = granted.is_some_and(|g| !g.contains(&p.to_string()));
        println!(
            "  - {}{}",
            p.describe(),
            if off { "  (not granted here)" } else { "" }
        );
    }
    println!("\nChat opens: `/app {name}`");
    for v in &m.info.views {
        println!(
            "  `/app {name} {}`  {}",
            v.path,
            okbot_core::localized(&v.title, None, "")
        );
    }
    Ok(())
}

async fn sessions(config: &Config, revoke: Option<&str>, all: bool) -> anyhow::Result<()> {
    let store = crate::owner::store(config).await?;
    if all || revoke.is_some() {
        let person = revoke.map(|r| {
            if r.starts_with("person:") || r.contains('/') {
                r.to_string()
            } else {
                format!("person:{r}")
            }
        });
        let n = store
            .end_app_sessions(None, person.as_deref())
            .await
            .context("cannot end the sessions")?;
        println!("Signed out {n} browser(s).");
        return Ok(());
    }
    let list = store.app_sessions(None, jiff::Timestamp::now()).await?;
    if list.is_empty() {
        println!("Nobody is signed in to the apps of {}.", config.bot.name);
        return Ok(());
    }
    let tz = config
        .bot
        .timezone
        .as_deref()
        .and_then(|z| jiff::tz::TimeZone::get(z).ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    for e in list {
        let until = e.expires_at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M");
        println!("  {}  {:<32} until {until}", &e.handle[..8], e.person);
    }
    println!("\n`okbot app sessions --revoke <person>` signs one person out; `--all` everyone.");
    Ok(())
}
