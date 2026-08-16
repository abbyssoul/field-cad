# Task: live observation event stream for external tools

## Status: open

## Goal

Every plotable value — field-probe readings per channel, distance-probe
series, and mass-aggregate ("center of mass") readings — should be
streamable live from the server. A user subscribes to an event stream
(server-sent events) and tracks the same values the app measures with
external tools (curl, a Python script, a dashboard bridge), rather than
only after the fact through a file export.

Origin: `goal.md` backlog item "All plotable values: Prob recorded, CoM
computed etc. should be exportable via a server. As a user I should be able
to subscribe to an event stream (server side events?) and track the same
values measured by the app using external tools."

## Current limitation

The batch half of this is done; the live half does not exist:

- Server-retained, authoritative observation histories exist (P1-6 of
  `docs/tasks/product-capability-gaps-audit.md`):
  `HeadlessServer::{probe_history, distance_history,
  mass_aggregate_history}`, folded from every published snapshot in
  `publish()`/`record_observations` (`crates/fieldcad-server/src/lib.rs`).
  Bounded per probe's `history_capacity`, deduplicated by snapshot
  sequence, pruned on probe deletion.
- Batch export exists (P1-8, `docs/tasks/observation-export.md`):
  `HeadlessServer::export_observations(ObservationExportScope)` →
  `fieldcad.observation-export/v1`, MCP `export_experiment`/
  `import_experiment`, desktop Export buttons. After-the-fact and
  file-based only.
- `EventHub` (`crates/fieldcad-server/src/event_hub.rs`) broadcasts
  invalidation signals (`SnapshotUpdated`, …) — deliberately never
  payloads. MCP exposes it through resources plus `subscriptions/listen`
  (`crates/fieldcad-mcp/src/lib.rs`), but: only MCP clients can consume
  it; there is no observation-series resource (the snapshot resource is
  the full field grid, not the plotable scalar series); and a tool that
  does not speak MCP has nothing to connect to.

So no path exists for a non-MCP external tool to live-track the values the
app plots.

## Design decision: fold into `fieldcad-mcp`, not a new crate

