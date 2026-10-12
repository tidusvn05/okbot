//! Exit codes shared with okbase's agent contract.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | success |
//! | 1 | error |
//! | 3 | the user must decide or provide something (`--json` prints the question) |
//! | 4 | findings (`doctor` found problems) |

use std::process::ExitCode;

/// An error with an exit code and, for code 3, a question for the user.
#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) code: u8,
    pub(crate) message: String,
    /// For agents: the flag that answers the question.
    pub(crate) flag: Option<&'static str>,
    /// The command already printed its result (for example `doctor --json`).
    pub(crate) reported: bool,
}

impl CliError {
    /// The user must provide `flag` (exit 3).
    pub(crate) fn needs(flag: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
            flag: Some(flag),
            reported: false,
        }
    }

    /// Problems were found (exit 4).
    pub(crate) fn findings(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
            flag: None,
            reported: false,
        }
    }

    /// Problems were found and the command already printed its report (exit 4, no more output).
    pub(crate) fn reported_findings(message: impl Into<String>) -> Self {
        Self {
            reported: true,
            ..Self::findings(message)
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Prints `error` and maps it to an exit code; JSON when `json`.
pub(crate) fn report(error: &anyhow::Error, json: bool) -> ExitCode {
    let (code, flag, reported) = error
        .downcast_ref::<CliError>()
        .map_or((1, None, false), |e| (e.code, e.flag, e.reported));
    if reported && json {
        return ExitCode::from(code);
    }
    if json {
        let kind = match code {
            3 => "needs_input",
            4 => "findings",
            _ => "error",
        };
        let body = serde_json::json!({
            "ok": false,
            "error": { "code": kind, "message": format!("{error:#}"), "flag": flag },
        });
        println!("{body}");
    } else {
        eprintln!("error: {error:#}");
    }
    ExitCode::from(code)
}
