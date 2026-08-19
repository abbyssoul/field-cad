use std::{net::SocketAddr, path::PathBuf, process::ExitCode, time::Duration};

use clap::Parser;
use fieldcad_desktop::LaunchOptions;

/// Active only in builds compiled with `--features dhat`. dhat needs to own
/// every allocation to attribute it to a call site, so the global allocator
/// swap has to live at the top of the binary crate, not behind a runtime
/// flag.
#[cfg(feature = "dhat")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const ENV_HELP: &str = "\
ENVIRONMENT:
    WGPU_BACKEND            vulkan | gl | metal | dx12 (comma-separated list)
    FIELDCAD_PRESENT_MODE   vsync | no-vsync | fifo | mailbox | immediate
    FIELDCAD_FORCE_FALLBACK 1 to demand a software adapter
    RUST_LOG                e.g. fieldcad_desktop=debug";

#[derive(Parser)]
#[command(
    name = "fieldcad",
    version,
    about = "Field CAD — interactive laboratory for spatial fields",
    after_help = ENV_HELP
)]
struct Cli {
    /// Render offscreen with no window, report the adapter, and exit. Use
    /// this to check whether a graphics backend works on this machine
    /// without risking a windowed session.
    #[arg(long, value_name = "FRAMES", num_args = 0..=1, default_missing_value = "60")]
    smoke: Option<u32>,

    /// Open the window normally, then quit on its own after SECONDS. Use
    /// this the first time you try a windowed run on a machine where one
    /// has previously misbehaved.
    #[arg(long, value_name = "SECONDS")]
    exit_after: Option<f64>,

    /// Start with the embedded MCP server already listening at ADDRESS
    /// (e.g. 127.0.0.1:8642) and print its bearer token to the startup log,
    /// instead of leaving MCP off until a user opens the panel. For an agent
    /// that launches this process itself and needs to connect immediately.
    #[arg(long, value_name = "ADDRESS")]
    mcp: Option<SocketAddr>,

    /// Compose the test-only live-expression field fixture. This flag is
    /// available only in binaries built with `--features expression-fixture`.
    #[arg(long)]
    expression_fixture: bool,

    /// Open this fieldcad.scene/v1 document at startup instead of the
    /// built-in demo scene. Fails to start, rather than falling back to the
    /// demo scene, if the file cannot be loaded.
    #[arg(value_name = "SCENE")]
    scene: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Held for the rest of `main`; dropping it (on any return path below,
    // including the smoke-test and CLI-error early returns) is what flushes
    // `dhat-heap.json`. A killed process (Ctrl-C, window-manager kill) skips
    // the drop and writes nothing — quit the app normally (window close or
    // the app's own Quit action) to capture a profile.
    #[cfg(feature = "dhat")]
    let _profiler = dhat::Profiler::builder()
        .file_name("dhat-heap-desktop.json")
        .build();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "fieldcad_desktop=info,fieldcad_simulation=info,wgpu_core=warn,wgpu_hal=warn".into()
            }),
        )
        .init();

    let cli = Cli::parse();

    if cli.expression_fixture && !cfg!(feature = "expression-fixture") {
        eprintln!(
            "Error: --expression-fixture requires building with --features expression-fixture"
        );
        return ExitCode::FAILURE;
    }

    if let Some(frames) = cli.smoke {
        return run_smoke_test(frames);
    }

    let options = LaunchOptions {
        lifetime: cli
            .exit_after
            .map(|seconds| Duration::from_secs_f64(seconds.max(0.1))),
        mcp: cli.mcp,
        open_path: cli.scene,
        expression_fixture: cli.expression_fixture,
    };
    match fieldcad_desktop::run_for(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Exercise the GPU path without a window, so a misbehaving compositor or
/// surface cannot be part of the result.
fn run_smoke_test(frames: u32) -> ExitCode {
    let config = fieldcad_desktop::GpuConfig::from_env();
    println!(
        "Offscreen smoke test: backends={:?} present_mode={:?} force_fallback={}",
        config.backends, config.present_mode, config.force_fallback_adapter
    );

    match pollster::block_on(fieldcad_desktop::smoke_test(config, frames)) {
        Ok(report) => {
            println!(
                "OK — rendered {} frames on {} ({}, {})",
                report.frames, report.adapter, report.backend, report.device_type
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("FAILED — {error}");
            ExitCode::FAILURE
        }
    }
}
