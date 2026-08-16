# Task: authoritative probe histories and object trajectories

## Status: resolved (2026-08-16)

**Update (2026-08-16, later still still):** items 3 and 4 closed together —
they turned out to be linked: item 3 (dropping the desktop's client-local
copy) couldn't be done safely until item 4 (a transport-neutral recorder)
gave the desktop something authoritative and *complete* to read from
instead. Investigating both surfaced a third, previously-undetected gap,
fixed as part of the same change: **loading a scene never restored its
saved observation history into `HeadlessServer`**, for either desktop or
MCP — `replace_source` unconditionally zeroed the three histories with no
restore path, unlike `run_records`/`document_entries`. This was invisible
before because the desktop kept its own separate copy and restored *that*
from the document; an MCP-driven `open_scene` silently lost a saved scene's
history entirely, and MCP's `save_scene` always wrote empty history (per
its own now-stale "MCP has no client-local probe-plot recording to
capture" comment).

- **Recorder moved to `AsyncLocalDataSource`** — new
  `fieldcad_simulation::ObservationRecorder`
  (`crates/fieldcad-simulation/src/observation_recorder.rs`), extracted
  verbatim from the old `HeadlessServer::record_observations` (run-
  generation clear, capacity sync, record, prune) and updated inside
  `AsyncLocalDataSource::adopt` — the one place a fresh snapshot becomes
  newly visible, confirmed by exploration to be the single correct hook for
  every code path (no production code anywhere constructs a bare
  `LocalDataSource`/`LoopbackDataSource`; both are test-only, so extending
  them was scoped out rather than done speculatively — their own
  snapshot-adoption points are documented in the new module's doc comment
  for whenever a real need appears). `HeadlessServer` no longer owns
  `probe_history`/`distance_history`/`mass_aggregate_history` fields at
  all — its own accessors are now one-line forwards to `self.source`.
- **Restore-on-load gap fixed for both transports.** New
  `HeadlessServer::restore_observation_history`/`capture_observation_history`
  (mirroring `restore_run_records`); `crates/fieldcad-mcp/src/lib.rs`'s
  `open_scene` now calls the restore, and `save_scene` now captures real
  history instead of always-empty defaults. `ObservationRecorder::restore`
  takes raw per-series readings (not a pre-built `ProbeHistory`) so it can
  sync per-probe capacities from the already-loaded world *before*
  inserting each series — building the `ProbeHistory` first and handing it
  over would already be too late (a large declared capacity wouldn't be
  known yet, so `insert_series` would clamp against the flat default).
  `ProbeHistory::insert_series` itself was also fixed to trim against the
  per-key capacity override, not the flat one.
- **Desktop drops its client-local copy.** `WindowState.probe_history`/
  `distance_history`/`mass_aggregate_history` and the whole
  generation-reset/record/retain-probes block inside `refresh_world` are
  gone; `apps/fieldcad-desktop/src/probe_history_state.rs` (both capture
  and restore halves) is deleted entirely, superseded by
  `HeadlessServer::capture_observation_history`/`restore_observation_history`.
  `FrameContext`'s three fields now borrow from a new `probe_history_cache`
  field — a clone of the server's histories refreshed only when
  `compute.snapshot_sequence` advances (mirroring `ComputeView::build`'s
  existing reuse-if-unchanged pattern), not every frame: cloning is not
  free, and holding the model mutex across a whole UI-drawing frame (the
  alternative to cloning) risked stalling a concurrent MCP request.

Tests: `ObservationRecorder` unit tests in `observation_recorder.rs`
(capacity honoring, run-generation clear, prune, and the restore-syncs-
capacity-before-inserting fix); `fieldcad-server` integration test
`observation_history_survives_replace_source_via_restore_observation_history`
(the specific restore-on-load gap, round-tripped through a rebuilt session
exactly the way `open_scene`/`replace_session` drive it, readable
afterward via both `probe_history_series` and `export_observations`).
Full desktop rebuild/retest/clippy clean, plus the `--smoke 60` headless
render check — no driven GUI harness exists, so interactive verification
(open a scene with a saved plot, confirm it repopulates on load instead of
starting blank) remains your own to do.

**Update (2026-08-16, later still):** items 1 and 2 closed.

