//! The app registry (ADR 0020): a signed index of reviewed apps, each pinned to a commit with
//! the SHA-256 of every file, and the installer that checks all of it before anything is
//! written.
//!
//! The index is `index.json`; its Ed25519 signature is `index.json.sig` (base64). Keys are
//! raw 32-byte Ed25519 public keys, written `ed25519:<base64>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::Error;
use crate::catalog::{SOURCE_FILE, check_folder};
use crate::manifest::{API_VERSION, MAX_FILE, MAX_FILES, Manifest, valid_path};

/// The index format this build reads.
pub const INDEX_FORMAT: u32 = 1;
/// The largest index accepted.
const MAX_INDEX: usize = 2 * 1024 * 1024;

/// The keys of okbot's own registry (`tidusvn05/okbot-apps`): the current key, then the
/// standby key for rotation. The secret halves are kept offline by the maintainer
/// (`okbot app keygen`, ADR 0020).
pub const OFFICIAL_KEYS: &[&str] = &[
    "ed25519:EZrgaFudD8Rj1/wa+NudP1cp/grZw7q8G0TJL7KRxmo=",
    "ed25519:MW+6hfikbeCRQdg2Yv7eAGLmSGbypxQIfdhtRlwp5TM=",
];

/// The official registry's index.
pub const OFFICIAL_INDEX: &str =
    "https://raw.githubusercontent.com/tidusvn05/okbot-apps/main/index.json";

/// One app in an index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexApp {
    /// App name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// The App API major version it needs.
    pub api: u32,
    /// The lowest okbot version it needs, as in its manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub okbot: Option<String>,
    /// Title by language.
    #[serde(default)]
    pub title: BTreeMap<String, String>,
    /// Description by language.
    #[serde(default)]
    pub description: BTreeMap<String, String>,
    /// What it asks for.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Every file, by path inside the app, with its SHA-256 (hex).
    pub files: BTreeMap<String, String>,
    /// Why this version must not be used, when it was found bad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yanked: Option<String>,
}

/// A registry index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// [`INDEX_FORMAT`].
    pub format: u32,
    /// When it was generated (RFC 3339).
    pub generated: String,
    /// The commit every file is pinned to.
    pub commit: String,
    /// Where the files are: `<base>/<app>/<path>` (a URL with the commit in it).
    pub base: String,
    /// The apps, by name.
    pub apps: Vec<IndexApp>,
}

fn bad(m: impl Into<String>) -> Error {
    Error::Registry(m.into())
}

/// A public key from `ed25519:<base64>`.
pub fn parse_key(key: &str) -> Result<Vec<u8>, Error> {
    let raw = key
        .trim()
        .strip_prefix("ed25519:")
        .ok_or_else(|| bad(format!("key `{key}`: write ed25519:<base64>")))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| bad(format!("key `{key}` is not base64")))?;
    if bytes.len() != 32 {
        return Err(bad(format!("key `{key}` is not a 32-byte Ed25519 key")));
    }
    Ok(bytes)
}

