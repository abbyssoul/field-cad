# Task completion audit: CAD authoring and reproducibility capabilities

## Status: P0 complete (5/5), P1 complete (3/3)

**Update (2026-08-16, verification run):** the re-review planned in
`docs/tasks/product-capability-gaps-verification-plan.md` was run against the
working tree. P1-6/7/8 and the landed P0-4/P0-5 surfaces all verified, but
the first audit's follow-up points were found **unaddressed**: configuration
changes are still not undoable (A5), there are still no queued-path or
MCP-level tests for `SetFieldSystemConfiguration`/`validate_world_transaction`
(A6/A7/B5), the desktop config editor was never manually verified (C), and
this document's P0-4/P0-5 section headers below still say "missing" (E3).
Every outstanding item is aggregated in
`docs/tasks/product-capability-gaps-completion.md` — treat that document as
the authoritative completion checklist; the section bodies below describe the
state at first audit (2026-08-15) and are superseded by the summary table and
update notes.

**Update (2026-08-16, later still):** P1-8 closed. New file format
`fieldcad.observation-export/v1`
(`crates/fieldcad-scene-document/src/observation_export.rs`) exports a
caller-scoped subset of a session's retained observations — probe/channel
series, distance-probe series, mass-aggregate-probe series, and optionally
the current field snapshot — independent of the whole scene document.
`fieldcad_server::{ObservationExportScope, HeadlessServer::
export_observations}` build it from the server-side-retained histories
P1-6 introduced; a probe/channel named in the scope with no recorded
readings is simply absent from the result, never an error. MCP:
`export_experiment`/`import_experiment`. Desktop: an "Export…" button on
each of the three probe-kind inspector panels, scoped to that one probe.
Session recording itself (originally bundled into this same task doc) was
already split out and closed as P1-7. See
`docs/tasks/observation-export.md`.

**Update (2026-08-16, later same day):** P1-7 closed. `SessionRecording`
(`crates/fieldcad-simulation/src/recording.rs`) now gains `Serialize`/
`Deserialize`. Two new blocking primitives on `AsyncLocalDataSource`
(`execute_blocking`/`poll_blocking`, `crates/fieldcad-simulation/src/
async_source.rs`) let `HeadlessServer::replay_recording` fully settle each
recorded event before the next, over the async transport this crate runs
on — the async equivalent of `recording.rs`'s own synchronous replay-
equivalence tests, reused as the model. `HeadlessServer::{start_recording,
stop_recording, is_recording, replay_recording}` capture every
`execute`/`advance` call while active (skipping zero-duration polls — pure
busy-wait noise from any transport's own completion-wait loop, never a
moment of simulated time). Recordings persist as their own
`fieldcad.recording/v1` file (`fieldcad_scene_document::{save_recording_to_path,
load_recording_from_path}`), distinct from a scene document. MCP:
`start_recording`/`stop_recording`/`recording_status`/`replay_session`.
Desktop: File-menu "Start Recording"/"Stop Recording…"/"Replay Recording…"
plus a menu-bar "● Recording" indicator, manually smoke-tested via `cargo
run -p fieldcad-desktop -- --smoke 60`. See
`docs/tasks/session-recording-and-replay.md`.

**Update (2026-08-16):** P1-6 closed. Named, retained run records
(`fieldcad_scene_document::RunRecord`) capture `run_generation`,
domain/time-step, field-system composition, and a copy of a new
server-side-retained probe/distance/mass-aggregate observation history
(`HeadlessServer::{probe_history,distance_history,mass_aggregate_history}`,
fed on every `publish()` — previously this history was a desktop-only,
client-local concern per this doc's own `fieldcad-mcp` module-doc citation
below; MCP sessions now retain it too). Exposed as MCP
`save_run`/`list_runs`/`get_run`/`delete_run`/`compare_runs` and persisted
with the scene document (`SceneDocument.run_records`, `FORMAT_VERSION` 6 → 7).
See `docs/tasks/run-records-and-comparison.md`.

Audit of `docs/tasks/product-capability-gaps.md` against current code
(2026-08-15). Verification was source-level: command enums, MCP tool
registration, desktop panels, and the scene-document crate. Every claim below
names its file/line evidence.