- **Per-probe `history_capacity` now honored.** `ProbeHistory`
  (`crates/fieldcad-simulation/src/history.rs`) gained a per-`(ProbeId,
  ChannelId)` capacity override (`set_capacity`/`capacity_for`), mirroring
  `BodyHistory::set_capacity` exactly — trims immediately, pruned on
  `retain_probes`. `HeadlessServer::record_observations`
  (`crates/fieldcad-server/src/lib.rs`) syncs every probe's declared
  `history_capacity` into its channels' overrides before recording, gated
  on `WorldRevision` (a probe's capacity never changes after creation, so
  this only needs to run when the probe set could have changed, not on
  every tick — same reuse-if-unchanged discipline `ComputeView::build`
  already uses). Distance/mass-aggregate probes have no declared
  `history_capacity`, so this was field-probe-only, as expected. Tests:
  `history.rs`'s three new capacity tests (mirroring `BodyHistory`'s own),
  `server_side_probe_history_honors_each_probes_own_declared_capacity`
  (`crates/fieldcad-server/tests/headless_session.rs`).
- **MCP read tools added**: `get_probe_history`, `get_distance_history`,
  `get_mass_aggregate_history`, `get_trajectory`,
  `list_recorded_observations` (`crates/fieldcad-mcp/src/lib.rs`). A valid
  id with no recorded readings returns an empty/`null` result; an unknown
  id is a structured error (validated against the live world first).
  Trajectories needed a new blocking primitive —
  `AsyncLocalDataSource::body_history_blocking`/
  `tracked_body_history_objects_blocking`
  (`crates/fieldcad-simulation/src/async_source.rs`), following the
  `capture_document`/`execute_blocking`/`poll_blocking` blocking-round-trip
  pattern exactly, because the existing `request_body_history`/cache pair
  is shaped for the desktop's per-frame polling loop, not a one-shot tool
  call. `HeadlessServer` gained thin wrappers (`probe_history_series`,
  `distance_history_series`, `mass_aggregate_history_series`, `trajectory`,
  `recorded_observations`) — the first three reuse
  `history_capture::capture_*_series`, already written for
  `export_observations`. `BodySample`/`RecordedObservationsInventory` carry
  no `Serialize` (neither `fieldcad-simulation` nor `fieldcad-server`
  depend on `serde`/`serde_json`), so the MCP layer builds its own small
  result DTOs (`TrajectorySampleResult`, `RecordedObservationsInventoryResult`)
  — same pattern `ReplayStep`'s own MCP-side conversion already used. Tests:
  `trajectory_returns_the_free_bodys_recorded_kinematics`,
  `trajectory_for_an_object_with_no_recorded_ticks_is_empty_not_an_error`,
  `recorded_observations_lists_exactly_what_was_actually_recorded`
  (`crates/fieldcad-server/tests/headless_session.rs`).

Items 3 (desktop client-local dedup) and 4 (recorder not at a
transport-neutral boundary) remain deliberately deferred — see below.

**What's already in place, and where it came from:**

- **Probe/distance/mass-aggregate histories are authoritative**, not
  desktop-only. `HeadlessServer::{probe_history, distance_history,
  mass_aggregate_history}` (`crates/fieldcad-server/src/lib.rs`) are fed
  from every published snapshot in `record_observations` (called from
  `publish`), pruned of deleted probes, and used by `save_run`
  (`docs/tasks/run-records-and-comparison.md`) and `export_observations`
  (`docs/tasks/observation-export.md`). This satisfies most of "Move bounded
  probe-history recording to the authoritative source/session boundary."
- **Deduplication by snapshot sequence** and **removal of deleted probes'
  histories** both already work — inherited from
  `fieldcad_simulation::{ProbeHistory, DistanceHistory,
  MassAggregateHistory}`'s own `record`/`retain_probes`, unchanged by this
  work.
- **Run-generation reset now clears server-side histories** (fixed
  2026-08-16): `record_observations` compares the session's current
  `run_generation` against `observed_run_generation` and clears all three
  histories on a mismatch, before recording the reset's own fresh publish.
  Previously only `replace_source` (a whole new/loaded session) cleared
  them — a mid-session `reconfigure_domain`/`set_field_system_configuration`
  left stale readings from the discarded run sitting in the same series as
  the new run's readings. Regression test:
  `a_run_generation_reset_clears_server_side_observation_histories`
  (`crates/fieldcad-server/tests/headless_session.rs`).
- **Object trajectories are already fully authoritative** —
  `fieldcad_simulation::BodyHistory`, owned directly by `SimulationRuntime`
  (`crates/fieldcad-simulation/src/runtime.rs`), populated inside
  `apply_tick`. Bounded (`DEFAULT_BODY_HISTORY = 2048`), with per-object
  capacity override (`set_capacity`), pruned on object deletion
  (`retain_objects`, `runtime.rs:1747`), and cleared on every run-generation
  reset (`runtime.rs:877,1389,1676`). This appears to predate this session's
  work entirely — the "Object trajectories" section of this task's
  "Required model behavior" is done, just not exposed over MCP (see below).

