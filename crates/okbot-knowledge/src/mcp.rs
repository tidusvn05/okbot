//! The loopback MCP endpoint agents call during a run.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use okbot_core::CancellationToken;

use crate::tokens::{RunTokens, TokenScopes};

/// Path of the knowledge MCP endpoint (okbase tools).
pub(crate) const KB_PATH: &str = "/kb/mcp";

async fn require_run_token(
    tokens: Arc<RunTokens>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    match token {
        Some(t) if tokens.is_valid(t) => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// The router: okbase's MCP server under `/kb`, every request authenticated by a run token.
pub(crate) fn router(bundle: okbase::Bundle, tokens: &Arc<RunTokens>) -> Router {
    let kb = okbase_mcp::router(
        bundle,
        Arc::new(TokenScopes(tokens.clone())),
        &okbase_mcp::ServerOptions::default(),
        &okbase_mcp::HttpOptions::default(),
    );
    let auth = tokens.clone();
    Router::new()
        .nest("/kb", kb)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let tokens = auth.clone();
            async move { require_run_token(tokens, req, next).await }
        }))
}

/// Serves `router` on a random loopback port until `stop`.
pub(crate) async fn serve(router: Router, stop: CancellationToken) -> std::io::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await
        {
            tracing::error!(%error, "knowledge MCP server failed");
        }
    });
    tracing::info!(%addr, "knowledge MCP server listening on loopback");
    Ok(addr)
}
