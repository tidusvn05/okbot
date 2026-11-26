//! Apps for okbot (ADR 0019): small web pages served by the bot behind chat summaries.
//!
//! An app is an `app.toml` manifest and static files. Built-in apps are embedded in the binary;
//! others are installed by the operator into `<bot>/apps/<name>/`. Nothing from an app runs in
//! the okbot process: its files are served to a sandboxed frame inside okbot's shell page, and
//! everything it reads or changes goes through the runtime's App API.
//!
//! | Part | What |
//! |---|---|
//! | [`Manifest`] | parses and checks `app.toml` |
//! | [`Catalog`] | the built-in and installed apps of a bot; an [`AppDirectory`](okbot_core::AppDirectory) |
//! | [`router`] | the shell, the SDK and app files, with their security headers |
//! | [`registry`] | the signed index, the installer, local apps (ADR 0020) |
#![deny(missing_docs)]

mod catalog;
mod manifest;
pub mod registry;
mod serve;

pub use catalog::{App, Catalog, SOURCE_FILE, YANKED_FILE, builtin, check_folder, source_of};
pub use manifest::{
    API_VERSION, ENTRY, FILE_TYPES, MANIFEST, MAX_APP, MAX_FILE, MAX_FILES, Manifest, content_type,
    valid_path,
};
pub use serve::{SHELL_CSP, frame_csp, router};

/// An app could not be read or installed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `app.toml` is invalid.
    #[error("app.toml: {0}")]
    Manifest(String),
    /// A file breaks the rules.
    #[error("{0}")]
    Files(String),
    /// The registry, its index, signature or a download failed.
    #[error("registry: {0}")]
    Registry(String),
}
