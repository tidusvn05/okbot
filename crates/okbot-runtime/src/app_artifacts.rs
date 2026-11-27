//! Artifacts in apps (ADR 0021): the `artifacts.*` App API methods, the sandboxed viewer
//! (`GET /apps/view/<token>`) and the Open buttons under a reply that saved one.
//!
//! HTML, SVG and Mermaid artifacts never run in an app's frame: `artifacts.open` hands out a
//! short-lived link to the viewer, which serves the content with a policy of its own (no
//! network, an opaque origin, no API).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    AppPermission, Artifact, ArtifactError, ArtifactKind, InboundEvent, MemoryScope, Requester,
};
use serde_json::{Value, json};

use crate::Shared;
use crate::app_api::{ApiError, Call};

/// How long a viewer link works.
const VIEW_TTL: SignedDuration = SignedDuration::from_secs(600);
/// Viewer links kept at most; the oldest go first.
const MAX_GRANTS: usize = 2000;
/// Open buttons under one reply.
const MAX_BUTTONS: usize = 2;
/// The built-in app that shows artifacts.
pub(crate) const APP: &str = "artifacts";
/// Artifacts `/artifacts` lists.
const LISTED: usize = 10;
/// Open buttons under that list; one more button opens the app.
const LIST_BUTTONS: usize = okbot_core::MAX_BUTTONS - 1;

/// What a viewer link shows: one version of one artifact, as the person who asked sees it.
#[derive(Debug, Clone)]
struct Grant {
    who: Requester,
    id: String,
    version: u32,
    download: bool,
    expires: Timestamp,
}

/// Viewer links by the hash of their secret.
#[derive(Debug, Default)]
pub(crate) struct Grants(Mutex<HashMap<String, Grant>>);

impl Grants {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Grant>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn put(&self, grant: Grant, now: Timestamp) -> String {
        let secret = crate::apps::secret();
        let mut map = self.lock();
        map.retain(|_, g| g.expires > now);
        if map.len() >= MAX_GRANTS
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, g)| g.expires)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(crate::apps::hash(&secret), grant);
        secret
    }

    fn get(&self, secret: &str, now: Timestamp) -> Option<Grant> {
        let map = self.lock();
        map.get(&crate::apps::hash(secret))
            .filter(|g| g.expires > now)
            .cloned()
    }
}

/// The permission a method of this module needs; `None` for methods it does not have.
pub(crate) fn permission_of(method: &str) -> Option<AppPermission> {
    Some(match method {
        "artifacts.list" | "artifacts.get" | "artifacts.open" => AppPermission::ArtifactsRead,
        "artifacts.rename" | "artifacts.delete" => AppPermission::ArtifactsWrite,
        _ => return None,
    })
}

/// Whether a method of this module changes data.
pub(crate) fn writes(method: &str) -> bool {
    matches!(method, "artifacts.rename" | "artifacts.delete")
}

/// Kinds the app shows itself; the others go to the viewer.
fn shown_in_app(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::Markdown | ArtifactKind::Csv | ArtifactKind::Json | ArtifactKind::Text
    )
}

fn store(call: &Call<'_>) -> Result<Arc<dyn okbot_core::ArtifactStore>, ApiError> {
    call.shared.artifacts.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "artifacts are not enabled for this bot",
        )
    })
}

fn error(call: &Call<'_>, error: ArtifactError) -> ApiError {
    match error {
        ArtifactError::NotFound(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such artifact here")
        }
        ArtifactError::Backend(error) => {
            tracing::error!(%error, "an artifact call failed");
            ApiError::internal(call.shared)
        }
        other => ApiError::invalid(other.to_string()),
    }
}

