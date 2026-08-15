# Task: server-authoritative catalog session

## Status (2026-08-15)

**Core ownership closed.** `HeadlessServer` now holds a `revision: u64` on
`CatalogSession`, bumped and broadcast (`SessionEvent::CatalogUpdated`, MCP
resource `fieldcad://session/catalog`) on every catalog change.
`set_document_catalog` is renamed `restore_document_catalog` and used only
for scene lifecycle restore (startup, `replace_session`, MCP
`open_scene`/`create_scene`); steady-state edits go through
`create_catalog_entry`/`update_catalog_entry`/`delete_catalog_entry`/
`set_quick_add_visibility`. The desktop's `WindowState` document
entries/quick-add/catalog fields are now read-only caches refreshed from the
server (`sync_catalog_cache`/`reload_catalog`) and never pushed back — the
"desktop reload overwrites concurrent MCP edits" defect is closed. Desktop
catalog CRUD (`apply_catalog_action`) now calls the server's CRUD methods
instead of writing YAML directly via `fieldcad_catalog::write`.
Instantiate/preview/apply propagation logic is unified in
`HeadlessServer::resolve_catalog_instantiation`/
`preview_catalog_propagation`/`resolve_catalog_propagation` (resolve-only;
callers submit through their own pipeline — MCP via `submit_and_wait`,
desktop's propagation dialog via `HeadlessServer::submit` directly), and MCP
delegates to them thinly. `fieldcad-mcp` gained CRUD/instantiate/unlink/
preview/apply test coverage (previously none). A desktop propagation
confirmation dialog (`ui/panels/catalog.rs::catalog_propagation_window`)
appears after a catalog entry save when tracking instances exist, with a
per-object selected/all choice and an explicit Apply.

**Deliberately deferred, not done:**
- The confirmation dialog triggers on **save**, not on **reload**. Reload
  can change many entries at once with no cheap way to say which changed;
  offering propagation there needs a report diff, not just a revision bump.
- `apps/fieldcad-desktop/src/ui/panels/object_inspector.rs`'s single-object
  "Apply current template" button and `scene_tree.rs`'s quick-add
  instantiate path still call `fieldcad_catalog::instantiate_template`
  directly rather than the new `HeadlessServer::resolve_catalog_*` methods —
  both submit through the UI's own edit-gesture/undo pipeline (`output.edit`),
  which the resolve-only split was designed to accommodate, but wiring it up
  needs the server reference threaded into inspector rendering, which it
  doesn't currently have. Low-risk, moderate-size follow-up.

## Goal

Make `fieldcad-server` the sole owner of a session's effective catalog:
global source report/fingerprints, document-scoped entries, and scene-local
quick-add preferences. Desktop and MCP must read and mutate this one model, so
an embedded MCP client can never be overwritten by stale desktop-local catalog
state.

## Current limitation

The server has catalog CRUD and MCP tools, but the desktop still keeps mirrors
of document entries, quick-add preferences, and file state. Its reload path
can push those stale mirrors back into the server. The desktop also writes
global and document catalog sources directly instead of using the server's
catalog API. Propagation matching/instantiation is duplicated in MCP and the
desktop has no explicit propagation confirmation workflow.

## Required behaviour

### One session catalog owner

- `HeadlessServer` owns the configured global catalog root, effective report,
  source fingerprints, document entries, quick-add preferences, and a
  monotonically changing catalog revision.
- Scene create/open replaces document-scoped entries and quick-add preferences
  atomically with the session; save captures those values from the server.
- Expose narrow server operations for document-entry CRUD and one-entry
  quick-add visibility changes. Do not retain or accept a broad client-side
  `set_document_catalog` synchronization path after lifecycle restoration.
- A catalog change produces a server event/revision so desktop and MCP
  consumers refresh their cached offer view without polling private state.

### Desktop as client

- Remove `WindowState` ownership of document entries, quick-add preferences,
  catalog source fingerprints, and catalog file writes.
- Route every catalog action — global/document create, update/rename, delete,
  reload, and quick-add hide/show — through `HeadlessServer`.
- Read the catalog list and quick-add state from the server. A successful
  mutation reselects/reseeds the source-qualified entry returned by the
  server.
- Keep starter YAML seeding desktop-only startup convenience; it happens before
  the shared server loads the configured global root.

### Explicit linked-template propagation

- A catalog save or reload only changes catalog offers. It never changes a
  world object.
- Centralize preview and apply in `HeadlessServer`: matching uses tracking link
  source origin, preview reports candidate object IDs/count, and apply resolves
  the current available template then submits one authoritative
  `ApplyCatalogTemplate` transaction for selected or all previewed objects.
- The desktop offers a post-save/reload propagation dialog with affected count,
  selected/all choice, unavailable/no-match explanation, and explicit apply.
- MCP preview/apply tools delegate to the same server operations. Apply remains
  undoable through ordinary authoritative edit history.

## Limitations

- This task does not add shared concurrent catalog editing or merge conflict
  resolution. Source fingerprint conflicts remain non-destructive errors.
- It does not make catalog changes a world mutation, and does not add an MCP
  filesystem path parameter outside the configured catalog root.
- Invalid YAML remains diagnostic-only; structurally valid unavailable entries
  remain editable/removable through the schema-independent catalog model.

## Tests and acceptance

- A document-scope mutation through embedded MCP is immediately visible in the
  desktop catalog and cannot be overwritten by a desktop reload; the converse
  holds for desktop edits.
- Server scene save/open/new round trips document entries and quick-add
  preferences without a desktop-owned copy.
- Server CRUD preserves document UUIDs and selected YAML stream positions,
  rejects collisions/read-only/conflict writes, and reports catalog revision
  changes.
- Desktop actions call server catalog APIs; no desktop-local catalog write or
  merge helper remains.
- Propagation preview is non-mutating; apply affects exactly selected tracking
  objects, is atomic, and undo restores previous resolved values/link
  fingerprints.
- MCP catalog CRUD, instantiate, unlink, preview, and apply tests use the
  server APIs, including unavailable-entry and stale-reference failures.
- Run catalog, server, MCP, desktop, and scene-document tests, MCP Unix-socket
  transport tests, desktop smoke check, and manual embedded desktop+MCP
  synchronization/propagation verification.

## Relevant code

- `crates/fieldcad-server/src/lib.rs` — catalog session ownership, lifecycle,
  revision/events, and propagation authority.
- `apps/fieldcad-desktop/src/app.rs` and `ui/panels/catalog.rs` — convert the
  desktop into a server catalog client and add propagation confirmation.
- `crates/fieldcad-mcp/src/lib.rs` — thin catalog tool delegation.
- `crates/fieldcad-scene-document/src/lib.rs` — durable document catalog state.
- `docs/adr/0007-validate-before-adopting-a-world-edit.md` — propagation must
  remain an atomic, validated authoritative world edit.
