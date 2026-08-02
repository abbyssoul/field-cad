# Remediation report — Milestone 2 gate and the integration increment

Date: 2026-08-02
Companion document: [the review](2026-08-02-milestone-1-2-review.md) whose
findings this addresses.

## Summary

All findings from the review were addressed. The work ran in the six steps the
review recommended, in order.

| | Before | After |
| --- | --- | --- |
| Tests passing | 27 | 91 |
| Crates | 4 + root binary | 4 + `apps/fieldcad-desktop` |
| Rust source | 4,093 lines | 7,871 lines |
| ADRs | 0 | 8 |
| CI | none | 7 jobs across Linux, Windows, macOS |

Verification at completion:

- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets` — clean
- `cargo test --workspace` — 91 passing
  (core 30, plugin-api 3, simulation 24, test-field 5, desktop 29)
- `./target/release/fieldcad` — launches, initializes Vulkan on an Intel Iris Xe
  via Mesa, runs a stable event loop

Nothing was committed. The tree is left untracked, as it was found.

---

## Step 1 — Core model

**`crates/fieldcad-core/src/domain.rs`** (new). `Domain` = `DomainBounds` +
`Resolution` + per-axis `BoundaryConditions` + `Precision`, with `cell_size`,
`cell_lattice`, and `decimated_lattice`. Reaches solvers through `SolverContext`
and rides on every `FieldSnapshot`. Closes A1.

**`crates/fieldcad-core/src/sampling.rs`** (new). `SampleGeometry`
(`Probes`/`Plane`/`Grid`), `PlaneLattice`, `GridLattice`, `FieldColumn`,
`FieldBatch`, and `SampleValidity`. Closes A2 and A7.

**`ids.rs`.** A private `QualifiedName` now backs two distinct newtypes via a
`qualified_id!` macro, so `ChannelId` and `ComponentTypeId` are no longer
substitutable (A4). A test exists specifically so that collapsing them back fails
to compile. Entity IDs dropped their always-zero generation for a monotonic `u64`
(D2), documented at the macro: IDs are never reused, so a stale one can only fail
to resolve.

**`world.rs`.** `WorldCommand::RegisterComponentSchema` moves schema registration
onto the command boundary (D1). `SlicePlaneSpec` gained a `u_axis`, a
`half_extent`, and a `visible` flag, with private fields so it cannot be
constructed unvalidated — which also removed the third copy of the plane
predicate (D3, B3). Added `ObjectShape` (`Point`/`Sphere`/`Box`), `ProbeSpec`
history capacity (D4), and `WorldSnapshot::objects_with` for the component query
Milestone 3 needs.

**`time.rs`.** `SimulationClock` carries an epoch: time is
`epoch_seconds + (tick - epoch_tick) * dt`, so `set_time_step` preserves elapsed
time instead of retroactively rewriting it (D5). `ClockSnapshot` now wraps a
`StepContext` rather than repeating its fields (B2).

**`schema.rs`.** One `validate_properties` free function, shared by
`ComponentSchema` and plugin configuration (B1). Added `FieldValue::magnitude`
and `ChannelSchema::dimension`.

**`units.rs`.** Added `ELECTRIC_FIELD`, `ELECTRIC_POTENTIAL`,
`MAGNETIC_FLUX_DENSITY`, and `ACCELERATION` for Milestones 3, 5, and 7.

## Step 2 — Plugin API

The sampling signature changed from

```rust
fn sample(&self, channel: &ChannelId, position: DVec3) -> Result<FieldValue, PluginError>;
```

to

```rust
fn sample(&self, channel: ChannelHandle, geometry: &SampleGeometry)
    -> Result<SampledColumn, PluginError>;