fn artifact_json(call: &Call<'_>, a: &Artifact, versions: bool) -> Value {
    let tz = call.shared.config.time_zone();
    let latest = a.latest();
    let mut out = json!({
        "id": a.short_id(),
        "title": a.title,
        "kind": a.kind.as_str(),
        "summary": a.summary,
        "scope": if a.scope == MemoryScope::Room { "room" } else { "personal" },
        "mine": a.author == call.account(),
        "created": okbot_core::format_time(a.created, &tz),
        "updated": okbot_core::format_time(a.updated(), &tz),
        "version": latest.n,
        "bytes": latest.bytes,
        "stored": a.stored_bytes(),
        "viewer": !shown_in_app(a.kind),
    });
    if versions {
        out["versions"] = a
            .versions
            .iter()
            .rev()
            .map(|v| {
                json!({
                    "n": v.n,
                    "at": okbot_core::format_time(v.at, &tz),
                    "bytes": v.bytes,
                    "note": v.note,
                    "pruned": v.pruned,
                })
            })
            .collect();
    }
    out
}

fn version_param(call: &Call<'_>) -> Result<Option<u32>, ApiError> {
    match call.params.get("v") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| ApiError::invalid("v: a version number")),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| ApiError::invalid("v: a version number")),
        Some(_) => Err(ApiError::invalid("v: a version number")),
    }
}

/// Runs an `artifacts.*` method.
pub(crate) async fn run(call: &Call<'_>, method: &str) -> Result<Value, ApiError> {
    let store = store(call)?;
    let who = call.requester().await;
    match method {
        "artifacts.list" => {
            let mut list = store.list(&who).await.map_err(|e| error(call, e))?;
            if let Some(q) = call.str("query") {
                let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
                list.retain(|a| {
                    let text = format!("{} {}", a.title, a.summary).to_lowercase();
                    words.iter().all(|w| text.contains(w.as_str()))
                });
            }
            if let Some(kind) = call.str("kind") {
                let kind: ArtifactKind = kind.parse().map_err(ApiError::invalid)?;
                list.retain(|a| a.kind == kind);
            }
            match call.str("scope") {
                None => {}
                Some("personal") => list.retain(|a| a.scope != MemoryScope::Room),
                Some("room") => list.retain(|a| a.scope == MemoryScope::Room),
                Some(_) => return Err(ApiError::invalid("scope: personal or room")),
            }
            let items: Vec<Value> = list.iter().map(|a| artifact_json(call, a, false)).collect();
            Ok(json!({"items": items, "personal": who.personal}))
        }
        "artifacts.get" => {
            let id = call.required("id")?;
            let (a, n, content) = store
                .open(&who, id, version_param(call)?)
                .await
                .map_err(|e| error(call, e))?;
            let content = shown_in_app(a.kind).then_some(content);
            Ok(json!({"artifact": artifact_json(call, &a, true), "v": n, "content": content}))
        }
        "artifacts.open" => {
            let id = call.required("id")?;
            let (a, n, _) = store
                .open(&who, id, version_param(call)?)
                .await
                .map_err(|e| error(call, e))?;
            let download = call.params.get("download").and_then(Value::as_bool) == Some(true);
            let Some(apps) = &call.shared.apps else {
                return Err(ApiError::internal(call.shared));
            };
            let now = call.shared.clock.now();
            let secret = apps.views.put(
                Grant {
                    who,
                    id: a.id.clone(),
                    version: n,
                    download,
                    expires: now + VIEW_TTL,
                },
                now,
            );
            Ok(json!({
                "url": format!("{}/view/{secret}", crate::app_api::cookie_path(call.shared)),
                "file": file_name(&a),
                "title": a.title,
                "v": n,
            }))
        }
        "artifacts.rename" => {
            let id = call.required("id")?;
            let title = call
                .text("title", 80)?
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ApiError::invalid("`title` is required"))?;
            let a = store
                .rename(&who, id, &title)
                .await
                .map_err(|e| error(call, e))?;
            Ok(artifact_json(call, &a, true))
        }
        "artifacts.delete" => {
            let id = call.required("id")?;
            let a = store.delete(&who, id).await.map_err(|e| error(call, e))?;
            Ok(json!({"id": a.short_id(), "deleted": true}))
        }
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no method `{method}`"),
        )),
    }
}

