//! `app.toml` and the rules every app's files must follow (ADR 0019).

use std::collections::BTreeMap;

use okbot_core::{AppInfo, AppPermission, AppSource, AppView, valid_app_name};
use serde::Deserialize;

use crate::Error;

/// The manifest file of an app.
pub const MANIFEST: &str = "app.toml";
/// The page an app starts on.
pub const ENTRY: &str = "index.html";
/// The App API major version this build serves.
pub const API_VERSION: u32 = 1;
/// The largest file of an app.
pub const MAX_FILE: u64 = 2 * 1024 * 1024;
/// All the files of an app together.
pub const MAX_APP: u64 = 10 * 1024 * 1024;
/// Files per app.
pub const MAX_FILES: usize = 200;

/// File types an app may contain, by extension, with the type they are served as.
pub const FILE_TYPES: [(&str, &str); 12] = [
    ("html", "text/html; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("mjs", "text/javascript; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("json", "application/json"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("woff2", "font/woff2"),
    ("txt", "text/plain; charset=utf-8"),
    ("toml", "text/plain; charset=utf-8"),
    ("md", "text/plain; charset=utf-8"),
];

/// The type a file is served as; `None` for files an app may not contain.
pub fn content_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    FILE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t)
}

/// Whether `path` (relative, `/`-separated) is a file name an app may serve: no `..`, no hidden
/// files, no backslashes, an allowed type.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && !path.contains('\\')
        && path.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        && content_type(path).is_some()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Text {
    One(String),
    ByLanguage(BTreeMap<String, String>),
}

impl Text {
    fn into_map(self) -> BTreeMap<String, String> {
        match self {
            Self::One(s) => BTreeMap::from([("en".to_string(), s)]),
            Self::ByLanguage(m) => m,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewFile {
    path: String,
    title: Option<Text>,
    #[serde(default)]
    params: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    name: String,
    title: Text,
    version: String,
    api: u32,
    #[serde(default)]
    okbot: Option<String>,
    #[serde(default)]
    description: Option<Text>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default, rename = "view")]
    views: Vec<ViewFile>,
}

/// A parsed, checked manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// What the runtime needs.
    pub info: AppInfo,
    /// One paragraph by language.
    pub description: BTreeMap<String, String>,
    /// An icon file of the app.
    pub icon: Option<String>,
    /// SPDX license expression.
    pub license: Option<String>,
    /// The lowest okbot version it needs (`>=0.15`), as written.
    pub okbot: Option<String>,
}

fn check_text(what: &str, map: &BTreeMap<String, String>, max: usize) -> Result<(), String> {
    for (lang, text) in map {
        if lang.is_empty()
            || lang.len() > 8
            || !lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("{what}: `{lang}` is not a language code"));
        }
        if text.trim().is_empty() || text.chars().count() > max {
            return Err(format!("{what} ({lang}): 1 to {max} characters"));
        }
    }
    Ok(())
}

