# Review — Milestones 1 and 2

Date: 2026-08-02
Scope: the whole implementation as it stood at that date — 4,093 lines across
five crates, plus `README.md`, `CONTEXT.md`, and `PLAN.md`.
Baseline at review time: `cargo test --workspace` → 27 passing;
`cargo clippy --workspace --all-targets` → clean.

Companion document: [remediation report](2026-08-02-remediation-report.md),
which records what was subsequently done about these findings.

> **Line references are to the pre-review code.** They no longer resolve against
> the current tree; several files have since been rewritten or moved
> (`src/` → `apps/fieldcad-desktop/src/`). They are kept because they are part of
> the evidence for each finding.

Every finding below carries a disposition. `Fixed` means the defect is gone and a
test covers it; `Deferred` means it was accepted and left, with a reason.

---

## Overall assessment

The code was disciplined — validated newtypes, atomic commits, `Arc`-shared
immutable state, no UI leakage into the core, and tests that made honest claims.

Two structural problems dominated everything else:

1. **The two milestones never met.** Milestone 1 rendered a hardcoded cube;
   Milestone 2 modelled a world that nothing rendered. The `World` type held
   objects with transforms that no code path drew, and the inspector printed the
   position as a string literal.

2. **The plugin API was about to fail its own review gate.** `Domain` — the
   concept `CONTEXT.md` requires a solver to be initialized from, and a snapshot
   to carry — did not exist anywhere in the codebase. Verified by search: no
   `Domain`, no `resolution`, no `BoundaryCondition`, no `validity`, no probe
   history.

That gate exists precisely to catch the second problem before a real physical
model builds on the wrong shape. It caught it.

---

## A. Blocking for the Milestone 2 plugin-API review gate

These would each have forced an API break during Milestone 3.

### A1 — No `Domain` concept — **Fixed**

`CONTEXT.md:230` promises "initialization from a domain and a read-only world
view". `create_solver(&self, configuration)` received neither. `CONTEXT.md:196`
requires snapshots to carry "domain and channel descriptors, numerical
representation, precision, and boundary-condition metadata"; none existed.

This was the single largest missing abstraction. Milestone 3 is built on it.

### A2 — `FieldSnapshot` was really a probe-sample snapshot — **Fixed**

`snapshot.rs:89` carried only `channels: BTreeMap<ChannelId, ChannelSamples>`,
where `ChannelSamples` (`snapshot.rs:83`) was a list of per-probe points. There
was no representation for a plane image or a 3D grid — both required by
Milestone 3's colour maps and sparse whole-domain glyphs.

### A3 — `sample()` was per-point, per-channel, and allocating — **Fixed**

`plugin-api/src/lib.rs:62`:

```rust
fn sample(&self, channel: &ChannelId, position: DVec3) -> Result<FieldValue, PluginError>;
```

Three problems at Milestone 3 scale (tens of thousands of samples per frame):

- `ChannelId` is `String`-backed, so `test-field/src/lib.rs:124` allocated **two
  heap strings per sample** purely to compare channel identity;
- every returned `FieldValue` re-wrapped a `Dimension` the channel already
  declared, and `simulation/src/lib.rs:195` re-validated it per value;
- a per-point virtual call cannot be batched, handed to a GPU, or chunked over a
  network — and Milestone 3 requires all three.

### A4 — `ComponentTypeId` and `ChannelId` were the same type — **Fixed**

`ids.rs:85-86` — both were `type X = QualifiedId` aliases. The compiler would
accept a channel ID wherever a component ID belonged. Milestone 7's exit
criterion is explicitly "without channel ID or unit ambiguity".

### A5 — A rejected world edit left the world committed — **Fixed**

`simulation/src/lib.rs:151-165`:

```rust
let report = self.world.commit(commands)?;   // world has already advanced
if report.revision != previous_revision {
    for slot in &mut self.plugins {
        slot.solver.on_world_changed(&snapshot)?;   // ← Err here
    }
    self.publish_snapshot()?;
}
```

If a solver refused, the error propagated but the world stayed at the new
revision, some solvers had adopted the edit and some had not, and no snapshot was
published — pinning the UI at `Stale` permanently. The world held a revision that
nothing had computed. This violated `CONTEXT.md:319`: "A plugin failure is
reported with context and must not corrupt the saved world."

Related: the plugin contract listed "configuration and world validation"
(`CONTEXT.md:229`) but the trait had no validation entry point.

### A6 — `step()` was mandatory for analytic plugins — **Fixed**

