//! Headless allocation/memory profiling pass over a real saved scene.
//!
//! Loads an `.fcscene` document exactly the way the desktop app's
//! `build_session` does (same domain/plugin/subscription wiring), then runs
//! it for a fixed wall-clock duration calling `advance_running()` back to
//! back — the same call the desktop's per-frame loop makes while the sim is
//! playing — sampling process RSS periodically to reproduce the
//! Diagnostics panel's memory plot outside the GUI.
//!
//! Build with the `dhat` feature to also capture a `dhat-heap.json` call-site
//! allocation profile for the same run:
//!
//! ```sh
//! cargo run --release -p fieldcad-bench --example profile_scene --features dhat -- \
//!     ~/Documents/field-cad/earth-moon-titan.fcscene 60
//! ```

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use fieldcad_electromagnetism::ElectromagnetismPlugin;
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_gravitostatics::NewtonianGravityPlugin;
use fieldcad_simulation::{PluginRegistration, RuntimeConfig, SimulationRuntime, Subscription};
use glam::{UVec2, UVec3};

#[cfg(feature = "dhat")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    #[cfg(feature = "dhat")]
    let _profiler = dhat::Profiler::builder().build();

    let mut args = std::env::args().skip(1);
    let scene_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: profile_scene <scene.fcscene> [duration_seconds]");
    let duration_seconds: u64 = args
        .next()
        .map(|value| value.parse().expect("duration_seconds is an integer"))
        .unwrap_or(60);

    let outcome = fieldcad_scene_document::load_newest_valid(&scene_path)
        .unwrap_or_else(|error| panic!("could not load {}: {error}", scene_path.display()));
    let doc = outcome.document;

    let catalog = vec![
        PluginRegistration::with_default_configuration(Box::new(ElectrostaticsPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(NewtonianGravityPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(ElectromagnetismPlugin::new())),
    ];
    let (plugins, warnings) = fieldcad_scene_document::resolve_plugins(catalog, &doc.field_systems)
        .expect("scene's plugin composition resolves against this build's catalog");
    for warning in &warnings {
        eprintln!(
            "warning: {} document={} linked={}",
            warning.plugin, warning.document_version, warning.linked_version
        );
    }

    // The desktop app drives this scene with a GPU (f32) evaluator; this
    // headless pass has no GPU, so it runs the CPU (f64) reference evaluator
    // instead — same object count, same solver code paths, same allocation
    // shape, just not the same domain precision the saved scene declares.
    let domain = fieldcad_core::Domain::new(
        doc.domain.bounds(),
        doc.domain.resolution(),
        doc.domain.boundaries(),
        fieldcad_core::Precision::F64,
    );

    let initial_sequence = doc.next_snapshot_sequence();
    let mut config = RuntimeConfig::new(
        domain,
        doc.time_step,
        fieldcad_core::SessionId::from_u128(1),
    )
    .with_world(fieldcad_core::World::from_document(doc.world))
    .with_expressions(doc.expressions)
    .with_scene_scale(doc.scene_scale)
    .with_integration_scheme(doc.integration_scheme)
    .with_initial_sequence(initial_sequence)
    .with_subscription(
        Subscription::PROBES_ONLY
            .with_planes(UVec2::splat(33))
            .with_domain_stride(8)
            .with_boxes(UVec3::splat(9))
            .with_spheres(9),
    );
    for plugin in plugins {
        config = config.with_plugin_registration(plugin);
    }
    let mut runtime = SimulationRuntime::new(config).expect("document composes a valid runtime");
    runtime
        .play()
        .expect("a freshly built session can start running");

    let budget = Duration::from_secs(duration_seconds);
    let started = Instant::now();
    let mut ticks = 0u64;
    let mut rss_samples: Vec<(Duration, u64)> = Vec::new();
    let mut next_sample = Duration::ZERO;
    let sample_every = Duration::from_millis(200);

    loop {
        let elapsed = started.elapsed();
        if elapsed >= budget {
            break;
        }
        if runtime
            .advance_running()
            .expect("scene stays within the runtime's own validity checks")
        {
            ticks += 1;
        }
        if elapsed >= next_sample {
            rss_samples.push((elapsed, rss_kib()));
            next_sample += sample_every;
        }
    }

    let (min, max, avg) = summarize(&rss_samples);
    eprintln!(
        "ran {ticks} ticks over {:.1}s wall-clock ({:.0} ticks/s)",
        started.elapsed().as_secs_f64(),
        ticks as f64 / started.elapsed().as_secs_f64()
    );
    eprintln!(
        "RSS over the run: min {min} KiB, max {max} KiB, avg {avg:.0} KiB, samples {}",
        rss_samples.len()
    );
    // A coarse CSV so a spike can be correlated back to a wall-clock offset
    // without re-running under a heavier profiler.
    let mut csv = String::from("elapsed_ms,rss_kib\n");
    for (elapsed, rss) in &rss_samples {
        csv.push_str(&format!("{},{rss}\n", elapsed.as_millis()));
    }
    fs::write("profile_scene_rss.csv", csv).expect("scratch CSV is writable");
    eprintln!("wrote profile_scene_rss.csv");
}

fn summarize(samples: &[(Duration, u64)]) -> (u64, u64, f64) {
    if samples.is_empty() {
        return (0, 0, 0.0);
    }
    let min = samples.iter().map(|(_, rss)| *rss).min().unwrap();
    let max = samples.iter().map(|(_, rss)| *rss).max().unwrap();
    let avg = samples.iter().map(|(_, rss)| *rss as f64).sum::<f64>() / samples.len() as f64;
    (min, max, avg)
}

/// `VmRSS` from `/proc/self/status`, in KiB. Linux-only, but so is this
/// profiling pass — the desktop app's own Diagnostics panel reads the same
/// value on this platform.
fn rss_kib() -> u64 {
    let status = fs::read_to_string("/proc/self/status").expect("/proc/self/status is readable");
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("VmRSS line is present and numeric")
}