/// Checks `>=X.Y` or `>=X.Y.Z` against this build's version.
fn check_okbot(requirement: &str) -> Result<(), String> {
    let wanted = requirement
        .trim()
        .strip_prefix(">=")
        .ok_or_else(|| format!("okbot = \"{requirement}\": write \">=0.15\""))?;
    let parse = |v: &str| -> Option<Vec<u64>> {
        let core = v.trim().split(['-', '+']).next()?;
        let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
        parts.filter(|p| (2..=3).contains(&p.len()))
    };
    let mut want =
        parse(wanted).ok_or_else(|| format!("okbot = \"{requirement}\": not a version"))?;
    let mut have = parse(env!("CARGO_PKG_VERSION")).unwrap_or_default();
    want.resize(3, 0);
    have.resize(3, 0);
    if have < want {
        return Err(format!(
            "needs okbot {wanted} or newer (this is {})",
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

impl Manifest {
    /// Parses and checks `app.toml`.
    pub fn parse(text: &str, source: AppSource) -> Result<Self, Error> {
        let file: ManifestFile =
            toml::from_str(text).map_err(|e| Error::Manifest(e.message().to_string()))?;
        Self::check(file, source).map_err(Error::Manifest)
    }

    fn check(file: ManifestFile, source: AppSource) -> Result<Self, String> {
        if !valid_app_name(&file.name) {
            return Err(format!(
                "name `{}`: a-z, 0-9 and `-`, starting with a letter, at most {}",
                file.name,
                okbot_core::MAX_APP_NAME
            ));
        }
        if file.api != API_VERSION {
            return Err(format!(
                "api = {}: this okbot serves App API {API_VERSION}",
                file.api
            ));
        }
        if let Some(req) = &file.okbot {
            check_okbot(req)?;
        }
        if file.version.is_empty()
            || file.version.len() > 32
            || !file
                .version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        {
            return Err(format!("version `{}` is not a version", file.version));
        }
        let title = file.title.into_map();
        check_text("title", &title, 40)?;
        let description = file.description.map(Text::into_map).unwrap_or_default();
        check_text("description", &description, 300)?;
        if let Some(icon) = &file.icon
            && !valid_path(icon)
        {
            return Err(format!("icon `{icon}` is not a file name of the app"));
        }
        let mut permissions = Vec::new();
        for p in &file.permissions {
            let p: AppPermission = p.parse()?;
            if !permissions.contains(&p) {
                permissions.push(p);
            }
        }
        let mut views: Vec<AppView> = Vec::new();
        for v in file.views {
            let ok = v.path.starts_with('/')
                && v.path.len() <= 64
                && v.path[1..].bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'/'
                });
            if !ok {
                return Err(format!(
                    "view `{}`: a path such as /open (a-z, 0-9, -, /)",
                    v.path
                ));
            }
            if views.iter().any(|w| w.path == v.path) {
                return Err(format!("view `{}` is listed twice", v.path));
            }
            for p in &v.params {
                if p.is_empty()
                    || p.len() > 32
                    || !p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    return Err(format!("view `{}`: param `{p}` (a-z and _)", v.path));
                }
            }
            let title = v.title.map(Text::into_map).unwrap_or_default();
            check_text(&format!("view `{}` title", v.path), &title, 40)?;
            views.push(AppView {
                path: v.path,
                title,
                params: v.params,
            });
        }
        if views.len() > 20 {
            return Err("at most 20 views".into());
        }
        Ok(Self {
            info: AppInfo {
                name: file.name,
                title,
                version: file.version,
                permissions,
                views,
                source,
            },
            description,
            icon: file.icon,
            license: file.license,
            okbot: file.okbot,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "shopping-list"
title = { en = "Shopping list", vi = "Danh sách mua sắm" }
version = "0.3.0"
api = 1
okbot = ">=0.1"
permissions = ["me", "tasks:read", "tasks:read"]

[[view]]
path = "/open"
title = "Open items"
params = ["tag"]
"#;

    #[test]
    fn parses_a_manifest() {
        let m = Manifest::parse(GOOD, AppSource::Local).unwrap();
        assert_eq!(m.info.name, "shopping-list");
        assert_eq!(m.info.title["vi"], "Danh sách mua sắm");
        assert_eq!(
            m.info.permissions,
            [AppPermission::Me, AppPermission::TasksRead],
            "duplicates dropped"
        );
        assert_eq!(m.info.views[0].params, ["tag"]);
        assert_eq!(m.info.default_view(), "/open");
    }

    #[test]
    fn refuses_bad_manifests() {
        for (change, says) in [
            (("name = \"shopping-list\"", "name = \"Shop\""), "name"),
            (("api = 1", "api = 2"), "App API"),
            (("okbot = \">=0.1\"", "okbot = \">=99.0\""), "needs okbot"),
            (("okbot = \">=0.1\"", "okbot = \"0.1\""), ">="),
            (("\"tasks:read\"]", "\"shell\"]"), "unknown permission"),
            (("path = \"/open\"", "path = \"open\""), "view"),
            (("params = [\"tag\"]", "params = [\"Tag\"]"), "param"),
            (("api = 1", "api = 1\nscripts = []"), "unknown field"),
        ] {
            let text = GOOD.replacen(change.0, change.1, 1);
            let error = Manifest::parse(&text, AppSource::Local)
                .unwrap_err()
                .to_string();
            assert!(error.contains(says), "{change:?}: {error}");
        }
    }

    #[test]
    fn file_rules() {
        assert!(valid_path("index.html"));
        assert!(valid_path("locales/vi.json"));
        assert!(!valid_path("../secrets.html"));
        assert!(!valid_path(".env.txt"));
        assert!(!valid_path("a//b.js"));
        assert!(!valid_path("run.sh"));
        assert!(!valid_path("app.exe"));
        assert!(!valid_path("a\\b.js"));
        assert_eq!(
            content_type("x/App.JS"),
            Some("text/javascript; charset=utf-8")
        );
    }
}
