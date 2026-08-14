# Task: server-authoritative catalog session

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