/// Checks `index` (the exact bytes) against its signature (base64) with any of `keys`.
pub fn verify(index: &[u8], signature: &str, keys: &[Vec<u8>]) -> Result<(), Error> {
    if keys.is_empty() {
        return Err(bad(
            "no key to check this registry with: set [apps] registry_keys (the official \
             registry has no signing key in this build yet)",
        ));
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .map_err(|_| bad("the signature is not base64"))?;
    let ok = keys.iter().any(|k| {
        UnparsedPublicKey::new(&ED25519, k)
            .verify(index, &signature)
            .is_ok()
    });
    if ok {
        Ok(())
    } else {
        Err(bad("the index signature does not match any trusted key"))
    }
}

/// Parses a verified index and checks its entries.
pub fn parse(index: &[u8]) -> Result<Index, Error> {
    let index: Index = serde_json::from_slice(index).map_err(|e| bad(format!("index: {e}")))?;
    if index.format != INDEX_FORMAT {
        return Err(bad(format!(
            "index format {} (this okbot reads {INDEX_FORMAT}): upgrade okbot",
            index.format
        )));
    }
    if !index.base.starts_with("https://") || index.base.ends_with('/') {
        return Err(bad("index base must be an https URL without a trailing /"));
    }
    for app in &index.apps {
        if !okbot_core::valid_app_name(&app.name) {
            return Err(bad(format!("index: `{}` is not an app name", app.name)));
        }
        if app.files.len() > MAX_FILES || !app.files.contains_key(crate::MANIFEST) {
            return Err(bad(format!(
                "index: {} lists no manifest or too many files",
                app.name
            )));
        }
        for (path, hash) in &app.files {
            if !valid_path(path) || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(bad(format!("index: {}: bad file entry `{path}`", app.name)));
            }
        }
    }
    Ok(index)
}

/// Whether this build can install `app`: its App API and okbot version; the error says why not.
pub fn installable(app: &IndexApp) -> Result<(), String> {
    if app.api != API_VERSION {
        return Err(format!(
            "needs App API {} (this okbot serves {API_VERSION})",
            app.api
        ));
    }
    if let Some(reason) = &app.yanked {
        return Err(format!("yanked: {reason}"));
    }
    if let Some(req) = &app.okbot {
        // The manifest check knows the comparison; a minimal manifest asks it.
        let probe = format!(
            "name = \"probe\"\ntitle = \"p\"\nversion = \"1\"\napi = {API_VERSION}\nokbot = \"{req}\"\n"
        );
        Manifest::parse(&probe, okbot_core::AppSource::Registry).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

async fn fetch(client: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, Error> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| bad(format!("{url}: {e}")))?;
    if !response.status().is_success() {
        return Err(bad(format!("{url}: HTTP {}", response.status())));
    }
    if response.content_length().is_some_and(|l| l > max as u64) {
        return Err(bad(format!("{url}: larger than {max} bytes")));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| bad(format!("{url}: {e}")))?;
    if bytes.len() > max {
        return Err(bad(format!("{url}: larger than {max} bytes")));
    }
    Ok(bytes.to_vec())
}

/// Fetches `index_url` and `<index_url>.sig`, checks the signature with `keys`, and parses it.
pub async fn fetch_index(
    client: &reqwest::Client,
    index_url: &str,
    keys: &[Vec<u8>],
) -> Result<Index, Error> {
    if !index_url.starts_with("https://") {
        return Err(bad("the registry index must be an https URL"));
    }
    if keys.is_empty() {
        return Err(bad(
            "no key to check this registry with: set [apps] registry_keys (the official \
             registry has no signing key in this build yet)",
        ));
    }
    let bytes = fetch(client, index_url, MAX_INDEX).await?;
    let signature = fetch(client, &format!("{index_url}.sig"), 1024).await?;
    verify(&bytes, &String::from_utf8_lossy(&signature), keys)?;
    parse(&bytes)
}

/// What an installed app came from (the first line of [`SOURCE_FILE`] says `registry`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Installed {
    /// The commit of the index it was installed from.
    pub commit: Option<String>,
    /// Its version then.
    pub version: Option<String>,
}

/// Reads the registry record of an installed app.
pub fn installed(dir: &Path) -> Option<Installed> {
    let text = std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "registry" {
        return None;
    }
    let mut out = Installed::default();
    for line in lines {
        match line.split_once('=') {
            Some(("commit", v)) => out.commit = Some(v.trim().to_string()),
            Some(("version", v)) => out.version = Some(v.trim().to_string()),
            _ => {}
        }
    }
    Some(out)
}

