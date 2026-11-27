//! Apps: web pages served by the bot behind chat summaries (ADR 0019).
//!
//! An app is a manifest and static files. This module holds what the runtime needs to know about
//! installed apps ([`AppInfo`], through the [`AppDirectory`] port) and what a press on an app
//! button returns to the presser ([`PrivateReply`]). Manifests are parsed by `okbot-apps`.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::prompt::PromptButton;

/// The longest app name.
pub const MAX_APP_NAME: usize = 40;

/// Whether `name` is a valid app name: 1 to [`MAX_APP_NAME`] of `a-z 0-9 -`, starting with a
/// letter, not ending with `-`.
pub fn valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_APP_NAME
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What an app may do through the App API (ADR 0019).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AppPermission {
    /// Who is signed in, where from, locale.
    Me,
    /// List and read tasks.
    TasksRead,
    /// Add, change, finish, snooze, pause and resume tasks.
    TasksWrite,
    /// Read notes.
    NotesRead,
    /// Add notes.
    NotesWrite,
    /// Read memory.
    MemoryRead,
    /// Change and forget memory.
    MemoryWrite,
    /// Search and read the knowledge bundle.
    KnowledgeRead,
    /// Reminders, routine firings and automation schedules.
    ScheduleRead,
    /// Automations and their runs (administrators only).
    AutomationsRead,
    /// Connected accounts, never their tokens.
    ConnectionsRead,
    /// One tool of one `[[mcp]]` server, through the gateway.
    Mcp {
        /// The `[[mcp]]` server.
        server: String,
        /// The tool.
        tool: String,
    },
}

impl AppPermission {
    /// Every permission without arguments, as written in manifests.
    pub const FIXED: [(&'static str, Self); 11] = [
        ("me", Self::Me),
        ("tasks:read", Self::TasksRead),
        ("tasks:write", Self::TasksWrite),
        ("notes:read", Self::NotesRead),
        ("notes:write", Self::NotesWrite),
        ("memory:read", Self::MemoryRead),
        ("memory:write", Self::MemoryWrite),
        ("knowledge:read", Self::KnowledgeRead),
        ("schedule:read", Self::ScheduleRead),
        ("automations:read", Self::AutomationsRead),
        ("connections:read", Self::ConnectionsRead),
    ];

    /// A line for people deciding whether to grant it.
    pub fn describe(&self) -> String {
        match self {
            Self::Me => "know who you are and where you opened it".into(),
            Self::TasksRead => "read your tasks and the room's".into(),
            Self::TasksWrite => "add, change and finish tasks".into(),
            Self::NotesRead => "read notes".into(),
            Self::NotesWrite => "add notes".into(),
            Self::MemoryRead => "read what the bot remembers".into(),
            Self::MemoryWrite => "change and forget memories".into(),
            Self::KnowledgeRead => "search and read the knowledge base".into(),
            Self::ScheduleRead => "read reminders and schedules".into(),
            Self::AutomationsRead => "read automations and their runs (admins)".into(),
            Self::ConnectionsRead => "list connected accounts (never their tokens)".into(),
            Self::Mcp { server, tool } => format!("call `{tool}` of the `{server}` server"),
        }
    }
}

impl fmt::Display for AppPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Mcp { server, tool } = self {
            return write!(f, "mcp:{server}/{tool}");
        }
        let name = Self::FIXED
            .iter()
            .find(|(_, p)| p == self)
            .map_or("?", |(name, _)| name);
        f.write_str(name)
    }
}

impl FromStr for AppPermission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some((_, p)) = Self::FIXED.iter().find(|(name, _)| *name == s) {
            return Ok(p.clone());
        }
        if let Some(rest) = s.strip_prefix("mcp:") {
            let ok = |part: &str| {
                !part.is_empty()
                    && part.len() <= 64
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            };
            if let Some((server, tool)) = rest.split_once('/')
                && ok(server)
                && ok(tool)
            {
                return Ok(Self::Mcp {
                    server: server.into(),
                    tool: tool.into(),
                });
            }
            return Err(format!("`{s}`: write mcp:<server>/<tool>"));
        }
        let known: Vec<&str> = Self::FIXED.iter().map(|(name, _)| *name).collect();
        Err(format!(
            "unknown permission `{s}` (one of {}, or mcp:<server>/<tool>)",
            known.join(", ")
        ))
    }
}

/// Where an app comes from (ADR 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AppSource {
    /// Embedded in the binary.
    Builtin,
    /// Installed from a signed registry index.
    Registry,
    /// Copied from a directory or a git commit by the operator: not reviewed.
    Local,
}

impl AppSource {
    /// The name shown in listings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "built-in",
            Self::Registry => "registry",
            Self::Local => "local (unverified)",
        }
    }
}