```

`ChannelHandle` is an index into the plugin's declared channel list, resolved
once, so the hot loop no longer allocates strings. `SampledColumn` is a
`FieldColumn` plus one `SampleValidity` per element; shape and length are checked
once per batch by the runtime. Closes A3. Reasoning in ADR 0006.

Also added: `SolverContext` carrying configuration, domain, and world (A1);
`EquationSystemSolver::validate_world`, taking `&self` so a refusal costs nothing
(A5); `SolverKind::{Analytic, TimeStepped}` with a defaulted `step` (A6). Removed
two stringly-typed `PluginError` variants made redundant by B1.

**`plugins/test-field`** was rewritten against the new contract and now exercises
the singularity and out-of-domain validity paths it previously could not express.

## Step 3 — Runtime and data-source contract

**`crates/fieldcad-simulation/`** was split into `runtime.rs`, `source.rs`, and
`history.rs`.

`commit_world_commands` now clones the world, commits onto the clone, asks every
solver to validate it, and adopts only on unanimous success (A5, ADR 0007).
`RuntimeConfig` replaced the five-argument constructor. `PluginSlot` holds
`Arc<ChannelSchema>` (B4). Added `Subscription` — probes, plane density, domain
decimation — which is what makes "visualization density does not change the
physical result" a testable claim rather than a convention.

`empty_snapshot` is now marked `Partial` rather than `Complete`, so the initial
placeholder is a meaningful "no result yet" that the mailbox correctly refuses to
present.

On the source boundary: both sources publish through `SnapshotMailbox` (C1);
`FakeRemoteDataSource` became `LoopbackDataSource` with a full command path, an
in-flight snapshot link, disconnect/reconnect, and a replicated world (C2);
commands carry a `CommandId` echoed in the receipt (C3); `SourceError` names no
in-process type, with a `From<RuntimeError>` that maps to a stable code plus a
message (C4).

`ProbeHistory` (D4) assembles bounded time series from published snapshots alone,
so it works identically behind either source — a remote client does not own a
world to consult.

The equivalence test now drives both sources through one script and asserts the
observations are byte-identical, including that an acknowledgement arrives
*before* the snapshot describing it.

## Step 4 — ADRs

Eight records in `docs/adr/`. 0001–0005 recover Milestone 0's decisions, which
had been marked accepted but never written down (G4): the field-data-source
boundary, no-ECS, direct `egui`/`wgpu` integration, SI-in-the-core, and deferring
runtime plugins. 0006–0008 record decisions made during this pass: columnar
batched sampling, validate-before-adopt, and epoch-based tick time.

## Step 5 — Integration increment

**`apps/fieldcad-desktop/src/scene.rs`** (new) turns a `WorldSnapshot` into draw
instances and does ray-vs-oriented-box picking. Pure geometry, so selection and
framing are tested without a window or a GPU.

The renderer draws `world.objects()` with one unit-cube mesh and a per-object
instance transform, replacing the hardcoded vertex buffer. Selection became
`Option<ObjectId>`; the inspector reads the selected `WorldObject` instead of
printing a literal. A selection that stops resolving is cleared rather than left
in the inspector. Closes F.

`ui::show` split into six panel functions over a `ComputeView` — a per-frame
value built once — so panels no longer take a `&dyn FieldDataSource` and the
tests no longer build a runtime per frame.

One physical `Viewport` type replaced the logical/physical pair, so picking and
rendering derive their aspect ratio from the same value (E3, E4). A regression
test checks this at a fractional scale factor, where the mismatch was largest.

`FieldDataSource::world()` was added rather than reaching into
`LocalDataSource::runtime()`. For a local source this is the authoritative world;
for a remote one it is a replica updated on acknowledgement, which is what
`CONTEXT.md` requires the desktop to draw.

## Step 6 — Milestone 1 gaps and hygiene

Adapter selection retries with `force_fallback_adapter: true` and an error naming
lavapipe as a remedy (E1). A `naga` test parses and validates `scene.wgsl` on the
CPU and asserts its entry points exist (E2). `.github/workflows/ci.yml` runs the
headless crates, the desktop app, and format/lint across Linux, Windows, and
macOS — the headless job deliberately installs no graphics dependencies, so the
core staying GPU-free is enforced rather than merely intended.

The binary moved to `apps/fieldcad-desktop` (G1), shared versions moved to
`[workspace.dependencies]` (G2), the unused `fieldcad-plugin-api` dependency was
dropped (G3), and the empty `scripts/` directory was removed (G5).

`README.md`, `PLAN.md`, and `CONTEXT.md` were updated to match what the code
actually does.

---

## Two bugs found while implementing

Neither was in the review; both surfaced as test failures.

**`TickPacer` lost roughly one tick in two.** The remainder was carried as `f64`
seconds. With `dt = 0.1`, `0.25 - 2*0.1` yields `0.04999999999999999`, and the
next poll's `0.09999999999999999 / 0.1` floors to 0 instead of 1. Now carried as
`Duration` — integer nanoseconds — with a regression test that runs 1,000 polls
of 25 ms and asserts exactly 250 ticks. A pacing bug of this shape presents as a
physics bug, which is the expensive kind.

**A test asserted `3 * 0.1 == 0.3`.** It does not; it is `0.30000000000000004`,
reproducibly. The clock was right and the expectation was wrong. The test now
asserts against `3.0 * dt`, because the invariant is determinism, not decimal
tidiness. Recorded in ADR 0008.

---

## Deviations and outstanding work

**One deliberate scope widening.** `FieldDataSource::poll` now takes elapsed
wall-clock time and paces whole ticks, where it previously advanced exactly one
tick per call — which tied simulation rate to frame rate and contradicted
`CONTEXT.md`'s "frames and simulation ticks are intentionally independent". This
was not among the six steps; it was added because `poll` was changing signature
anyway for `Command`, and because it makes Milestone 4's "a solver that falls
behind does not silently increase its numerical time step" demonstrable now. When
the budget is exceeded the backlog is dropped and `fell_behind` is reported;
`dt` is never stretched.

**Milestone 1 is not fully closed.** Windows and macOS now build and test in CI,
but interactive verification — window lifecycle, high-DPI camera behaviour,
surface-loss recovery — still needs a person on those platforms. `PLAN.md` says
so rather than claiming completion.

**Deliberately not done.** `fieldcad-render` and `fieldcad-ui` remain modules
inside the desktop app; splitting them before a second consumer exists would be
crate ceremony without isolation. No dimensional arithmetic was added to
`Quantity` — see ADR 0004. Solver state is not transactional: a solver that
passes `validate_world` and then fails `on_world_changed` is a plugin defect, and
making solvers roll back would be a large cost for a case that indicates a bug —
see ADR 0007.

## Where Milestone 3 starts

The seams it needs are in place, so the remaining work is a real solver and real
visualization layers rather than further plumbing:

- `Domain` and `SlicePlane::lattice` describe where to sample;
- `Subscription` describes how densely, independently of the physics;
- `SampleGeometry`/`FieldColumn` deliver batches in the layout a WGSL evaluator
  and a `wgpu` buffer upload both want;
- `SampleValidity::Undefined(InsideSourceRadius)` already exists for the Coulomb
  singularity; and
- `ObjectShape::Point { radius }` already carries the source radius.

Suggested order: the CPU `f64` reference evaluator first, since it is the oracle
every later GPU result is checked against; then visualization layers against its
output; then the batched WGSL evaluator, compared to the oracle within a
documented tolerance.
