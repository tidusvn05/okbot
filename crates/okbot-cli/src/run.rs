//! `okbot run`: the bot in the foreground (what the service runs).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use okbot_core::CancellationToken;
use okbot_runtime::RuntimeHandle;

/// How often `okbot.toml` is checked for changes.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// In the state directory: the process id of the running bot, written once it handles SIGHUP.
/// `okbot reload` signals only that process, since SIGHUP terminates one that does not handle it
/// (a bot still starting, or one started by an okbot older than its binary on disk).
const RELOAD_PID_FILE: &str = "reload.pid";

/// Whether the process `pid` handles SIGHUP (`okbot reload`), as recorded in `state_dir`.
pub(crate) fn accepts_reload(state_dir: &Path, pid: u32) -> bool {
    std::fs::read_to_string(state_dir.join(RELOAD_PID_FILE))
        .is_ok_and(|text| text.trim() == pid.to_string())
}

/// SIGHUP, which `okbot reload` sends. Registered before anything slow, so the signal never
/// terminates the bot; one sent while it starts is applied once the watcher runs.
#[cfg(unix)]
type Hangup = tokio::signal::unix::Signal;
#[cfg(not(unix))]
type Hangup = ();

#[cfg(unix)]
fn hangup(state_dir: &Path) -> Option<Hangup> {
    let signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .inspect_err(
            |error| tracing::warn!(%error, "cannot handle SIGHUP; `okbot reload` will not work"),
        );
    let signal = signal.ok()?;
    let written = std::fs::create_dir_all(state_dir).and_then(|()| {
        std::fs::write(
            state_dir.join(RELOAD_PID_FILE),
            format!("{}\n", std::process::id()),
        )
    });
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record the process id; `okbot reload` will not work");
    }
    Some(signal)
}

#[cfg(not(unix))]
fn hangup(_state_dir: &Path) -> Option<Hangup> {
    None
}

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
async fn watch(
    dir: PathBuf,
    runtime: RuntimeHandle,
    mut hangup: Option<Hangup>,
    stop: CancellationToken,
) {
    let file = dir.join(okbot_config::CONFIG_FILE);
    let mut last = stamp(&file);
    let mut pending = None;
    let mut tick = tokio::time::interval(WATCH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        #[cfg(unix)]
        let signalled = async {
            match hangup.as_mut() {
                Some(h) => h.recv().await,
                None => std::future::pending().await,
            }
        };
        #[cfg(not(unix))]
        let signalled = {
            let _ = hangup.take();
            std::future::pending::<Option<()>>()
        };
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
    let state_dir = config.state_dir();
    let hangup = hangup(&state_dir);
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
    let watcher = tokio::spawn(watch(
        dir.clone(),
        bot.runtime.handle(),
        hangup,
        stop.child_token(),
    ));
    let runtime = tokio::spawn(bot.runtime.run(stop.child_token()));
    shutdown_signal().await;
    tracing::info!("shutting down");
    stop.cancel();
    runtime.await??;
    infra.cancel();
    let _ = retention.await;
    let _ = watcher.await;
    let _ = std::fs::remove_file(state_dir.join(RELOAD_PID_FILE));
    Ok(())
}
