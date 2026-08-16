# Task: recording and selected-observation export/import

## Status: resolved (P1-8 of `docs/tasks/product-capability-gaps-audit.md`)

**Verification (2026-08-16):** confirmed done by
`docs/tasks/product-capability-gaps-verification-plan.md` item D3. One
deliberate-or-not deviation is tracked in
`docs/tasks/product-capability-gaps-completion.md` (item 12): the export
scope selects probes/channels and the latest snapshot, but offers no
time-range selection, which this task's "Required behavior" listed as one
scope shape.

**Resolution (2026-08-16):** Recording itself was split out to P1-7
(`docs/tasks/session-recording-and-replay.md`, resolved earlier the same
day); this closes the remaining "selected-observation export/import" scope.

- New file format `fieldcad.observation-export/v1`
  (`crates/fieldcad-scene-document/src/observation_export.rs`,
  `ObservationExport`/`ExportMetadata` + `save_observation_export_to_path`/
  `load_observation_export_from_path`), following
  `crates/fieldcad-scene-document/src/recording_file.rs`'s established
  "small sibling file, no atomic-write/backup" shape (written once, read
  once — unlike `fieldcad.scene/v1`, never repeatedly resaved over the same
  path). The "a specific snapshot" scope embeds the real
  `fieldcad_core::FieldSnapshot` directly — it and everything it's built
  from already derived `Serialize`/`Deserialize`, so no new DTO was needed.
- `fieldcad_server::ObservationExportScope` (probes/channels, distance
  probes, mass-aggregate probes, `include_latest_snapshot`) plus
  `HeadlessServer::export_observations` (`crates/fieldcad-server/src/
  lib.rs`) build the export from this session's server-side-retained
  histories (`probe_history`/`distance_history`/`mass_aggregate_history` —
  see P1-6's resolution for why those are retained server-side at all now).
  A probe/channel named in the scope but never recorded is simply absent
  from the result, not an error — same discipline
  `ProbeHistory::entries`/`ProbeHistoryState` already apply to "every
  *non-empty* series." New scoped capture helpers
  (`crates/fieldcad-server/src/history_capture.rs`:
  `capture_probe_series`/`capture_distance_series`/
  `capture_mass_aggregate_series`) sit alongside the existing whole-history
  ones `save_run` uses.
- MCP: `export_experiment` (scope params + `path`, plus `max_samples` —
  reuses `get_latest_snapshot`'s existing per-channel-breakdown
  size-rejection discipline for the `include_latest_snapshot` path, since
  that's the one part of an export an MCP transport's response-size budget
  actually constrains) and `import_experiment` (`path` → the full decoded
  `ObservationExport`, for local inspection — never merged into the live
  session, satisfying that requirement literally: there is no merge
  operation at all).
- Desktop: an "Export…" button on each of the three probe-kind inspector
  panels (`ui/panels/{probe,distance_probe,mass_aggregate_probe}_inspector.rs`),
  each scoped to *that one probe* (every channel it records, for the
  field-probe case) — deliberately narrower than the MCP surface (no
  multi-probe selection UI, no snapshot-inclusion toggle from the desktop);
  verified via `cargo build`/`test`/`clippy` and `cargo run -p
  fieldcad-desktop -- --smoke 60`, not interactive manual use (no driven GUI
  harness exists, `apps/fieldcad-desktop/AGENTS.md`).

Tests: `observation_export.rs` (round-trip, wrong format, unsupported
version), `crates/fieldcad-server/tests/headless_session.rs`
(`export_observations_includes_only_the_requested_probe_and_channel`,
`export_observations_never_includes_an_unrequested_probe` — the "never
requires or emits the rest of the scene" acceptance criterion — and
`exported_probe_history_round_trips_through_a_file_bit_for_bit`, driven off
a real charge-and-probe scene stepped through the async worker, matching
this task's own "reproduces the same samples, units, and provenance
metadata bit-for-bit" acceptance criterion).

---

## Goal

Let a modeller export a recording or a selected set of observations (probe
history, distance/mass-aggregate readings, a snapshot) to a portable file, and
import one back in, so results can leave the running session — shared,
archived, or diffed outside the app — not just saved/loaded as a whole scene.

## Current limitation

Save/load/import of the whole scene document is done (P0-1 in
`docs/tasks/product-capability-gaps-audit.md`): `fieldcad.scene/v1`, atomic
writes, migration/compatibility rules
(`crates/fieldcad-scene-document/src/lib.rs`). Named views round-trip too
(`SceneViewState`, captured/restored by the desktop, written by MCP
`save_scene`). But there is no way to export a *subset* — one probe's
recorded history, one distance-probe series, a single snapshot — independent
of the whole scene, and no import path for such a file. Sharing "here's what
this run observed" today means sharing the entire scene document, which drags
along the authored world and every plugin configuration whether or not the
recipient wants them.

## Required behavior

- An export operation that accepts a scope (one or more probes/channels, a
  time range, or a specific snapshot) and produces a self-contained, versioned
  file distinct from a full `fieldcad.scene/v1` document — its own format
  version and validity/provenance metadata, per `AGENTS.md`'s "publish
  immutable, versioned observations with validity and provenance" boundary.
- An import operation that reads such a file back for local inspection
  (plotting, comparison) without requiring it to be merged into a live scene.
- MCP exposure (`export_experiment` is named in the Suggested MCP surface
  table in `docs/user-stories/README.md`) alongside a desktop affordance —
  state plainly that desktop verification is manual, no driven GUI harness
  exists.

## Tests and acceptance

- Exporting a probe's history and reimporting it reproduces the same samples,
  units, and provenance metadata bit-for-bit.
- An export file's format version is checked on import, with a structured
  rejection for an unknown/incompatible version — same discipline
  `fieldcad-scene-document::resolve_plugins` already applies to plugin
  versions.
- Exporting a scoped selection never requires or emits the rest of the scene
  (authored world, unrelated plugin configurations).

## Relevant code

- `crates/fieldcad-scene-document/src/lib.rs` — existing versioned-document
  conventions (`FORMAT_VERSION`, atomic save with `.bak`, resolve/migration
  pattern) to follow for a new, narrower export format.
- `crates/fieldcad-simulation/src/history.rs`, `body_history.rs` — the
  retained observation data an export would draw from.
- `crates/fieldcad-mcp/src/lib.rs` — where `export_experiment`/an import tool
  would be registered.
- `docs/user-stories/README.md` — "Reproducibility" row of the Suggested MCP
  surface table.
