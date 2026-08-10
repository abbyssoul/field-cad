# Task: analytic solvers' `sample()` path re-allocates every tick a body moves

## Goal

Eliminate the per-tick allocation cascade in analytic field sampling
(gravity today, electrostatics shares the same shape) the same way
`forces-in-place-accumulation` eliminated it from force evaluation: reuse
fixed-shape buffers across ticks instead of allocating fresh ones, for a
scene where source positions change every tick (any body under active
dynamics) and therefore every existing per-tick cache is invalidated before
it can help.

## Current limitation

Found profiling `earth-moon-2.fcscene` (2 gravitating bodies,
`VelocityVerlet`) directly — see `crates/fieldcad-bench/examples/profile_scene.rs`
and its README section for how to reproduce against any saved scene. That
scene ticks at ~142µs/tick with **182 allocations/tick**, even though its
actual physics is nearly free (2 bodies; `gravity/forces` in the synthetic
suite measures tens of nanoseconds at this body count, consistent with the
`forces-in-place-accumulation` fix already landing). `valgrind --tool=callgrind`
on the same loop attributes the cost instead to sampling for presentation:

- **58%** combined: `fieldcad_newtonian_gravity::evaluate_sources` and
  `fieldcad_superposition::contribution` — the analytic superposition,
  called once per output sample point.
- **17%**: `memmove`, consistent with `Vec` growth/copy in the sampling path
  rather than the (now allocation-free) force path.

`NewtonianGravitySolver` already has a per-publish cache for exactly this —
`SampleCache<NewtonianSample>` (`crates/fieldcad-plugin-api/src/lib.rs:402`,
capacity 16, `plugins/gravity/src/lib.rs:164`) — so that gravity's two
channels (`GRAVITATIONAL_ACCELERATION_HANDLE`, `GRAVITATIONAL_POTENTIAL_HANDLE`)
share one evaluation per geometry within a publish instead of two
(`plugins/gravity/src/lib.rs:180-198`, `samples_for`). That cache is
correctly cleared on `on_world_changed`
(`plugins/gravity/src/lib.rs:174-178`) — necessary, since a moved source
really does invalidate a cached sample. But for any scene where bodies move
every tick (an orbit, or anything else under active `VelocityVerlet`/
`SymplecticEuler` integration), `on_world_changed` fires every tick, so the
cache is cleared every tick, so `SampleCache::get_or_try_insert_with`'s
`compute` closure (`crates/fieldcad-plugin-api/src/lib.rs:418-436`) — which
`.collect()`s a fresh `Vec<NewtonianSample>` from
`fieldcad_newtonian_gravity::evaluate_geometry`
(`crates/fieldcad-newtonian-gravity/src/lib.rs:67-75`) and converts it into a
fresh `Arc<[T]>` (line 430) — runs and allocates on every tick, for every
geometry (each visible plane, box, sphere, and the probe set). Downstream,
`NewtonianGravitySolver::sample` allocates two more fresh `Vec`s per channel
per geometry (`plugins/gravity/src/lib.rs:186,189,193`: `validity`, then a
`values` vector per channel), and `FieldBatch::new`
(`crates/fieldcad-core/src/sampling.rs:542`) allocates again to build the
published batch. None of these buffers change shape tick to tick — the
geometry (sample count) and channel set are stable for a session, only the
*values* change — which is exactly the condition
`forces-in-place-accumulation` used to justify reusing a scratch `Vec`
instead of reallocating one.

This is not gravity-specific. `fieldcad-electrostatics` samples through the
same `fieldcad-superposition` kernel with the same shape, and
`fieldcad-electromagnetism`'s `sample_yee_fields`
(`plugins/electromagnetism/src/lib.rs:1307`) independently allocates fresh
`values`/`gradients`/`validity` `Vec`s per call
(lines 1335-1336, 1363) — the same pattern, worth checking once a fix
lands here, though its dominant cost is `centred_fields` itself (see
`docs/tasks/maxwell-full-grid-diagnostics-every-tick.md`) rather than
allocation shape.

## Required behavior

