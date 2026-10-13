//! Writing `okbot.toml` and `.env` for `okbot init`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::dotenv;
use crate::{CONFIG_VERSION, Delivery};

/// What `okbot init` decided.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Bot name.
    pub name: String,
    /// Agent CLIs in fallback order, with the absolute path detected for each.
    pub chain: Vec<(String, Option<PathBuf>)>,
    /// A Zalo channel, if any.
    pub zalo: Option<ZaloInit>,
    /// A Google Chat channel, if any.
    pub gchat: Option<GChatInit>,
    /// A Discord bot: `(instance, token variable)`.
    pub discord: Option<(String, String)>,
    /// A Slack app: `(instance, app-token variable, bot-token variable)`.
    pub slack: Option<(String, String, String)>,
    /// HTTP listen address (webhooks).
    pub http_listen: Option<String>,
    /// IANA timezone.
    pub timezone: Option<String>,
}

/// The Google Chat part of [`InitOptions`].
#[derive(Debug, Clone)]
pub struct GChatInit {
    /// Instance name (`gchat:<instance>`).
    pub instance: String,
    /// Service-account key file, relative to the bot directory.
    pub credentials: PathBuf,
    /// Receiving mode.
    pub delivery: crate::GChatDelivery,
    /// Pub/Sub subscription.
    pub subscription: Option<String>,
    /// Endpoint URL (HTTP delivery).
    pub audience_url: Option<String>,
}

/// The Zalo part of [`InitOptions`].
#[derive(Debug, Clone)]
pub struct ZaloInit {
    /// Instance name (`zalo:<instance>`).
    pub instance: String,
    /// Variable holding the token.
    pub token_env: String,
    /// Receiving mode.
    pub delivery: Delivery,
    /// Public URL (webhook).
    pub public_url: Option<String>,
    /// Variable holding the webhook secret (webhook).
    pub secret_env: Option<String>,
}

fn quoted(value: &str) -> String {
    toml_edit::Value::from(value).to_string().trim().to_string()
}

/// The initial `okbot.toml`, with comments.
pub fn render_initial(o: &InitOptions) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# okbot bot configuration. Reference: docs/design.md §6.2."
    );
    let _ = writeln!(
        s,
        "# Secrets are never written here: they live in .env (or the environment)."
    );
    let _ = writeln!(s, "config_version = {CONFIG_VERSION}\n");
    let _ = writeln!(s, "[bot]");
    let _ = writeln!(s, "name = {}", quoted(&o.name));
    let _ = writeln!(s, "kb = \"kb\"");
    let _ = writeln!(s, "state_dir = \"state\"");
    let _ = writeln!(
        s,
        "admins = []                        # \"zalo:<instance>/<user id>\"; /whoami shows ids"
    );
    match &o.timezone {
        Some(tz) => {
            let _ = writeln!(s, "timezone = {}", quoted(tz));
        }
        None => {
            let _ = writeln!(s, "# timezone = \"Asia/Ho_Chi_Minh\"");
        }
    }
    let _ = writeln!(s, "retention_days = 90\n");

    let chain: Vec<String> = o.chain.iter().map(|(n, _)| quoted(n)).collect();
    let _ = writeln!(s, "[agent]");
    let _ = writeln!(
        s,
        "chain = [{}]               # fallback order",
        chain.join(", ")
    );
    let _ = writeln!(s, "timeout_secs = 180");
    let _ = writeln!(s, "max_concurrent = 2");
    let _ = writeln!(s, "tool_policy = \"web\"                # web | mcp-only");
    let _ = writeln!(
        s,
        "# home = \"agent-home\"              # separate CLI login/quota for this bot"
    );
    for (name, cli) in &o.chain {
        let _ = writeln!(s, "\n[agent.providers.{name}]");
        if let Some(cli) = cli {
            let _ = writeln!(s, "cli = {}", quoted(&cli.to_string_lossy()));
        }
        let _ = writeln!(
            s,
            "# model = \"…\"                      # the CLI's default when unset"
        );
    }

    if let Some(listen) = &o.http_listen {
        s.push_str(&render_http(listen));
    }
    s.push_str(&render_channels(o));
    s
}

/// The `[http]` section listening on `listen`.
pub fn render_http(listen: &str) -> String {
    format!(
        "\n[http]\nlisten = {}            # put a TLS reverse proxy in front\n",
        quoted(listen)
    )
}