/// Downloads `app` of `index` into `apps_dir/<name>`: every file is checked against its hash
/// and the folder against the file rules and the index's permissions before it replaces the
/// installed one in a single rename. Nothing is written to `apps_dir/<name>` on any failure.
pub async fn install(
    client: &reqwest::Client,
    index: &Index,
    app: &IndexApp,
    apps_dir: &Path,
) -> Result<Manifest, Error> {
    installable(app).map_err(bad)?;
    std::fs::create_dir_all(apps_dir).map_err(|e| bad(format!("{}: {e}", apps_dir.display())))?;
    let staging = apps_dir.join(format!(
        ".staging-{}-{}",
        app.name,
        hex::encode(rand_bytes())
    ));
    let result = stage(client, index, app, &staging).await;
    let manifest = match result {
        Ok(m) => m,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    swap(apps_dir, &app.name, &staging)?;
    Ok(manifest)
}

fn rand_bytes() -> [u8; 6] {
    let mut b = [0u8; 6];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    b
}

async fn stage(
    client: &reqwest::Client,
    index: &Index,
    app: &IndexApp,
    staging: &Path,
) -> Result<Manifest, Error> {
    let root = staging.join(&app.name);
    std::fs::create_dir_all(&root).map_err(|e| bad(e.to_string()))?;
    let max = usize::try_from(MAX_FILE).unwrap_or(usize::MAX);
    for (path, hash) in &app.files {
        let url = format!("{}/{}/{path}", index.base, app.name);
        let bytes = fetch(client, &url, max).await?;
        if sha256(&bytes) != hash.to_ascii_lowercase() {
            return Err(bad(format!(
                "{}/{path}: the file does not match the index",
                app.name
            )));
        }
        let at = root.join(path);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent).map_err(|e| bad(e.to_string()))?;
        }
        std::fs::write(&at, &bytes).map_err(|e| bad(e.to_string()))?;
    }
    let manifest = check_folder(&root, okbot_core::AppSource::Registry)?;
    if manifest.info.name != app.name || manifest.info.version != app.version {
        return Err(bad(format!(
            "{}: the manifest does not match the index",
            app.name
        )));
    }
    let asked: Vec<String> = manifest
        .info
        .permissions
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut listed = app.permissions.clone();
    listed.sort();
    let mut asked_sorted = asked.clone();
    asked_sorted.sort();
    if asked_sorted != listed {
        return Err(bad(format!(
            "{}: the manifest asks for other permissions than the index lists",
            app.name
        )));
    }
    std::fs::write(
        root.join(SOURCE_FILE),
        format!(
            "registry\ncommit={}\nversion={}\n",
            index.commit, app.version
        ),
    )
    .map_err(|e| bad(e.to_string()))?;
    Ok(manifest)
}

/// Puts `staging/<name>` in place of `apps_dir/<name>`.
fn swap(apps_dir: &Path, name: &str, staging: &Path) -> Result<(), Error> {
    let target = apps_dir.join(name);
    let old = apps_dir.join(format!(".old-{name}-{}", hex::encode(rand_bytes())));
    let had = target.exists();
    if had {
        std::fs::rename(&target, &old).map_err(|e| bad(e.to_string()))?;
    }
    if let Err(e) = std::fs::rename(staging.join(name), &target) {
        if had {
            let _ = std::fs::rename(&old, &target);
        }
        let _ = std::fs::remove_dir_all(staging);
        return Err(bad(e.to_string()));
    }
    let _ = std::fs::remove_dir_all(staging);
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Removes an installed app. Built-in apps are not in `apps_dir`; they are turned off with
/// `[apps.<name>] disabled = true`.
pub fn remove(apps_dir: &Path, name: &str) -> Result<bool, Error> {
    if !okbot_core::valid_app_name(name) {
        return Err(bad(format!("`{name}` is not an app name")));
    }
    let dir = apps_dir.join(name);
    if !dir.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir).map_err(|e| bad(e.to_string()))?;
    Ok(true)
}

