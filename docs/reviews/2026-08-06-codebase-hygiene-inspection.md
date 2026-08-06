# Codebase hygiene inspection — 2026-08-06

Date: 2026-08-06
Reviewer: inspection pass over the full workspace, split into three clusters and
read in full (not sampled): backend/transport (`crates/fieldcad-simulation`,
`crates/fieldcad-server`, `crates/fieldcad-mcp`), physics/plugin core
(`crates/fieldcad-core`, `fieldcad-plugin-api`, `fieldcad-dynamics`,
`fieldcad-particles`, `fieldcad-mass-sources`,
`fieldcad-electromagnetic-sources`, `fieldcad-newtonian-gravity`,
`fieldcad-bench`, and `plugins/*`), and the desktop UI
(`apps/fieldcad-desktop`).

## Scope and method

This is not a milestone-acceptance review (compare `docs/reviews/2026-08-03-milestone-5-review.md`
and siblings) — it is a general codebase-hygiene inspection requested as the
basis for a refactoring iteration. Static reading only; no numerical
experiments were run, though a few findings below are flagged as worth one.
Each cluster was read in full by a separate pass and asked to report only
verified findings with file:line evidence, not speculation. Two findings from
each cluster were independently spot-checked against the current source
during synthesis and confirmed accurate (see "Spot-checks" below).

At the time of this review, `crates/fieldcad-simulation/src/{source,async_source,lib}.rs`,
`crates/fieldcad-server/` (including the new `event_hub.rs`), and
`crates/fieldcad-mcp/src/lib.rs` carry substantial **uncommitted** changes —
this is the newest, least-reviewed code in the tree, and it accounts for a
disproportionate share of the high-severity findings below.

## Executive summary

- **11 high-severity findings**, concentrated in two places: the
  session/queue execution path in `fieldcad-simulation::source` and the MCP
  transport's interaction with it (4), and the gravity/electromagnetism
  plugins' physics correctness (2 bugs, plus a contract violation, a
  performance issue, and a duplication finding, all in the same area).
- The **highest-value single fix** is restoring abort-on-rejected-flush
  behavior in `SessionCore::execute` (Backend F1/F2) — a flush regression
  introduced by the uncommitted diff that lets `Undo`/`Step`/`Pause` proceed
  past a rejected mutation.
