# Task: authoritative probe histories and object trajectories

## Status: partially done (2026-08-16) — see "Remaining work" below

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

1. **Field-probe `history_capacity` isn't honored server-side.**
   `ProbeSpec.history_capacity` (`crates/fieldcad-core/src/world.rs:853`) is
   a real per-probe declared field, but `HeadlessServer.probe_history` uses
   one uniform `ProbeHistory::default()` capacity (2048) for every probe —
   exactly "an unrelated client-selected buffer size," which this task's
   "Required model behavior" says not to do. `ProbeHistory` itself has no
   per-key capacity override today (unlike `BodyHistory`, which already
   supports one via `set_capacity`/an internal `capacities` map). Fix
   options, in order of how closely they follow existing precedent:
   - Add a `ProbeHistory::set_capacity(probe, channel, capacity)` mirroring
     `BodyHistory::set_capacity` exactly, then have `HeadlessServer` read
     each probe's `history_capacity` off the world and call it whenever a
     probe is created/edited (watch `WorldCommand::CreateProbe` and any
     future "set probe history capacity" command) — most faithful to the
     task's letter, moderate-sized change to `fieldcad-simulation`.
   - Note distance-probe and mass-aggregate-probe specs have **no** declared
     `history_capacity` field (checked: `DistanceProbeSpec`,
     `MassAggregateProbeSpec` in `world.rs` have none) — this gap is
     field-probe-only.
2. **No MCP read tools for any of this.** `get_probe_history`,
   `get_trajectory`, and `list_recorded_observations` (all named in this
   task's "Server and MCP interface") don't exist. Needed:
   - `HeadlessServer::probe_history(probe, channel)`/`distance_history(probe)`/
     `mass_aggregate_history(probe)` single-series reads — the underlying
     `&ProbeHistory`/etc. accessors already exist
     (`HeadlessServer::probe_history()` etc., added for `save_run`); a
     single-series read is a thin wrapper (`history.readings(probe,
     channel).collect()`), similar in shape to
     `history_capture::capture_probe_series` already written for
     `export_observations`.
   - A **blocking** trajectory read. `HeadlessServer::request_body_history`/
     `body_history(object)` already exist but are a fire-and-forget-request-
     then-poll-drains-a-cache pair, designed for the desktop's per-frame
     polling loop — wrong shape for a synchronous one-shot MCP tool call
     (there's no event to wait on; the result just eventually appears in a
     cache after enough `advance(ZERO)` calls). Add
     `AsyncLocalDataSource::body_history_blocking(object) -> Vec<BodySample>`
     following the exact `capture_document`/`execute_blocking`/
     `poll_blocking` pattern (`crates/fieldcad-simulation/src/
     async_source.rs`) — send the request, block on `self.events.recv()`
     until `WorkerEvent::BodyHistoryCaptured` for that object arrives,
     applying any other event encountered along the way exactly like those
     three already do.
   - MCP tools: `get_probe_history(probe, channel)`, `get_distance_history(probe)`,
     `get_mass_aggregate_history(probe)` (three, or one tagged-enum tool —
     match whichever shape `list_runs`/`save_run` already established feels
     more consistent with), `get_trajectory(object)`, and
     `list_recorded_observations` (every history type already has a
     `tracked()` method — `ProbeHistory::tracked()`,
     `DistanceHistory::tracked()`, `MassAggregateHistory::tracked()`,
     `BodyHistory::tracked()` — reporting which series actually have
     retained readings right now, without a caller having to guess IDs via
     `get_world` first).
   - Per this task's own requirement: "a missing series is an empty result,
     not an error; an invalid entity/channel identifier is a structured tool
     error" — validate the id/channel against the live world
     (`get_world`'s own objects/probes) before returning, same as every
     other MCP tool's id-resolution convention.
3. **Desktop still duplicates the recording client-side.** `app.rs`'s
   `self.probe_history`/`self.distance_history`/`self.mass_aggregate_history`
   (fed by polling snapshots in the render loop,
   `apps/fieldcad-desktop/src/probe_history_state.rs`) are now redundant
   with the server-side copy `HeadlessServer` maintains — both are fed by
   the same snapshots, just on different cadences. This task's own
   "Relevant code" section names `app.rs`'s recorder for "remove or adapt."
   Given the desktop is a synchronous, same-process caller of
   `HeadlessServer`, the client-local copy could be dropped entirely in
   favor of reading `HeadlessServer::probe_history()`/etc. directly each
   frame — but this needs care: `FrameContext.probe_history` currently
   passes `&ProbeHistory` by reference into every history-plotting panel,
   and switching the source changes nothing about that shape, only where
   the desktop code populates the field from wall-clock-cheap reads instead
   of its own accumulation loop. Do this last, after the MCP reads above
   land and are exercised — it's a pure simplification, not a new
   capability, and safest done once the authoritative side has more mileage
   on it.
4. **Recorder lives in `HeadlessServer`, not a transport-neutral boundary**
   (architecture constraint: "prefer a transport-neutral recorder interface
   if placing it solely in `HeadlessServer` would diverge from
   [local/async/loopback/remote] semantics"). A bare `LocalDataSource`/
   `AsyncLocalDataSource` consumer without `HeadlessServer` gets no
   authoritative probe/distance/mass-aggregate history today (though it
   *does* get authoritative trajectories, since `BodyHistory` lives on
   `SimulationRuntime` itself, one layer lower). In this codebase
   `HeadlessServer` is the only real session owner every transport (desktop,
   MCP) actually goes through, so this is likely acceptable as-is — flagging
   it rather than prescribing a fix, since moving the recorder down to
   `SimulationRuntime`/`LocalDataSource` would be a real architecture change,
   not a small addition, and nothing in this codebase today needs
   authoritative probe history from a bare `LocalDataSource`.

## Suggested order

(1) is the only correctness/fidelity gap with no MCP dependency — do it
first if picked up in isolation. (2) is the bulk of the remaining task and
should land as one pass (all three MCP tools + the blocking trajectory
primitive together, mirroring how P1-6/P1-7/P1-8 each landed server+MCP+
tests together). (3) depends on (2) existing and being trustworthy. (4) is a
documented trade-off, not planned work, unless a concrete need for a bare
transport-neutral consumer shows up.

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
