//! Runs the headless model with nothing driving it yet.
//!
//! No transport is attached — this is the shape from `docs/mcp-plan.md`
//! phase 2, proving the model runs detached from the desktop app, on a
//! machine with no display and no GPU. Phase 3 onward attaches a real command
//! source (MCP or otherwise) to the same [`fieldcad_server::HeadlessServer`]
//! this binary builds.

use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use fieldcad_server::HeadlessServer;
use fieldcad_simulation::CommandPayload;

const STATUS_INTERVAL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(16);

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fieldcad_server=info,fieldcad_simulation=info".into()),
        )
        .init();

    let source = match fieldcad_server::default_session() {
        Ok(source) => source,
        Err(error) => {
            tracing::error!(%error, "failed to build the default session");
            return ExitCode::FAILURE;
        }
    };
    let mut server = HeadlessServer::new(source);
    if let Err(error) = server.submit(CommandPayload::Play) {
        tracing::error!(%error, "failed to start the session");
        return ExitCode::FAILURE;
    }

    tracing::info!("fieldcad-server running headless, no window or GPU attached");

    let mut last_status = Instant::now();
    let mut last_tick = Instant::now();
    loop {
        let elapsed = last_tick.elapsed();
        last_tick = Instant::now();
        if let Err(error) = server.advance(elapsed) {
            tracing::error!(%error, "simulation advance failed");
            return ExitCode::FAILURE;
        }
        for event in server.drain_events() {
            tracing::debug!(?event, "command event");
        }

        if last_status.elapsed() >= STATUS_INTERVAL {
            let status = server.simulation_status();
            tracing::info!(
                tick = status.tick(),
                time_seconds = status.time_seconds(),
                mode = ?status.mode(),
                "session status"
            );
            last_status = Instant::now();
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
