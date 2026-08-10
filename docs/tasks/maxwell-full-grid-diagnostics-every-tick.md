# Task: Maxwell's full-grid diagnostics sweep runs on every tick, unconditionally

## Goal

Stop paying an `O(cells)` cost every tick for energy/divergence diagnostics
nobody may be looking at. Either gate the full-grid sweep behind whether a
subscriber actually wants it, or make the per-cell reconstruction it's built
from (`centred_fields`) cheap enough that the unconditional cost stops
dominating.

## Current limitation

Found via `valgrind --tool=callgrind` on `fieldcad-bench`'s existing
`maxwell/*` sweeps (see `crates/fieldcad-bench/examples/profile_scene.rs` and
its README section for how to reproduce). One function,
`fieldcad_electromagnetism::centred_fields` (`plugins/electromagnetism/src/lib.rs:1586`,
de-staggers the Yee lattice's `E`/`B` storage onto one shared cell-centred
point), dominates every Maxwell benchmark's instruction count:

- **70%** of `maxwell/sample-plane` (up to 107ms at 66k samples — the single
  largest absolute number in the whole benchmark suite).
- **36%**, alongside `yee_conservation` itself at **47%**, of
  `maxwell/diagnostics` (4.8ms at 262k cells).
- Even shows up at 18% inside an isolated `maxwell/step` capture, because the
  benchmark's own per-rep `setup` (`create_solver`) computes
  `yee_conservation` once for `initial_field_energy`
  (`plugins/electromagnetism/src/lib.rs:728`) — a one-time cost there, not
  representative of steady-state `step()`.

The steady-state part of this that *is* real: `SimulationRuntime::publish_snapshot`
(`crates/fieldcad-simulation/src/runtime.rs:2074`) calls
`slot.solver().diagnostics()` unconditionally for every enabled plugin, on
every publish, at line 2106 — before the `unchanged_by_tick`/`deferred` reuse
checks a few lines below that *do* gate ordinary channel sampling. For
`MaxwellSolver`, `diagnostics()` (`plugins/electromagnetism/src/lib.rs:1547`)
calls `yee_conservation` (`plugins/electromagnetism/src/lib.rs:1397`), which
walks every one of the domain's cells — all 262,144 of them at 64³ — calling
`centred_fields` (via `energy_at_cell`/`electric_divergence`/
`magnetic_divergence`) at each one, regardless of whether any subscriber
reads the energy/divergence channels this diagnostic feeds
(`crates/fieldcad-core/src/snapshot.rs:92`, `SolverDiagnostic`). Under
realtime playback (`apply_tick_inner` publishes with
`SamplingPolicy::TimeDependentOnly`, `crates/fieldcad-simulation/src/runtime.rs:1771`),
this runs every single tick.

`runtime/tick` at 64³ costs 11.6ms; `maxwell/step` (the leapfrog advance
alone) costs 3.4ms at the same size. Diagnostics is a real fraction of the
7ms gap between them — comparable in size to `maxwell/sample-plane`'s own
per-tick sampling cost, paid whether or not a diagnostics panel is even open.

## Required behavior

- Diagnostics must not be computed for a plugin whose result nothing is
  subscribed to read. The runtime already tracks per-channel subscription
  (`geometries_by_channel`, `crates/fieldcad-simulation/src/runtime.rs:2084`);
  extend that same notion to whether energy/divergence diagnostics are wanted
  at all this publish, and skip the `slot.solver().diagnostics()` call
  (line 2106) when they are not. Decide during implementation whether that's
  a session-level toggle (a diagnostics panel open/closed) or something
  finer — but it must default to *not* silently dropping a diagnostic a
  client is actually reading, matching this crate's existing "no unrequested
  work, no silently missing data" discipline elsewhere (e.g. the
  `unchanged_by_tick`/`reuse_from` logic immediately below this call).
- Independently of gating: `centred_fields`' cost itself is worth reducing
  regardless of how often it's called, since `maxwell/sample-plane` legitimately
  needs it for every subscribed sample, gating or not. Profile whether the
  four-lookup average for `E` and two-lookup average for `B`
  (`plugins/electromagnetism/src/lib.rs:1594-1619`) can be restructured for
  better cache locality (`linear_index`'s `x + counts.x * (y + counts.y * z)`
  layout means the `y`/`z`-neighbour lookups `centred_fields` needs are not
  adjacent in memory), or whether `yee_conservation`'s full-grid loop
  (`plugins/electromagnetism/src/lib.rs:1406-1420`) can compute energy and
  both divergences from one pass over each cell's already-loaded neighbours
  instead of the current three separate `centred_fields`-driven calls
  (`energy_at_cell`, `electric_divergence`, `magnetic_divergence`) per cell.
- Whichever direction is taken, `maxwell/diagnostics` and `maxwell/sample-plane`
  are the acceptance benchmarks (`fieldcad-bench --filter maxwell/diagnostics`,
  `--filter maxwell/sample-plane`) — a fix should show up as a real median-ns
  drop at fixed cell/sample count, not just a smaller callgrind share.

## Tests and acceptance

- `fieldcad-bench`'s `maxwell/diagnostics` and `maxwell/sample-plane` medians
  should drop at fixed scene size; save a baseline first
  (`fieldcad-bench --save-baseline before.json`) and compare after
  (`--baseline before.json`).
- If diagnostics gating is added: a test that an ungated (subscribed)
  diagnostics client still receives energy/divergence values every tick, and
  a test that disabling the subscription actually skips the
  `slot.solver().diagnostics()` call (e.g. via a call-counting test double
  solver, the way other runtime tests already verify solver call
  discipline — see `crates/fieldcad-simulation/src/runtime.rs`'s existing
  `unchanged_by_tick`/reuse tests for the pattern).
- No change to what a subscribed diagnostics reader actually observes — this
  is a cost change, not a behavior change, for anyone currently reading
  energy/divergence.

## Relevant code

- `plugins/electromagnetism/src/lib.rs:1586` — `centred_fields`.
- `plugins/electromagnetism/src/lib.rs:1397` — `yee_conservation`.
- `plugins/electromagnetism/src/lib.rs:1547` — `MaxwellSolver::diagnostics`.
- `crates/fieldcad-simulation/src/runtime.rs:2074` — `publish_snapshot`, in
  particular line 2106 (`diagnostics.extend(...)`, unconditional) versus lines
  2150-2160 (`reuse_from`, the existing conditional-reuse pattern to mirror).
- `crates/fieldcad-bench/examples/profile_scene.rs` — how to reproduce the
  callgrind capture this doc is based on against a real or synthetic scene.

Found during a performance-analysis pass requested after the
`forces-in-place-accumulation` task landed; not urgent, recorded so it isn't
lost. See also `docs/tasks/sample-cache-still-allocates-every-tick.md` for a
related but distinct finding from the same pass (allocation shape in the
sampling path generally, rather than Maxwell's specific full-grid cost).
