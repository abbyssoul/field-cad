# Task: authoritative probe histories and object trajectories

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