/// The `[channels.*]` sections of `o` (also used by `okbot channel add`).
pub fn render_channels(o: &InitOptions) -> String {
    let mut s = String::new();
    if let Some(z) = &o.zalo {
        render_zalo(&mut s, z);
    }
    if let Some(g) = &o.gchat {
        render_gchat(&mut s, g);
    }
    if let Some((instance, token)) = &o.discord {
        let _ = writeln!(s, "\n[channels.discord.{instance}]");
        let _ = writeln!(s, "token_env = {}", quoted(token));
        let _ = writeln!(
            s,
            "message_content = false             # true to answer group messages without a mention"
        );
        let _ = writeln!(s, "reply_on = [\"dm\", \"mention\"]");
    }
    if let Some((instance, app, bot)) = &o.slack {
        let _ = writeln!(s, "\n[channels.slack.{instance}]");
        let _ = writeln!(s, "app_token_env = {}", quoted(app));
        let _ = writeln!(s, "bot_token_env = {}", quoted(bot));
        let _ = writeln!(s, "reply_on = [\"dm\", \"mention\"]");
    }
    s
}

fn render_zalo(s: &mut String, z: &ZaloInit) {
    let _ = writeln!(s, "\n[channels.zalo.{}]", z.instance);
    let _ = writeln!(s, "token_env = {}", quoted(&z.token_env));
    match z.delivery {
        Delivery::Polling => {
            let _ = writeln!(
                s,
                "delivery = \"polling\"               # or \"webhook\" (recommended for production)"
            );
        }
        Delivery::Webhook => {
            let _ = writeln!(s, "delivery = \"webhook\"");
            if let Some(url) = &z.public_url {
                let _ = writeln!(s, "public_url = {}", quoted(url));
            }
            if let Some(secret) = &z.secret_env {
                let _ = writeln!(s, "secret_env = {}", quoted(secret));
            }
        }
    }
    let _ = writeln!(s, "reply_on = [\"dm\", \"mention\"]");
    let _ = writeln!(s, "\n# A room without web access:");
    let _ = writeln!(s, "# [[channels.zalo.{}.rooms]]", z.instance);
    let _ = writeln!(s, "# room = \"<chat id>\"");
    let _ = writeln!(s, "# tool_policy = \"mcp-only\"");
}

fn render_gchat(s: &mut String, g: &GChatInit) {
    let _ = writeln!(s, "\n[channels.gchat.{}]", g.instance);
    let _ = writeln!(
        s,
        "credentials = {}   # service-account key, never commit it",
        quoted(&g.credentials.to_string_lossy())
    );
    match g.delivery {
        crate::GChatDelivery::Pubsub => {
            let _ = writeln!(s, "delivery = \"pubsub\"");
            let _ = writeln!(
                s,
                "subscription = {}",
                quoted(
                    g.subscription
                        .as_deref()
                        .unwrap_or("projects/<project>/subscriptions/<subscription>")
                )
            );
        }
        crate::GChatDelivery::Http => {
            let _ = writeln!(s, "delivery = \"http\"");
            let _ = writeln!(
                s,
                "audience_url = {}",
                quoted(g.audience_url.as_deref().unwrap_or("https://…"))
            );
        }
    }
    let _ = writeln!(s, "reply_on = [\"dm\", \"mention\"]");
}

/// Sets `name=value` in the `.env` file at `path` (created with mode 0600), keeping other lines.
pub fn upsert_env(path: &Path, name: &str, value: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let l = l.trim_start().trim_start_matches("export ");
            !l.starts_with(&format!("{name}="))
        })
        .map(str::to_string)
        .collect();
    lines.push(dotenv::line(name, value));
    let text = lines.join("\n") + "\n";
    write_private(path, &text)
}

/// Removes `name` from the `.env` file at `path`. Returns whether it was there.
pub fn remove_env(path: &Path, name: &str) -> std::io::Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let is_it = |l: &&str| {
        l.trim_start()
            .trim_start_matches("export ")
            .starts_with(&format!("{name}="))
    };
    if !existing.lines().any(|l| is_it(&l)) {
        return Ok(false);
    }
    let kept: Vec<&str> = existing.lines().filter(|l| !is_it(l)).collect();
    let text = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    write_private(path, &text)?;
    Ok(true)
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // An existing file keeps its mode on open; tighten it.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}