Electrostatics has no time evolution, yet had to implement `step`
(`plugin-api/src/lib.rs:61`), and the runtime republished a snapshot every tick
regardless. `CONTEXT.md:22` treats inspection and simulation as two modes of one
architecture; only one was expressible.

### A7 — No sample validity — **Fixed**

`FieldSample` (`snapshot.rs:76`) had no validity field, though `CONTEXT.md:234`
requires "probe sampling with validity/error information" and `CONTEXT.md:97`
requires point singularities to be "marked as undefined inside a declared source
radius" rather than silently clamped. Electrostatics needs this on day one.

---

## B. Duplication and simplification

### B1 — Property-bag validation existed twice, verbatim — **Fixed**

`ComponentSchema::validate` (`core/schema.rs:86-105`) and
`PluginConfigurationSchema::validate` (`plugin-api/src/lib.rs:21-38`) were the
same algorithm differing only in error type.

### B2 — Three near-identical clock structs — **Fixed**

`ClockSnapshot` (`time.rs:33`) and `StepContext` (`time.rs:41`) differed by one
field; `SimulationStatus` (`simulation:280`) was `ClockSnapshot` plus
`world_revision`.

### B3 — "Finite and non-degenerate" written five times — **Fixed**

`Transform::new`/`validate` (`world.rs:27-45`) duplicated each other; likewise
`Velocity` (`world.rs:55-67`); and `SlicePlaneSpec::new` (`world.rs:107`) was
re-inlined a third time in `apply_command` (`world.rs:410-415`).

### B4 — Channel schema cloned into every snapshot, every tick — **Fixed**

`simulation:205` cloned a whole `ChannelSchema` including its `String` display
name, per channel per publication — steady allocation churn at 60 Hz for data
that never changes.

### B5 — Minor — **Fixed**

- `simulation:1`: `use std::{collections::BTreeMap, collections::BTreeSet, ...}`.
- `status_from_runtime`/`receipt_from_runtime` (`simulation:367,378`) were free
  functions that read better as methods.
- `empty_snapshot` (`simulation:230`) was allocated and immediately overwritten.
- `camera.rs:3-4`: `MIN_PITCH: f32 = -1.553_343` — a magic number for
  `FRAC_PI_2 - 0.0175`.

---

## C. The two data sources did not offer the same contract

Milestone 2's own exit criterion, only half-met.

### C1 — `SnapshotMailbox` guarded only the remote path — **Fixed**

`FakeRemoteDataSource` routed through the mailbox (`simulation:475`), which
enforced completeness, session identity, and monotonic sequence.
`LocalDataSource::latest_snapshot` (`simulation:362`) returned the runtime's
`Arc` directly, enforcing none of them. The invariant at `CONTEXT.md:313` was
therefore tested on one path only.

### C2 — The remote source rejected every command — **Fixed**

`FakeRemoteDataSource::execute` (`simulation:467`) returned `UnsupportedCommand`
unconditionally, and the equivalence test (`simulation:608`) compared only
sequence numbers. A consumer issuing Play or Step *did* break on swap.

### C3 — Acknowledgements were not correlated — **Fixed**

`CommandReceipt` (`simulation:296`) carried no command identity, against
`CONTEXT.md:192`: "Play, pause, and step are commands with correlated
acknowledgements; they are not inferred from incoming frame timing."

### C4 — `SourceError` leaked in-process types — **Fixed**

`SourceError::Runtime(RuntimeError)` (`simulation:492`) transitively carried
`PluginError` and `WorldError`. A remote source could never produce one.

---

## D. Core model holes

### D1 — Schema registration bypassed the revision — **Fixed**

`World::register_component_schema` (`world.rs:240-250`) mutated state via
`Arc::make_mut` **without bumping the revision**. Two `WorldSnapshot`s could both
report revision 0 while describing different schema sets, breaking
`CONTEXT.md:309`. Latent in practice, but a hole in the model rather than in its
usage.

### D2 — Entity generations were decorative — **Fixed**

`ObjectId::new(counters.object, 0)` — generation was hardcoded `0` at all three
call sites (`world.rs:344, 417, 438`) and slots were never recycled. The field
cost four bytes and, worse, *signalled* that stale-ID detection was handled when
it was not.

### D3 — `SlicePlane` could not be sampled or hidden — **Fixed**

`world.rs:118-124` was origin plus normal: an infinite plane with no extent, no
resolution, no in-plane basis, and no visibility flag. Milestone 3 requires
creating, orienting, duplicating, hiding, and removing slice planes, and sampling
them into an image. Without a u/v basis a sampled image has no stable
orientation.

