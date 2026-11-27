//! `okbot artifact`: see, export and clean up artifacts from the terminal (ADR 0021).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, bail};
use okbot_core::{Artifact, ArtifactLimits, MemoryScope};
use serde_json::{Value, json};

/// `okbot artifact` subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum ArtifactCommand {
    /// List the artifacts of every person and chat, newest change first.
    List {
        /// Bot name; default: the bot in the current directory.
        name: Option<String>,
        /// Print a JSON array (for scripts; the text output may change).
        #[arg(long)]
        json: bool,
    },
    /// Show one artifact and its versions.
    Show {
        /// Artifact id (`a-7k2m9q4x2c` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print the artifact as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write a version's content to a file (default: `<id>.<ext>` here) or to stdout (`-o -`).
    Export {
        /// Artifact id (`a-7k2m9q4x2c` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Version number (`-v 2`); default: the newest.
        #[arg(id = "n", short = 'v', value_name = "N")]
        version: Option<u32>,
        /// Output file, or `-` for stdout.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
        /// Replace the output file if it exists.
        #[arg(long)]
        force: bool,
    },
    /// Delete an artifact with every version.
    #[command(alias = "remove")]
    Rm {
        /// Artifact id (`a-7k2m9q4x2c` or the full id).
        id: String,
        /// Bot name; default: the bot in the current directory.
        #[arg(long)]
        bot: Option<String>,
        /// Print what was removed as JSON.
        #[arg(long)]
        json: bool,
    },
}

impl ArtifactCommand {
    /// Whether errors should be reported as JSON.
    pub(crate) fn json(&self) -> bool {
        match self {
            Self::List { json, .. } | Self::Show { json, .. } | Self::Rm { json, .. } => *json,
            Self::Export { .. } => false,
        }
    }
}

/// The owner folder of an artifact: `artifacts/people/luan/a-…` → `people/luan`.
pub(crate) fn owner_of(a: &Artifact) -> &str {
    a.id.strip_prefix("artifacts/")
        .unwrap_or(&a.id)
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder)
}

/// An artifact as JSON: the contract of `--json` (times in UTC RFC 3339).
fn artifact_json(a: &Artifact) -> Value {
    let versions: Vec<Value> = a
        .versions
        .iter()
        .map(|v| {
            json!({"n": v.n, "at": v.at.to_string(), "by": v.by, "bytes": v.bytes,
                   "sha256": v.sha256, "note": v.note, "pruned": v.pruned})
        })
        .collect();
    json!({
        "id": a.id,
        "short_id": a.short_id(),
        "title": a.title,
        "kind": a.kind.as_str(),
        "summary": a.summary,
        "scope": match a.scope {
            MemoryScope::Room => "room",
            _ => "user",
        },
        "owner": owner_of(a),
        "author": a.author,
        "created": a.created.to_string(),
        "updated": a.updated().to_string(),
        "bytes": a.stored_bytes(),
        "versions": versions,
    })
}

