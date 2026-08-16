# Task: recording and selected-observation export/import

## Status: open (remainder of P1-8 in `docs/tasks/product-capability-gaps-audit.md`)

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