**Update (2026-08-15, same day):** P0-4 and P0-5 closed. `CommandPayload::SetFieldSystemConfiguration`
(reset-class, mirrors `ReconfigureDomain`: queued at the tick boundary, rebuilds
active solvers, resets to paused `t = 0`, advances `run_generation`) lands in
`crates/fieldcad-simulation/src/{source,runtime}.rs`, exposed as MCP
`set_field_system_configuration` and an editable desktop panel
(`apps/fieldcad-desktop/src/ui/panels/world_inspector.rs`). Non-mutating
preflight (`SimulationRuntime::validate_world_commands` /
`validate_field_system_configuration`, threaded through
`AsyncLocalDataSource`/`HeadlessServer`) is exposed as MCP
`validate_world_transaction`, covering both a `commit_world`/`edit_world`-shaped
batch and a proposed field-system configuration. The stale doc markers below
and in `docs/user-stories/README.md` (US-17, US-18, US-24, US-26) are
corrected in the same change.

## Summary

| Capability | Item | Status | MCP-exposed? |
| --- | --- | --- | --- |
| Scene lifecycle/files | P0-1 | **Done** | Yes — `create_scene` / `open_scene` / `save_scene` |
| Rename without replacing identity | P0-2 | **Done** | Yes — typed `edit_world` / `commit_world` |
| First-class particle templates | P0-3 | **Done** | Yes — catalog tools + `instantiate_catalog_entry` |
| Solver-configuration editing | P0-4 | **Done** | Yes — `set_field_system_configuration` |
| Preflight validation | P0-5 | **Done** | Yes — `validate_world_transaction` |
| Run records and comparison | P1-6 | **Done** | Yes — `save_run`/`list_runs`/`get_run`/`delete_run`/`compare_runs` |
| Semantic recording and replay | P1-7 | **Done** | Yes — `start_recording`/`stop_recording`/`recording_status`/`replay_session` |
| Export/share | P1-8 | **Done** | Yes — `export_experiment`/`import_experiment` |

## P0-1 — Scene lifecycle and files: done

- `fieldcad.scene/v1` format, `FORMAT_VERSION` 6, `.fcscene` extension
  (`crates/fieldcad-scene-document/src/lib.rs:47-60`).
- Versioned document: world, domain, time step, playback speed, scene scale,
  integration scheme, field-system composition **including per-plugin
  `configuration`** (`FieldSystemComposition`, `lib.rs:152-160`), plugin
  version resolution with structured rejection of unknown/major-mismatched
  plugins (`resolve_plugins`, `lib.rs:306-394`), paused-queue write-ahead log.
- Atomic save with fsync and one `.bak` of the previous verified document
  (`lib.rs:490-511`).
- MCP: `save_scene` / `open_scene` / `create_scene`
  (`crates/fieldcad-mcp/src/lib.rs:1384-1488`), fresh session id per scene
  (`lib.rs:1574-1580`), session always starts paused.
- Desktop: File > New (Empty/Demo), Open, Save/Save As (`apps/fieldcad-desktop/src/app.rs:2187-2215, 2695-2760`).

Acceptance satisfied: load never reinterprets; IDs survive round trips.

## P0-2 — Rename without replacing identity: done

- Typed commands in `WorldCommand` for every authored entity, all stable-ID
  edits (never delete-and-recreate): `SetObjectName`, `SetPlaneName`,
  `SetBoxName`, `SetSphereName`, `SetProbeName`, `SetDistanceProbeName`,
  `SetMassAggregateProbeName`
  (`crates/fieldcad-core/src/world.rs:1483-1640`).
- MCP: typed `WorldEditParam::{SetObjectName, SetPlaneName, SetBoxName,
  SetSphereName, SetProbeName}` (`crates/fieldcad-mcp/src/typed_world.rs:363-501`)
  plus raw `commit_world` for the remaining variants.
- Desktop: rename fields in object/probe/plane/box/sphere inspectors
  (`apps/fieldcad-desktop/src/ui/panels/object_inspector.rs:141`,
  `probe_inspector.rs:23`, `shape_inspector.rs:30,398,571`).
- Undo/history semantics: rename is one labelled edit (`WorldCommand::label`).

Minor gap: distance-probe and mass-aggregate-probe renames are absent from
the typed `WorldEditParam`; they work only via raw `commit_world`.

## P0-3 — First-class particle templates: done

- `fieldcad-catalog` crate: versioned YAML (`fieldcad.catalog/v1`), atomic
  writes, source-qualified identities, template provenance with
  `CatalogLink`, `UnlinkCatalogTemplate`, `ApplyCatalogTemplate`,
  `LinkCatalogTemplate` (`crates/fieldcad-core/src/world.rs:1508-1534`).
