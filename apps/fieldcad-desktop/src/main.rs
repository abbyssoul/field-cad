use std::process::ExitCode;

const HELP: &str = "\
Field CAD — interactive laboratory for spatial fields

USAGE:
    fieldcad [OPTIONS]

OPTIONS:
    --smoke [FRAMES]     Render offscreen with no window, report the adapter,
                         and exit. Use this to check whether a graphics backend
                         works on this machine without risking a windowed
                         session.
    --exit-after SECONDS Open the window normally, then quit on its own after
                         SECONDS. Use this the first time you try a windowed run
                         on a machine where one has previously misbehaved.
    -h, --help           Show this message.

ENVIRONMENT:
    WGPU_BACKEND            vulkan | gl | metal | dx12 (comma-separated list)
    FIELDCAD_PRESENT_MODE   vsync | no-vsync | fifo | mailbox | immediate
    FIELDCAD_FORCE_FALLBACK 1 to demand a software adapter
    RUST_LOG                e.g. fieldcad_desktop=debug
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "fieldcad_desktop=info,fieldcad_simulation=info,wgpu_core=warn,wgpu_hal=warn".into()
            }),
        )
        .init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("-h" | "--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("--smoke") => {
            let frames = args
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(60);
            run_smoke_test(frames)
        }
        Some("--exit-after") => {
            let Some(seconds) = args.next().and_then(|value| value.parse::<f64>().ok()) else {
                eprintln!("--exit-after needs a duration in seconds\n\n{HELP}");
                return ExitCode::FAILURE;
            };
            launch(Some(std::time::Duration::from_secs_f64(seconds.max(0.1))))
        }
        Some(unknown) => {
            eprintln!("unrecognised argument '{unknown}'\n\n{HELP}");
            ExitCode::FAILURE
        }
        None => launch(None),
    }
}

fn launch(lifetime: Option<std::time::Duration>) -> ExitCode {
    match fieldcad_desktop::run_for(lifetime) {
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
