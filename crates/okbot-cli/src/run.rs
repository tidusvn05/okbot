//! `okbot run`: the bot in the foreground (what the service runs).

use okbot_core::CancellationToken;

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
    let runtime = tokio::spawn(bot.runtime.run(stop.child_token()));
    shutdown_signal().await;
    tracing::info!("shutting down");
    stop.cancel();
    runtime.await??;
    infra.cancel();
    let _ = retention.await;
    Ok(())
}
