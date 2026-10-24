//! The shared HTTP server: health and channel webhooks.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use okbot_core::{CancellationToken, InboundSink, WebhookHandler, WebhookRequest};

use crate::Shared;

/// Where the HTTP server listens.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// Listen address (keep it on loopback behind a TLS reverse proxy).
    pub listen: SocketAddr,
    /// Largest accepted webhook body.
    pub max_body: usize,
}

impl HttpOptions {
    /// Options for `listen` with a 1 MiB body limit.
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            max_body: 1 << 20,
        }
    }
}

fn health(shared: &Shared) -> Response {
    let channels: Vec<String> = shared.channels.keys().map(ToString::to_string).collect();
    let body = serde_json::json!({
        "status": "ok",
        "bot": shared.config.bot_name,
        "channels": channels,
        "in_flight": shared.in_flight.current(),
    });
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn webhook(
    handler: Arc<dyn WebhookHandler>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = WebhookRequest {
        method: method.as_str().to_string(),
        path: String::new(),
        headers: headers
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().ok()?.to_string(),
                ))
            })
            .collect(),
        body: body.to_vec(),
    };
    let response = handler.handle(request).await;
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut reply = (
        status,
        [(header::CONTENT_TYPE, response.content_type)],
        response.body,
    )
        .into_response();
    for (name, value) in response.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            header::HeaderValue::from_str(&value),
        ) {
            reply.headers_mut().insert(name, value);
        }
    }
    reply
}

/// The decoded `key=value` pairs of a query string.
fn query_pairs(query: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://local/?{query}"))
        .map(|url| url.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
}

/// The router: `GET /healthz` and one route per channel webhook. `sink` takes automation
/// prompts (`POST /automation/<name>`).
pub(crate) fn router(
    shared: &Arc<Shared>,
    max_body: usize,
    sink: Option<Arc<dyn InboundSink>>,
) -> Router {
    let health_state = shared.clone();
    let metrics_state = shared.clone();
    let mut router = Router::new()
        .route(
            "/healthz",
            get(move || {
                let state = health_state.clone();
                async move { health(&state) }
            }),
        )
        .route(
            "/metrics",
            get(move || {
                let s = metrics_state.clone();
                async move {
                    (
                        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                        s.metrics.render(&s.config.bot_name, s.in_flight.current()),
                    )
                }
            }),
        );
    if shared.config.mcp.iter().any(|m| m.signed) {
        let state = shared.clone();
        router = router.route(
            "/interact",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let state = state.clone();
                    async move {
                        let bearer = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.strip_prefix("Bearer "))
                            .map(str::trim);
                        let (status, body) = crate::interact::create(&state, bearer, &body).await;
                        (status, axum::Json(body)).into_response()
                    }
                },
            ),
        );
    }
    if !shared.config.notify.is_empty() {
        let state = shared.clone();
        router = router.route(
            "/notify/{name}",
            axum::routing::post(
                move |axum::extract::Path(name): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap,
                      body: axum::body::Bytes| {
                    let state = state.clone();
                    async move {
                        let bearer = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.strip_prefix("Bearer "))
                            .map(str::trim);
                        let reply = crate::notify::post(&state, &name, bearer, &body).await;
                        let mut response = (reply.status, axum::Json(reply.body)).into_response();
                        if let Some(wait) = reply.retry_after
                            && let Ok(value) = wait.as_secs().max(1).to_string().parse()
                        {
                            response.headers_mut().insert(header::RETRY_AFTER, value);
                        }
                        response
                    }
                },
            ),
        );
    }
    router = oauth_and_automation_routes(router, shared, sink);
    for channel in shared.channels.values() {
        if let Some(handler) = channel.webhook() {
            let path = format!("/{}", handler.path().trim_start_matches('/'));
            tracing::info!(channel = %channel.id(), %path, "webhook mounted");
            router = router.route(
                &path,
                any(move |req: Request| {
                    let handler = handler.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        match axum::body::to_bytes(body, max_body).await {
                            Ok(bytes) => webhook(handler, parts.method, parts.headers, bytes).await,
                            Err(_) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
                        }
                    }
                }),
            );
        }
    }
    router
}