/// A download's file name: the title without characters file systems refuse, and the
/// extension.
fn file_name(a: &Artifact) -> String {
    let stem: String = a
        .title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .take(80)
        .collect();
    let stem = stem.trim().trim_matches('.');
    let stem = if stem.is_empty() { a.short_id() } else { stem };
    format!("{stem}.{}", a.kind.extension())
}

/// `Content-Disposition` for a download: an ASCII name for old clients, the title for others
/// (RFC 6266).
fn disposition(a: &Artifact) -> String {
    let mut encoded = String::new();
    for b in file_name(a).bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    format!(
        "attachment; filename=\"{}.{}\"; filename*=UTF-8''{encoded}",
        a.short_id(),
        a.kind.extension()
    )
}

/// The viewer's policy (design-artifacts §5): inline styles, inline scripts unless
/// `[artifacts] scripts = false`, pictures and fonts from `data:` only, no network, an opaque
/// origin. A Mermaid page runs only okbot's own scripts. No `frame-ancestors`: the viewer's
/// parent is the app's sandboxed frame, whose opaque origin matches no source; the link's
/// secret, valid for minutes, decides who sees it.
fn viewer_csp(shared: &Shared, kind: ArtifactKind) -> String {
    let origin = shared
        .config
        .apps
        .public_url
        .as_deref()
        .map(crate::app_api::origin_of);
    let own = match &origin {
        Some(o) => format!("'self' {o}"),
        None => "'self'".into(),
    };
    let scripts = match kind {
        ArtifactKind::Mermaid => own,
        ArtifactKind::Html | ArtifactKind::Svg if shared.config.apps.artifact_scripts => {
            "'unsafe-inline'".into()
        }
        _ => "'none'".into(),
    };
    let sandbox = if scripts == "'none'" {
        "sandbox"
    } else {
        "sandbox allow-scripts"
    };
    format!(
        "default-src 'none'; style-src 'unsafe-inline'; script-src {scripts}; \
         img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; \
         form-action 'none'; base-uri 'none'; {sandbox}"
    )
}

/// HTML-escapes text for an element's content.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The page that draws a Mermaid diagram with okbot's bundled build, strict mode.
fn mermaid_page(source: &str) -> String {
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <style>body{{margin:0;padding:16px;font-family:system-ui,sans-serif}}\
         #out svg{{max-width:100%;height:auto}}pre{{white-space:pre-wrap}}</style></head>\
         <body><pre id=\"src\" hidden>{}</pre><div id=\"out\"></div>\
         <script src=\"../sdk/mermaid.min.js?v={}\"></script>\
         <script src=\"../sdk/artifact-view.js\"></script></body></html>\n",
        escape(source),
        // The build is cached for a week: a new okbot (maybe with a new Mermaid) renews it.
        env!("CARGO_PKG_VERSION")
    )
}

