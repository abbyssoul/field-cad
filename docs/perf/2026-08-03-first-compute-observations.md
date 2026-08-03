# First compute observations

Date: 2026-08-03
Harness: `crates/fieldcad-bench`, schema version 1
Hardware: 13th Gen Intel Core i7-1370P, 20 threads, 64 GB
Toolchain: rustc 1.94.1, `--release`

**These are observations, not budgets.** Milestone 5's review gate sets budgets
from measurements on named hardware; this is the starting point that gate needs,
recorded before Milestone 6 changes the Maxwell solver. Nothing here has been
optimized, and nothing here should be optimized on the strength of one machine.

The harness measures the **CPU reference** path in `f64`. The desktop runs `f32`
GPU backends, which are not yet measurable from here — see the gap at the end.

## Where a default tick goes

At the shipped scene (32³ cells, 1 charge, 1 probe, 1 plane at 33², sparse 3D at
stride 8), one `runtime/tick` costs **1.543 ms**. It decomposes as:

| stage | cost | share |
| --- | --- | --- |
| Yee field advance (`maxwell/step`) | 397 µs | 26% |
| Sampling 5 Maxwell channels over 1154 points each | ~623 µs | 40% |
| Full-grid conservation diagnostics | ~559 µs | 36% |

The two published-but-unrequested costs together outweigh the physics by nearly
3:1. That is the headline for whoever closes the performance gate.

## Measured scaling

Every sweep below varies one parameter and reports the fitted log-log exponent.

| benchmark | parameter | measured | declared | per unit at largest size |
| --- | --- | --- | --- | --- |
| `maxwell/step` | cells | O(N^1.00) | O(N) | 11.9 ns/cell |
| `maxwell/sample-plane` | samples | O(N^1.00) | O(N) | 108 ns/sample |
| `maxwell/diagnostics` | cells | O(N^1.03) | O(N) | 17.8 ns/cell |
| `maxwell/solver-init` | cells | no clean fit | O(N) | 6.5 ns/cell to 48³ |
| `maxwell/solver-init-by-charges` | charges | O(N^0.77) | O(N) | see note |
| `maxwell/edit-probe` | cells | O(N^0.00) | O(1) | flat at 133 ns |
| `runtime/tick` | cells | O(N^0.62) | O(N) | 33 ns/cell at 64³ |
| `runtime/commit-charge-edit` | cells | no clean fit | O(N) | 31 ns/cell at 64³ |
| `runtime/commit-probe-edit` | cells | O(N^0.52) | O(N) | 21 ns/cell at 64³ |

Nothing grew faster than its declared complexity.

## Findings worth carrying into the gate

### 1. Diagnostics are unconditional and cost as much as the physics

`SimulationRuntime::publish_snapshot` calls `solver.diagnostics()` for every
enabled plugin *before* the sampling-policy check
(`crates/fieldcad-simulation/src/runtime.rs:819`). Maxwell's `yee_conservation`
sweeps the whole lattice for energy and both divergence residuals, so a full-grid
pass runs on every publication no matter how little the visualizer subscribed
to — 559 µs of a 1.543 ms tick at the default scene, and 4.65 ms at 64³.

This is a design question, not a micro-optimization: diagnostics are part of the
scientific legibility the product promises, so the answer is probably about
*when* they are computed rather than whether. Left untouched.

### 2. Solver init has a cache cliff between 48³ and 64³

Per-cell cost holds near 6.4 ns from 16³ through 48³, then jumps to 18.0 ns at
64³ — a 2.8x step, not a slope. The harness correctly refuses to quote an
exponent through it (`scatter = 0.296`). At 64³ the constrained state touches a
potential array plus three `DVec3` components, around 8 MB, which is past
last-level cache on this part.

Worth knowing before Milestone 6 chooses its deposition data layout.

### 3. Source count is cheaper than fixed lattice work

`solver-init-by-charges` measures O(charges^0.77), below its linear declaration,
because a large charge-independent cost — the gradient pass and allocation —
dominates at low source counts. Per-source marginal cost is roughly 85 µs at 32³.
The sub-linear exponent is an artifact of that fixed term, not a sub-linear
algorithm.

### 4. The Milestone 5 edit-skip optimization is confirmed

`maxwell/edit-probe` is flat at **133 ns across a 64x range in cells** (4.1k to
262k). The source-equality check added in the Milestone 5 remediation is doing
its job: probe and slice-plane drags no longer rebuild the lattice. This
benchmark exists to catch that regressing.

### 5. Runtime ticks look sub-linear because of a large fixed cost

`runtime/tick` measures O(N^0.62) only because plane and probe sampling does not
depend on cell count and dominates at small lattices. At 16³ a tick is 678 µs
against a 48 µs field advance. The exponent is honest arithmetic on the sweep,
but the shape is fixed-cost-plus-linear rather than genuinely sub-linear.

## Known gap: GPU backends are not measured

The Milestone 5 gate explicitly wants GPU step submission and full-grid readback
profiled. `GpuMaxwellBackend` is `pub(crate)` inside `fieldcad-desktop`, so the
harness cannot construct it. Closing this needs a deliberate decision — export
the backend, or move the host-owned GPU backends into a crate both the desktop
and the harness can depend on. That is an architecture choice and was left to
whoever closes the gate rather than forced by a benchmark.

## Reproducing

```sh
cargo run --release -p fieldcad-bench
cargo run --release -p fieldcad-bench -- --save-baseline perf.json
```

Run-to-run variation on this machine was 1–3%.