/// `GET /oauth/callback` (ADR 0012) and `POST /automation/<name>` (ADR 0013), when used.
fn oauth_and_automation_routes(
    mut router: Router,
    shared: &Arc<Shared>,
    sink: Option<Arc<dyn InboundSink>>,
) -> Router {
    if shared.oauth.is_some() && shared.config.mcp.iter().any(|m| m.oauth.is_some()) {
        let state = shared.clone();
        router = router.route(
            "/oauth/callback",
            get(move |uri: axum::http::Uri| {
                let state = state.clone();
                async move {
                    let query = query_pairs(uri.query().unwrap_or_default());
                    let (status, html) = crate::connect::callback(&state, &query).await;
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
                        [
                            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                            // The page may show who connected; keep it out of caches.
                            (header::CACHE_CONTROL, "no-store"),
                        ],
                        html,
                    )
                }
            }),
        );
    }
    let webhooks = shared
        .config
        .automations
        .iter()
        .any(|a| matches!(a.trigger, crate::config::Trigger::Webhook(_)));
    if let (true, Some(sink)) = (webhooks, sink) {
        let state = shared.clone();
        router = router.route(
            "/automation/{name}",
            axum::routing::post(
                move |axum::extract::Path(name): axum::extract::Path<String>,
                      headers: HeaderMap,
                      body: Bytes| {
                    let state = state.clone();
                    let sink = sink.clone();
                    async move {
                        let (status, body) = crate::automation::post(
                            &state,
                            sink.as_ref(),
                            &name,
                            bearer(&headers),
                            &body,
                        )
                        .await;
                        (status, axum::Json(body)).into_response()
                    }
                },
            ),
        );
    }
    router
}

