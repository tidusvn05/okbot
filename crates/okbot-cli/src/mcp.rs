//! `okbot mcp` (ADR 0012): the `[[mcp]]` servers, presets for common services, and connecting
//! OAuth accounts from the terminal.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use okbot_config::{Config, ConfigEditor, McpAuth, McpCredentials};
use okbot_core::{ConversationStore, Secret, SystemClock};
use okbot_runtime::oauth::{Connector, SecretKey};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// Where the provider sends the browser back to after `okbot mcp login`. Registered with every
/// provider next to the bot's own `/oauth/callback`.
pub(crate) const TERMINAL_REDIRECT: &str = "http://127.0.0.1:47823/oauth/callback";
const TERMINAL_PORT: u16 = 47823;
/// The key that seals stored credentials, in the state directory.
const KEY_FILE: &str = "secrets.key";

/// `okbot mcp` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum McpCommand {
    /// The `[[mcp]]` servers and, for OAuth ones, whose accounts are connected.
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Add a reviewed `[[mcp]]` block for a common service: notion, google-workspace, linear.
    Add {
        /// The preset.
        #[arg(value_parser = ["notion", "google-workspace", "linear"])]
        preset: String,
        /// The server's name in okbot (default: the preset's).
        #[arg(long)]
        name: Option<String>,
        /// Another address (google-workspace: where your workspace-mcp listens).
        #[arg(long)]
        url: Option<String>,
        /// One account for everyone (an administrator connects it) instead of one per person.
        #[arg(long)]
        shared: bool,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Connect an account of an OAuth server from the terminal: open the link, then paste the
    /// address the browser ends on (or let it come back to this machine).
    Login {
        /// The `[[mcp]]` server.
        server: String,
        /// Whose account: `person:<id>` or an account such as `discord:main/1234` (default on a
        /// personal bot: the owner; ignored for shared credentials).
        #[arg(long = "as", value_name = "WHO")]
        who: Option<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// Forget an account of an OAuth server.
    Logout {
        /// The `[[mcp]]` server.
        server: String,
        /// Whose account (as for `login`).
        #[arg(long = "as", value_name = "WHO")]
        who: Option<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
}

/// The sealing key in `state/secrets.key`, created (mode 0600) when missing.
pub(crate) fn secret_key(config: &Config) -> anyhow::Result<SecretKey> {
    let state = config.state_dir();
    std::fs::create_dir_all(&state)
        .with_context(|| format!("cannot create {}", state.display()))?;
    let path = state.join(KEY_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let bytes: [u8; SecretKey::LEN] = bytes.try_into().map_err(|_| {
                anyhow::anyhow!(
                    "{} is not a {}-byte key; move it away to start over (connected accounts are lost)",
                    path.display(),
                    SecretKey::LEN
                )
            })?;
            Ok(SecretKey::new(&Secret::new(bytes)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            match write_key(&path, key.expose()) {
                Ok(()) => Ok(SecretKey::new(&key)),
                // The bot and the terminal started together: use the key the other one wrote.
                Err(_) if path.exists() => secret_key(config),
                Err(error) => Err(error),
            }
        }
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_key(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    std::io::Write::write_all(&mut file, bytes)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// The bot's own callback address: `[http] public_url`, else the loopback `[http] listen`.
pub(crate) fn link_redirect(config: &Config) -> Option<String> {
    if let Some(public) = &config.http.public_url {
        return Some(format!("{public}/oauth/callback"));
    }
    let listen: std::net::SocketAddr = config.http.listen.as_deref()?.parse().ok()?;
    let host = if listen.ip().is_unspecified() {
        std::net::IpAddr::from([127, 0, 0, 1])
    } else {
        listen.ip()
    };
    Some(format!(
        "http://{}/oauth/callback",
        std::net::SocketAddr::new(host, listen.port())
    ))
}

/// The OAuth connector of a bot with `auth = "oauth"` servers, storing in `store`.
pub(crate) fn connector(
    config: &Config,
    store: Arc<dyn ConversationStore>,
) -> anyhow::Result<Option<Arc<Connector>>> {
    if !config.mcp.iter().any(|m| m.auth == McpAuth::Oauth) {
        return Ok(None);
    }
    Ok(Some(Arc::new(Connector::new(
        store,
        secret_key(config)?,
        Arc::new(SystemClock),
        link_redirect(config),
        vec![TERMINAL_REDIRECT.to_string()],
        &config.bot.name,
    ))))
}

fn load(name: Option<&str>) -> anyhow::Result<Config> {
    let dir = crate::registry::resolve(name)?;
    Ok(Config::load(&dir)?)
}

/// The subject (`person:<id>`, an account, or `shared`) of `server` for `who`.
fn subject(
    config: &Config,
    server: &okbot_config::McpSection,
    who: Option<&str>,
) -> anyhow::Result<String> {
    if server.credentials == McpCredentials::Shared {
        return Ok(okbot_runtime::oauth::SHARED.to_string());
    }
    let personal = config.bot.profile == okbot_config::BotProfile::Personal;
    let who = match who {
        Some(who) => who.to_string(),
        None if personal => "person:owner".to_string(),
        None => bail!(
            "`{}` connects each person's own account: say whose with --as person:<id> or --as <platform:instance/user-id>",
            server.name
        ),
    };
    if let Some(id) = who.strip_prefix("person:") {
        if personal && id != "owner" {
            bail!("a personal bot connects the owner's accounts: use --as person:owner");
        }
        if !personal && !config.people.contains_key(id) {
            bail!("`{who}` is not in [people] (`okbot people link`)");
        }
        return Ok(who);
    }
    let (channel, user) = okbot_config::parse_account(&who).map_err(anyhow::Error::msg)?;
    let account = format!("{channel}/{user}");
    // A linked account's credentials belong to its person, as in chat.
    let person = config
        .people
        .iter()
        .find(|(_, p)| p.accounts.contains(&account))
        .map(|(id, _)| format!("person:{id}"));
    if personal && person.is_none() {
        bail!("on a personal bot accounts are the owner's: use --as person:owner");
    }
    Ok(person.unwrap_or(account))
}

fn oauth_server<'a>(
    config: &'a Config,
    name: &str,
) -> anyhow::Result<&'a okbot_config::McpSection> {
    let Some(server) = config.mcp.iter().find(|m| m.name == name) else {
        bail!("no [[mcp]] `{name}`; `okbot mcp list` shows them");
    };
    if server.auth != McpAuth::Oauth {
        bail!("`{name}` does not use OAuth (auth = \"oauth\")");
    }
    Ok(server)
}

pub(crate) async fn run(command: McpCommand) -> anyhow::Result<()> {
    match command {
        McpCommand::List { name } => list(&load(name.as_deref())?).await,
        McpCommand::Add {
            preset,
            name,
            url,
            shared,
            bot,
        } => add(&preset, name, url.as_deref(), shared, bot.as_deref()),
        McpCommand::Login { server, who, bot } => {
            login(&load(bot.as_deref())?, &server, who.as_deref()).await
        }
        McpCommand::Logout { server, who, bot } => {
            let config = load(bot.as_deref())?;
            let section = oauth_server(&config, &server)?;
            let subject = subject(&config, section, who.as_deref())?;
            let store: Arc<dyn ConversationStore> = Arc::new(crate::owner::store(&config).await?);
            if store
                .delete_sealed("oauth-token", &server, &subject)
                .await?
            {
                eprintln!("✓ Forgot the `{server}` account of {subject}");
            } else {
                bail!("{subject} has no `{server}` account connected");
            }
            Ok(())
        }
    }
}

async fn list(config: &Config) -> anyhow::Result<()> {
    if config.mcp.is_empty() {
        println!("no [[mcp]] servers; `okbot mcp add notion` adds one");
        return Ok(());
    }
    let connected = if config.mcp.iter().any(|m| m.auth == McpAuth::Oauth) {
        let store: Arc<dyn ConversationStore> = Arc::new(crate::owner::store(config).await?);
        store.sealed_entries("oauth-token").await?
    } else {
        Vec::new()
    };
    for m in &config.mcp {
        let place = if m.command.is_empty() {
            m.url.clone()
        } else {
            format!("command {}", m.command.join(" "))
        };
        let auth = match (m.auth, m.credentials) {
            (McpAuth::Oauth, McpCredentials::Person) => "oauth, each person's account",
            (McpAuth::Oauth, McpCredentials::Shared) => "oauth, one shared account",
            (McpAuth::Signed, _) => "signed",
            (McpAuth::Bearer, _) if m.command.is_empty() => "bearer",
            _ => "local",
        };
        println!("{:<16} {place}  ({auth})", m.name);
        if m.auth == McpAuth::Oauth {
            let who: Vec<&str> = connected
                .iter()
                .filter(|e| e.server == m.name)
                .map(|e| e.subject.as_str())
                .collect();
            if who.is_empty() {
                println!(
                    "{:<16} no account connected yet (`/connect {}` or `okbot mcp login {}`)",
                    "", m.name, m.name
                );
            } else {
                println!("{:<16} connected: {}", "", who.join(", "));
            }
        }
    }
    Ok(())
}

/// The `[[mcp]]` block of a preset.
fn preset_block(
    preset: &str,
    name: &str,
    url: Option<&str>,
    shared: bool,
) -> anyhow::Result<String> {
    let (default_url, comment) = match preset {
        "notion" => ("https://mcp.notion.com/mcp", "Notion's hosted MCP server"),
        "google-workspace" => (
            "http://127.0.0.1:8000/mcp",
            "Gmail, Drive, Docs, Sheets, Calendar through workspace-mcp in OAuth 2.1 mode \
             (docs/services.md)",
        ),
        "linear" => ("https://mcp.linear.app/mcp", "Linear's hosted MCP server"),
        other => bail!("unknown preset `{other}`"),
    };
    let url = url.unwrap_or(default_url);
    let credentials = if shared {
        "credentials = \"shared\"            # an administrator connects one account for everyone\n"
    } else {
        "# Each person connects their own account with /connect in a direct chat.\n"
    };
    Ok(format!(
        "\n[[mcp]]                              # {comment}\n\
         name = \"{name}\"\n\
         url = \"{url}\"\n\
         auth = \"oauth\"\n\
         {credentials}\
         confirm = \"destructive\"              # sending, sharing and deleting wait for approval\n"
    ))
}

fn add(
    preset: &str,
    name: Option<String>,
    url: Option<&str>,
    shared: bool,
    bot: Option<&str>,
) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(bot)?;
    let name = name.unwrap_or_else(|| match preset {
        "google-workspace" => "google".to_string(),
        other => other.to_string(),
    });
    let mut editor = ConfigEditor::open(&dir)?;
    editor.append(&preset_block(preset, &name, url, shared)?)?;
    let config = editor.save()?;
    eprintln!("✓ Added [[mcp]] `{name}` to okbot.toml");
    if link_redirect(&config).is_none() {
        eprintln!(
            "  People connect accounts with `/connect {name}`, which needs [http] listen (and \
             public_url when they are not on this machine); until then use `okbot mcp login {name}`."
        );
    }
    if preset == "google-workspace" {
        eprintln!(
            "  Run workspace-mcp next to the bot first: docs/services.md shows the Google Cloud \
             client and the command."
        );
    }
    eprintln!("  `okbot restart` applies it.");
    Ok(())
}

async fn login(config: &Config, server: &str, who: Option<&str>) -> anyhow::Result<()> {
    let section = oauth_server(config, server)?;
    let subject = subject(config, section, who)?;
    let extra = crate::bot::extra_mcp(config, section)?;
    let store: Arc<dyn ConversationStore> = Arc::new(crate::owner::store(config).await?);
    let connector = connector(config, store)?.context("no OAuth servers")?;
    let url = connector
        .begin_with(
            &extra,
            &subject,
            "the terminal",
            None,
            Some(TERMINAL_REDIRECT),
        )
        .await?;
    println!(
        "Open this address in a browser and allow access to {server} for {subject}:\n\n  {url}\n"
    );
    eprintln!(
        "When the browser ends on a page that does not load (127.0.0.1:{TERMINAL_PORT}), copy \
         that page's address and paste it here. On this machine it completes by itself."
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", TERMINAL_PORT))
        .await
        .ok();
    let address = tokio::select! {
        pasted = read_line() => pasted?,
        Some(target) = callback(listener) => format!("http://127.0.0.1:{TERMINAL_PORT}{target}"),
    };
    let done = connector.complete_url(&address).await?;
    eprintln!("✓ Connected `{}` for {}", done.server, done.subject);
    Ok(())
}

async fn read_line() -> anyhow::Result<String> {
    let mut line = String::new();
    tokio::io::BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await?;
    if line.trim().is_empty() {
        bail!("nothing pasted");
    }
    Ok(line)
}

/// The request target of the first `GET /oauth/callback?…` the listener gets.
async fn callback(listener: Option<tokio::net::TcpListener>) -> Option<String> {
    let listener = listener?;
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let mut buffer = vec![0; 8192];
        let Ok(n) = stream.read(&mut buffer).await else {
            continue;
        };
        let request = String::from_utf8_lossy(&buffer[..n]).to_string();
        let target = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|rest| rest.split(' ').next())
            .filter(|t| t.starts_with("/oauth/callback?"))
            .map(str::to_string);
        let body = if target.is_some() {
            "Done: return to the terminal."
        } else {
            "Not found."
        };
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        if target.is_some() {
            return target;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_configuration() {
        for (preset, shared) in [
            ("notion", false),
            ("google-workspace", true),
            ("linear", false),
        ] {
            let block = preset_block(preset, "svc", None, shared).unwrap();
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(okbot_config::CONFIG_FILE),
                format!("config_version = 1\n[bot]\nname = \"x\"\n{block}"),
            )
            .unwrap();
            let config = Config::load(dir.path()).unwrap();
            let m = &config.mcp[0];
            assert_eq!(m.auth, McpAuth::Oauth, "{preset}");
            assert_eq!(m.confirm, okbot_config::Confirm::Destructive, "{preset}");
            assert_eq!(m.credentials == McpCredentials::Shared, shared, "{preset}");
        }
    }

    #[test]
    fn callback_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let load = |http: &str| {
            std::fs::write(
                dir.path().join(okbot_config::CONFIG_FILE),
                format!("config_version = 1\n[bot]\nname = \"x\"\n{http}"),
            )
            .unwrap();
            link_redirect(&Config::load(dir.path()).unwrap())
        };
        assert_eq!(load(""), None);
        assert_eq!(
            load("[http]\nlisten = \"0.0.0.0:8787\"\n").as_deref(),
            Some("http://127.0.0.1:8787/oauth/callback")
        );
        assert_eq!(
            load("[http]\nlisten = \"127.0.0.1:8787\"\npublic_url = \"https://bot.example.com\"\n")
                .as_deref(),
            Some("https://bot.example.com/oauth/callback")
        );
    }

    #[test]
    fn keys_are_created_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(okbot_config::CONFIG_FILE),
            "config_version = 1\n[bot]\nname = \"x\"\n",
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        secret_key(&config).unwrap();
        let path = config.state_dir().join(KEY_FILE);
        let first = std::fs::read(&path).unwrap();
        assert_eq!(first.len(), SecretKey::LEN);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        secret_key(&config).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            first,
            "an existing key is kept"
        );
        std::fs::write(&path, b"short").unwrap();
        assert!(secret_key(&config).is_err());
    }
}
