//! Runs the MCP tool surface against a fresh default session, over one or
//! more transports at once.
//!
//! All requested transports share the same `Arc<Mutex<HeadlessServer>>`
//! session (via cloned [`fieldcad_mcp::McpServer`] handles) — an agent
//! talking over stdio and a web client talking over HTTP see and mutate the
//! same model, the way the architecture in `docs/mcp-plan.md` intends. Stdio
//! suits a client that spawns this process itself (an editor's local MCP
//! integration, the MCP inspector); Streamable HTTP and a Unix domain socket
//! both suit a client connecting to an already-running server, over a
//! network or purely locally respectively.
//!
//! HTTP is restricted to loopback addresses: `docs/mcp-plan.md` phase 5
//! requires an explicit opt-in plus bearer-token auth before any
//! non-loopback bind. Bearer-token auth exists now (this file always
//! requires one for `--http`, auto-generating and printing it if `--token`
//! wasn't given) — the loopback restriction itself is not relaxed in this
//! change; that's deliberately deferred, see `docs/mcp-plan.md`.

use std::{
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use fieldcad_mcp::McpServer;
use fieldcad_server::HeadlessServer;
use tokio_util::sync::CancellationToken;

const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8642";

/// How often the standalone session's own wall clock advances. Matches
/// `fieldcad-server`'s equivalent headless loop; fast enough that a queued
/// edit reaches its next tick boundary promptly, slow enough not to matter
/// against typical simulation time steps.
const DRIVE_INTERVAL: Duration = Duration::from_millis(16);

const HELP: &str = "\
fieldcad-mcp — MCP transport onto the Field CAD simulation model

USAGE:
    fieldcad-mcp [OPTIONS]

OPTIONS:
    --stdio           Serve over stdio. Default if no transport is given.
    --http [ADDR]     Serve Streamable HTTP at ADDR (default 127.0.0.1:8642),
                      mounted at /mcp. ADDR must be a loopback address —
                      remote access needs an explicit opt-in that does not
                      exist yet (docs/mcp-plan.md phase 5). Always requires a
                      bearer token: pass --token, or one is generated and
                      printed once.
    --token TOKEN     The bearer token --http requires. Ignored without
                      --http.
    --unix PATH       Serve Streamable HTTP over a Unix domain socket at
                      PATH, mounted at /mcp. Local IPC only, never a network
                      listener, and unauthenticated — the socket file's 0600
                      permissions are its trust boundary. Requires a Unix
                      platform.
    -h, --help        Show this message.

Options are additive: pass more than one to serve the same session over
several transports at once.

ENVIRONMENT:
    RUST_LOG          e.g. fieldcad_mcp=debug
";

#[derive(Default)]
struct Transports {
    stdio: bool,
    http: Option<SocketAddr>,
    token: Option<String>,
    unix: Option<PathBuf>,
}

enum ArgOutcome {
    Run(Transports),
    Help,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<ArgOutcome, String> {
    let mut transports = Transports::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ArgOutcome::Help),
            "--stdio" => transports.stdio = true,
            "--http" => {
                let addr = match args.peek() {
                    Some(next) if !next.starts_with("--") => args.next().unwrap(),
                    _ => DEFAULT_HTTP_ADDR.to_owned(),
                };
                let addr: SocketAddr = addr
                    .parse()
                    .map_err(|error| format!("invalid --http address '{addr}': {error}"))?;
                if !addr.ip().is_loopback() {
                    return Err(format!(
                        "refusing to bind {addr}: only loopback addresses are accepted \
                         (docs/mcp-plan.md phase 5)"
                    ));
                }
                transports.http = Some(addr);
            }
            "--token" => {
                transports.token = Some(
                    args.next()
                        .ok_or_else(|| "--token requires a value".to_owned())?,
                );
            }
            "--unix" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--unix requires a socket path".to_owned())?;
                transports.unix = Some(PathBuf::from(path));
            }
            other => return Err(format!("unrecognized argument '{other}'")),
        }
    }
    if !transports.stdio && transports.http.is_none() && transports.unix.is_none() {
        transports.stdio = true;
    }
    Ok(ArgOutcome::Run(transports))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fieldcad_mcp=info,fieldcad_simulation=info".into()),
        )
        // Stdio uses stdout for MCP protocol frames; logs must go to stderr
        // or they corrupt that stream, even when stdio isn't the transport
        // in use this run — the filter is chosen before we know which
        // transports were requested.
        .with_writer(std::io::stderr)
        .init();

    let transports = match parse_args(std::env::args().skip(1)) {
        Ok(ArgOutcome::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(ArgOutcome::Run(transports)) => transports,
        Err(error) => {
            eprintln!("{error}\n\n{HELP}");
            return ExitCode::FAILURE;
        }
    };

    let source = match fieldcad_server::default_session() {
        Ok(source) => source,
        Err(error) => {
            tracing::error!(%error, "failed to build the default session");
            return ExitCode::FAILURE;
        }
    };
    let model = Arc::new(Mutex::new(HeadlessServer::new(source)));
    let server = McpServer::new(
        Arc::clone(&model),
        Arc::new(fieldcad_mcp::mcp_plugin_catalog),
    );

    let root_ct = CancellationToken::new();
    tokio::spawn({
        let root_ct = root_ct.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("shutdown requested");
                root_ct.cancel();
            }
        }
    });

    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(drive_session(model, root_ct.child_token()));
    if transports.stdio {
        let server = server.clone();
        let ct = root_ct.child_token();
        tasks.spawn(async move { fieldcad_mcp::run_stdio(server, ct).await });
    }
    if let Some(addr) = transports.http {
        let server = server.clone();
        let ct = root_ct.child_token();
        let token = transports.token.clone().unwrap_or_else(|| {
            let token = fieldcad_mcp::generate_token();
            eprintln!("generated MCP bearer token (pass --token to set your own): {token}");
            token
        });
        tasks.spawn(async move { run_http(server, addr, token, ct).await });
    }
    if let Some(path) = transports.unix {
        let server = server.clone();
        let ct = root_ct.child_token();
        tasks.spawn(async move { run_unix(server, path, ct).await });
    }

    let mut failed = false;
    while let Some(outcome) = tasks.join_next().await {
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "a transport failed");
                failed = true;
                root_ct.cancel();
            }
            Err(join_error) => {
                tracing::error!(%join_error, "a transport task panicked");
                failed = true;
                root_ct.cancel();
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// This session's wall-clock driver for a standalone `fieldcad-mcp`: no MCP
/// tool exposes wall-clock advance (by design — that isn't a client
/// decision), so something in-process has to call `HeadlessServer::advance`
/// on a fixed cadence the way the embedded desktop app's per-frame pump does
/// when this crate is embedded there instead. `submit_and_wait`'s own
/// per-tool-call tick deliberately does *not* do this (see its doc comment):
/// it would double this task's real elapsed time into the same shared
/// `TickPacer` whenever both ran concurrently.
async fn drive_session(
    model: Arc<Mutex<HeadlessServer>>,
    ct: CancellationToken,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(DRIVE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tick = Instant::now();
    loop {
        tokio::select! {
            () = ct.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let elapsed = last_tick.elapsed();
                last_tick = Instant::now();
                let mut server = model.lock().unwrap_or_else(PoisonError::into_inner);
                server
                    .advance(elapsed)
                    .map_err(|error| format!("simulation advance failed: {error}"))?;
                for event in server.drain_events() {
                    tracing::debug!(?event, "command event");
                }
            }
        }
    }
}

async fn run_http(
    server: McpServer,
    addr: SocketAddr,
    token: String,
    ct: CancellationToken,
) -> Result<(), String> {
    let listener = fieldcad_mcp::bind_http(addr)
        .await
        .map_err(|error| format!("binding {addr}: {error}"))?;
    fieldcad_mcp::serve_http(
        listener,
        server,
        Some(token),
        fieldcad_mcp::McpConnections::new(),
        ct,
    )
    .await
}

#[cfg(unix)]
async fn run_unix(server: McpServer, path: PathBuf, ct: CancellationToken) -> Result<(), String> {
    // The lock guards this path for the server's whole life: it is what
    // makes another server's stale-socket cleanup refuse this path while we
    // are bound to it, so it must outlive both the listener and the
    // socket-file removal below.
    let (listener, _lock) = fieldcad_mcp::bind_unix(&path).await?;
    let result = fieldcad_mcp::serve_unix(
        listener,
        server,
        None,
        fieldcad_mcp::McpConnections::new(),
        ct,
    )
    .await;
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(not(unix))]
async fn run_unix(_server: McpServer, path: PathBuf, _ct: CancellationToken) -> Result<(), String> {
    Err(format!(
        "--unix {} requested, but Unix domain sockets are not available on this platform",
        path.display()
    ))
}
