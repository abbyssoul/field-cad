# Task: complete missing CAD authoring and reproducibility capabilities

## Short prompt

Implement the missing user-facing Field CAD capabilities: durable scene
lifecycle/files, rename, first-class particle templates, solver-configuration
editing, preflight validation, and reproducible run comparison/recording. Keep
the document model authoritative: desktop UI, file import/export, and MCP must
all use the same typed commands and validation paths.

## Why

The runtime and desktop can author and run a scene, but an experiment is not
yet a durable, complete CAD document. Several ordinary modelling workflows
either require rebuilding entities manually or cannot be performed at all.
These are product/model gaps, not merely missing MCP tools. The canonical user
story inventory is `docs/user-stories/README.md` (US-01, 02, 17, 18, 24–26,
55, 62, and 63).

## Scope and desired outcomes

### P0 — durable authoring

1. **Scene lifecycle and files**
   - Create a named empty or templated scene with a stable experiment ID.
   - Save and load a versioned experiment document.
   - Persist the authored world, numerical domain and time-step configuration,
     field-system composition/configuration, plugin/schema/catalog versions,
     and required provenance.
   - Reject incompatible documents with structured diagnostics; never silently
     reinterpret a scene.

2. **Rename without replacing identity**
   - Rename objects, planes, probes, boxes, and spheres through typed world
     commands.
   - Preserve IDs, attachments, component values, and history semantics.

3. **First-class particle templates**
   - Create electron/proton/positron/neutron/etc. through a typed template
     command rather than requiring clients to assemble component bags.
   - Make available templates/configuration discoverable and versioned.
   - Preserve template provenance until a relevant component is edited.

4. **Field-system configuration editing**
   - Expose each plugin's declared configuration schema and provide a generic
     authoritative command to update valid settings, initial conditions, and
     coupling parameters.
   - Define whether each setting is an authored-state change, a numerical-run
     reset, or a presentation-only setting. A reset must publish a new run
     generation and leave the simulation paused at `t = 0`.

5. **Preflight validation**
   - Add non-mutating validation for proposed world and experiment
     configuration transactions.
   - Reuse the exact authority validation rules; preflight is advisory and a
     commit remains the final check.
   - Return machine-readable errors with affected path/entity and an actionable
     explanation.

### P1 — reproducibility and analysis

6. **Run records and comparison**
   - Retain/name selected snapshots, probe series, and diagnostics from
     separate runs/configurations.
   - Compare records with their complete configuration and provenance so a user
     can explain a difference.

7. **Semantic recording and replay**
   - Record typed commands plus elapsed intervals, then replay against a fresh
     session deterministically.
   - Report divergence using snapshot/run provenance rather than rendered
     output.

8. **Export/share**
   - Export an experiment document and optional named views, recordings, and
     selected observations.
   - Import follows the same compatibility and migration rules as load.

## Architecture constraints

- The core document model owns durable state. UI, MCP, and file codecs are
  adapters that issue commands or serialize declared document types.
- Use stable IDs, not names, for references. Rename is an edit, never a
  delete-and-recreate operation.
- Preserve optimistic world-revision semantics and atomic transactions.
- Keep transient solver memory and client-local presentation state out of the
  base scene document unless deliberately recorded/exported as an optional
  artefact.
- Keep numerical domain changes consistent with the existing reset contract:
  rebuild solvers from authored state, reset to paused `t = 0`, and increment
  run generation.
- MCP parity follows these model capabilities; do not invent a parallel remote
  document format or validation path.

## Suggested delivery sequence

1. Define the versioned experiment-document schema and scene lifecycle API.
2. Add typed rename and particle-template commands, then wire desktop/UI and
   MCP adapters.
3. Add generic plugin-configuration mutation plus preflight validation.
4. Implement save/load/export/import with compatibility tests.
5. Add retained run records, comparison, and semantic replay.

## Acceptance criteria

- A user can author, rename, save, load, and reproduce an experiment without
  losing IDs, schemas, numerical configuration, or provenance.
- A loaded/exported experiment produces the same initial authoritative state
  as the saved one, or reports an explicit incompatibility.
- Every new durable mutation has one authoritative command path, undo/history
  behavior, structured validation errors, and a corresponding MCP operation.
- Tests cover document round trips, incompatibility rejection, rename identity
  preservation, template provenance, configuration reset behavior, preflight
  non-mutation, and deterministic replay.
