//! Transports onto [`McpServer`], and the bits any caller assembling one
//! needs: a token generator and the bearer-auth layer HTTP/Unix can opt
//! into. Split into a fast `bind_*` step and a long-running `serve_*` step
//! so a caller — the standalone binary today, the desktop app once it
//! embeds this (`docs/mcp-plan.md`) — can learn "did the bind succeed, what
//! address did it get" without waiting on the whole serve loop.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rmcp::{
    ServiceExt,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;

use crate::McpServer;

/// A fresh bearer token for one MCP session. Not persisted anywhere and not
/// reused across `Enable MCP` clicks or process runs — regenerated every
/// time, deliberately (see `docs/mcp-plan.md`).
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Wrap `router` so every request must carry `Authorization: Bearer
/// <token>`, or does nothing when `token` is `None` — the Unix-socket
/// transport's file permissions (owner-only) are its trust boundary
/// instead, not a token.
fn require_bearer_token(router: Router, token: Option<String>) -> Router {
    let Some(token) = token else {
        return router;
    };
    router.layer(axum::middleware::from_fn(
        move |request: Request, next: Next| {
            let token = token.clone();
            async move {
                let authorized = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .is_some_and(|provided| {
                        constant_time_eq(provided.as_bytes(), token.as_bytes())
                    });
                if authorized {
                    next.run(request).await
                } else {
                    unauthorized()
                }
            }
        },
    ))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
    )
        .into_response()
}

/// A queryable handle onto how many MCP sessions a running Streamable HTTP
/// server currently knows about — created via `initialize`, not yet
/// explicitly closed. Constructed by the caller *before* the server starts
/// (`McpConnections::new`) precisely so it can be handed to a UI (the
/// embedded desktop panel) without waiting on anything the server itself
/// produces — it wraps the same `LocalSessionManager` [`serve_http`]/
/// [`serve_unix`] use as the session store, not a snapshot of it.
#[derive(Clone, Default)]
pub struct McpConnections {
    sessions: Arc<LocalSessionManager>,
}

impl McpConnections {
    pub fn new() -> Self {
        Self::default()
    }

    /// `None` only if a request happens to be touching the session table at
    /// this exact instant (a non-blocking `try_read` failed) — meant to be
    /// polled every frame by a UI, not awaited, so retry next frame rather
    /// than treating that as zero.
    ///
    /// Not a live "still streaming right now" signal: a session persists
    /// until explicitly closed or the server restarts, so a client that
    /// disappeared without a clean disconnect still counts. Good enough to
    /// answer "do I have any connection at all", which is what this exists
    /// for — deciding whether it's safe to shut the server down.
    pub fn count(&self) -> Option<usize> {
        self.sessions
            .sessions
            .try_read()
            .ok()
            .map(|sessions| sessions.len())
    }
}

fn mcp_service(
    server: McpServer,
    connections: &McpConnections,
    ct: CancellationToken,
) -> StreamableHttpService<McpServer, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default().with_cancellation_token(ct);
    StreamableHttpService::new(
        move || Ok(server.clone()),
        connections.sessions.clone(),
        config,
    )
}

pub async fn run_stdio(server: McpServer, ct: CancellationToken) -> Result<(), String> {
    tracing::info!("MCP stdio transport ready");
    let running = server
        .serve_with_ct(stdio(), ct)
        .await
        .map_err(|error| format!("starting the stdio transport: {error}"))?;
    running
        .waiting()
        .await
        .map_err(|error| format!("stdio transport ended: {error}"))?;
    Ok(())
}

pub async fn bind_http(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await
}

pub async fn serve_http(
    listener: tokio::net::TcpListener,
    server: McpServer,
    token: Option<String>,
    connections: McpConnections,
    ct: CancellationToken,
) -> Result<(), String> {
    let addr = listener
        .local_addr()
        .map_err(|error| format!("reading the bound address: {error}"))?;
    let router = Router::new().nest_service("/mcp", mcp_service(server, &connections, ct.clone()));
    let router = require_bearer_token(router, token);
    tracing::info!(%addr, "MCP Streamable HTTP listening at http://{addr}/mcp");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await
        .map_err(|error| format!("HTTP transport ended: {error}"))
}

#[cfg(unix)]
pub async fn bind_unix(path: &std::path::Path) -> Result<tokio::net::UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;

    if path.exists() {
        // A stale socket file from a crashed previous run and a live one
        // look identical on the filesystem; only remove it once we've
        // confirmed nothing answers.
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => {
                return Err(format!(
                    "{} is already in use by a running server",
                    path.display()
                ));
            }
            Err(_) => {
                std::fs::remove_file(path).map_err(|error| {
                    format!("removing stale socket {}: {error}", path.display())
                })?;
            }
        }
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|error| format!("binding {}: {error}", path.display()))?;
    // A Unix socket has no auth layer of its own (see `require_bearer_token`
    // — callers pass `None` here); restrict it to the owner rather than
    // trust the process umask.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("securing {}: {error}", path.display()))?;
    Ok(listener)
}

#[cfg(unix)]
pub async fn serve_unix(
    listener: tokio::net::UnixListener,
    server: McpServer,
    token: Option<String>,
    connections: McpConnections,
    ct: CancellationToken,
) -> Result<(), String> {
    use hyper_util::rt::TokioIo;

    let router = Router::new().nest_service("/mcp", mcp_service(server, &connections, ct.clone()));
    let router = require_bearer_token(router, token);
    tracing::info!("MCP Streamable HTTP listening on a Unix socket");

    // Not `axum::serve(UnixListener, ...)`: axum's `Listener` impl for Unix
    // sockets uses `spawn_local` on Linux, which needs a `LocalSet` this
    // crate doesn't set up. This is the same manual hyper accept loop
    // rmcp's own Unix-socket transport test uses on the server side.
    loop {
        tokio::select! {
            () = ct.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _addr) = match accepted {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::warn!(%error, "unix socket accept failed");
                        continue;
                    }
                };
                let router = router.clone();
                let conn_ct = ct.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let hyper_service = hyper::service::service_fn(move |request| {
                        let mut router = router.clone();
                        async move {
                            use tower_service::Service;
                            router.call(request).await
                        }
                    });
                    let connection = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, hyper_service);
                    tokio::select! {
                        _ = connection => {}
                        () = conn_ct.cancelled() => {}
                    }
                });
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub async fn bind_unix(path: &std::path::Path) -> Result<std::convert::Infallible, String> {
    Err(format!(
        "{} requested, but Unix domain sockets are not available on this platform",
        path.display()
    ))
}