- `SampleCache<T>` (or a caller-owned equivalent) should own a
  reusable per-geometry output buffer, resized (not reallocated) when a
  geometry's sample count is unchanged from last time, rather than
  `compute()?.into()` building a fresh `Arc<[T]>` on every cache miss. Since
  the cache already keys by `SampleGeometry` equality
  (`crates/fieldcad-plugin-api/src/lib.rs:427`), the natural place to retain
  a buffer is per cache entry, reused across the clear-and-recompute cycle a
  moving body forces every tick — not just across channels within one
  publish, which is all it currently helps with.
- `evaluate_geometry` (`crates/fieldcad-newtonian-gravity/src/lib.rs:67`)
  writing into a caller-provided `&mut [NewtonianSample]` (mirroring
  `EquationSystemSolver::add_forces`'s `out: &mut [DVec3]` shape from the
  just-landed force refactor) is one plausible route, if `SampleCache`'s
  `Arc<[T]>` sharing (needed because multiple channels borrow the same slice
  concurrently within one publish) can be reconciled with a mutable
  reuse buffer — decide during implementation whether that means the cache
  owns `Box<[T]>` scratch storage it publishes as `Arc<[T]>` only once fully
  written, or some other shape; this needs more design than the force path
  did, precisely because of the multi-reader sharing `SampleCache` exists
  for.
- `NewtonianGravitySolver::sample`'s per-channel `values`/`validity`
  extraction (`plugins/gravity/src/lib.rs:186-195`) and `FieldBatch::new`
  are secondary — worth reusing scratch buffers for once the primary
  `evaluate_geometry`/cache allocation is fixed, but check whether they
  remain a meaningful fraction of the 182 allocations/tick first rather than
  assuming.
- Preserve existing behavior exactly: cache correctness (a moved source's
  next sample must reflect the move), the channel-sharing property
  `SampleCache` exists for, and eviction at `capacity` (currently 16,
  oldest-first). This is an allocation-shape change, not a behavior change.

## Tests and acceptance

- `fieldcad-bench`'s `gravity/sample-plane` and `gravity/sample-by-charges`
  medians should not regress (ideally improve) — these already exist and
  sweep exactly this path.
- `crates/fieldcad-bench/examples/profile_scene.rs` against
  `earth-moon-2.fcscene` (or any scene with moving bodies) should show a
  materially lower allocations-per-tick count than the 182 baseline recorded
  here. Re-run under `valgrind --tool=callgrind` to confirm
  `evaluate_sources`/`contribution`'s *own* cost (the actual math, which
  isn't going away — 2 sources × several thousand sample points is real
  work) now dominates over `memmove`/allocator overhead, rather than the
  reverse.
- `SampleCache` has no dedicated unit tests today (`crates/fieldcad-plugin-api/src/lib.rs`'s
  `mod tests` doesn't mention it) — add coverage for "a cache hit skips
  `compute`" and "a cache hit reuses the same buffer" as part of this
  change, not just for gravity's integration-level tests that exercise it
  indirectly.
- No observable behavior/physics difference: a sampled value at a given
  world state must be bit-identical to before.

## Relevant code

- `crates/fieldcad-plugin-api/src/lib.rs:402` — `SampleCache`.
- `plugins/gravity/src/lib.rs:159-198` — `NewtonianGravitySolver`, its cache
  field, and `sample`.
- `crates/fieldcad-newtonian-gravity/src/lib.rs:67` — `evaluate_geometry`.
- `plugins/electrostatics/src/lib.rs` — same shape, not yet checked in
  detail; revisit once the gravity fix lands.
- `crates/fieldcad-core/src/sampling.rs:542` — `FieldBatch::new`.
- `crates/fieldcad-bench/examples/profile_scene.rs` — reproduction harness.

Found during the same performance-analysis pass as
`docs/tasks/maxwell-full-grid-diagnostics-every-tick.md`; not urgent,
recorded so it isn't lost. Distinct from that doc: this one is about
allocation shape in the sampling path generally (any analytic solver, any
scene with moving bodies), not about Maxwell's specific full-grid
diagnostics cost.
