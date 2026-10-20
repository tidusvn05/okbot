//! `okbot send`: posts a notification through the running bot's `[[notify]]` target (ADR 0010),
//! for cron jobs and shell scripts on the same machine.

use std::io::Read as _;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use okbot_config::Config;
use serde_json::{Value, json};

use crate::exit::CliError;

/// `okbot send` arguments.
#[derive(Debug, clap::Args)]
pub(crate) struct SendArgs {
    /// The `[[notify]]` name (`alerts`).
    target: String,
    /// Markdown text; read from stdin when left out or `-`.
    text: Option<String>,
    /// An id for this notification: sending the same id again within an hour does nothing.
    #[arg(long)]
    id: Option<String>,
    /// Bot name; default: the bot in the current directory.
    #[arg(long)]
    bot: Option<String>,
    /// Print the bot's answer as JSON.
    #[arg(long)]
    pub(crate) json: bool,
}

/// `http://<listen>/notify/<name>`, with an "any address" listener reached on loopback.
fn endpoint(listen: &str, name: &str) -> anyhow::Result<String> {
    let mut addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("[http] listen `{listen}` is not an address"))?;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Ok(format!("http://{addr}/notify/{name}"))
}

pub(crate) async fn run(args: SendArgs) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(args.bot.as_deref())?;
    let config = Config::load(&dir)?;
    let Some(route) = config.notify.iter().find(|n| n.name == args.target) else {
        let names: Vec<&str> = config.notify.iter().map(|n| n.name.as_str()).collect();
        bail!(
            "no [[notify]] `{}` in {}{}",
            args.target,
            dir.join(okbot_config::CONFIG_FILE).display(),
            if names.is_empty() {
                String::new()
            } else {
                format!(" (there is {})", names.join(", "))
            }
        );
    };
    let text = match args.text.as_deref() {
        Some(text) if text != "-" => text.to_string(),
        _ => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("cannot read the text from stdin")?;
            text.trim_end().to_string()
        }
    };
    let listen = config
        .http
        .listen
        .as_deref()
        .context("[[notify]] needs [http] listen")?;
    let url = endpoint(listen, &route.name)?;
    let token = config.secret(&route.token_env)?;
    let mut body = json!({ "text": text });
    if let Some(id) = &args.id {
        body["id"] = json!(id);
    }
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?
        .post(&url)
        .bearer_auth(token.expose())
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                anyhow::anyhow!(
                    "the bot is not answering on {listen}; is it running? (`okbot status {}`)",
                    config.bot.name
                )
            } else {
                anyhow::anyhow!("cannot reach the bot: {e}")
            }
        })?;
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if args.json {
        println!("{answer}");
    }
    let sent = answer["sent"].as_array().map_or(0, Vec::len);
    let failed = answer["failed"].as_array().cloned().unwrap_or_default();
    if !args.json {
        if answer["duplicate"] == true {
            eprintln!(
                "✓ Already sent (id {})",
                args.id.as_deref().unwrap_or_default()
            );
        } else if sent > 0 {
            eprintln!("✓ Sent to {sent} chat(s)");
        }
        for f in &failed {
            eprintln!(
                "✗ {}: {}",
                f["to"].as_str().unwrap_or("?"),
                f["error"].as_str().unwrap_or("?")
            );
        }
    }
    if status.is_success() && failed.is_empty() {
        return Ok(());
    }
    let message = match answer["error"].as_str() {
        Some(error) => format!("the bot refused it ({status}): {error}"),
        None => format!(
            "{} of {} chat(s) not reached",
            failed.len(),
            sent + failed.len()
        ),
    };
    Err(CliError {
        code: 1,
        message,
        flag: None,
        reported: args.json,
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_address_is_reached_on_loopback() {
        assert_eq!(
            endpoint("0.0.0.0:8787", "alerts").unwrap(),
            "http://127.0.0.1:8787/notify/alerts"
        );
        assert_eq!(
            endpoint("[::]:8787", "a").unwrap(),
            "http://[::1]:8787/notify/a"
        );
        assert_eq!(
            endpoint("10.0.0.5:80", "a").unwrap(),
            "http://10.0.0.5:80/notify/a"
        );
        assert!(endpoint("localhost:80", "a").is_err());
    }
}