fn not_found() -> Response {
    let mut response = (
        StatusCode::NOT_FOUND,
        "This link has expired: open it again.",
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /apps/view/<token>`: the artifact version a viewer link was made for.
pub(crate) async fn view(shared: &Shared, token: &str) -> Response {
    let (Some(apps), Some(store)) = (&shared.apps, &shared.artifacts) else {
        return not_found();
    };
    if token.len() > 100 {
        return not_found();
    }
    let Some(grant) = apps.views.get(token, shared.clock.now()) else {
        return not_found();
    };
    let (a, _, content) = match store.open(&grant.who, &grant.id, Some(grant.version)).await {
        Ok(found) => found,
        Err(ArtifactError::Backend(error)) => {
            tracing::error!(%error, "cannot read an artifact for the viewer");
            return (StatusCode::INTERNAL_SERVER_ERROR, "failed").into_response();
        }
        Err(_) => return not_found(),
    };
    let (body, kind) = if grant.download {
        (content, a.kind.content_type())
    } else {
        match a.kind {
            ArtifactKind::Html => (content, "text/html; charset=utf-8"),
            ArtifactKind::Svg => (content, "image/svg+xml"),
            ArtifactKind::Mermaid => (mermaid_page(&content), "text/html; charset=utf-8"),
            _ => (content, "text/plain; charset=utf-8"),
        }
    };
    let mut response = (StatusCode::OK, body).into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    if let Ok(csp) = HeaderValue::from_str(&viewer_csp(shared, a.kind)) {
        h.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=()",
        ),
    );
    if grant.download
        && let Ok(v) = HeaderValue::from_str(&disposition(&a))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    response
}

/// After a turn that started at `since`: Open buttons for the artifacts the asker saved in it,
/// the newest [`MAX_BUTTONS`].
pub(crate) async fn offer_saved(shared: &Shared, event: &InboundEvent, since: Timestamp) {
    let Some(store) = &shared.artifacts else {
        return;
    };
    if shared.apps.is_none() || crate::team::is_peer(event) {
        return;
    }
    let who = crate::command::requester(shared, event).await;
    let me = format!("{}/{}", event.conversation.channel, event.author.id);
    let saved: Vec<Artifact> = match store.list(&who).await {
        Ok(list) => list
            .into_iter()
            .filter(|a| a.latest().by == me && a.updated() >= since)
            .take(MAX_BUTTONS)
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "cannot list the artifacts of a turn");
            return;
        }
    };
    if saved.is_empty() {
        return;
    }
    let mut lines = Vec::new();
    let mut buttons = Vec::new();
    for a in &saved {
        let title: String = a.title.chars().take(50).collect();
        lines.push(format!(
            "-# {} · {} · v{}",
            a.title,
            a.kind.as_str(),
            a.latest().n
        ));
        buttons.push(crate::ui::AppButton {
            label: format!("Open {title}"),
            app: APP.to_string(),
            view: "/a".to_string(),
            params: BTreeMap::from([("id".to_string(), a.short_id().to_string())]),
        });
    }
    let fallback = saved
        .iter()
        .map(|a| format!("`/app {APP} {}`", a.short_id()))
        .collect::<Vec<_>>()
        .join(", ");
    crate::apps::offer_buttons(
        shared,
        event,
        APP,
        lines.join("\n"),
        buttons,
        &format!("-# Open it from a direct chat with me: {fallback}."),
    )
    .await;
}

/// The lines `/artifacts` answers with: the newest `LISTED`, then where the rest is.
fn list_text(list: &[Artifact], tz: &jiff::tz::TimeZone) -> String {
    if list.is_empty() {
        return "No artifacts here yet. Ask me for a page, a table, a document or a diagram and \
                I will keep it here."
            .into();
    }
    let mut text = String::new();
    for (i, a) in list.iter().take(LISTED).enumerate() {
        let _ = writeln!(
            text,
            "{}. **{}** · {} · v{} · {} · `{}`",
            i + 1,
            a.title,
            a.kind.as_str(),
            a.latest().n,
            a.updated().to_zoned(tz.clone()).strftime("%d/%m %H:%M"),
            a.short_id()
        );
    }
    if list.len() > LISTED {
        let _ = writeln!(text, "-# {} more in the app.", list.len() - LISTED);
    }
    text.trim_end().to_string()
}

/// `/artifacts`: the newest artifacts the asker sees here, then Open buttons.
pub(crate) async fn command(shared: &Shared, event: &InboundEvent) {
    let Some(store) = &shared.artifacts else {
        crate::turn::reply(shared, event, "Artifacts are not enabled for this bot.").await;
        return;
    };
    let who = crate::command::requester(shared, event).await;
    let list = match store.list(&who).await {
        Ok(list) => list,
        Err(error) => {
            tracing::warn!(%error, "cannot list artifacts");
            crate::turn::reply(shared, event, &shared.config.replies.error).await;
            return;
        }
    };
    let tz = shared.config.time_zone();
    crate::turn::reply(shared, event, &list_text(&list, &tz)).await;
    if list.is_empty() || shared.apps.is_none() {
        return;
    }
    let mut buttons: Vec<crate::ui::AppButton> = list
        .iter()
        .take(LIST_BUTTONS)
        .map(|a| crate::ui::AppButton {
            label: format!("Open {}", a.title.chars().take(40).collect::<String>()),
            app: APP.to_string(),
            view: "/a".to_string(),
            params: BTreeMap::from([("id".to_string(), a.short_id().to_string())]),
        })
        .collect();
    buttons.push(crate::ui::AppButton {
        label: "All artifacts".to_string(),
        app: APP.to_string(),
        view: "/".to_string(),
        params: BTreeMap::new(),
    });
    crate::apps::offer_buttons(
        shared,
        event,
        APP,
        "-# Open one, or see them all with search and filters.".to_string(),
        buttons,
        &format!("-# Open one from a direct chat with me: `/app {APP} <id>`."),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(title: &str, kind: ArtifactKind) -> Artifact {
        Artifact {
            id: "artifacts/people/p/a-1234567890".into(),
            title: title.into(),
            kind,
            summary: String::new(),
            scope: MemoryScope::User,
            author: "console/me".into(),
            origin: None,
            created: Timestamp::UNIX_EPOCH,
            versions: vec![okbot_core::ArtifactVersion {
                n: 1,
                at: Timestamp::UNIX_EPOCH,
                by: "console/me".into(),
                bytes: 1,
                sha256: String::new(),
                note: String::new(),
                pruned: false,
            }],
        }
    }

    #[test]
    fn the_list_shows_the_newest_ten_and_counts_the_rest() {
        let tz = jiff::tz::TimeZone::UTC;
        assert!(list_text(&[], &tz).starts_with("No artifacts here yet"));
        let list: Vec<Artifact> = (1..=12)
            .map(|i| artifact(&format!("Page {i}"), ArtifactKind::Markdown))
            .collect();
        let text = list_text(&list, &tz);
        assert!(
            text.starts_with("1. **Page 1** · markdown · v1 · 01/01 00:00 · `a-1234567890`"),
            "{text}"
        );
        assert!(
            text.contains("10. **Page 10**") && !text.contains("Page 11"),
            "{text}"
        );
        assert!(text.ends_with("-# 2 more in the app."), "{text}");
    }

    #[test]
    fn downloads_keep_the_title_as_their_name() {
        let a = artifact("Kế hoạch Q3: bán/hàng", ArtifactKind::Html);
        assert_eq!(file_name(&a), "Kế hoạch Q3- bán-hàng.html");
        let d = disposition(&a);
        assert!(
            d.starts_with(
                "attachment; filename=\"a-1234567890.html\"; filename*=UTF-8''K%E1%BA%BF%20ho"
            ),
            "{d}"
        );
        assert_eq!(
            file_name(&artifact(" .. ", ArtifactKind::Markdown)),
            "a-1234567890.md"
        );
    }

    #[test]
    fn mermaid_source_cannot_close_its_element() {
        let page = mermaid_page("graph TD; A-->B</pre><script>alert(1)</script>");
        assert!(!page.contains("<script>alert"), "{page}");
        assert!(page.contains("&lt;/pre&gt;"), "{page}");
    }

    #[test]
    fn every_method_needs_a_permission() {
        for m in ["artifacts.list", "artifacts.get", "artifacts.open"] {
            assert_eq!(permission_of(m), Some(AppPermission::ArtifactsRead));
            assert!(!writes(m));
        }
        for m in ["artifacts.rename", "artifacts.delete"] {
            assert_eq!(permission_of(m), Some(AppPermission::ArtifactsWrite));
            assert!(writes(m));
        }
    }
}
