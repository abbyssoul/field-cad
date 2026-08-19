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

use clap::Parser;
use fieldcad_bench::{measure::summarize, report::format_ns};
use fieldcad_core::{SessionId, World};
use fieldcad_electromagnetism::ElectromagnetismPlugin;
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_gravitostatics::NewtonianGravityPlugin;
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
    // Without this override, `GlobalAlloc::realloc`'s default falls back to
    // alloc-copy-dealloc: allocate a new block, `ptr::copy_nonoverlapping`
    // every byte over, free the old one. `System`'s own `realloc` can grow
    // in place when the allocator has room, skipping the copy — forwarding
    // to it here is what keeps this profiler from inventing a copy a real
    // (non-instrumented) release build wouldn't necessarily pay.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
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

#[derive(Parser)]
#[command(
    name = "profile_scene",
    about = "Load one authored .fcscene file and tick it in a loop, meant to sit under a profiler"
)]
struct Cli {
    /// An authored scene document (File > Save in the desktop app).
    #[arg(value_name = "PATH")]
    scene: PathBuf,

    /// Ticks to run after warmup.
    #[arg(
        value_name = "TICKS",
        default_value_t = 2000,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    ticks: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let path = cli.scene;
    let ticks = cli.ticks;

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

    // Each tick timed individually rather than once over the whole loop, so
    // the reported cost is a min/mean/median/max distribution over `ticks`
    // independent samples — the same statistical footing `fieldcad-bench`
    // itself reports — not a single average that a GC pause or scheduler
    // hiccup partway through the run could silently absorb.
    let mut tick_ns = Vec::with_capacity(ticks as usize);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..ticks {
        let start = Instant::now();
        runtime
            .step_once()
            .expect("a Courant-limited step is accepted");
        tick_ns.push(start.elapsed().as_secs_f64() * 1.0e9);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);

    let timing = summarize(tick_ns, 1);

    println!("scene: {}", path.display());
    println!("ticks: {ticks}");
    println!(
        "per tick: min {} | mean {} | median {} | max {} | {} iters",
        format_ns(timing.min_ns),
        format_ns(timing.mean_ns),
        format_ns(timing.median_ns),
        format_ns(timing.max_ns),
        timing.total_iterations()
    );
    println!(
        "allocations over {ticks} ticks: {} ({:.3} per tick)",
        after - before,
        (after - before) as f64 / ticks as f64
    );
    ExitCode::SUCCESS
}