/// `12 KB`, `3.4 MB`.
pub(crate) fn size(bytes: u64) -> String {
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    if bytes >= GB {
        #[expect(clippy::cast_precision_loss, reason = "a rounded size for people")]
        let gb = bytes as f64 / GB as f64;
        format!("{gb:.1} GB")
    } else if bytes >= MB {
        #[expect(clippy::cast_precision_loss, reason = "a rounded size for people")]
        let mb = bytes as f64 / MB as f64;
        format!("{mb:.1} MB")
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn line(a: &Artifact, tz: &jiff::tz::TimeZone) -> String {
    format!(
        "{}  [{}, v{}, {}] {}\n    {}  updated {}",
        a.short_id(),
        a.kind,
        a.latest().n,
        size(a.stored_bytes()),
        a.title,
        owner_of(a),
        a.updated().to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M"),
    )
}

/// Bytes used per owner folder, largest first.
pub(crate) fn usage(all: &[Artifact]) -> Vec<(String, u64)> {
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for a in all {
        *by.entry(owner_of(a).to_string()).or_default() += a.stored_bytes();
    }
    let mut out: Vec<(String, u64)> = by.into_iter().collect();
    out.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    out
}

/// One line on artifacts against their quotas, for `okbot doctor`; `None` when there are none.
pub(crate) fn quota_report(all: &[Artifact], limits: ArtifactLimits) -> Option<(bool, String)> {
    if all.is_empty() {
        return None;
    }
    let total: u64 = all.iter().map(Artifact::stored_bytes).sum();
    let mut text = format!(
        "{} {}, {} of {}",
        all.len(),
        if all.len() == 1 {
            "artifact"
        } else {
            "artifacts"
        },
        size(total),
        size(limits.quota_bot)
    );
    let mut near = total.saturating_mul(10) >= limits.quota_bot.saturating_mul(9);
    if let Some((owner, bytes)) = usage(all).into_iter().next() {
        use std::fmt::Write as _;
        let _ = write!(
            text,
            "; largest {owner}: {} of {}",
            size(bytes),
            size(limits.quota_owner)
        );
        near |= bytes.saturating_mul(10) >= limits.quota_owner.saturating_mul(9);
    }
    if near {
        text.push_str(" (near the quota: `okbot artifact list` and `okbot artifact rm`)");
    }
    Some((!near, text))
}

async fn show(id: &str, bot: Option<&str>, json: bool) -> anyhow::Result<()> {
    let (_, k) = crate::memory::open(bot).await?;
    let a = k
        .all_artifacts()
        .await
        .into_iter()
        .find(|a| a.id == id || a.short_id() == id)
        .with_context(|| format!("no artifact `{id}`; `okbot artifact list` shows them"))?;
    if json {
        println!("{}", artifact_json(&a));
        return Ok(());
    }
    let tz = k.timezone();
    println!("{}", line(&a, tz));
    println!("    id {}", a.id);
    println!("    author {}", a.author);
    if !a.summary.is_empty() {
        println!("    {}", a.summary.replace('\n', "\n    "));
    }
    for v in &a.versions {
        let mut row = format!(
            "  v{}  {}  {}  {}",
            v.n,
            v.at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M"),
            size(v.bytes),
            v.by
        );
        if v.pruned {
            row.push_str("  (pruned)");
        }
        if !v.note.is_empty() {
            row.push_str("  ");
            row.push_str(&v.note);
        }
        println!("{row}");
    }
    Ok(())
}

async fn export(
    id: &str,
    bot: Option<&str>,
    version: Option<u32>,
    output: Option<PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    let (_, k) = crate::memory::open(bot).await?;
    let (a, n, content) = k.open_artifact(id, version).await?;
    let path =
        output.unwrap_or_else(|| PathBuf::from(format!("{}.{}", a.short_id(), a.kind.extension())));
    if path.as_os_str() == "-" {
        std::io::stdout().write_all(content.as_bytes())?;
        return Ok(());
    }
    if path.exists() && !force {
        bail!("{} exists; pass --force to replace it", path.display());
    }
    std::fs::write(&path, content).with_context(|| format!("cannot write {}", path.display()))?;
    eprintln!("wrote {} v{n} to {}", a.short_id(), path.display());
    Ok(())
}

pub(crate) async fn run(command: ArtifactCommand) -> anyhow::Result<()> {
    match command {
        ArtifactCommand::List { name, json } => {
            let (_, k) = crate::memory::open(name.as_deref()).await?;
            let all = k.all_artifacts().await;
            if json {
                println!("{}", Value::Array(all.iter().map(artifact_json).collect()));
                return Ok(());
            }
            if all.is_empty() {
                println!("no artifacts");
            }
            for a in &all {
                println!("{}", line(a, k.timezone()));
            }
            Ok(())
        }
        ArtifactCommand::Show { id, bot, json } => show(&id, bot.as_deref(), json).await,
        ArtifactCommand::Export {
            id,
            bot,
            version,
            output,
            force,
        } => export(&id, bot.as_deref(), version, output, force).await,
        ArtifactCommand::Rm { id, bot, json } => {
            let (_, k) = crate::memory::open(bot.as_deref()).await?;
            let a = k.remove_artifact(&id).await?;
            if json {
                println!("{}", json!({ "removed": a.id }));
            } else {
                println!("removed {} ({})", a.id, a.title);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quota_report_warns_near_a_limit() {
        let a = |owner: &str, bytes: u64| Artifact {
            id: format!("artifacts/{owner}/a-2345678abc"),
            title: "Page".into(),
            kind: okbot_core::ArtifactKind::Html,
            summary: String::new(),
            scope: MemoryScope::User,
            author: String::new(),
            origin: None,
            created: jiff::Timestamp::UNIX_EPOCH,
            versions: vec![okbot_core::ArtifactVersion {
                n: 1,
                at: jiff::Timestamp::UNIX_EPOCH,
                by: String::new(),
                bytes,
                sha256: String::new(),
                note: String::new(),
                pruned: false,
            }],
        };
        let limits = ArtifactLimits {
            quota_owner: 100 << 10,
            quota_bot: 1 << 20,
            ..ArtifactLimits::default()
        };
        assert_eq!(quota_report(&[], limits), None);
        let (ok, text) = quota_report(
            &[a("people/ana", 10 << 10), a("people/bo", 5 << 10)],
            limits,
        )
        .unwrap();
        assert!(ok, "{text}");
        assert!(
            text.starts_with("2 artifacts, 15 KB of 1.0 MB; largest people/ana: 10 KB"),
            "{text}"
        );
        let (ok, text) = quota_report(&[a("people/ana", 95 << 10)], limits).unwrap();
        assert!(!ok && text.contains("near the quota"), "{text}");
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size(0), "0 KB");
        assert_eq!(size(1), "1 KB");
        assert_eq!(size(3 << 20), "3.0 MB");
        assert_eq!(size(1 << 30), "1.0 GB");
    }
}
