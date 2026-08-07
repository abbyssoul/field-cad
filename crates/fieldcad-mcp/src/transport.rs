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

/// Exclusive advisory lock on `<socket path>.lock`, held for a Unix socket's
/// whole lifetime. This is the mutual-exclusion mechanism that makes stale
/// socket reclamation race-free: a probe/remove/bind sequence can never be
/// atomic with respect to a racing peer, but a lock acquired *before* the
/// probe and held until shutdown can — a cooperating server's socket file is
/// never touched by another `bind_unix`, because that peer cannot hold the
/// lock while this one does.
///
/// Released by dropping. The lock file itself is never deleted: unlinking a
/// held lock file lets the next acquirer lock a freshly created inode while
/// the old one is still held, silently voiding the exclusion.
#[cfg(unix)]
pub struct UnixSocketLock {
    /// Never read: the lock exists to be held, and is released when this
    /// field's file is dropped.
    _file: std::fs::File,
}

#[cfg(unix)]
impl UnixSocketLock {
    fn acquire(socket_path: &std::path::Path) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt;

        let mut lock_name = socket_path.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_path = std::path::PathBuf::from(lock_name);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            // Same trust boundary as the socket itself: owner-only.
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| format!("opening socket lock {}: {error}", lock_path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(format!(
                "{} is already in use by a running server",
                socket_path.display()
            )),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(format!("locking {}: {error}", lock_path.display()))
            }
        }
    }
}

#[cfg(unix)]
pub async fn bind_unix(
    path: &std::path::Path,
) -> Result<(tokio::net::UnixListener, UnixSocketLock), String> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    // Acquired before anything is probed or removed and returned alongside
    // the listener so the caller holds it until shutdown — see the type's
    // doc comment.
    let lock = UnixSocketLock::acquire(path)?;

    if path.exists() {
        // Only a socket is ever removed: anything else at this path is
        // refused rather than deleted.
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("inspecting {}: {error}", path.display()))?;
        if !metadata.file_type().is_socket() {
            return Err(format!(
                "refusing to remove {}: it is not a socket",
                path.display()
            ));
        }
        // With the lock held, no cooperating server can be bound at this
        // path, so a failed connect means the file is stale. The probe
        // remains only to diagnose a live *non-cooperating* binder (e.g. an
        // older binary that predates the lock) with a proper error instead
        // of a bare bind failure.
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
    Ok((listener, lock))
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
pub async fn bind_unix(
    path: &std::path::Path,
) -> Result<(std::convert::Infallible, std::convert::Infallible), String> {
    Err(format!(
        "{} requested, but Unix domain sockets are not available on this platform",
        path.display()
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::bind_unix;

    /// Socket paths live under `sun_path`'s 108-byte limit, so keep these
    /// directory names short; the uuid keeps parallel tests apart.
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("fc-mcp-{name}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create the test directory");
            Self(dir)
        }

        fn socket(&self) -> std::path::PathBuf {
            self.0.join("s.sock")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_second_bind_is_refused_even_after_the_socket_file_is_removed_externally() {
        let dir = TestDir::new("second-bind-refused");
        let path = dir.socket();
        let _first = bind_unix(&path).await.expect("the first bind succeeds");
        // The losing interleave of two racing servers: a peer's stale-file
        // removal lands after our bind. Our listener is live but its pathname
        // is gone — a second bind must still be refused rather than orphaned
        // over ours.
        std::fs::remove_file(&path).expect("remove the live socket file externally");
        let second = bind_unix(&path).await;
        assert!(
            second.is_err(),
            "a second bind must be refused while the first server holds the path"
        );
    }

    #[tokio::test]
    async fn a_regular_file_at_the_socket_path_is_not_deleted() {
        let dir = TestDir::new("regular-file-kept");
        let path = dir.socket();
        std::fs::write(&path, b"not a socket").expect("write a regular file");
        let result = bind_unix(&path).await;
        assert!(
            result.is_err(),
            "a non-socket file at the socket path must be refused, not deleted"
        );
        assert_eq!(
            std::fs::read(&path).expect("the regular file survives the refused bind"),
            b"not a socket"
        );
    }

    #[tokio::test]
    async fn a_stale_socket_is_reclaimed() {
        let dir = TestDir::new("stale-reclaimed");
        let path = dir.socket();
        {
            let _stale = std::os::unix::net::UnixListener::bind(&path).expect("bind a socket");
            // Dropped without removing the file: exactly what a killed server
            // leaves behind.
        }
        let rebound = bind_unix(&path).await;
        assert!(
            rebound.is_ok(),
            "a stale socket file should be reclaimed: {:?}",
            rebound.err()
        );
    }

    #[tokio::test]
    async fn a_live_socket_is_refused() {
        let dir = TestDir::new("live-refused");
        let path = dir.socket();
        let _first = bind_unix(&path).await.expect("the first bind succeeds");
        let error = bind_unix(&path).await.err().unwrap_or_default();
        assert!(
            error.contains("already in use"),
            "a live socket must be reported as in use, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn rebinding_after_shutdown_succeeds() {
        let dir = TestDir::new("rebind-after-shutdown");
        let path = dir.socket();
        let first = bind_unix(&path).await.expect("the first bind succeeds");
        // A clean shutdown: the listener (and its lock) is dropped, then the
        // socket file is removed.
        drop(first);
        std::fs::remove_file(&path).expect("shutdown removes the socket file");
        let second = bind_unix(&path).await;
        assert!(
            second.is_ok(),
            "rebinding after shutdown should succeed: {:?}",
            second.err()
        );
    }
}