### D4 — Probes had no history — **Fixed**

`Probe` (`world.rs:149`) carried only `channels`, against `CONTEXT.md:268`, which
specifies "a bounded time-series buffer" where each sample records simulation
time, snapshot revision, value, units, and validity. Milestone 4 depends on it.

### D5 — `dt` was immutable, and changing it would rewrite history — **Fixed**

`SimulationClock` (`time.rs:49`) exposed no way to change `time_step`, and
computed `time_seconds = tick as f64 * dt` (`time.rs:68, 93`). Milestone 4
requires an editable `dt` — but with that formula, changing it retroactively
alters every past tick's timestamp, invalidating recorded probe history.

---

## E. Milestone 1 — two exit criteria were not met

### E1 — No fallback-adapter path — **Fixed**

Deliverable (`PLAN.md:74`): "a software/fallback-adapter path with a clear error
if no usable GPU exists." `renderer.rs:72` set `force_fallback_adapter: false`
with no retry. The error was clear; the fallback path did not exist.

### E2 — No shader compilation test, no CI — **Fixed**

Deliverable (`PLAN.md:73`): "shader compilation in tests or CI where practical."
Nothing compiled `scene.wgsl` outside a live device, and there was no `.github/`.

### E3 — Picking and rendering used different projections — **Fixed**

`renderer.rs:184` fed the camera `physical_viewport.aspect_ratio()` — integer-
rounded via floor/ceil/`max(1)` (`renderer.rs:300-325`). Picking (`app.rs:298` →
`camera.rs:180`) used `Viewport::aspect_ratio()`, the unrounded logical value.
The ray was therefore cast through a different frustum than the one that produced
the pixels. Sub-pixel at the time, but growing with display scaling.

### E4 — Two viewport types for one concept — **Fixed**

`Viewport` (logical, `camera.rs:9`) and `PhysicalViewport` (pixels,
`renderer.rs:292`), with the conversion in a third place. This is what made E3
possible.

---

## F. The integration gap

Milestones 1 and 2 never met. Concretely:

- `OBJECT_MIN/MAX/CENTER/RADIUS` were `const` in `renderer.rs:18-21`;
- the placeholder was a hardcoded 8-vertex buffer (`renderer.rs:663-703`);
- selection was `object_selected: bool` (`ui.rs:16`);
- the inspector printed the position as the string literal `"0.0, 0.0, 0.6 m"`
  (`ui.rs:136`);
- meanwhile `World` held real objects that nothing rendered, and
  `create_local_data_source` (`app.rs:309`) committed a probe and zero objects.

**Disposition: Fixed**, as a distinct increment inserted between Milestones 2 and
3 — Milestone 3 as written bundled this plumbing with Coulomb solving, GPU
evaluation, gizmos, and slice planes, which is too much for one gate.

Two supporting findings, both **Fixed**:

- `ui::show` was a 230-line function (`ui.rs:58-287`) spanning menu bar, scene
  tree, inspector, compute status, viewport interaction, and diagnostics.
- The UI tests constructed a real `SimulationRuntime` **per frame**
  (`ui.rs:321-331`) — three per test — because `show` demanded a
  `&dyn FieldDataSource`.

---

## G. Workspace hygiene

| | Finding | Disposition |
| --- | --- | --- |
| G1 | Root package was both workspace root and desktop binary; `PLAN.md:25` specifies `apps/fieldcad-desktop/` | Fixed |
| G2 | No `[workspace.dependencies]`; `glam`, `thiserror`, `serde` repeated across five manifests | Fixed |
| G3 | `fieldcad-plugin-api` declared in the root manifest but unused by `src/` | Fixed |
| G4 | No ADRs, though Milestone 0's deliverable is to record decisions and its exit criteria were marked accepted | Fixed — eight ADRs |
| G5 | `scripts/` existed but was empty and untracked | Fixed |

---

## Recommended sequence

Given at the time of review, and the order the work was subsequently done in:

1. Close the M2 gate properly — A1, A4, A5, A7, plus B1–B3.
2. Decide A2/A3 deliberately — snapshot payload shape and batched sampling are
   the two calls that determine whether Milestone 3 is additive or a rewrite.
3. Write the ADRs (G4) while the reasoning is fresh.
4. Do the integration increment (F) as a distinct milestone between 2 and 3.
5. Close the two M1 gaps (E1, E2).
6. Then Milestone 3.