- Reference templates ship as data: `etc/catalogs/particles.yaml` (electron
  with CODATA provenance, and further particles), `etc/catalogs/planets.yaml`;
  desktop seeds a bundled `starter_catalog.yaml` on first run
  (`apps/fieldcad-desktop/src/catalog.rs:19-27`).
- MCP: `list/get/create/update/delete_catalog_entry`, `reload_catalog`,
  `instantiate_catalog_entry`, `link_catalog_instance`,
  `unlink_catalog_instance`, `preview_catalog_propagation`,
  `apply_catalog_propagation` (`crates/fieldcad-mcp/src/lib.rs:687-925`).
- Desktop catalog panel (Add/rename/fill quick-add) plus world-inspector
  link/unlink.
- ADR 0019: templates are data and provenance, not hidden species physics.

## P0-4 — Solver-configuration editing: missing

The declared schema is fully plumbed, but there is **no mutation path**:

- `PluginConfigurationSchema` on the plugin trait, validated at runtime
  construction (`crates/fieldcad-simulation/src/runtime.rs:724-725`) and at
  document resolve (`crates/fieldcad-scene-document/src/lib.rs:347-349`).
- Persisted per plugin in `SceneDocument.field_systems[].configuration`
  (`crates/fieldcad-scene-document/src/lib.rs:160`).
- However `CommandPayload` has no configuration variant (enable/disable,
  realtime, field model, domain, time step exist — `configuration` does not;
  `crates/fieldcad-simulation/src/source.rs:69-161`), there is no MCP
  `set_field_system_configuration` tool, and the desktop world inspector
  renders settings **read-only** (`apps/fieldcad-desktop/src/ui/panels/world_inspector.rs:419-439`).

Blocking the P0-4 acceptance criteria (validate-before-adopt, run-generation
semantics for reset-class settings, "accepted snapshots report the values
that produced them").

## P0-5 — Preflight validation: missing

- No non-mutating preflight/dry-run API anywhere in the workspace
  (`validate_world_transaction` does not exist; greps for
  preflight/dry_run/validate_edit find nothing).
- Validation happens only at commit/adopt: world edits at
  `crates/fieldcad-simulation/src/runtime.rs:1943` and plugin/domain at
  construction. The only "preview" is catalog-specific
  `preview_catalog_propagation`.
- The Suggested MCP surface (`docs/user-stories/README.md:442`) lists
  `validate_world_transaction`; the authority-side machinery exists to reuse,
  but no advisory, non-mutating entry point has been added.

## P1-6 — Run records and comparison: missing

- Probe/distance/mass-aggregate histories and body trajectories are retained
  and persisted (`fieldcad-simulation/src/history.rs`, `body_history.rs`), but
  there are no named runs, no retained run records keyed by configuration,
  and no comparison capability (US-55 not implemented).

## P1-7 — Semantic recording and replay: runtime-only

- `SessionRecording` / `RecordedEvent` / `replay` exist in
  `crates/fieldcad-simulation/src/recording.rs` (used by tests in
  `src/lib.rs:2247-2367`).
- Not wired anywhere user-facing: no desktop integration, no server method,
  no MCP `record_session` / `replay_session` tools.

## P1-8 — Export/share: partial

- Save/load/import with compatibility and migration rules: done (P0-1).
- Named views: the document carries `SceneViewState`, and the desktop
  captures/restores it; MCP `save_scene` writes default view state
  (`crates/fieldcad-mcp/src/lib.rs:1406-1409`).
- Recordings and selected-observation export/import: not implemented.

## Stale documentation found

- `crates/fieldcad-mcp/src/lib.rs:15-19` still lists scene lifecycle,
  particle templates, and rename as "left for later" — all three are
  implemented.
- `docs/user-stories/README.md` US-17 ("Required for API/MCP parity") and
  US-18 ("dedicated rename commands are not yet present") are outdated;
  both are implemented. US-26 is marked "Implemented in authority; Required
  for API/MCP exposure" but no preflight entry point exists yet — the status
  note should be corrected when P0-5 lands.

## Recommended next steps

1. ~~Close P0-4~~ — done (see the update note above).
2. ~~Close P0-5~~ — done (see the update note above).
3. ~~Refresh the two stale doc markers above~~ — done, plus US-24.
4. Follow-ons (P1, still open): named-run records with comparison, wiring
   `SessionRecording` into server/MCP, and recording/observation export. See
   `docs/tasks/run-records-and-comparison.md`,
   `docs/tasks/session-recording-and-replay.md`, and
   `docs/tasks/observation-export.md`.
