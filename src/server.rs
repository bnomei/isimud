//! HTTP server wiring (PLAN.md task 7).
//!
//! Mounts the `rmcp` `StreamableHttpService` (with `LocalSessionManager`) on an `axum` router
//! at `/mcp`, bound to loopback by default, with a graceful-shutdown broadcast.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use tokio::sync::broadcast;
use tracing::info;

use crate::config::AppConfig;
use crate::mcp::{IsimudMcp, McpInitError};
use crate::worker::SpeechEngine;
use crate::TARGET_SERVER;

/// Errors raised while starting or running the HTTP/MCP server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("invalid bind host '{host}' (expected an IP address): {source}")]
    BindAddr {
        host: String,
        #[source]
        source: std::net::AddrParseError,
    },
    #[error("refusing to bind non-loopback address {0}: set [server].allow_remote = true")]
    RemoteNotAllowed(SocketAddr),
    #[error("refusing to bind non-loopback address {0} without an auth token (ISIMUD_AUTH_TOKEN or [server].auth_token)")]
    InsecureRemote(SocketAddr),
    #[error("failed to initialize MCP service: {0}")]
    Mcp(#[from] McpInitError),
    #[error("server io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
struct AuthState {
    token: Option<Arc<String>>,
}

/// Run the MCP/HTTP server until the `shutdown_tx` channel fires. Consumes the engine,
/// which is moved into the MCP service.
pub async fn run_server(
    engine: SpeechEngine,
    shutdown_tx: broadcast::Sender<()>,
) -> Result<(), ServerError> {
    let config = engine.config();
    let addr = resolve_bind_addr(&config)?;
    let auth_token = config.resolved_auth_token();

    if !addr.ip().is_loopback() {
        if !config.server.allow_remote {
            return Err(ServerError::RemoteNotAllowed(addr));
        }
        if auth_token.is_none() {
            return Err(ServerError::InsecureRemote(addr));
        }
    }

    let mut shutdown_rx = shutdown_tx.subscribe();
    let service = IsimudMcp::streamable_http_service_with_shutdown(engine, shutdown_tx.clone())?;

    let auth_state = AuthState { token: auth_token.map(Arc::new) };
    let router = Router::new()
        .route_service(&config.server.path, service)
        .layer(middleware::from_fn_with_state(auth_state, auth_middleware));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(
        target: TARGET_SERVER,
        %addr,
        path = %config.server.path,
        auth = router_auth_label(&config),
        "isimud MCP server listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.recv().await;
        })
        .await?;

    info!(target: TARGET_SERVER, "isimud MCP server stopped");
    Ok(())
}

fn router_auth_label(config: &AppConfig) -> &'static str {
    if config.resolved_auth_token().is_some() {
        "bearer"
    } else {
        "none"
    }
}

fn resolve_bind_addr(config: &AppConfig) -> Result<SocketAddr, ServerError> {
    let ip = config
        .server
        .host
        .parse::<std::net::IpAddr>()
        .map_err(|source| ServerError::BindAddr { host: config.server.host.clone(), source })?;
    Ok(SocketAddr::new(ip, config.server.port))
}

async fn auth_middleware(State(auth): State<AuthState>, request: Request, next: Next) -> Response {
    let Some(expected) = auth.token.as_deref() else {
        return next.run(request).await;
    };

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    if provided == Some(expected.as_str()) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_bind_addr;
    use crate::config::AppConfig;

    #[test]
    fn resolve_bind_addr_parses_loopback_default() {
        let config = AppConfig::default();
        let addr = resolve_bind_addr(&config).expect("loopback should resolve");
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), crate::DEFAULT_PORT);
    }

    #[test]
    fn resolve_bind_addr_rejects_non_ip_host() {
        let mut config = AppConfig::default();
        config.server.host = "localhost".to_string();
        assert!(resolve_bind_addr(&config).is_err());
    }
}