- **A recurring shape of bug**: a fix made in one implementation was not
  propagated to a structurally identical sibling. This happened at least
  three times independently: the electrostatics exclusion-radius fix was not
  carried into gravity (Physics B2); the Yee de-staggering/interpolation math
  exists in two places that have already diverged (Physics D1); and the
  "view toggle hides everything" bug goal.md already recorded as fixed at
  the plane level recurs one level down, at the individual-entity level
  (Desktop #4). This suggests the project's practice of fixing bugs
  point-in-place is outrunning any mechanism for propagating the fix to
  siblings — worth a deliberate deduplication pass rather than one-off
  patches.
- No `TODO`/`FIXME`/`unimplemented!()`/`todo!()`/`#[allow(dead_code)]` litter
  was found anywhere in the three clusters — the codebase is clean on that
  axis. Dead code that does exist is subtler: unused public re-exports that
  are themselves buggy (Desktop #21), and duplicate logic rather than unused
  logic (most of the "duplication" category below).

| Cluster | high | medium | low | total |
| --- | --- | --- | --- | --- |
| Backend/transport | 4 | 8 | 11 | 23 |
| Physics/plugin core | 5 | 8 | 7 | 20 |
| Desktop UI | 2 | 9 | 17 | 28 |
| **Total** | **11** | **25** | **35** | **71** |

## Cross-cutting patterns

These are worth fixing once, deliberately, rather than as N separate patches:

1. **Exclusion-radius / undefined-sample handling drifts between physically
   identical solvers.** `plugins/electrostatics` fixed "a body inside one
   source's exclusion radius loses every other source's contribution" via
   `field_excluding` (skip only the offending source). `plugins/gravity`
   reuses the same sampling kernel shape but never inherited the fix
   (Physics B2, **high**) — and separately, `field_excluding`'s own
   exclusion-radius collapse is wrong for `UniformSphere` distributions, where
   an interior analytic solution exists and should be used instead of treated
   as undefined (Physics B3). Recommend extracting the "sum sources, skip
   only what's actually singular at this point" kernel once, shared by both
   plugins, so the next fix lands in one place.

2. **Duplicated Yee de-staggering/interpolation code has already diverged.**
   `plugins/electromagnetism/src/lib.rs` and `.../coupling.rs` each implement
   the same cell-centering and trilinear reconstruction independently
   (Physics D1, **high**), and one now wraps position before computing grid
   coordinates while the other doesn't — a real behavioral difference between
   the particle pusher's view of the field and the display's view of it. This
   is the same failure mode the Milestone 5 review already flagged once
   (duplicated CPU/GPU solver scaffolding); it has recurred in a different
   pair of code paths within the same plugin.

3. **Gizmo/UI code has a "default size" trap.** Several gizmo helper
   functions come in `_with_display(display)` and bare (`GizmoDisplay::default()`)
   forms. Two production call sites — the plane-normal handle's draw length
   and the drag-radius computation — call the bare form while the rest of the
   gizmo respects the user's configured size (Desktop #1, **high**), and six
   more bare-form re-exports are reachable only from `#[cfg(test)]`, meaning
   nothing would catch a fresh instance of the same mistake (Desktop #21).
   Recommend removing the bare forms (or making `_with_display` the only
   public entry point) rather than continuing to maintain the pair.

4. **The command queue is deep-cloned for cheap change detection, at two
   layers.** `AsyncLocalDataSource::SourceState::capture` clones the full
   queue on every command/poll to ship across a channel; `EventHub::publish_state`
   clones it again on the reader side — both to extract four scalars
   (Backend F16, **medium**). Same shape of waste as the desktop's
   `ComputeView::build` reconstructing its entire view model every frame
   regardless of whether the snapshot changed (Desktop #11, **medium**).
   Both would benefit from keying off `snapshot.identity.sequence` /
   `world_revision` rather than diffing full clones.

5. **Copy-pasted per-kind blocks with visible drift.** The desktop's
   probe/plane/box/sphere UI panels repeat ~4 near-identical blocks each for
   listing (Desktop #16), field-layer controls (Desktop #17, with an
   already-mis-indented copy), and properties/duplicate actions (Desktop
   #18) — mirroring the same "shape → distribution" duplication between
   `fieldcad-mass-sources` and `fieldcad-electromagnetic-sources` at the
   plugin layer (Physics D2). None of these are large in isolation, but
   they're the same authoring mistake (add a fourth case by copy-paste)
   repeated at two layers of the stack, and in three cases the copies have
   already measurably diverged.

## Cluster A — Backend / transport

`crates/fieldcad-simulation`, `crates/fieldcad-server`, `crates/fieldcad-mcp`.
The uncommitted diff (`event_hub.rs`, `source.rs`, `async_source.rs`, MCP tool
surface) is the newest code here and carries most of the high-severity items.

### Bugs

**BE-1 — `crates/fieldcad-simulation/src/source.rs:662-671,727-739` — `Pause`/`Step`/`Undo`/`Redo` proceed even when their preceding flush failed. (high)** — **Fixed.** Added `SessionCore::flush_and_check`; `Pause`/`Step`/`Undo`/`Redo` now abort with `SourceError::FlushRejected` instead of proceeding, and leave the runtime mode unchanged so the rest of the queue gets another flush attempt. Regression test: `pause_step_undo_redo_are_refused_when_their_own_flush_rejects_a_mutation` (`fieldcad-simulation/src/lib.rs`).
`flush_pending_mutations()` was changed from fallible to infallible. On a
rejected mutation the flush breaks, leaving every *later* queued record still
in `pending_mutations`, but `execute` runs `runtime.undo()`/`step_once()`/
`pause()` anyway — confirmed by reading the current code, where `Pause` calls
`self.flush_pending_mutations();` with no `?`. Concrete failure: while
running, queue an invalid commit A then a valid commit B, then call `Undo`. A
is rejected, B stays queued, `runtime.undo()` executes and steps over an edit
the history hasn't recorded, and B later applies on top of the undone world —
exactly the invariant the surviving doc comment at line 728-730 claims to
protect.

**BE-2 — same file, `662-666` — a `Pause` that leaves records queued strands them permanently. (high, same root cause as BE-1)** — **Fixed** as part of BE-1: aborting before `runtime.pause()` runs means the mode stays `Running`, so the remaining queue is retried on the next `advance` instead of freezing.
`advance()` only flushes when running, so a broken flush's leftovers never
apply again until `Play`. `pending_command_count()` stays non-zero
indefinitely, which also keeps the desktop's redraw predicate (`app.rs:533`)
permanently hot.

**BE-3 — `crates/fieldcad-mcp/src/lib.rs:178` — `submit_and_wait`'s tick loop destructively steals every other transport's command events. (high)** — **Fixed.** The tick loop no longer calls `drain_events()` at all; `publish()` (inside `advance`) already resolves this call's own waiter independently of whoever else reads the shared buffer. Regression test: `submit_and_wait_never_drains_the_shared_events_buffer_other_transports_read` (`fieldcad-mcp/src/lib.rs`), verified to fail against the old code.
`server.drain_events()` empties the same buffer the desktop's per-frame pump
reads (`apps/fieldcad-desktop/src/app.rs:409`). While any MCP tool call is
awaiting, it drains and discards every event every 2ms. In the app's
supported embedded-MCP configuration, `ui_model.command_error` is never set
for a UI command that fails while an MCP call is pending, and the
completion/rejection trace log for that command silently disappears. The
`EventHub` added in this same diff exists to make observation
non-destructive; the MCP path bypasses it.

**BE-4 — same file, `177` — the MCP wait loop feeds extra wall-clock time into a shared session. (high)** — **Fixed.** The tick now advances with `Duration::ZERO`; only safe because BE-5 (below) was fixed first, so a completion no longer depends on real elapsed time to surface. This exposed that standalone `fieldcad-mcp` had no wall-clock driver of its own at all (`docs/mcp-plan.md` says that's "the standalone binary's poll loop," but it never existed) — added `drive_session` to `fieldcad-mcp/src/main.rs` to close that gap.
`server.advance(Duration::from_millis(2))` runs every 2ms for the duration of
a pending tool call, *in addition to* the desktop's own per-frame `poll`. A
shared session under a pending tool call receives roughly 2x real elapsed
time, so `TickPacer` schedules ticks faster than wall clock.

**BE-5 — `crates/fieldcad-simulation/src/async_source.rs:384-400` — terminal events from an `Execute` are never drained on that path. (medium)** — **Fixed** (pulled forward while fixing BE-3/BE-4, since BE-4's real-interval hack was the only thing masking this). `worker_loop`'s `Execute` arm now drains and forwards terminal events too, via a new `terminal` field on `WorkerEvent::CommandCompleted`/`CommandFailed`. Regression test: `a_side_effect_flush_reports_completion_without_a_subsequent_poll` (`fieldcad-simulation/src/lib.rs`).
Only the `Poll` arm calls `drain_command_events`. A pump that only calls
`advance(Duration::ZERO)` never issues a poll (`submit_poll_if_idle` returns
early on zero elapsed), so flush-emitted terminal events are unobservable
through it — both `crates/fieldcad-server/tests/concurrent_transports.rs:31`
and `tests/event_hub.rs:30` pump this way, and a `submit_and_await` waiter on
such a command would hang.

**BE-6 — `async_source.rs:294-304` + `source.rs:746-753` — a `Submitted` record is advertised as pending but is not cancellable. (medium)**
`get_queue()` synthesizes `Submitted` records into `status.pending`, and the
MCP tool docs tell clients to cancel by id from that list, but
`CancelQueuedCommand` only searches `pending_mutations`, which never contains
a `Submitted` record — cancelling the newest queued item returns a spurious
"already applied, rejected, cancelled, or unknown" error.

**BE-7 — `source.rs:770-775` — execute-time rejections leave no queue-history trace. (medium)**
`reject_if_queue_paused` returns `Err` without recording a `Rejected` record
or emitting `CommandEvent::Failed`; only flush-time rejections do. Contradicts
`docs/tasks/session-events-and-queue-control.md:60` ("command terminal
records must remain recoverable through queue history").

**BE-8 — `crates/fieldcad-server/src/lib.rs:91,147` — `waiters` entries are never pruned. (medium)**
Only `publish()` removes a `submit_and_await` waiter. A 30s MCP timeout drops
the receiver but leaves the sender in the map forever; nothing bounds it.

**BE-9 — `async_source.rs:233-237` — `PollFailed` discards an already-applied aggregate, and `poll_in_flight` never clears after worker disconnect. (low)**
An error return after a partial `adopt(state)` means the caller never learns
`snapshot_updated` was true and won't redraw; after a disconnect, no further
poll is ever submitted.

**BE-10 — `source.rs:796-803` — a rejected flush silently discards the tick budget already taken from the pacer. (low)**

**BE-11 — `crates/fieldcad-mcp/src/transport.rs:175-192` — TOCTOU on stale Unix-socket removal. (low, local/owner-only so low impact)**

### Contract violations

**BE-12 — `crates/fieldcad-server/src/event_hub.rs:26-28` vs `:110` — the docstring says the opposite of what the code does. (medium)**
Docstring promises change detection "without cloning [the queue] on every
publish"; `publish_state` calls `source.get_queue()`, which deep-clones the
queue (up to 256 records) to read four scalars.

**BE-13 — `source.rs:1123-1128` — `LoopbackDataSource::get_queue` returns authoritative state through a source that otherwise deliberately only exposes "believed" state. (low)**
Every other read on this type is filtered through `believed_*` specifically
so a consumer can't assume acknowledgement == visibility; `get_queue` is the
one exception, acknowledged but not justified in the inline comment.

**BE-14 — `crates/fieldcad-server/src/lib.rs:124` — `HeadlessServer::execute` accepts caller-minted `CommandId`s that can collide with the internal sequencer. (low-medium)**
Not currently exercised (desktop routes through `submit`), but the public API
permits a caller to resolve the wrong `submit_and_await` waiter.

**BE-15 — `source.rs:295-305` — `CommandRecord::submitted` sets `sequence: 0`, serialized to MCP clients whose docs say to sort pending by sequence. (low)**
Sorting by the documented field puts every `Submitted` record first — backwards.

### Performance

**BE-16 — `event_hub.rs:110` and `async_source.rs:96` — the full command queue is deep-cloned twice per publish. (medium)** — **Fixed.** Added `QueueSummary` (`fieldcad-simulation`) and `AsyncLocalDataSource::queue_summary()`, computing the same four scalars from `.len()`/`.last()` with no clone at all. `EventHub::publish_state` uses it instead of `get_queue()`; the now-redundant `QueueFingerprint` (structurally identical) is gone, replaced by `QueueSummary` directly in `SessionEvent::QueueUpdated`. Regression test: `queue_summary_agrees_with_get_queue` (`fieldcad-simulation/src/lib.rs`).
At a saturated 256-record history, two full `Vec<CommandRecord>` allocations
per `advance` — roughly 120/s from the desktop's frame pump alone, and
~1000/s while an MCP tool call's 2ms loop runs — to extract four scalars.

**BE-17 — `crates/fieldcad-mcp/src/lib.rs:539` — `ids.contains(id)` inside a channel loop is O(channels × requested). (low, currently small operands)** — **Fixed.** `wanted` is a `BTreeSet<ChannelId>` instead of a `Vec`, so the per-channel membership check is O(log n) instead of O(n).

### Duplication

**BE-18 — `crates/fieldcad-mcp/src/lib.rs:625-675` — `edit_world` and `commit_world` are two tools mapping to the same `CommandPayload::CommitWorld`. (low)** — **Fixed** the duplication, not the two-tools question (a public API decision, not mine to make unilaterally): `submit_world_commands` shares the identical submit/format tail; each tool keeps its own distinct parsing (typed schema discovery vs. raw JSON), which is the actual difference between them.
`commit_world`'s own description calls it "Legacy/compatibility path" — an
agent has to reason about two tools for one command.

**BE-19 — `source.rs:974-998` / `1146-1200` — `LocalDataSource` and `LoopbackDataSource` repeat identical `command_events` buffer/drain boilerplate. (low)** — **Fixed**, exactly as suggested: the buffer now lives once on `SessionCore` (its existing `emitted` field, previously drained immediately into each wrapper's own copy every `execute`/`poll`, now just accumulates until an external `drain_command_events()` call). Both wrappers' `drain_command_events` are now `self.core.take_emitted()` directly; pure internal refactor, no API or behavior change — confirmed by the full existing suite passing unchanged.
Could live once on `SessionCore` with both wrappers forwarding.

### Hygiene

**BE-20 — `crates/fieldcad-mcp/src/lib.rs:13-27` — module doc contradicts the code it documents. (medium)**
Claims push notifications are "left for later, everything here is pull" —
but `listen()`, `enable_resources_subscribe()`, and four `fieldcad://session/*`
resources exist in the same file. Also claims `commit_world` takes a
JSON-encoded string; its param type is `Vec<serde_json::Value>` and its own
field doc says the opposite.

**BE-21 — `crates/fieldcad-server/src/lib.rs:13-14` — "No transport is wired up yet," while `fieldcad-mcp` is a working transport depending on this crate. (medium)** — **Fixed.** Updated to name `fieldcad-mcp` as the working transport, both standalone and embedded in the desktop app sharing one session with its own UI commands.

**BE-22 — `event_hub.rs:157-160` — `Closed` and `Empty` are indistinguishable via `try_next`, so a polling consumer can't tell "nothing right now" from "hub is gone" and will poll a dead session forever. (low)**

**BE-23 — `docs/tasks/session-events-and-queue-control.md:32` asks for "enabled/paused state"; `QueueStatus` exposes only `paused`. (low, doc/code reconciliation)**

## Cluster B — Physics / plugin core

`crates/fieldcad-core`, `fieldcad-plugin-api`, `fieldcad-dynamics`,
`fieldcad-particles`, `fieldcad-mass-sources`,
`fieldcad-electromagnetic-sources`, `fieldcad-newtonian-gravity`,
`fieldcad-bench`, `plugins/{electrostatics,electromagnetism,gravity,test-field}`.

### Bugs

**PH-1 — `plugins/electromagnetism/src/lib.rs:643-648` — `regularized_potential` divides by zero for a zero-radius point charge, poisoning the whole Yee grid. (high)** — **Fixed.** A declared radius of exactly `0.0` now floors to half the local grid spacing before the interior/exterior split; a genuinely small but positive radius is untouched. Regression test: `a_zero_radius_point_charge_on_a_lattice_node_does_not_poison_the_grid` (`plugins/electromagnetism/src/lib.rs`).
`ObjectShape::point(0.0)` is valid and reachable via MCP. At `radius == 0.0`
the "smoothed interior" branch can never trigger, so a charge sitting exactly
on a lattice node yields `φ = ±inf`, which propagates to `NaN` across the
lattice within a few curl evaluations; energy/divergence diagnostics go NaN
and `FieldBatch::new` rejects every publication. Electrostatics handles the
identical input correctly (`plugins/electrostatics/src/lib.rs:128`,
`distance <= exclusion_radius` catches `0 <= 0`) — same edge case, one plugin
handles it and the sibling doesn't.

**PH-2 — `plugins/gravity/src/lib.rs:156-160` + `crates/fieldcad-newtonian-gravity/src/lib.rs:48-49` — a body inside any one source's exclusion radius loses gravity from *every* source. (high)** — **Fixed.** Added `evaluate_acceleration_excluding` to `fieldcad-newtonian-gravity`, mirroring electrostatics' `field_excluding`: skips only the offending source's contribution instead of voiding the whole sample. `forces()` now calls it instead of the whole-sample `evaluate_sources`. Regression test: `a_body_grazing_one_sources_exclusion_radius_still_feels_the_others` (`plugins/gravity/src/lib.rs`), exactly the two-body-plus-grazing-third-body scenario suggested below.
`evaluate_sources` returns a whole-sample `Undefined` on the first source
whose exclusion sphere contains the point, without visiting later sources;
gravity maps any non-`Exact` validity to zero force. A body grazing a small
incidental third body has its total force — including the primary body's —
drop discontinuously to exactly zero. Electrostatics already solved this
exact problem via `field_excluding` (skip only the offending source, keep
summing the rest); gravity reused the sampling kernel without inheriting the
fix. Worth a numerical regression test: two-body orbit with a third small
body grazing it.

**PH-3 — `plugins/electrostatics/src/lib.rs:363-372` — the force path and the sampled/displayed field disagree inside a uniformly charged sphere. (medium)** — **Fixed**, as a side effect of unifying PH-19 with gravity's already-correct interior handling. `field_excluding` now delegates to `fieldcad_superposition::field_excluding`, whose `UniformSphere` arm uses the same finite interior formula `evaluate_sources` always had. Regression test: `a_body_inside_a_charged_sphere_feels_its_finite_interior_field` (`plugins/electrostatics/src/lib.rs`), verified to fail against the old code.
`field_excluding` collapses `UniformSphere` to the same exclusion-radius
treatment as `Point`, even though `evaluate_sources` has a correct finite
interior solution (`E = kQr/R³`) for that region. A charge inside a large
uniform sphere is drawn with a real interior field and feels exactly nothing.

**PH-4 — `crates/fieldcad-mass-sources/src/lib.rs:231-236` — an object with gravitational mass but no inertial mass silently sources no gravity. (medium)**
`collect_mass_sources` gates on inertial mass despite the crate's own
documentation stating gravitational mass is an independent coupling.
Attaching only `gravitational-mass` via the generic component editor produces
an object that appears to gravitate in the model but contributes nothing,
with no error or diagnostic.

**PH-5 — `plugins/electromagnetism/src/lib.rs:1358-1375` — the field brush deposits at node positions, ignoring Yee staggering, so painted values land displaced by up to half a cell per axis. (low)**
Painting `B` also breaks `∇·B = 0` unconditionally with nothing documenting
that this is expected.

**PH-6 — `plugins/gravity/src/lib.rs:204-213` — `quantize` under `Precision::F32` can turn a finite potential into `inf` with no overflow guard (the F64 kernel has one). (low)**

### Contract violations

**PH-7 — `crates/fieldcad-core/src/world.rs:1210-1212,1262-1264,1312` — the "cannot be constructed unvalidated" invariant for plane/box/sphere specs is false, and inline comments assert it anyway. (high)** — **Fixed.** Added `validate`/`normalized` to `SlicePlaneSpec`/`FieldBoxSpec`/`FieldSphereSpec`, the same split `Transform` already uses, called from all six `apply_command` arms that construct or replace one (`Create`/`Set` × plane/box/sphere). The false-invariant comments are gone. Regression tests in `fieldcad-core/src/world.rs` deserialize tampered JSON directly (the actual `commit_world` attack path) and verify rejection, plus one proving benign non-unit input gets normalized rather than rejected — all four verified to fail against the old code.
All three spec types have private fields but derive `Deserialize`, and
`WorldCommand` is `Deserialize` end-to-end — `commit_world` in
`crates/fieldcad-mcp/src/lib.rs:658-666` deserializes caller-supplied JSON
directly into it. A `CreatePlane` with `normal: [0,0,0]` and negative
`half_extent` commits successfully and produces NaN sample positions fed to
every solver; same hazard for zero-radius `FieldSphereSpec` and
unnormalized-rotation `FieldBoxSpec`. `Transform` defends against exactly
this (its `normalized()` doc explains the deserialization argument, and
`apply_command` validates/normalizes every transform) — the hazard was
recognized and patched in one of four places, not all four.

**PH-8 — `plugins/electromagnetism/src/lib.rs:761-768` vs `810-857` — `validate_world` accepts edits that `on_world_changed` then rejects, after the world has already moved to the new revision. (medium)**
`validate_world` doesn't run `periodic_charge_initial_state`, which the
post-commit path calls and which can fail (solver non-convergence or lost
positive-definiteness). ADR 0007 requires rejection to happen before
adoption; here it can happen after.

**PH-9 — `plugins/gravity/src/lib.rs:157-160` — gravity maps both "undefined" and "numerical overflow" results to zero force, indistinguishable from "no force here." (medium)**
Electrostatics reports non-finite fields as `PluginError::Solver`, and
`fieldcad-dynamics` has a dedicated `NonFiniteForce` rejection gravity can
never trigger — two implementations of the same trait method report the same
failure class two different ways.

**PH-10 — `plugins/test-field/src/lib.rs:144-159` — declares `SolverKind::Analytic` ("ticks do not change the result") while `step` mutates observable state that `diagnostics()` reports. (low)**
This is a test fixture whose own diagnostics go stale exactly when the
runtime honors the contract the fixture exists to verify.

### Performance

~~**PH-11 — `plugins/electromagnetism/src/lib.rs:1384-1390` — every tick clones both full field grids even when there is no particle coupling. (high)**~~ — **Done.** Guarded the clone+advance_particles with `self.core.has_particle_coupling()`, matching the GPU path (`electromagnetism_gpu.rs:468-479`) that already had the same guard. The clones now only execute when there are particles to couple.
`advance_particles` short-circuits to `Ok(None)` when coupling is absent, but
the two grid clones are evaluated unconditionally first — ~1.6MB per tick at
32³, ~12.6MB per tick at 64³, pure waste in the common no-particle case. This
is the single hottest item found in this cluster.

~~**PH-12 — `plugins/gravity/src/lib.rs:150-155` — `forces()` builds a fresh filtered `Vec<MassSource>` per body, per tick — O(n²) copies, O(n) allocations. (medium)**~~ — **Done.** `evaluate_acceleration_excluding` now accepts an iterator instead of `&[MassSource]`; `forces()` passes the filter iterator directly, eliminating the per-body `Vec` allocation. Matches electrostatics' allocation-free `field_excluding` pattern.
Electrostatics does the equivalent job allocation-free by filtering inline
during accumulation (`field_excluding`).

~~**PH-13 — `plugins/electromagnetism/src/lib.rs:1137-1177` — trilinear sampling recomputes Yee de-staggering per stencil corner rather than once per cell. (medium)**~~ — **Done.** Precomputed `centred_electric` and `centred_magnetic` arrays in `YeeFieldView::new()`; `interpolate_vector` and `energy_at_cell` now read from those indexed arrays instead of calling `centred_fields` per stencil corner. Eliminates ~526k de-staggering evaluations per plane.
On a 257²-sample plane, ~526k `centred_fields` calls for ~33k distinct cells.

~~**PH-14 — `plugins/electromagnetism/src/coupling.rs:397,406,430` — three heap allocations per axis segment in current-deposition's inner loop, called 18×/particle/tick → 54 allocations/particle/tick. (medium)**~~ — **Done.** `transverse: Vec<usize>` → `[usize; 2]` match; `delta` and `flux` buffers hoisted to `deposit_charge_conserving_current` and passed as `&mut [f64]` through the call chain, replacing 36 per-particle heap allocations with 2. 54 allocs/tick/particle → 2.
`transverse` is a two-element `Vec` that should be `[usize; 2]`.

~~**PH-15 — `plugins/gravity/src/lib.rs:188` / `plugins/electrostatics/src/lib.rs:391` — the geometry cache does full structural equality (`Arc` element-by-element) per lookup instead of `Arc::ptr_eq`, plus O(n) `remove(0)` eviction. (low)**~~ — **Done.** `SampleCache` now uses `VecDeque` instead of `Vec`: `entries.pop_front()` is O(1) vs `entries.remove(0)` O(cap). The structural equality on `Arc` fields is a non-issue in practice — the runtime creates fresh `Arc` allocations each tick (`runtime.rs:1691`), so `Arc::ptr_eq` would always miss; structural comparison is the correct and necessary behaviour.

~~**PH-16 — `crates/fieldcad-dynamics/src/lib.rs:51-80` — `collect_dynamic_bodies` and `collect_carried_bodies` each run a full independent scan that differ only by filter predicate; one scan could produce both partitions. (low)**~~ — **Done.** Replaced both with `collect_bodies` returning `(dynamic, carried)` from a single scan of `collect_mass_sources`. Updated `runtime.rs` caller and test accordingly.

### Duplication

**PH-17 — `plugins/electromagnetism/src/lib.rs:1088-1448` vs `.../coupling.rs:487-719` — Yee de-staggering and trilinear reconstruction exist twice and have already diverged. (high)** — **Fixed.** `centred_fields`, `wrapped_cell`, and a new `interpolation_cell` free function (which now always wraps position via a `wrap_position` also promoted out of `coupling.rs`) are the single implementation both `lib.rs` and `coupling.rs` call through `use super::{...}`; `corner_weight` is gone, everything uses `axis_weight`. The two call sites' `base` cell index disagreed by a full domain width for any out-of-bounds position before this — latent rather than live, since the display path is always gated by `domain.bounds().contains(position)` first, but a real landmine for the next caller that isn't. Regression tests: `interpolation_cell_agrees_for_a_position_and_its_periodic_wrap` (`lib.rs`, verified to fail against the old code), `interpolate_particle_fields_agrees_for_a_position_and_its_periodic_wrap` (`coupling.rs`).
`centred_fields` and `corner_weight`/`axis_weight` are character-for-character
duplicates in two files; `interpolate_particle_fields` wraps position before
computing grid coordinates while `interpolation_cell` does not — a real
behavioral difference between what the particle pusher sees and what the
display shows. Same failure class the Milestone 5 review already flagged
once (see cross-cutting pattern #2 above).

**PH-18 — `crates/fieldcad-mass-sources/src/lib.rs:289-323` vs `crates/fieldcad-electromagnetic-sources/src/lib.rs:149-189` — shape→distribution mapping duplicated with parallel error enums. (medium)** — **Fixed**, exactly as suggested: `PointOrSphere::from_shape` (`fieldcad-core`, new) answers the shape question once; both crates map `PointOrSphereError` onto their own existing `NonPositiveSphere`/`UnsupportedShape` variants (no public error-surface change) and add `From<PointOrSphere>`/`From<MassDistribution|ChargeDistribution>` conversions.
Identical variant sets (mass-sources' own doc says so), identical four-arm
match on `object.shape`, identical guard, four structurally identical error
variants. A shared `PointOrSphere::from_shape` with per-crate `From` would
collapse this.

**PH-19 — `plugins/electrostatics/src/lib.rs:114-172` vs `crates/fieldcad-newtonian-gravity/src/lib.rs:33-81` — the two superposition kernels are the same algorithm with one constant and one sign changed. (medium)** — **Fixed.** New crate `fieldcad-superposition` owns the shared kernel (`evaluate_sources`, `field_excluding`) generic over a signed `coupling_constant`; both `plugins/electrostatics` and `fieldcad-newtonian-gravity` are now thin adapters converting their own source types to/from it. Fixed PH-3 as a direct consequence (see above) — the sibling-fix propagation this finding asked to prevent recurring.
Same loop, same three distribution arms, same interior-potential formula,
same finiteness check, same undefined-early-return structure. This is the
root cause of PH-2: the shape was copied but electrostatics' separate correct
force path (`field_excluding`) wasn't. If these are meant to stay
independent, that decision should be recorded so the next physics fix
doesn't land in only one of them again.

**PH-20 — `plugins/gravity/src/lib.rs:179-201` vs `plugins/electrostatics/src/lib.rs:383-415` — `samples_for` cache (mutex, linear-scan, 16-entry cap, `remove(0)` eviction) duplicated verbatim including comments. (low)** — **Fixed.** `SampleCache<T>` (new, `fieldcad-plugin-api`) generalizes it once; both solvers now hold a `SampleCache<Sample>` field and call `get_or_try_insert_with`. Available to any future solver too.

### Dead code / hygiene

**PH-21 — `crates/fieldcad-bench/` has zero gravity/mass-source coverage. (medium)**
Every workload benchmark is Maxwell or electrostatics; the newest solver —
and the one carrying the O(n²) `forces()` path (PH-12) — has no scaling
verdict at all, despite the project's explicit scaling-discipline convention
(`scaling.rs`).

**PH-22 — `crates/fieldcad-mass-sources/src/lib.rs:289-291` — `source_from_object` takes an unused `&PropertyBag` purely to mirror a sibling signature. (low)**

**PH-23 — `plugins/electromagnetism/src/coupling.rs:54-81` — `ParticleCoupling::new` returns `Result` with no error path, forcing a misleading `.expect(...)` and a vestigial `?` at its call site. (low)**

**PH-24 — `crates/fieldcad-particles/src/lib.rs:181-188` — `particle_properties` returns `Result<_, QuantityError>` with no fallible operation inside. (low)**

**PH-25 — `crates/fieldcad-dynamics/src/lib.rs:16-28` — module doc calls the integration scheme "momentum-form leapfrog"; `advance_body` actually implements semi-implicit (symplectic) Euler — the pseudocode is right, the name is what a reader will quote when reasoning about order/energy behavior. (low)**

### Regressions checked and not found

Confirmed clean: no stale duplicate electrostatics plugin; both electrostatics
and electromagnetism still assert backend-metadata/channel/schema equality
across their injected backends; the periodic-seam exclusion machinery from
the Milestone 5 remediation is present and consistent across E,
energy-density, and div-E channels; `ChannelId`/`ComponentTypeId` remain
distinct newtypes with a compile-fence test; no `TODO`/`FIXME`/`unimplemented!()`/
`todo!()`/stray `#[allow(dead_code)]` anywhere in this cluster.

## Cluster C — Desktop UI

`apps/fieldcad-desktop/src/` — `app.rs`, `ui/{mod,panels,compute,viewcontrols,plot,help}.rs`,
`scene/{mod,gizmo,field,authoring,pick}.rs`, `camera.rs`, `mcp.rs`,
`renderer.rs`, `gpu.rs`.

### Bugs

**UI-1 — `apps/fieldcad-desktop/src/scene/gizmo.rs:749` (and `:353`) — the plane-normal handle is computed at the default gizmo size while it's drawn at the user-configured size. (high)** — **Fixed**, together with UI-21. `plane_normal_tip`/`plane_normal_label_position` now take a `display: GizmoDisplay` parameter and call `transform_gizmo_with_display`; both `app.rs` call sites pass `self.ui_model.view.gizmo_display`. Regression test: `plane_normal_tip_scales_with_the_configured_display` (`scene/gizmo.rs`), verified to fail against the old code.
`append_transform_gizmo_with_display` derives length from the configured
`GizmoDisplay`, but `plane_normal_tip` calls the bare `transform_gizmo(...)`,
which hardcodes `GizmoDisplay::default()` — confirmed by reading the code:
`plane_normal_tip` (line 738) calls `transform_gizmo(...)` at line 748, not
the `_with_display` form. Both its consumers (`app.rs:448` label position,
`app.rs:1090` drag-arcball radius) inherit the mismatch. Setting the origin
arrow length to 300px and selecting a slice plane draws the N arrow 2.5× the
label's actual anchor, and the drag radius ends up 2.5× too small, so
dragging the normal rotates faster than the pointer. goal.md added this
length control explicitly as a feature; this is a regression against its
intent.

**UI-2 — `apps/fieldcad-desktop/src/app.rs:1281` + `crates/fieldcad-simulation/src/source.rs:663` — a paused command queue silently defeats "dragging pauses the simulation." (high)** — **Fixed**, within the limits `AsyncLocalDataSource`'s architecture allows: submission still can't block, so a drag can still begin before a `Pause` rejection is known. `EditGesture` now carries the `Pause` submission's own `CommandId` and clears `resume` when a matching `CommandEvent::Failed` arrives, so closing the gesture no longer submits an unconditional, unneeded `Play` on top of a run that was never actually stopped. Regression tests: `a_rejected_pause_does_not_resume_a_run_that_never_stopped`, `pause_rejected_ignores_an_unrelated_command_id` (`app.rs`).
`EditGesture::transition` emits `CommandPayload::Pause`, which
`reject_if_queue_paused` can reject whenever the queue is paused with a
pending mutation. `synchronize_edit_gesture` submits and discards the
outcome, so the gesture believes it paused even when the rejection lands
later as `CommandEvent::Failed` (under `AsyncLocalDataSource`). With the new
Queue panel's "Pause queue" active and a mutation pending, dragging a charge
lets the solver keep ticking through the teleport the gesture exists to
bracket, and release still submits `Play`. This is the newest UI feature
(queue control) interacting badly with one of the oldest documented
contracts in PLAN.md.

**UI-3 — `apps/fieldcad-desktop/src/ui/panels.rs:855` — `domain_draft` is seeded once (`get_or_insert`) and never resynchronized or invalidated. (medium)**
If an MCP client reconfigures the domain, or Undo crosses a domain change,
while the user has the Simulation node deselected, reselecting it shows stale
draft values with "Apply domain and reset" spuriously enabled — clicking it
resets the run to a lattice the user never typed.

**UI-4 — `apps/fieldcad-desktop/src/scene/field.rs:46-144` — field geometry is filtered by the class-wide view toggle but not by the individual entity's own `visible` flag. (medium)**
The authoring-geometry path filters per-entity `visible`; `field_geometry`
only checks the class-level `show.planes/boxes/spheres` toggle and relies on
the source not publishing batches for hidden regions. Paused, toggling one of
two planes' visibility hides its outline immediately but its arrows/magnitude
mesh keep drawing from the retained snapshot. This is a one-level-down
sibling of the bug goal.md already recorded as fixed at the plane-class
level.

**UI-5 — `apps/fieldcad-desktop/src/scene/gizmo.rs:144` vs `apps/fieldcad-desktop/src/camera.rs:49` — `GizmoDisplay` is documented in logical pixels but applied in physical pixels. (medium)**
`Viewport::from_logical` multiplies by `pixels_per_point` (asserted by its
own test), so a configured `axis_length_px: 120.0` renders a 60-logical-point
gizmo on a 2× HiDPI display — half the intended apparent size. Picking stays
internally consistent since it converts through the same physical space, so
this is a sizing/docs bug, not a functional break, but it undermines the
scale-independence feature's stated purpose.

**UI-6 — `apps/fieldcad-desktop/src/ui/panels.rs:1460` — `ShapeKind::ALL` omits `Box`, so a boxed object can't be reselected as a box once changed. (medium)**
The combo iterates `[None, Point, Sphere]`; `ShapeKind::build`'s `Box` arm is
unreachable from the UI even though it exists and objects with `ObjectShape::Box`
are constructed elsewhere (including the crate's own test world).

**UI-7 — `apps/fieldcad-desktop/src/app.rs:137-141` — any synchronous `SourceError` from a UI command quits the application, logged as an "initialization" failure. (low)**
Requires the worker thread to have already died, but a dead solver thread
shouldn't silently close the window, and it isn't an initialization failure.

**UI-8 — `apps/fieldcad-desktop/src/ui/panels.rs:1892` — the rename editor's Escape handler reads global input state rather than being scoped to its own text edit. (low)**
Harmless today because egui consumes the key while focused, but shares a key
binding with the global "clear selection" handler (`app.rs:371`) one
focus-routing change away from conflicting.

### Contract violations

**UI-9 — `apps/fieldcad-desktop/src/ui/panels.rs:39-84,290-354` — transport/history controls don't know the command queue can be paused. (medium)**
Enablement is gated only on `DataSourceStatus::Ready`; `ComputeView.queue.paused`
(added in the current diff) isn't read outside the queue window. Pressing
Step or Undo while the queue is paused with a pending mutation looks live and
is silently rejected, with no `on_disabled_hover_text` explanation the way
every other disabled control in this file provides.

**UI-10 — `apps/fieldcad-desktop/src/app.rs:770-787` — a field-brush stroke is silently discarded when its channel/strength is no longer resolvable, even though a real error string was constructed and then thrown away. (low)**

### Performance

**UI-11 — `apps/fieldcad-desktop/src/ui/compute.rs:65` — `ComputeView::build` reconstructs the entire view model every frame, up to 250Hz. (medium)** — **Fixed.**
Clones/reformats channel descriptions, field-system schemas, the full queue,
body forces, edit history, and every snapshot channel × probe as formatted
strings — almost none of which changes between snapshots, and none of it is
keyed on `snapshot.identity.sequence`/`world_revision`.

`build` now takes `previous: Option<&Self>` (the prior frame's view, stored on
`WindowState`/`App`). Snapshot-derived fields (`total_samples`,
`domain_summary`, `probe_readings`, `channel_names`, `vector_channels`,
`diagnostics`, `has_errors` — bundled into a new `SnapshotDerived` struct) are
reused verbatim when `snapshot_sequence` is unchanged, since every path that
changes them publishes a new snapshot (`commit_world_commands`,
`set_field_system_enabled`, a field brush stroke). `field_systems` and
everything derived directly from it stay unconditional, because
`set_field_system_realtime` only republishes conditionally and gating that
data on the snapshot sequence would risk showing a stale realtime toggle.
`queue` is reused separately, keyed on a new cheap
`FieldDataSource::queue_summary()` (paused flag, pending/history lengths,
newest history id) added to the trait with a safe default and cheap overrides
in `SessionCore`/`LocalDataSource`/`LoopbackDataSource`/`AsyncLocalDataSource`/
`HeadlessServer`, so the queue's up to 256 terminal records are not cloned
just to check whether anything moved. Regression tests
(`build_reuses_snapshot_derived_fields_until_the_snapshot_changes`,
`build_reuses_the_queue_until_it_changes_shape`,
`queue_matches_summary_agrees_only_when_shape_and_newest_entry_match`) verify
both the reuse and invalidation paths, confirmed to fail against the prior
always-rebuild behavior.

**UI-12 — `apps/fieldcad-desktop/src/app.rs:557-576` + `scene/field.rs:35-148` — glyph geometry is regenerated and double-allocated every frame regardless of whether the scene changed. (medium)** — **Fixed.**
At default density settings, ~6.5k vertices and ~4.3k interpolation calls per
plane per frame, recomputed identically while paused and static — the GPU
side already reuses buffers; the CPU side doesn't.

The channel-layer loop's contribution to the scene (`scene::field_geometry`
called per visible channel, `app.rs:557-576`) is now cached on `WindowState`
and reused verbatim when the inputs it is a pure function of — the snapshot
(compared by `(session, sequence)`, the same reasoning as UI-11), the
per-channel `ChannelLayerSettings` (visibility, density, plane/box/sphere
overrides), and `SceneVisibility` — are unchanged from the previous frame.
Authoring proxies, compute bounds, and the transform gizmo stay unconditional,
since they depend on live drag/selection state that changes far more often
than a snapshot and were outside this finding's scope. The caching decision
itself was pulled out into a free function
(`compute_field_layer_geometry`) rather than left as a `WindowState` method,
specifically so it is testable without a window or GPU device; regression
tests (`reuses_the_cache_until_the_snapshot_sequence_changes`,
`reuses_the_cache_until_the_layer_settings_change`) verify both the reuse and
invalidation paths, confirmed to fail against the prior always-rebuild
behavior.

~~**UI-13 — `apps/fieldcad-desktop/src/app.rs:1231` — `submit_world_manipulation` calls the full `refresh_world()` (mutex + clone + six retain passes) on every pointer-move during a drag, in addition to two other unconditional call sites per frame. (low)**~~ — **Done.** Removed `self.refresh_world()` from `submit_world_manipulation` — the per-frame `refresh_world()` at line 554 already picks up command results within at most one frame, which is imperceptible during a drag.

~~**UI-14 — `apps/fieldcad-desktop/src/scene/gizmo.rs:248,287,289,291` — `world_units_per_pixel` is recomputed seven times per gizmo frame for a value constant across the whole gizmo — the exact "recomputed per-glyph" pattern the module's own doc comment claims to have solved. (low)**~~ — **Done.** `append_transform_gizmo_with_display` now computes `scale` once and derives all six pixel-based sizes from a shared local; `pick_transform_handle_with_display` hoists `scale` to function scope and uses it for both the ring-radius and free-rotation sphere.

~~**UI-15 — `apps/fieldcad-desktop/src/app.rs:461-463` — `adapter_name().to_owned()` and `self.world.clone()` per frame, purely to satisfy closure lifetimes. (low)**~~ — **Done.** Cached `adapter_name: String` on `WindowState`, populated once at renderer creation; the per-frame `to_owned()` replaced with a borrow from the cached field. `self.world.clone()` kept as-is — `WorldSnapshot` is `Arc<WorldState>`, the clone is already a cheap refcount bump required by the borrow checker (`&mut self.ui_model` in the same closure prevents a direct `&self.world` borrow).

### Duplication

**UI-16 — `apps/fieldcad-desktop/src/ui/panels.rs:557-671` — four ~28-line copy-pasted entity-list blocks (probes/planes/boxes/spheres). (medium)** — **Fixed.** `entity_row` (new) draws the shared "visibility toggle, select, delete" shape once and returns which action was taken; every call site (object, probe, plane, box, sphere — five now, the object list included) builds its own `WorldCommand`/`SceneSelection`, since those genuinely differ per kind. The delete-markup drift is gone with it — one implementation, not five.
Already drifted: delete button markup differs between probes/planes/boxes vs.
spheres; only the object section has a "no items yet" message.

**UI-17 — `apps/fieldcad-desktop/src/ui/panels.rs:2016-2342` — `plane_field_layers`/`box_field_layers`/`sphere_field_layers` are the same function three times, including a byte-identical warning message block. (medium)** — **Fixed.** `hidden_everywhere_warning` shares the byte-identical block across all three. `box_field_layers`/`sphere_field_layers` — genuinely identical apart from display text and which settings map they read — collapse into one generic `volume_field_layers` behind a small local `VectorLayerSettings` trait; `plane_field_layers` stays its own function since it has real extra content (magnitude/vector-mode controls) the other two don't.
Already drifted: the box copy's hover-text continuation is mis-indented.

**UI-18 — `apps/fieldcad-desktop/src/ui/panels.rs:1913-2260` — `plane_properties`/`box_properties`/`sphere_properties` share the same skeleton with three hand-rolled `Duplicate X` bodies. (low)** — **Fixed**, narrowly: `entity_actions` shares just the trailing "duplicate / focus / remove" block (the actually-identical part — `Focus selection` needed no parameterization at all). The name editor, hint text, and two `super::section` calls above it stay inline per function; genericizing those too would have needed more closure parameters than the three-function duplication it removed.

**UI-19 — `apps/fieldcad-desktop/src/ui/panels.rs:2686-2758` — `transport_sampling` repeats one block four times; the first copy is structured differently from the other three and is correct only because it happens to run first. (low)** — **Fixed.** `density_field` (new) gates a write on its own widget response only — there is no shared `changed` accumulator left to leak from, so the bug is structurally impossible now regardless of call order (previously live only by accident of position, not exercised by the shipped order, so no test could distinguish old from new behavior for this one — added a smoke test for "no interaction, no submission" instead).
Reordering the four blocks would make it write a stale value — a latent bug
waiting on an unrelated edit.

**UI-20 — `apps/fieldcad-desktop/src/scene/gizmo.rs:288,573` — the view-ring radius factor and `rotation_radius_px()` scaling are written as literals in both the draw and pick paths. (low)**

### Dead code

**UI-21 — `apps/fieldcad-desktop/src/scene/mod.rs:20-28` — six public re-exports (`append_transform_gizmo`, `pick_transform_handle`, `selection_gizmo_length`, `rotation_gizmo_radius`, `pick_object`, `ObjectInstance::bounding_sphere`) have no production caller, only `#[cfg(test)]` ones. (medium)** — **Fixed.** Four of the six (`append_transform_gizmo`, `pick_transform_handle`, `selection_gizmo_length`, `rotation_gizmo_radius`) were bare `GizmoDisplay::default()` wrappers and are deleted outright, along with their `pub use` entries — every caller (all tests) now calls the `_with_display` form explicitly. `pick_object` and `ObjectInstance::bounding_sphere` aren't display wrappers (a correction to this finding's own generalization), but were equally true dead `pub` surface; both are now `#[cfg(test)]`-gated private items instead of exported.
These are the bare `GizmoDisplay::default()` wrappers — the same mechanism
behind UI-1. They're not just clutter, they're the trap that produced UI-1.

**UI-22 — `apps/fieldcad-desktop/src/ui/mod.rs:199` — `#[derive(Debug, Default)]` on `UiModel` produces a value contradicting `UiModel::new()` (three visibility flags flipped), and is never actually called (23 `::new()` sites, 0 `::default()`). (low)**

**UI-23 — `apps/fieldcad-desktop/src/scene/gizmo.rs:815-825` — `#[cfg(test)]` helpers `rotation_ring_radius`/`view_ring_radius` re-derive production geometry with a literal ratio instead of calling the production function, so they'd silently stop catching a regression in the constant they duplicate. (low)**

### Hygiene

**UI-24 — `apps/fieldcad-desktop/src/ui/panels.rs` is 3774 lines and accreting; `menu_bar` is ~160 lines mixing five concerns, `inspector` is a six-arm chain whose arms already duplicate a "heading + separator + delegate" shape. (medium)**

**UI-25 — probe size is three different unnamed world-space constants across `authoring.rs:112` (0.09), `pick.rs:71` (0.13), `app.rs:1315` (0.2) — none screen-relative, unlike the gizmo. (medium)**
At the nanometre scales the project is explicitly targeting (goal.md), a
probe would be simultaneously invisible and unclickable, and even at metre
scale the click target doesn't match the glyph.

**UI-26 — `apps/fieldcad-desktop/src/ui/panels.rs:2855,2896,3026` — `diagnostics_window`, `mcp_window`, and the new `queue_window` all default-position to the same coordinates, stacking exactly on first open. (low)**

**UI-27 — `apps/fieldcad-desktop/src/scene/field.rs:654-659` — `plane_interpolation` lacks the explicit bounds guard its siblings (`grid_interpolation`, `box_interpolation`) have; safe today only by an indirect `Option` short-circuit its siblings deliberately don't rely on. All three then index the batch's value/validity slices against the lattice's bounds, so a batch shorter than its declared geometry panics rather than degrading. (low)**

**UI-28 — `apps/fieldcad-desktop/src/mcp.rs:44-47` — doc says `fatal` is checked "each frame the MCP panel is open"; `app.rs:391` checks it unconditionally every frame. (low)**

### Spot-checks of PLAN.md / goal.md claims that held up

Verified correct, not regressed: undo/redo and rename route through the
command system with no direct-mutation bypass; charge/mass edits pause the
simulation via `note_held_edit`; view toggles hide all associated visuals at
the *class* level (the gap found is per-entity, UI-4); the multi-channel
probe recorder and per-plugin realtime toggle both work as documented; gizmo
scale-independence holds except for the two defects above (UI-1, UI-5) and
the probe/authoring proxies that were never converted (UI-25). No
`TODO`/`FIXME`/`todo!()`/`unimplemented!()`/`#[allow(dead_code)]` anywhere in
the crate.

## Recommended next-iteration plan

In priority order — correctness first, then contracts, then the
duplication/performance items that compound if left alone, then hygiene:

1. ~~**Fix the queue/flush regression** (BE-1, BE-2)~~ — **Done.** Restored
   abort-on-rejected-flush in `SessionCore::execute`.
2. ~~**Fix the two physics correctness bugs** (PH-1 divide-by-zero, PH-2
   exclusion-radius zeroing)~~ — **Done.**
3. ~~**Fix MCP's destructive event drain and double-clocking** (BE-3, BE-4)~~
   — **Done**, along with BE-5 (`crates/fieldcad-simulation/src/async_source.rs`),
   which turned out to be the same underlying issue: BE-4's real-interval
   tick was the only thing masking BE-5's undrained buffer, so fixing BE-4
   correctly required fixing BE-5 first. Also added a standalone-MCP
   wall-clock driver (`fieldcad-mcp/src/main.rs`) that was missing entirely
   before this fix exposed the gap.
4. ~~**Fix the plane-normal gizmo size mismatch** (UI-1) and **remove the bare
   `GizmoDisplay::default()` re-exports** (UI-21)~~ — **Done.**
5. ~~**Fix drag-pause defeated by a paused queue** (UI-2) and **close the world
   spec validation hole** (PH-7)~~ — **Done.**
6. ~~**Deduplicate, don't re-patch, the three recurring duplication
   clusters**: the Yee de-staggering math in electromagnetism (PH-17), the
   electrostatics/gravity/newtonian-gravity superposition kernel
   (PH-2/PH-3/PH-18/PH-19/PH-20), and the desktop's per-shape-kind panel
   blocks (UI-16/17/18/19)~~ — **Done.**
7. **Everything else** (remaining mediums/lows above) is reasonable
   refactoring-iteration backlog — none block correctness. ~~BE-16~~,
                               ~~UI-11~~, ~~UI-12~~, ~~UI-13~~, ~~UI-14~~, ~~UI-15~~, ~~PH-11~~, ~~PH-12~~, ~~PH-13~~, ~~PH-14~~, ~~PH-15~~, and ~~PH-16~~ are **done** (see above).
