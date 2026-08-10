# fieldcad-bench

Headless compute performance harness. It measures how long solver and runtime
operations take, and how that cost grows with scene size.

It does not optimize anything and it does not encode performance budgets.
Budgets belong to the Milestone 5 review gate, which sets them from measurements
on named hardware.

## Always build with `--release`

A debug-profile number is meaningless here — `f64` lattice arithmetic is roughly
an order of magnitude slower unoptimized.

```sh
cargo run --release -p fieldcad-bench
```

## Driving it

```sh
# Everything. Ends with a list of anything growing faster than it claims to.
cargo run --release -p fieldcad-bench

# Iterate on one path: fewer samples, smaller sweeps.
cargo run --release -p fieldcad-bench -- --filter maxwell/step --quick

# What exists, and what each benchmark sweeps, without running it.
cargo run --release -p fieldcad-bench -- --list

# Machine-readable.
cargo run --release -p fieldcad-bench -- --format json

# Record, change something, then compare.
cargo run --release -p fieldcad-bench -- --save-baseline perf.json
cargo run --release -p fieldcad-bench -- --baseline perf.json --fail-on-regression
```

`--fail-on-regression` exits non-zero when a benchmark is more than 10% slower
than the baseline, or when any measured growth exceeds its declared complexity.
Ten percent is deliberately above the run-to-run noise on a developer machine,
which is typically 1–3%.

## Reading a result

```
maxwell/step
  measures : one Yee leapfrog tick (B half, E full, B half)
  matters  : the inner loop of every running simulation; Milestone 6 changes it
  declared : O(N) in cells
  scene                           cells       median     per unit     noise
  step-32cubed                    32.8k   396.888 µs    12.112 ns      1.0%
  measured : O(cells^1.00)  scatter=0.012  R²=1.000  -> as declared
```

- **per unit** is usually the number to act on. It is what a budget gets written
  in, and it makes two scene sizes directly comparable.
- **noise** is the spread of kept samples. Above ~10% the machine was busy and a
  small difference should not be believed.
- **scatter** is the RMS residual in log space — how far the points sit from the
  fitted power law. Above 0.20 the harness refuses to quote an exponent and says
  `no clean power law`.
- **R²** is reported because it is familiar, but it does *not* decide the
  verdict. A correctly flat cost has almost no variance for a fit to explain, so
  its R² collapses toward zero however clean the measurement is. Judging a
  constant-time operation by R² would condemn exactly the result it should
  confirm.

`WORSE than declared` is the interesting verdict: it means the measured growth
outpaced the complexity the benchmark claims, which is where an accidental
quadratic lives.

## Scenes

Cost here is a function of the scene, so no number is reported without one. The
sweeps are built from `Scene::desktop_default()` — the configuration the
application actually ships: one off-centre 1 nC point charge, one probe, one XY
slice plane at 33 samples per axis, a sparse whole-domain view at stride 8, and
a 32³ periodic domain over ±5 m.

Charges are placed on a deterministic spiral that is never centred and never
lattice-aligned. A charge at the origin makes the periodic seam symmetric — the
symmetry that hid the Milestone 5 seam defect — and would measure an
unrepresentatively tidy scene.

**One difference from the shipped application:** these scenes are `f64`, because
the harness drives the CPU reference solvers. The desktop runs `f32` GPU
backends. See the gap below.

## Adding a benchmark

Add a `Benchmark` to `workload::benchmarks`. It must declare:

- `id` — stable; baselines and tooling key on it;
- `what` and `why` — the operation, and what makes it a hot path;
- `parameter` — cells, charges, or samples, which is what the exponent is in;
- `declared` — the complexity you believe it has;
- `scenes` — a sweep that actually varies that parameter;
- `runner` — a closure driving it through a *published* Interface.

Two unit tests enforce the shape: IDs must be unique and namespaced, and every
sweep must vary its declared parameter — a sweep that does not cannot produce a
slope, so its complexity claim would be untestable.

## Profiling an authored scene

The benchmark suite above only runs synthetic, seed-free scenes — good for a
reproducible O() sweep, useless for chasing a regression seen in one specific
saved session. `examples/profile_scene.rs` loads a real `.fcscene` file (scene
save/load, `fieldcad-scene-document`) and ticks it in a loop instead:

```sh
cargo build --release -p fieldcad-bench --example profile_scene
./target/release/examples/profile_scene ~/Documents/field-cad/earth-moon-2.fcscene 2000
```

It reports wall-clock per tick and a per-tick allocation count (via a counting
global allocator), and it's meant to run *under* a profiler rather than to
produce a trusted headline number by itself:

```sh
valgrind --tool=callgrind --callgrind-out-file=scene.callgrind -- \
    ./target/release/examples/profile_scene ~/Documents/field-cad/earth-moon-2.fcscene 300
callgrind_annotate --threshold=95 scene.callgrind
```

`perf record -g` / `cargo flamegraph` work the same way where
`kernel.perf_event_paranoid` allows it; `valgrind --tool=callgrind` needs no
special kernel permission, at the cost of running the target much slower
(instrumented execution, not sampling), so pass a small tick count.

It builds a CPU-only plugin catalog (`ElectrostaticsPlugin::new()`,
`NewtonianGravityPlugin::new()`, `ElectromagnetismPlugin::new()`), not the
desktop's GPU-backed one — a headless CLI has no GPU device to hand it. A
number from here can therefore overstate a cost the desktop app offloads to
the GPU (electrostatics/gravity/Maxwell `sample()`); it understates nothing,
since every other code path (dynamics, publish, sampling glue, allocation
shape) runs the same on both.

## Why these boundaries

Runners drive `EquationSystemPlugin::create_solver`,
`EquationSystemSolver::{step, sample, diagnostics, on_world_changed}`, and
`SimulationRuntime` rather than solver internals. Milestone 6 is rewriting how
Maxwell advances charge and current; a harness bolted to the current lattice
code would be rewritten with it, while these boundaries are the contract meant
to survive.

## What is not measured

- **Visualization.** Scene extraction and rendering perform acceptably today and
  computation is the target, so they are left out rather than measured badly.
  The `sample-plane` benchmarks are the *solver* side of presentation: the
  interpolation a visualizer's density setting asks the solver to do.
- **GPU backends.** This is a real gap for the Milestone 5 review gate, which
  wants GPU step submission and full-grid readback profiled. `GpuMaxwellBackend`
  is `pub(crate)` in `fieldcad-desktop`, so it cannot be constructed from here.
  Wiring it up needs a deliberate decision — export it, or move the host-owned
  backends into a crate both the desktop and the harness can depend on — and
  that decision was left to whoever closes the gate rather than forced by a
  benchmark.
- **Allocation, cache misses, and instruction counts.** This harness reports
  wall-clock only. Use `perf` or `valgrind --tool=cachegrind` on a single
  filtered benchmark when a number here says *where* to look but not *why*.