/// Copies an app folder (`okbot app add ./dir`, ADR 0020 local apps) into `apps_dir/<name>`
/// after the same checks, marked local.
pub fn install_local(source: &Path, apps_dir: &Path) -> Result<Manifest, Error> {
    let manifest = check_folder_named(source)?;
    let name = manifest.info.name.clone();
    std::fs::create_dir_all(apps_dir).map_err(|e| bad(e.to_string()))?;
    let staging = apps_dir.join(format!(".staging-{name}-{}", hex::encode(rand_bytes())));
    let root = staging.join(&name);
    copy_tree(source, &root)?;
    std::fs::write(root.join(SOURCE_FILE), "local\n").map_err(|e| bad(e.to_string()))?;
    // Checked again where it will be served: the copy is what runs.
    if let Err(e) = check_folder(&root, okbot_core::AppSource::Local) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    swap(apps_dir, &name, &staging)?;
    Ok(manifest)
}

/// The manifest of an app folder whatever the folder is called (for copying it in).
fn check_folder_named(source: &Path) -> Result<Manifest, Error> {
    let text = std::fs::read_to_string(source.join(crate::MANIFEST))
        .map_err(|e| bad(format!("{}: {e}", source.join(crate::MANIFEST).display())))?;
    let manifest = Manifest::parse(&text, okbot_core::AppSource::Local)?;
    // The rules on files are checked on a copy named like the app.
    let probe = std::env::temp_dir().join(format!("okbot-app-check-{}", hex::encode(rand_bytes())));
    let named = probe.join(&manifest.info.name);
    let result =
        copy_tree(source, &named).and_then(|()| check_folder(&named, okbot_core::AppSource::Local));
    let _ = std::fs::remove_dir_all(&probe);
    result
}

/// Copies regular files and folders; links and hidden entries are refused by the checks after.
fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(to).map_err(|e| bad(e.to_string()))?;
    for entry in std::fs::read_dir(from).map_err(|e| bad(format!("{}: {e}", from.display())))? {
        let entry = entry.map_err(|e| bad(e.to_string()))?;
        let kind = entry.file_type().map_err(|e| bad(e.to_string()))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            // `.git` and editor files are not part of an app.
            continue;
        }
        if kind.is_symlink() {
            return Err(bad(format!(
                "{}: links are not allowed",
                entry.path().display()
            )));
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &to.join(&name))?;
        } else {
            std::fs::copy(entry.path(), to.join(&name)).map_err(|e| bad(e.to_string()))?;
        }
    }
    Ok(())
}

/// Builds the index of the apps under `apps_root/<name>/` (for the registry's CI), pinned to
/// `commit`, with files at `base/<name>/<path>`. Every app passes the install checks first.
pub fn generate(
    apps_root: &Path,
    base: &str,
    commit: &str,
    generated: &str,
    yanked: &BTreeMap<String, String>,
) -> Result<Index, Error> {
    let mut apps = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(apps_root)
        .map_err(|e| bad(format!("{}: {e}", apps_root.display())))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    dirs.sort();
    for dir in dirs {
        let manifest = check_folder(&dir, okbot_core::AppSource::Registry)?;
        let mut files = BTreeMap::new();
        hash_tree(&dir, "", &mut files)?;
        let key = format!("{}@{}", manifest.info.name, manifest.info.version);
        apps.push(IndexApp {
            yanked: yanked.get(&key).cloned(),
            name: manifest.info.name.clone(),
            version: manifest.info.version.clone(),
            api: API_VERSION,
            okbot: manifest.okbot.clone(),
            title: manifest.info.title.clone(),
            description: manifest.description.clone(),
            permissions: manifest
                .info
                .permissions
                .iter()
                .map(ToString::to_string)
                .collect(),
            files,
        });
    }
    let index = Index {
        format: INDEX_FORMAT,
        generated: generated.to_string(),
        commit: commit.to_string(),
        base: base.trim_end_matches('/').to_string(),
        apps,
    };
    // What the installer would refuse, CI refuses.
    let bytes = serde_json::to_vec(&index).map_err(|e| bad(e.to_string()))?;
    parse(&bytes)?;
    Ok(index)
}