`serve_http`/`serve_unix` already build a plain axum `Router`
(`crates/fieldcad-mcp/src/transport.rs`) wrapped whole by
`require_bearer_token`, and `McpServer` already holds the
`Arc<Mutex<HeadlessServer>>` a stream handler needs. Mounting the stream
next to `/mcp` reuses the token auth, loopback-only binding, desktop
embedding (`apps/fieldcad-desktop/src/mcp.rs`'s dedicated thread), the
standalone drive loop, and the binary — one port, one token, one "Enable
MCP" toggle covers both surfaces. A separate transport crate would
duplicate all of that hosting machinery for one route family. The cost is
one documentation line: this crate's charter wording ("thin MCP transport
over `fieldcad-server`", in `AGENTS.md` and the crate module doc) widens
to "MCP tool surface plus observation event stream — thin transports over
`fieldcad-server`".

## Required behavior

### Server: cursor reads over the retained histories (`fieldcad-server`)

- `HeadlessServer::observation_updates_since(after_sequence: Option<u64>)`
  — every reading with `snapshot_sequence > after` across all non-empty
  probe/distance/mass-aggregate series, reusing
  `crates/fieldcad-server/src/history_capture.rs`'s existing
  `*ReadingRecord` types (already the serializable mirrors with full
  provenance: tick, simulation time, world revision, snapshot sequence,
  value, validity). `None` means "everything retained".
- A tracked-series inventory read (probe/channel pairs, distance probes,
  mass-aggregate probes with data) for a subscriber's initial `hello` —
  coordinate with `docs/tasks/authoritative-observation-history.md`'s
  `list_recorded_observations` so one read serves both.
- No new `SessionEvent` variant: `SnapshotUpdated` already fires after
  `record_observations()` inside `publish()`, and `SnapshotIdentity`
  carries sequence plus `run_generation`, which is enough for a subscriber
  to detect both new readings and a run reset.

### Transport: SSE endpoint in `fieldcad-mcp`

- `GET /observations/stream` (server-sent events, axum) mounted on the
  same router as `/mcp` in both `serve_http` and `serve_unix`, behind the
  same bearer-token middleware.
- On connect: a `hello` event with session id, current `run_generation`,
  and the tracked-series inventory. Then one event per published snapshot
  carrying that snapshot's delta readings, with the SSE `id:` set to the
  snapshot sequence. Keep-alive comments so proxies/idle clients do not
  drop the connection.
- On `WatchEvent::Lagged`, or on a `run_generation` bump (histories are
  cleared on a run reset), emit a `resync` event instead of deltas; the
  client refetches via the catch-up read below.
- Deliberate departure from `EventHub`'s invalidation-only discipline, and
  worth a short ADR at implementation time (`AGENTS.md` asks for one on
  expensive-to-reverse decisions; this defines a wire contract): external
  tools are the point of this stream, the payloads are small per-snapshot
  scalar readings with provenance — never field grids — and a per-event
  HTTP re-read would cost more than the payload it fetches.
- Reconnect catch-up: honor `Last-Event-ID` by replaying retained readings
  newer than that sequence from the bounded histories — this is exactly
  what P1-6's server-side retention buys. A cursor older than the retained
  window gets a `resync`, not silent gaps.
- `GET /observations` catch-up read with probe/channel/distance/
  mass-aggregate scope filters, returning the same JSON shape
  `export_observations`/`ObservationExportScope` already produce
  (`crates/fieldcad-scene-document/src/observation_export.rs`) — the
  server method grows a "return the value" path; the file write stays
  optional.

### Security

- Identical boundary to MCP HTTP (`docs/mcp-plan.md` phase 5 rule):
  loopback-only bind, bearer token required. The Unix-socket transport
  keeps its file-permissions trust boundary. No non-loopback opt-in is
  introduced by this task.

### MCP parity (small, do together)

- Add a `fieldcad://session/observations` resource invalidated on
  `SnapshotUpdated` (extend `affected_resource_uris`), so MCP-native
  clients get the same liveness signal the SSE stream carries.
- The open `docs/tasks/authoritative-observation-history.md` MCP reads
  (`get_probe_history`, `list_recorded_observations`) draw on the same new
  server read — implement them in the same change. Object trajectories
  (`body_history.rs`) are a later series kind for the same stream, not
  this task's scope.

### Desktop

- No new panel: the existing "Enable MCP" toggle already starts the
  listener that now also serves the stream, on the same port and token.
  Show the stream URL alongside the existing MCP address/token display so
  a user knows what to point external tools at. State plainly that no
  driven GUI harness exists (`apps/fieldcad-desktop/AGENTS.md`);
  verification tops out at build/test/clippy plus the smoke run, with the
  embedded stream checked manually (e.g. `curl -N` against the running
  app).

## Tests and acceptance

- `observation_updates_since`: cursor semantics (only readings newer than
  the sequence), empty-series omission, and clearing on run reset —
  driven at the `HeadlessServer` level the way
  `crates/fieldcad-server/tests/headless_session.rs` already does.
- Stream equivalence: an integration test drives a real charge-and-probe
  scene through the async worker, connects an SSE client, and asserts the
  events received reproduce the server-retained readings — same samples,
  units, and provenance — the desktop would have plotted from the same
  snapshots (mirrors `authoritative-observation-history.md`'s acceptance).
- `Last-Event-ID` replay returns exactly the retained readings newer than
  the cursor; a cursor older than the retained window yields `resync`.
- Auth: a request without the bearer token is rejected; loopback-only
  binding behavior is unchanged.
- No allocation added to any simulation hot path: delta extraction walks
  the existing history deques under a brief per-snapshot lock held by the
  stream task, never by the tick loop.

## Relevant code

- `crates/fieldcad-server/src/lib.rs` — `publish()`/`record_observations`
  (~line 838-874), `probe_history()`/`distance_history()`/
  `mass_aggregate_history()` (~1056-1064), `export_observations` (~1146),
  `subscribe_events` (~894).
- `crates/fieldcad-server/src/event_hub.rs` — `EventHub`/`SessionEvent`/
  `EventWatcher`, the `Lagged`-means-resync discipline this task reuses.
- `crates/fieldcad-server/src/history_capture.rs` — serializable reading
  records and scoped capture helpers to reuse for deltas and catch-up.
- `crates/fieldcad-mcp/src/transport.rs` — `serve_http`/`serve_unix`
  router construction (~162/~279), `require_bearer_token`,
  `generate_token`.
- `crates/fieldcad-mcp/src/lib.rs` — `McpServer.model` (~798),
  `listen()`/`affected_resource_uris` (~2277/~741) as the MCP-native
  subscription path to extend.
- `apps/fieldcad-desktop/src/mcp.rs` — embedded hosting pattern the stream
  inherits automatically; where the URL/token display lives.
- `crates/fieldcad-scene-document/src/observation_export.rs` —
  `ObservationExport`/`ObservationExportScope` shape reused for the
  catch-up read.
- `docs/tasks/authoritative-observation-history.md` — related open task
  sharing the same server read.
- `docs/mcp-plan.md` — loopback/token security boundary the stream must
  not relax.
