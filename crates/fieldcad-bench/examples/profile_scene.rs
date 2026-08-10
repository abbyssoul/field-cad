//! Load one authored `.fcscene` file and tick it in a loop.
//!
//! Complements the crate's synthetic benchmark suite (`workload::benchmarks`),
//! which sweeps seed-free scenes built for reproducible O() fits, not for
//! matching what a particular saved session actually contains. Scene
//! save/load (`fieldcad-scene-document`) means a real, hand-authored scene —
//! the one a regression was actually seen in — can now be pointed at directly:
//! this is the harness for that, meant to sit under a profiler (`valgrind
//! --tool=callgrind`, or `perf`/`cargo flamegraph` where `perf_event_paranoid`
//! allows it) rather than to produce a trusted headline number on its own.
//!
//! Counts allocations across the loop with a wrapping global allocator so a
//! steady-state allocation regression shows up even without a sampling
//! profiler attached.
//!
//! Usage: cargo run --release -p fieldcad-bench --example profile_scene -- <path.fcscene> [ticks]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use fieldcad_core::{SessionId, World};
use fieldcad_electromagnetism::ElectromagnetismPlugin;
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_gravity::NewtonianGravityPlugin;
use fieldcad_simulation::{PluginRegistration, RuntimeConfig, SimulationRuntime, Subscription};
use glam::{UVec2, UVec3};

struct CountingAlloc;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

fn cpu_catalog() -> Vec<PluginRegistration> {
    vec![
        PluginRegistration::with_default_configuration(Box::new(ElectrostaticsPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(NewtonianGravityPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(ElectromagnetismPlugin::new())),
    ]
}

const USAGE: &str = "\
usage: profile_scene <path.fcscene> [ticks]

    path.fcscene   An authored scene document (File > Save in the desktop app).
    ticks          Ticks to run after warmup, default 2000.
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let path = PathBuf::from(path);
    let ticks: u64 = match args.next() {
        Some(value) => match value.parse() {
            Ok(ticks) => ticks,
            Err(_) => {
                eprintln!("error: '{value}' is not a valid tick count\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        },
        None => 2000,
    };
    if ticks == 0 {
        eprintln!("error: ticks must be at least 1\n\n{USAGE}");
        return ExitCode::FAILURE;
    }

    let outcome =
        fieldcad_scene_document::load_newest_valid(&path).expect("scene file loads and parses");
    let doc = outcome.document;

    let (plugins, warnings) =
        fieldcad_scene_document::resolve_plugins(cpu_catalog(), &doc.field_systems)
            .expect("document plugin composition resolves against the CPU catalog");
    for warning in &warnings {
        eprintln!(
            "warning: {} document={} linked={}",
            warning.plugin, warning.document_version, warning.linked_version
        );
    }

    let mut config = RuntimeConfig::new(doc.domain, doc.time_step, SessionId::from_u128(1))
        .with_world(World::from_document(doc.world))
        .with_scene_scale(doc.scene_scale)
        .with_integration_scheme(doc.integration_scheme)
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

    let mut runtime = SimulationRuntime::new(config).expect("scene builds into a valid runtime");

    // Warm up: first tick pays for any lazy first-use allocation (scratch
    // buffers growing from empty) that every later tick should not repeat.
    runtime.step_once().expect("warmup tick succeeds");

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let started = Instant::now();
    for _ in 0..ticks {
        runtime
            .step_once()
            .expect("a Courant-limited step is accepted");
    }
    let elapsed = started.elapsed();
    let after = ALLOCATIONS.load(Ordering::Relaxed);

    println!("scene: {}", path.display());
    println!("ticks: {ticks}");
    println!("total: {:?}", elapsed);
    println!("per tick: {:?}", elapsed / ticks as u32);
    println!(
        "allocations over {ticks} ticks: {} ({:.3} per tick)",
        after - before,
        (after - before) as f64 / ticks as f64
    );
    ExitCode::SUCCESS
}
