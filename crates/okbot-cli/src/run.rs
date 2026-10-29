//! `okbot run`: the bot in the foreground (what the service runs).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use okbot_core::CancellationToken;
use okbot_runtime::RuntimeHandle;

/// How often `okbot.toml` is checked for changes.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Arguments of `okbot run`.
#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// Bot name (see `okbot list`); default: the bot in the current directory.
    name: Option<String>,
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// The modification time and length of `okbot.toml`, to notice an edit.
fn stamp(file: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(file).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Reads `okbot.toml` again and applies its access lists. An invalid file changes nothing.
fn reload(dir: &Path, runtime: &RuntimeHandle, why: &str) {
    let lists = okbot_config::Config::load(dir)
        .map_err(anyhow::Error::from)
        .and_then(|config| crate::bot::access_lists(&config));
    match lists {
        Ok(lists) => {
            tracing::info!(
                why,
                admins = lists.admins.len(),
                allow_users = lists
                    .allow_users
                    .as_ref()
                    .map_or(0, std::collections::HashSet::len),
                roles = lists.roles.names.len(),
                "reloaded the access lists ([bot] admins, allow_users, blocked_users, [people], \
                 [roles]); other settings apply after `okbot restart`"
            );
            runtime.reload_access(lists);
        }
        Err(error) => tracing::warn!(
            why,
            %error,
            "okbot.toml is not valid; keeping the current access lists"
        ),
    }
}

/// Reloads the access lists when `okbot.toml` changes, and on SIGHUP, until `stop`. A change is
/// applied once the file has stayed the same for one more check, so an editor that is still
/// writing it is not read half way.
async fn watch(dir: PathBuf, runtime: RuntimeHandle, stop: CancellationToken) {
    let file = dir.join(okbot_config::CONFIG_FILE);
    let mut last = stamp(&file);
    let mut pending = None;
    let mut tick = tokio::time::interval(WATCH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    #[cfg(unix)]
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
    loop {
        #[cfg(unix)]
        let signalled = async {
            match hangup.as_mut() {
                Some(h) => h.recv().await,
                None => std::future::pending().await,
            }
        };
        #[cfg(not(unix))]
        let signalled = std::future::pending::<Option<()>>();
        tokio::select! {
            () = stop.cancelled() => return,
            Some(()) = signalled => {
                last = stamp(&file);
                pending = None;
                reload(&dir, &runtime, "SIGHUP");
            }
            _ = tick.tick() => {
                let now = stamp(&file);
                if now.is_none() || now == last {
                    pending = None;
                } else if pending == Some(now) {
                    last = now;
                    pending = None;
                    reload(&dir, &runtime, "okbot.toml changed");
                } else {
                    pending = Some(now);
                }
            }
        }
    }
}

pub(crate) async fn run(args: RunArgs) -> anyhow::Result<()> {
    crate::logging::init("info,rmcp=warn");
    let dir = crate::registry::resolve(args.name.as_deref())?;
    let config = okbot_config::Config::load(&dir)?;
    let stop = CancellationToken::new();
    // The knowledge endpoint outlives the runtime's grace period, so turns finishing during
    // shutdown can still use their tools.
    let infra = CancellationToken::new();
    let bot = crate::bot::build(&config, infra.clone()).await?;
    tracing::info!(bot = %config.bot.name, dir = %dir.display(), "okbot starting");
    let retention = tokio::spawn(crate::bot::retention(
        bot.store.clone(),
        bot.retention_days,
        stop.child_token(),
    ));
    let watcher = tokio::spawn(watch(dir.clone(), bot.runtime.handle(), stop.child_token()));
    let runtime = tokio::spawn(bot.runtime.run(stop.child_token()));
    shutdown_signal().await;
    tracing::info!("shutting down");
    stop.cancel();
    runtime.await??;
    infra.cancel();
    let _ = retention.await;
    let _ = watcher.await;
    Ok(())
}