/// A page of an app that chat may link to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppView {
    /// Path inside the app, `/open`.
    pub path: String,
    /// Title by language code (`en`, `vi`); `en` is the fallback.
    pub title: BTreeMap<String, String>,
    /// Query parameters chat may pass.
    pub params: Vec<String>,
    /// When this view helps, by language, for the agent (design-app-hints §2).
    pub use_when: BTreeMap<String, String>,
}

/// An installed app, as the runtime sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// Unique name ([`valid_app_name`]).
    pub name: String,
    /// Title by language code; `en` is the fallback.
    pub title: BTreeMap<String, String>,
    /// Version from its manifest.
    pub version: String,
    /// What its manifest asks for.
    pub permissions: Vec<AppPermission>,
    /// Pages chat may link to.
    pub views: Vec<AppView>,
    /// Where it comes from.
    pub source: AppSource,
    /// When the app helps, by language, for the agent (design-app-hints §2).
    pub use_when: BTreeMap<String, String>,
}

/// Data the bot's own agent tools list, whose long results point at an app
/// (design-app-hints §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataArea {
    /// To-dos, reminders and routines (`task_list`).
    Tasks,
    /// What the bot remembers (`memory_list`).
    Memory,
    /// Pages and files the agent made (`artifact_list`, ADR 0021).
    Artifacts,
}

/// The app and view a long listing of `area` should point the agent at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLink {
    /// What is listed.
    pub area: DataArea,
    /// The app's name.
    pub app: String,
    /// One of its views.
    pub view: String,
}

/// The title in `locale` (`vi`, `vi-VN`), else English, else the first one, else `fallback`.
pub fn localized<'a>(
    titles: &'a BTreeMap<String, String>,
    locale: Option<&str>,
    fallback: &'a str,
) -> &'a str {
    let lang = locale
        .and_then(|l| l.split(['-', '_']).next())
        .map(str::to_ascii_lowercase);
    lang.and_then(|l| titles.get(&l))
        .or_else(|| titles.get("en"))
        .or_else(|| titles.values().next())
        .map_or(fallback, String::as_str)
}

impl AppInfo {
    /// The app's title in `locale`.
    pub fn title(&self, locale: Option<&str>) -> &str {
        localized(&self.title, locale, &self.name)
    }

    /// The view at `path`.
    pub fn view(&self, path: &str) -> Option<&AppView> {
        self.views.iter().find(|v| v.path == path)
    }

    /// The first view, where chat opens the app when it names none.
    pub fn default_view(&self) -> &str {
        self.views.first().map_or("/", |v| v.path.as_str())
    }
}

/// The apps installed in a bot: built-in, registry and local ones (implemented by `okbot-apps`).
pub trait AppDirectory: Send + Sync {
    /// Every app, by name.
    fn apps(&self) -> Vec<AppInfo>;

    /// One app.
    fn app(&self, name: &str) -> Option<AppInfo> {
        self.apps().into_iter().find(|a| a.name == name)
    }
}

/// What the presser of an app button sees, privately (ADR 0019): a line and, when they may open
/// the app, a link button with their own launch ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateReply {
    /// Plain text, already rendered for the channel.
    pub text: String,
    /// The link to open, at most one.
    pub link: Option<PromptButton>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_app_name("tasks"));
        assert!(valid_app_name("shopping-list2"));
        assert!(!valid_app_name("Tasks"));
        assert!(!valid_app_name("2tasks"));
        assert!(!valid_app_name("tasks-"));
        assert!(!valid_app_name("ta_sks"));
        assert!(!valid_app_name(""));
        assert!(!valid_app_name(&"a".repeat(41)));
    }

    #[test]
    fn permissions_round_trip() {
        for (name, p) in AppPermission::FIXED {
            assert_eq!(name.parse::<AppPermission>().unwrap(), p);
            assert_eq!(p.to_string(), name);
        }
        let mcp: AppPermission = "mcp:ops/restart_service".parse().unwrap();
        assert_eq!(mcp.to_string(), "mcp:ops/restart_service");
        assert!("mcp:ops".parse::<AppPermission>().is_err());
        assert!("mcp:/x".parse::<AppPermission>().is_err());
        assert!("mcp:a b/x".parse::<AppPermission>().is_err());
        assert!("shell".parse::<AppPermission>().is_err());
    }

    #[test]
    fn titles_by_locale() {
        let titles = BTreeMap::from([
            ("en".to_string(), "Tasks".to_string()),
            ("vi".to_string(), "Công việc".to_string()),
        ]);
        assert_eq!(localized(&titles, Some("vi-VN"), "x"), "Công việc");
        assert_eq!(localized(&titles, Some("ja"), "x"), "Tasks");
        assert_eq!(localized(&titles, None, "x"), "Tasks");
        assert_eq!(localized(&BTreeMap::new(), None, "x"), "x");
    }
}
