//! Log setup: pretty on a terminal, JSON for services, secrets redacted from every line.

use std::io::{IsTerminal, Write};
use std::sync::{Arc, OnceLock, RwLock};

use okbot_core::{Redactor, Secret};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Secrets to remove from log output, filled once the configuration is loaded.
fn redactor() -> &'static Arc<RwLock<Redactor>> {
    static REDACTOR: OnceLock<Arc<RwLock<Redactor>>> = OnceLock::new();
    REDACTOR.get_or_init(|| Arc::new(RwLock::new(Redactor::new())))
}

/// Adds a secret that must never appear in logs.
#[cfg_attr(
    not(any(
        feature = "zalo",
        feature = "discord",
        feature = "slack",
        feature = "bridge"
    )),
    expect(dead_code, reason = "only platform channels hold secrets")
)]
pub(crate) fn redact(secret: &Secret<String>) {
    redactor()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .add(secret);
}

/// Writes one formatted log record to stderr, with secrets replaced.
struct RedactingStderr(Vec<u8>);

impl Write for RedactingStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for RedactingStderr {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.0);
        let clean = redactor()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .redact(&text)
            .into_owned();
        let _ = std::io::stderr().write_all(clean.as_bytes());
    }
}

struct MakeRedacting;

impl<'a> MakeWriter<'a> for MakeRedacting {
    type Writer = RedactingStderr;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingStderr(Vec::with_capacity(256))
    }
}

/// Logs to stderr. `OKBOT_LOG` (or `RUST_LOG`) overrides `default` (for example `info`).
/// JSON lines when stderr is not a terminal (systemd, containers).
pub(crate) fn init(default: &str) {
    let filter = EnvFilter::try_from_env("OKBOT_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(MakeRedacting)
        .with_target(false);
    let _ = if std::io::stderr().is_terminal() {
        builder.try_init()
    } else {
        builder.json().flatten_event(true).try_init()
    };
}
