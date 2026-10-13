//! `okbot kb`: knowledge bundle maintenance.

use anyhow::Context;
use okbot_config::Config;

use crate::exit::CliError;

/// `okbot kb` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum KbCommand {
    /// Re-index changed files.
    Sync {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Check the bundle against the okbase standard (level L2).
    Lint {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Add default knowledge files that are missing (never overwrites your files).
    Upgrade {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
    /// Save a file (PDF, Word, PowerPoint, HTML, markdown, text, image) or a web page into
    /// kb/knowledge/inbox/, searchable at once.
    Add {
        /// A file path or an https:// URL.
        source: String,
        /// Title; default: the file name.
        #[arg(long)]
        title: Option<String>,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
    },
    /// List documents saved to kb/knowledge/inbox/ (from chat or `kb add`), newest first.
    Inbox {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
    },
}

/// Reads a local file or downloads a URL: `(bytes, file name, media type)`.
async fn read_source(source: &str) -> anyhow::Result<(Vec<u8>, String, String)> {
    if source.starts_with("https://") || source.starts_with("http://") {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let response = reqwest::Client::new()
            .get(source)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("cannot download {source}: {}", e.without_url()))?
            .error_for_status()
            .map_err(|e| anyhow::anyhow!("cannot download {source}: {}", e.without_url()))?;
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let name = source
            .split(['?', '#'])
            .next()
            .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
            .unwrap_or("page")
            .to_string();
        let bytes = response.bytes().await?.to_vec();
        return Ok((bytes, name, mime));
    }
    let path = std::path::Path::new(source);
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {source}"))?;
    let name = path
        .file_name()
        .map_or_else(|| source.to_string(), |n| n.to_string_lossy().into_owned());
    Ok((bytes, name, String::new()))
}

async fn add(source: &str, title: Option<String>, bot: Option<&str>) -> anyhow::Result<()> {
    let (_, k) = crate::memory::open(bot).await?;
    let (bytes, name, mime) = read_source(source).await?;
    let stem = std::path::Path::new(&name)
        .file_stem()
        .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
    let is_url = source.contains("://");
    let markdown = std::path::Path::new(&name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        || mime == "text/markdown";
    let mut doc = okbot_knowledge::NewDocument {
        title: title.unwrap_or(stem),
        source: Some(if is_url {
            source.to_string()
        } else {
            name.clone()
        }),
        added_by: "operator".into(),
        ..okbot_knowledge::NewDocument::default()
    };
    if markdown {
        let text = String::from_utf8(bytes).context("the markdown file is not UTF-8")?;
        // Keep the body; the inbox writes its own frontmatter.
        doc.text = match text.strip_prefix("---") {
            Some(rest) => rest
                .split_once("\n---")
                .map_or(text.as_str(), |(_, body)| body)
                .trim_start_matches('\n')
                .to_string(),
            None => text,
        };
    } else {
        let ext = okbot_knowledge::readable_extension(Some(&name), &mime).with_context(|| {
            format!(
                "okbase cannot read `{name}`{}; use PDF, Word, PowerPoint, OpenDocument, RTF, \
                 EPUB, HTML, markdown, text or an image",
                if mime.is_empty() {
                    String::new()
                } else {
                    format!(" ({mime})")
                }
            )
        })?;
        doc.file = Some((bytes, ext));
    }
    let id = k.add_document(doc).await?;
    println!("saved {id}");
    Ok(())
}

fn inbox(name: Option<&str>) -> anyhow::Result<()> {
    let dir = crate::registry::resolve(name)?;
    let config = Config::load(&dir)?;
    let inbox = config.kb_dir().join("knowledge/inbox");
    let mut docs: Vec<(String, String, String)> = std::fs::read_dir(&inbox)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .filter(|p| p.file_name().is_some_and(|n| n != "index.md"))
                .map(|p| {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    let (fm, _) = okbase::core::Frontmatter::split(&text);
                    let stem = p
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    (
                        stem.clone(),
                        fm.get_str("title").unwrap_or(&stem).to_string(),
                        fm.get_str("added_by").unwrap_or("-").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    docs.sort_by(|a, b| b.0.cmp(&a.0));
    if docs.is_empty() {
        println!("the inbox is empty");
    }
    for (stem, title, by) in docs {
        println!("knowledge/inbox/{stem}  {title}  (added by {by})");
    }
    Ok(())
}

fn open(name: Option<&str>) -> anyhow::Result<(Config, okbase::Bundle)> {
    let dir = crate::registry::resolve(name)?;
    let config = Config::load(&dir)?;
    let bundle = okbase::Bundle::open(
        &config.kb_dir(),
        okbase::OpenOptions {
            state_dir: okbase::StateDir::Path(config.state_dir().join("index")),
            ..okbase::OpenOptions::default()
        },
    )
    .with_context(|| format!("cannot open {}", config.kb_dir().display()))?;
    Ok((config, bundle))
}

pub(crate) async fn run(command: KbCommand) -> anyhow::Result<()> {
    match command {
        KbCommand::Add { source, title, bot } => add(&source, title, bot.as_deref()).await?,
        KbCommand::Inbox { name } => inbox(name.as_deref())?,
        KbCommand::Sync { name } => {
            let (_, bundle) = open(name.as_deref())?;
            let stats = bundle.sync()?;
            println!(
                "added {}, updated {}, removed {}",
                stats.added, stats.updated, stats.removed
            );
        }
        KbCommand::Lint { name } => {
            let (_, bundle) = open(name.as_deref())?;
            bundle.sync()?;
            let report = bundle.lint(&okbase::LintConfig::level(okbase::Level::L2))?;
            let json = serde_json::to_value(&report)?;
            if let Some(items) = json["diagnostics"].as_array() {
                for d in items {
                    println!(
                        "{:<7} {:<40} {} {}",
                        d["severity"].as_str().unwrap_or(""),
                        d["path"].as_str().unwrap_or(""),
                        d["code"].as_str().unwrap_or(""),
                        d["message"].as_str().unwrap_or("")
                    );
                }
            }
            println!("{} errors, {} warnings", json["errors"], json["warnings"]);
            if json["errors"].as_u64().unwrap_or(0) > 0 {
                return Err(CliError::findings("the knowledge bundle has lint errors").into());
            }
        }
        KbCommand::Upgrade { name } => {
            let dir = crate::registry::resolve(name.as_deref())?;
            let config = Config::load(&dir)?;
            let written = okbot_knowledge::write_default_bundle(&config.kb_dir())?;
            if written.is_empty() {
                println!("nothing to add: every default file exists");
            }
            for file in written {
                println!("added {}", file.display());
            }
        }
    }
    Ok(())
}