## Remaining work

None. `LocalDataSource`/`LoopbackDataSource` still don't own their own
`ObservationRecorder` — deliberately: no production code constructs either
directly (confirmed by exploration), only test code, always wrapped in
`AsyncLocalDataSource` in every real path. If a genuine need for a bare
transport-neutral consumer with authoritative history appears later, the
snapshot-adoption points for both are already documented in
`observation_recorder.rs`'s module doc, and `observe`/`restore` are
reusable as-is.

## Goal

Make time-series observations authoritative session data rather than a
desktop-only convenience. A desktop client, MCP client, and future remote
compute client must retrieve the same bounded probe histories and dynamic
object trajectories with complete run/snapshot provenance.

## Current limitation

`ProbeHistory` is assembled by desktop `WindowState` from incoming snapshots.
It is therefore unavailable to a headless/MCP session and is not a durable
observation owned by the simulation authority. The runtime exposes only the
current world pose/velocity; it retains no object trajectory.

## Required model behavior

### Probe histories

- Move bounded probe-history recording to the authoritative source/session
  boundary (`HeadlessServer` or a transport-neutral source-owned recorder),
  recording only complete published snapshots.
- Retain samples per `(probe ID, channel ID)` using the probe's declared
  `history_capacity`; do not create an unrelated client-selected buffer size.
- Preserve existing deduplication by snapshot sequence and remove histories of
  deleted probes.
- Each reading includes run generation, tick, simulation time, world revision,
  snapshot sequence, value, unit/representation through its channel, and
  validity.

### Object trajectories

- Add a bounded authoritative trajectory recorder for objects whose authored
  transform or velocity is changed by solver/dynamics ticks.
- Record one sample per object per complete snapshot, deduplicated by snapshot
  identity. A sample includes object ID, transform, velocity, run generation,
  tick, simulation time, world revision, and snapshot sequence.
- Retain a fixed session default of 2,048 samples per object for the first
  increment. Make the bound explicit in status/API responses; do not allow an
  unbounded remote request.
- Remove trajectories for deleted objects. Clear all observation histories when
  run generation changes, because a numerical reset starts a distinct run.

## Server and MCP interface

- Add headless-server reads:
  - `probe_history(probe, channel)`;
  - `trajectory(object)`;
  - optional inventories of tracked `(probe, channel)` pairs and object IDs.
- MCP tools:
  - `get_probe_history(probe_id, channel_plugin, channel_name)`;
  - `get_trajectory(object_id)`;
  - `list_recorded_observations` for discovery without guessing IDs/channels.
- Return immutable ordered readings with explicit provenance. A missing series
  is an empty result, not an error; an invalid entity/channel identifier is a
  structured tool error.
- Once session-event resources exist, publish observation-resource updates as
  invalidations; clients re-read the bounded series rather than receiving an
  unbounded stream.

## Architecture constraints

- The recorder must consume the same immutable snapshots exposed to remote
  clients; it must not query solvers or re-evaluate fields.
- Keep plotting, selected components, colours, and window layout client-local.
- Do not make raw solver state or client wall-clock time part of a reading.
- Recording must work identically behind local, async, loopback, and future
  remote sources. Prefer a transport-neutral recorder interface if placing it
  solely in `HeadlessServer` would diverge from those semantics.

## Tests and acceptance

- Probe history is bounded, deduplicated, provenance-complete, and removed on
  probe deletion.
- Trajectories record solver-driven object motion once per snapshot, are
  bounded, and are removed on object deletion.
- A domain/run reset clears both histories and starts subsequent readings at
  the new run generation.
- Headless server and MCP tests retrieve the same samples a desktop client
  would have plotted from the corresponding snapshots.
- MCP tests cover valid reads, empty histories, invalid IDs/channels, and
  serialization of scalar/vector values plus validity states.

## Relevant code

- `crates/fieldcad-simulation/src/history.rs` — existing probe-history logic.
- `crates/fieldcad-simulation/src/async_source.rs` — authoritative snapshot
  adoption boundary for local async computation.
- `crates/fieldcad-server/src/lib.rs` — shared headless session owner.
- `apps/fieldcad-desktop/src/app.rs` — current desktop-owned recorder to
  remove or adapt after the authoritative recorder exists.