/// Binds and returns the server future, which ends when `stop` is cancelled.
pub(crate) async fn serve(
    options: HttpOptions,
    shared: Arc<Shared>,
    sink: Arc<dyn InboundSink>,
    stop: CancellationToken,
) -> Result<impl std::future::Future<Output = ()> + Send + 'static, (SocketAddr, std::io::Error)> {
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .map_err(|e| (options.listen, e))?;
    let addr = listener.local_addr().unwrap_or(options.listen);
    tracing::info!(%addr, "HTTP server listening");
    let local = if addr.ip().is_unspecified() {
        SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), addr.port())
    } else {
        addr
    };
    let _ = shared.interact_url.set(format!("http://{local}/interact"));
    let app = router(&shared, options.max_body, Some(sink));
    Ok(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await
        {
            tracing::error!(%error, "HTTP server failed");
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use okbot_core::{
        Channel, ChannelCaps, ChannelError, ChannelId, InboundSink, RenderedMessage, ReplyTarget,
        SentRef, SystemClock, WebhookResponse, async_trait,
    };
    use okbot_testkit::{FakeAgent, ManualClock, MemoryStore, PlainRenderer};
    use tower::ServiceExt;

    use super::*;
    use crate::{InFlight, RuntimeConfig, policy::Gate};

    struct Echo;

    #[async_trait]
    impl WebhookHandler for Echo {
        fn path(&self) -> String {
            "hook/test".into()
        }
        async fn handle(&self, request: WebhookRequest) -> WebhookResponse {
            let secret = request.header("x-secret").unwrap_or("none").to_string();
            WebhookResponse::json(&serde_json::json!({
                "method": request.method,
                "secret": secret,
                "body": String::from_utf8_lossy(&request.body),
            }))
        }
    }

    struct HookChannel(ChannelId, ChannelCaps);

    #[async_trait]
    impl Channel for HookChannel {
        fn id(&self) -> &ChannelId {
            &self.0
        }
        fn caps(&self) -> &ChannelCaps {
            &self.1
        }
        async fn run(
            &self,
            _: Arc<dyn InboundSink>,
            stop: CancellationToken,
        ) -> Result<(), ChannelError> {
            stop.cancelled().await;
            Ok(())
        }
        async fn send(
            &self,
            _: &ReplyTarget,
            _: &RenderedMessage,
        ) -> Result<SentRef, ChannelError> {
            Err(ChannelError::Unsupported("send"))
        }
        fn webhook(&self) -> Option<Arc<dyn WebhookHandler>> {
            Some(Arc::new(Echo))
        }
    }

    fn shared() -> Arc<Shared> {
        shared_with(RuntimeConfig::new("okbot", std::env::temp_dir()))
    }

    fn shared_with(config: RuntimeConfig) -> Arc<Shared> {
        let channel: Arc<dyn Channel> = Arc::new(HookChannel(
            "hook:test".parse().unwrap(),
            ChannelCaps::basic(),
        ));
        Arc::new(Shared {
            gate: Gate::new(config.access.clone()),
            config,
            store: Arc::new(MemoryStore::new(Arc::new(ManualClock::default()))),
            agent: Arc::new(FakeAgent::echo("fake")),
            knowledge: None,
            memory: None,
            tasks: None,
            renderer: Arc::new(PlainRenderer),
            clock: Arc::new(SystemClock),
            channels: HashMap::from([(channel.id().clone(), channel)]),
            in_flight: Arc::new(InFlight::default()),
            metrics: crate::metrics::Metrics::default(),
            prompts: crate::interact::Prompts::default(),
            issued: crate::identity::Issued::default(),
            interact_url: std::sync::OnceLock::new(),
            notified: crate::notify::Recent::default(),
            gateway: Arc::default(),
            oauth: None,
            peers: None,
            sink: std::sync::OnceLock::new(),
        })
    }

    async fn body(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_and_webhooks() {
        let app = router(&shared(), 16, None);
        let health = app
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), 200);
        assert_eq!(body(health).await["channels"][0], "hook:test");
        let metrics = app
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let text = axum::body::to_bytes(metrics.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&text).contains("okbot_events_total{bot=\"okbot\"} 0"));

        let hook = app
            .clone()
            .oneshot(
                Request::post("/hook/test")
                    .header("X-Secret", "s3")
                    .body(Body::from("payload"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = body(hook).await;
        assert_eq!(
            (json["method"].as_str(), json["secret"].as_str()),
            (Some("POST"), Some("s3"))
        );
        assert_eq!(json["body"], "payload");

        let too_big = app
            .oneshot(
                Request::post("/hook/test")
                    .body(Body::from("x".repeat(100)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(too_big.status(), 413);
    }

    #[tokio::test]
    async fn notify_route_needs_the_targets_token() {
        let app = router(&shared(), 1024, None);
        let absent = app
            .oneshot(
                Request::post("/notify/alerts")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(absent.status(), 404, "no [[notify]]: no route");

        let mut config = RuntimeConfig::new("okbot", std::env::temp_dir());
        config.notify = vec![crate::NotifyRoute {
            name: "alerts".into(),
            token: okbot_core::Secret::new("t0k".into()),
            to: vec!["room:hook:test|-|r1|-".parse().unwrap()],
        }];
        let app = router(&shared_with(config), 1024, None);
        let call = |token: &'static str| {
            Request::post("/notify/alerts")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(r#"{"text":"hi"}"#))
                .unwrap()
        };
        let wrong = app.clone().oneshot(call("nope")).await.unwrap();
        assert_eq!(wrong.status(), 401);
        // The test channel cannot send: a permanent failure for its only chat.
        let right = app.oneshot(call("t0k")).await.unwrap();
        assert_eq!(right.status(), 502);
        let json = body(right).await;
        assert_eq!(json["failed"][0]["to"], "room:hook:test|-|r1|-");
        assert_eq!(json["failed"][0]["retry"], false);
    }
}
