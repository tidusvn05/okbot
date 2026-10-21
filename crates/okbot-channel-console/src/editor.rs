//! Line editing for an interactive terminal (feature `line-editing`): history, arrow keys, and the
//! bot's answers printed above the line being typed instead of through it.

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor, ExternalPrinter};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};

use crate::{ConsoleChannel, ConsoleOptions};

type Printer = Box<dyn ExternalPrinter + Send>;

/// Output that goes through the editor, which keeps the input line intact.
struct EditorOutput {
    printer: Arc<Mutex<Printer>>,
    pending: Vec<u8>,
}

impl EditorOutput {
    fn print(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned();
        // The editor adds the line break after the text.
        let text = text.strip_suffix('\n').unwrap_or(&text).to_string();
        self.printer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .print(text)
            .map_err(io::Error::other)
    }
}

impl AsyncWrite for EditorOutput {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.pending.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.print())
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.print())
    }
}

/// Whether `line` may be saved: not blank, and not a `:press` with form values (they are never
/// stored).
fn keep_in_history(line: &str) -> bool {
    let line = line.trim();
    let form_values = line.starts_with(":press") && line.contains('=');
    !line.is_empty() && !form_values
}

/// Saves the history, readable by the user only (it holds the conversation).
fn save(editor: &mut DefaultEditor, path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if editor.save_history(path).is_ok() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

impl ConsoleChannel {
    /// Reads the terminal with line editing and `history` (saved after each line), and writes
    /// above the input line. Call it inside a Tokio runtime, for an interactive terminal only.
    pub fn interactive(options: ConsoleOptions, history: Option<PathBuf>) -> io::Result<Self> {
        let mut editor = DefaultEditor::new().map_err(io::Error::other)?;
        if let Some(path) = &history {
            // A missing file is a first start.
            let _ = editor.load_history(path);
        }
        let printer: Printer =
            Box::new(editor.create_external_printer().map_err(io::Error::other)?);
        let output = EditorOutput {
            printer: Arc::new(Mutex::new(printer)),
            pending: Vec::new(),
        };
        let (mut lines, input) = tokio::io::duplex(64 * 1024);
        let runtime = tokio::runtime::Handle::current();
        std::thread::Builder::new()
            .name("okbot-console".into())
            .spawn(move || {
                loop {
                    match editor.readline("> ") {
                        Ok(line) => {
                            if keep_in_history(&line) {
                                let _ = editor.add_history_entry(line.as_str());
                                if let Some(path) = &history {
                                    save(&mut editor, path);
                                }
                            }
                            let sent = runtime.block_on(async {
                                lines.write_all(line.as_bytes()).await?;
                                lines.write_all(b"\n").await
                            });
                            if sent.is_err() {
                                break;
                            }
                        }
                        // Ctrl-C clears the line, as in a shell.
                        Err(ReadlineError::Interrupted) => {}
                        // Ctrl-D, or the terminal is gone: the input ends.
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self::new(
            options,
            Box::new(BufReader::new(input)),
            Box::new(output),
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn form_values_stay_out_of_the_history() {
        assert!(super::keep_in_history("hello"));
        assert!(super::keep_in_history(":press 1"));
        assert!(!super::keep_in_history(" :press 2 code=123456"));
        assert!(!super::keep_in_history("   "));
    }
}
