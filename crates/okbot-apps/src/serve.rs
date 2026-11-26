//! Static serving: the shell, the SDK and app files, each with its security headers (ADR 0019).
//!
//! The App API itself (`/apps/api/…`) is the runtime's: it needs the bot's access rules.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};

use crate::catalog::Catalog;
use crate::manifest::content_type;

static SHELL: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin/shell");
static SDK: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/builtin/sdk");

/// The shell page's policy: its own scripts and styles, frames and API calls to the bot only,
/// never framed itself.
pub const SHELL_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self' data:; connect-src 'self'; frame-src 'self'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

/// An app file's policy: files from the bot only, no network, no forms sent anywhere, framed
/// only by the shell, and sandboxed even when opened on its own (an opaque origin without the
/// session cookie).
pub fn frame_csp(origin: Option<&str>) -> String {
    let src = match origin {
        Some(o) => format!("'self' {o}"),
        None => "'self'".into(),
    };
    format!(
        "default-src 'none'; script-src {src}; style-src {src}; img-src {src} data:; \
         font-src {src}; connect-src 'none'; form-action 'none'; base-uri 'none'; \
         frame-ancestors {src}; sandbox allow-scripts allow-forms"
    )
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn with_headers(body: Vec<u8>, kind: &'static str, csp: &str, cache: &'static str) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=()",
        ),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    if let Ok(v) = HeaderValue::from_str(csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    response
}

fn shell_file(path: &str) -> Response {
    let Some(file) = SHELL.get_file(path) else {
        return not_found();
    };
    let Some(kind) = content_type(path) else {
        return not_found();
    };
    let mut response = with_headers(file.contents().to_vec(), kind, SHELL_CSP, "no-store");
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

fn sandboxed(body: Vec<u8>, path: &str, origin: Option<&str>) -> Response {
    let Some(kind) = content_type(path) else {
        return not_found();
    };
    let mut response = with_headers(body, kind, &frame_csp(origin), "no-cache");
    // The frame's origin is opaque: module scripts and fonts it loads are cross-origin requests.
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// Routes for the shell (`/apps/`), the SDK (`/apps/sdk/v1.js`) and app files
/// (`/apps/frame/<app>/<file>`). `origin` is the bot's public origin (`https://bot.example.com`),
/// named in the frame policy beside `'self'`.
pub fn router(catalog: Catalog, origin: Option<String>) -> Router {
    let origin_sdk = origin.clone();
    Router::new()
        .route(
            "/apps",
            get(|| async { axum::response::Redirect::permanent("/apps/") }),
        )
        .route("/apps/", get(|| async { shell_file("index.html") }))
        .route(
            "/apps/shell/{file}",
            get(|Path(file): Path<String>| async move { shell_file(&file) }),
        )
        .route(
            "/apps/sdk/{file}",
            get(move |Path(file): Path<String>| {
                let origin = origin_sdk.clone();
                async move {
                    match SDK.get_file(&file) {
                        Some(f) => sandboxed(f.contents().to_vec(), &file, origin.as_deref()),
                        None => not_found(),
                    }
                }
            }),
        )
        .route(
            "/apps/frame/{app}/{*path}",
            get(move |Path((app, path)): Path<(String, String)>| {
                let catalog = catalog.clone();
                let origin = origin.clone();
                async move {
                    let found = tokio_blocking(move || {
                        catalog
                            .get(&app)
                            .and_then(|a| a.file(&path).map(|b| (b, path)))
                    })
                    .await;
                    match found {
                        Some((body, path)) => sandboxed(body, &path, origin.as_deref()),
                        None => not_found(),
                    }
                }
            }),
        )
}

/// Runs file reads off the async threads.
async fn tokio_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    // axum runs on tokio; a blocking read of a small file is cheap, but a slow disk must not
    // stall the server.
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    async fn get(router: &Router, uri: &str) -> Response {
        router
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn shell_sdk_and_frames_carry_their_policies() {
        let router = router(Catalog::new(None), Some("https://bot.example.com".into()));
        let shell = get(&router, "/apps/").await;
        assert_eq!(shell.status(), 200);
        assert_eq!(shell.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(shell.headers()[header::CONTENT_SECURITY_POLICY], SHELL_CSP);
        let frame = get(&router, "/apps/frame/tasks/index.html").await;
        assert_eq!(frame.status(), 200);
        let csp = frame.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("connect-src 'none'"), "{csp}");
        assert!(csp.contains("sandbox allow-scripts allow-forms"), "{csp}");
        assert!(!csp.contains("allow-same-origin"), "{csp}");
        assert!(csp.contains("https://bot.example.com"), "{csp}");
        assert_eq!(get(&router, "/apps/sdk/v1.js").await.status(), 200);
        assert_eq!(get(&router, "/apps/shell/shell.js").await.status(), 200);
        for missing in [
            "/apps/frame/tasks/../../Cargo.toml",
            "/apps/frame/nope/index.html",
            "/apps/frame/tasks/missing.js",
            "/apps/shell/nope.js",
        ] {
            assert_eq!(get(&router, missing).await.status(), 404, "{missing}");
        }
    }
}