fn hash_tree(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir).map_err(|e| bad(e.to_string()))? {
        let entry = entry.map_err(|e| bad(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefix.is_empty() && name == SOURCE_FILE {
            continue;
        }
        let rel = format!("{prefix}{name}");
        if entry.file_type().map_err(|e| bad(e.to_string()))?.is_dir() {
            hash_tree(&entry.path(), &format!("{rel}/"), out)?;
        } else {
            let bytes = std::fs::read(entry.path()).map_err(|e| bad(e.to_string()))?;
            out.insert(rel, sha256(&bytes));
        }
    }
    Ok(())
}

/// A new signing key: its PKCS#8 bytes (keep them secret) and its public key
/// (`ed25519:<base64>`).
pub fn keygen() -> Result<(Vec<u8>, String), Error> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| bad("cannot generate a key"))?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| bad("bad key"))?;
    let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key().as_ref());
    Ok((pkcs8.as_ref().to_vec(), format!("ed25519:{public}")))
}

/// Signs `index` (its exact bytes) with a PKCS#8 key; the signature is base64.
pub fn sign(index: &[u8], pkcs8: &[u8]) -> Result<String, Error> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| bad("not an Ed25519 PKCS#8 key"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(pair.sign(index).as_ref()))
}

/// The public key of a PKCS#8 key.
pub fn public_of(pkcs8: &[u8]) -> Result<String, Error> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| bad("not an Ed25519 PKCS#8 key"))?;
    Ok(format!(
        "ed25519:{}",
        base64::engine::general_purpose::STANDARD.encode(pair.public_key().as_ref())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_keys_are_valid() {
        for key in OFFICIAL_KEYS {
            assert_eq!(parse_key(key).unwrap().len(), 32, "{key}");
        }
    }

    fn app_dir(root: &Path, name: &str, version: &str, permissions: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("js")).unwrap();
        std::fs::write(
            dir.join(crate::MANIFEST),
            format!(
                "name = \"{name}\"\ntitle = \"T\"\nversion = \"{version}\"\napi = 1\npermissions = [{permissions}]\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join(crate::ENTRY), "<p>hi</p>").unwrap();
        std::fs::write(dir.join("js/app.js"), "console.log(1)").unwrap();
        dir
    }

    #[test]
    fn signatures_and_keys() {
        let (pkcs8, public) = keygen().unwrap();
        assert_eq!(public_of(&pkcs8).unwrap(), public);
        let key = parse_key(&public).unwrap();
        let body = br#"{"format":1}"#;
        let sig = sign(body, &pkcs8).unwrap();
        verify(body, &sig, std::slice::from_ref(&key)).unwrap();
        assert!(
            verify(br#"{"format":2}"#, &sig, std::slice::from_ref(&key)).is_err(),
            "changed bytes"
        );
        let (_, other) = keygen().unwrap();
        assert!(
            verify(body, &sig, &[parse_key(&other).unwrap()]).is_err(),
            "another key"
        );
        assert!(verify(body, &sig, &[]).is_err(), "no key");
        assert!(parse_key("ed25519:abc").is_err());
        assert!(parse_key("rsa:AAAA").is_err());
    }

    #[test]
    fn generated_indexes_parse_and_pin_files() {
        let root = tempfile::tempdir().unwrap();
        app_dir(root.path(), "shop", "1.0.0", "\"me\", \"tasks:read\"");
        let yanked = BTreeMap::from([("shop@1.0.0".to_string(), "leaks".to_string())]);
        let index = generate(
            root.path(),
            "https://raw.example.com/apps/abc/apps/",
            "abc",
            "2026-11-26T00:00:00Z",
            &yanked,
        )
        .unwrap();
        assert_eq!(index.base, "https://raw.example.com/apps/abc/apps");
        let shop = &index.apps[0];
        assert_eq!(shop.files.len(), 3);
        assert_eq!(shop.files["js/app.js"], sha256(b"console.log(1)"));
        assert_eq!(shop.permissions, ["me", "tasks:read"]);
        assert!(installable(shop).unwrap_err().contains("yanked"));
        let bad_index = br#"{"format":1,"generated":"x","commit":"c","base":"http://x","apps":[]}"#;
        assert!(parse(bad_index).is_err(), "https only");
    }

    #[tokio::test]
    async fn installs_check_every_file_and_replace_atomically() {
        use axum::response::IntoResponse as _;
        // A registry served from a local folder over HTTP stands in for raw.githubusercontent.com.
        let source = tempfile::tempdir().unwrap();
        app_dir(source.path(), "shop", "1.0.0", "\"me\"");
        let files = source.path().to_path_buf();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let files = files.clone();
            async move {
                let path = uri.path().trim_start_matches("/apps/");
                std::fs::read(files.join(path)).map_or_else(
                    |_| axum::http::StatusCode::NOT_FOUND.into_response(),
                    axum::response::IntoResponse::into_response,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/apps", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut index = generate(
            source.path(),
            "https://unused",
            "c1",
            "now",
            &BTreeMap::new(),
        )
        .unwrap();
        // Tests fetch over plain HTTP; real indexes must be https (checked by `parse`).
        index.base = base;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::new();
        let apps = tempfile::tempdir().unwrap();
        let entry = index.apps[0].clone();
        install(&client, &index, &entry, apps.path()).await.unwrap();
        let installed_dir = apps.path().join("shop");
        assert_eq!(
            std::fs::read_to_string(installed_dir.join("js/app.js")).unwrap(),
            "console.log(1)"
        );
        assert_eq!(
            installed(&installed_dir).unwrap().commit.as_deref(),
            Some("c1")
        );
        assert_eq!(
            crate::source_of(&installed_dir),
            okbot_core::AppSource::Registry
        );

        // A file changed after the index was signed: nothing changes on disk.
        std::fs::write(source.path().join("shop/js/app.js"), "steal()").unwrap();
        let error = install(&client, &index, &entry, apps.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not match"), "{error}");
        assert_eq!(
            std::fs::read_to_string(installed_dir.join("js/app.js")).unwrap(),
            "console.log(1)"
        );
        let leftovers: Vec<_> = std::fs::read_dir(apps.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "no staging folders left");

        // The index lists fewer permissions than the manifest asks for.
        std::fs::write(source.path().join("shop/js/app.js"), "console.log(1)").unwrap();
        let mut lying = entry.clone();
        lying.permissions = vec![];
        assert!(install(&client, &index, &lying, apps.path()).await.is_err());

        assert!(remove(apps.path(), "shop").unwrap());
        assert!(!remove(apps.path(), "shop").unwrap());
        assert!(remove(apps.path(), "../x").is_err());
    }

    #[test]
    fn local_apps_are_copied_checked_and_marked() {
        let source = tempfile::tempdir().unwrap();
        let dir = app_dir(source.path(), "my-app", "0.1.0", "\"me\"");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/config"), "x").unwrap();
        let apps = tempfile::tempdir().unwrap();
        let m = install_local(&dir, apps.path()).unwrap();
        assert_eq!(m.info.name, "my-app");
        assert!(
            !apps.path().join("my-app/.git").exists(),
            "hidden files stay behind"
        );
        assert_eq!(
            crate::source_of(&apps.path().join("my-app")),
            okbot_core::AppSource::Local
        );
        std::fs::write(dir.join("run.sh"), "x").unwrap();
        assert!(
            install_local(&dir, apps.path()).is_err(),
            "a script is refused"
        );
    }
}
