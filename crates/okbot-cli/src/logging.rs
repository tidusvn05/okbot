//! Log setup.

use tracing_subscriber::EnvFilter;

/// Logs to stderr. `OKBOT_LOG` (or `RUST_LOG`) overrides `default` (for example `info`).
pub(crate) fn init(default: &str) {
    let filter = EnvFilter::try_from_env("OKBOT_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}
