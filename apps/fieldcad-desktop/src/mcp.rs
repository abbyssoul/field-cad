//! Embeds `fieldcad-mcp`'s HTTP transport into the running desktop app, so
//! an agent can drive the *same* live session the user is looking at rather
//! than a separate, empty one (see `docs/mcp-plan.md`).
//!
//! The desktop app has no tokio runtime otherwise — `enable` spawns a
//! dedicated OS thread that builds a minimal one and runs the server on it,
//! sharing the app's `Arc<Mutex<HeadlessServer>>` model. That `Mutex` is
//! `std::sync`, not `tokio::sync`: it's what lets this thread and the
//! synchronous winit frame loop lock the same model without either side
//! needing the other's runtime.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use fieldcad_mcp::McpServer;
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::PluginRegistration;
use tokio_util::sync::CancellationToken;

/// What `enable`/`enable_at` need in order to build `McpServer`'s
/// plugin-catalog closure: this window's own GPU-backed composition, not
/// the standalone server's CPU-only one — an agent driving `create_scene`/
/// `open_scene` through the embedded server must get the exact same
/// evaluator backends the desktop's own File menu would have built, or a
/// loaded/new scene would silently diverge from what the user sees.
pub type PluginCatalog = Arc<dyn Fn() -> Vec<PluginRegistration> + Send + Sync>;

/// Matches the standalone `fieldcad-mcp --http` default, so an agent's
/// config can hardcode one address either way.
const DEFAULT_ADDR: &str = "127.0.0.1:8642";
/// Generous margin over what a loopback `TcpListener::bind` actually takes,
/// so a slow-but-fine machine doesn't get a false `Failed`. `enable` blocks
/// the caller (the UI thread) for at most this long — acceptable for a rare,
/// explicit, one-shot button click; not worth a `Starting` state machine.
const BIND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub enum McpSession {
    #[default]
    Disabled,
    Running(McpRunning),
    Failed(String),
}

pub struct McpRunning {
    pub token: String,
    pub addr: SocketAddr,
    ct: CancellationToken,
    /// Checked (non-blocking) each frame the MCP panel is open. A message
    /// here, or the sender having dropped without one, means the server
    /// thread stopped after a successful bind — without this, `McpSession`
    /// would stay `Running` forever with a token nothing answers to.
    fatal: mpsc::Receiver<String>,
    connections: fieldcad_mcp::McpConnections,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpAction {
    Enable,
    Disable,
}

/// Start serving the shared model over HTTP with a fresh bearer token, on
/// the default address (see `DEFAULT_ADDR`) — what the MCP panel's "Enable"
/// button asks for, since a user clicking it has not named an address.
/// Known, accepted rough edge: re-`enable`ing immediately after `disable`
/// can transiently fail with "address in use" if the previous listener
/// hasn't finished tearing down yet — the message below names that as the
/// likely cause rather than surfacing a raw OS error.
pub fn enable(
    model: Arc<Mutex<HeadlessServer>>,
    plugin_catalog: PluginCatalog,
) -> Result<McpRunning, String> {
    let addr: SocketAddr = DEFAULT_ADDR.parse().expect("hardcoded address is valid");
    enable_at(model, addr, plugin_catalog)
}

/// Same as [`enable`], against an explicit address — what `--mcp <address>`
/// asks for at launch, where the caller has named where to listen.
pub fn enable_at(
    model: Arc<Mutex<HeadlessServer>>,
    addr: SocketAddr,
    plugin_catalog: PluginCatalog,
) -> Result<McpRunning, String> {
    let token = fieldcad_mcp::generate_token();
    let ct = CancellationToken::new();

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (fatal_tx, fatal_rx) = mpsc::channel::<String>();
    // Built here, not inside the spawned thread: the caller can hand this
    // straight to `McpRunning` without waiting on the thread at all — it
    // wraps the session table `serve_http` will use, not a snapshot of it.
    let connections = fieldcad_mcp::McpConnections::new();

    let thread_token = token.clone();
    let thread_ct = ct.clone();
    let thread_connections = connections.clone();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = ready_tx.send(Err(format!("starting the MCP runtime: {error}")));
                return;
            }
        };
        runtime.block_on(async move {
            let listener = match fieldcad_mcp::bind_http(addr).await {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!(
                        "binding {addr}: {error} (another Field CAD MCP server may already be running)"
                    )));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            let server = McpServer::new(model, plugin_catalog);
            if let Err(error) = fieldcad_mcp::serve_http(
                listener,
                server,
                Some(thread_token),
                thread_connections,
                thread_ct,
            )
            .await
            {
                let _ = fatal_tx.send(error);
            }
        });
    });

    match ready_rx.recv_timeout(BIND_TIMEOUT) {
        Ok(Ok(())) => Ok(McpRunning {
            token,
            addr,
            ct,
            fatal: fatal_rx,
            connections,
        }),
        Ok(Err(error)) => Err(error),
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => Err(
            format!("MCP server did not confirm startup within {BIND_TIMEOUT:?}"),
        ),
    }
}

/// Stop serving. Fire-and-forget: the server thread stops cooperatively once
/// it observes the cancellation, but nothing here waits for that — process
/// exit is what tears it down for certain, and blocking the UI thread on a
/// clean shutdown isn't worth it for a locally-run dev tool.
pub fn disable(running: McpRunning) {
    running.ct.cancel();
}

/// `None` while the server is still healthy; `Some(message)` once it has
/// stopped unexpectedly after a successful bind (a panic in a tool handler,
/// the listener erroring out) — the caller should move to `McpSession::Failed`.
pub fn check_alive(running: &McpRunning) -> Option<String> {
    match running.fatal.try_recv() {
        Ok(error) => Some(error),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            Some("the MCP server thread stopped unexpectedly".to_owned())
        }
    }
}

/// How many MCP clients currently have a session open — the panel's
/// connection indicator, and the signal a user watches before deciding it's
/// safe to disable the server. See [`fieldcad_mcp::McpConnections::count`]
/// for what "connected" means precisely (a known, not-yet-closed session,
/// not necessarily mid-request right now) and why this can be `None`.
pub fn connection_count(running: &McpRunning) -> Option<usize> {
    running.connections.count()
}
