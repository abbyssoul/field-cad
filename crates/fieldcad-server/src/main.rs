//! Runs the headless model with nothing driving it yet.
//!
//! No transport is attached — this is the shape from `docs/mcp-plan.md`
//! phase 2, proving the model runs detached from the desktop app, on a
//! machine with no display and no GPU. Phase 3 onward attaches a real command
//! source (MCP or otherwise) to the same [`fieldcad_server::HeadlessServer`]
//! this binary builds.
//!
//! Runs standalone: `--scene` loads an authored document instead of the
//! built-in default session, and `--duration` bounds how long the headless
//! loop runs before exiting on its own — useful for smoke-testing or
//! profiling a session without a second process sending Ctrl-C.

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use clap::Parser;
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{CommandPayload, PlaybackSpeed};

#[derive(Parser)]
#[command(
    name = "fieldcad-server",
    version,
    about = "Headless Field CAD simulation server: owns the model with no window or GPU attached"
)]
struct Cli {
    /// Load this authored scene document at startup instead of the
    /// built-in default session.
    #[arg(long, value_name = "PATH")]
    scene: Option<PathBuf>,

    /// Exit on its own after running for this many seconds. Runs until
    /// interrupted (Ctrl-C) if unset.
    #[arg(long, value_name = "SECONDS")]
    duration: Option<f64>,

    /// How often to log a status line, in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 5.0)]
    status_interval: f64,

    /// How often the headless loop polls and advances the session, in
    /// milliseconds.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 16)]
    poll_interval_ms: u64,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fieldcad_server=info,fieldcad_simulation=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let loaded = match &cli.scene {
        Some(path) => fieldcad_server::session_from_document(path).map_err(|error| {
            tracing::error!(%error, scene = %path.display(), "failed to load scene");
        }),
        None => fieldcad_server::default_session()
            .map(|source| (source, PlaybackSpeed::default()))
            .map_err(|error| {
                tracing::error!(%error, "failed to build the default session");
            }),
    };
    let (source, playback_speed) = match loaded {
        Ok(loaded) => loaded,
        Err(()) => return ExitCode::FAILURE,
    };

    let mut server = HeadlessServer::new(source);
    if let Err(error) = server.submit(CommandPayload::SetPlaybackSpeed(playback_speed)) {
        tracing::error!(%error, "failed to apply the scene's playback speed");
        return ExitCode::FAILURE;
    }
    if let Err(error) = server.submit(CommandPayload::Play) {
        tracing::error!(%error, "failed to start the session");
        return ExitCode::FAILURE;
    }

    tracing::info!("fieldcad-server running headless, no window or GPU attached");

    let status_interval = Duration::from_secs_f64(cli.status_interval.max(0.0));
    let poll_interval = Duration::from_millis(cli.poll_interval_ms);
    let duration = cli
        .duration
        .map(|seconds| Duration::from_secs_f64(seconds.max(0.0)));

    let started = Instant::now();
    let mut last_status = Instant::now();
    let mut last_tick = Instant::now();
    loop {
        if duration.is_some_and(|duration| started.elapsed() >= duration) {
            tracing::info!(elapsed = ?started.elapsed(), "requested duration elapsed, exiting");
            return ExitCode::SUCCESS;
        }

        let elapsed = last_tick.elapsed();
        last_tick = Instant::now();
        if let Err(error) = server.advance(elapsed) {
            tracing::error!(%error, "simulation advance failed");
            return ExitCode::FAILURE;
        }
        for event in server.drain_events() {
            tracing::debug!(?event, "command event");
        }

        if last_status.elapsed() >= status_interval {
            let status = server.simulation_status();
            tracing::info!(
                tick = status.tick(),
                time_seconds = status.time_seconds(),
                mode = ?status.mode(),
                "session status"
            );
            last_status = Instant::now();
        }

        std::thread::sleep(poll_interval);
    }
}
