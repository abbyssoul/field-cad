# Task: configuration-driven, linkable object catalog

## Goal

Replace the compiled desktop object/particle preset list with a user-configured
catalog of generic object templates. A catalog entry is a YAML document on disk
that describes an authorable object kind, its shape, and a schema-validated
component/property bag. Users can discover, search, add, edit, hide, reload,
and, where permitted, write these entries from Field CAD.

Instantiation must remain an authoritative scene mutation: a catalog is an
authoring convenience, never a second world model or a solver-specific object
creation path. A created object retains resolved authored values so its scene
opens without the catalog installed, while optionally tracking its source entry
for later propagation of catalog edits.

This generalises the current electron/proton/etc. convenience catalog. Mass
and charge are examples only; an entry may use any component schema registered
by the running application.

## Limitations and non-goals

- This task does not add runtime-loaded equation-system plugins. Catalogs may
  refer only to kinds and component schemas registered by the running build.
- It does not provide collaborative catalog editing, merge resolution, or a
  remote catalog registry. A changed source is detected and reported rather
  than overwritten blindly.
- It does not put solver state or dynamics controls into templates, and does
  not promise that an available template is valid for every possible active
  solver configuration. Authority remains the final representability check.
- A catalog link is an authoring/provenance relationship, not a live filesystem
  watch that mutates an experiment. Propagation always needs user confirmation.

## Migration of the compiled particle catalog

- Remove `crates/fieldcad-particles` as part of this work. Its compiled
  `CATALOG`, `ParticleTemplate`, and particle-specific catalog-provenance
  component are replaced by configuration-driven object templates and the
  generic durable catalog-link/provenance record.
- Move or retain only code that has a genuine non-catalog responsibility. In
  particular, generic collection of mass-bearing/charged bodies belongs with
  the shared mass/electromagnetic source schemas or the dynamics boundary, not
  in a particle-template crate.
- Migrate the current electron, positron, proton, anti-proton, and neutron
  values into ordinary YAML catalog entries, preserving their published source
  metadata. They are sample/user catalog data, not privileged application
  behaviour.
- The catalog-link and document-entry fields are introduced before Field CAD
  has released scene documents that contain the retired particle-provenance
  component. No compatibility migration or scene-format bump is required for
  this replacement. The first persisted contract is the source-qualified
  generic link format.

## Current state

- `fieldcad-particles` has been removed. The desktop instantiates ordinary
  YAML templates through the generic catalog path; starter particle values are
  configuration data rather than compiled offers.
- The generic inspector can attach and edit registered component schemas, so
  the world model already supports composition independent of a particle type
  (ADR 0021).
- Scene documents persist resolved world component values, source-qualified
  catalog links, document-scoped templates, and scene-local quick-add
  preferences. They remain self-contained when a catalog is unavailable.
- The desktop already uses `directories::ProjectDirs` for user configuration.
  On Unix the catalog directory belongs beneath its XDG configuration directory
  (for example `$XDG_CONFIG_HOME/fieldcad/catalog/`), with the equivalent
  `ProjectDirs` location on other platforms.

## Catalog document contract

### Files and documents

- Load YAML files from the catalog directory recursively only if a deliberate
  later UX requires folders; the first version may use a flat directory.
- Each YAML document in a YAML stream declares exactly one catalog entry. A
  file may therefore contain one or many `---`-separated documents.
- Keep a catalog format discriminator and version, for example
  `apiVersion: fieldcad.catalog/v1` and `kind: ObjectTemplate`. This is a
  format/version compatibility guard, not a server-issued resource version.
  The user remains authoritative for file contents.
- Every parsable entry has a user-authored catalog name and template name,
  display metadata, optional arbitrary string labels, a generic `spec`, and
  a source location `(file path, YAML-document ordinal)`. Names are labels,
  not world IDs. The source location plus catalog/template names identifies a
  live link within an installed catalog; a serialized link also carries enough
  descriptive/version/fingerprint information to explain a missing or changed
  source.
- `metadata` includes a user-facing name and optional description, author,
  labels, and provenance annotations. Do not reserve metadata for current
  particle data only.
- `spec` includes an object kind, optional shape and extent, and a generic list
  of component instances. A component instance names its `ComponentTypeId` and
  its property bag by stable schema/property identifiers. Validate it against
  the registered `ComponentSchema`; do not add fixed mass/charge fields to the
  catalog format.
- Start with the ordinary world-object kind. Unknown future kinds remain
  parsable catalog entries but are not instantiable until their kind provider
  is present. This keeps the format extensible to sources, emitters, and other
  authorable kinds without claiming those kinds already exist.
- Do not put position, velocity, pinning, visibility, simulation state, or a
  world object ID in a template. Instantiation supplies the placement; normal
  world/dynamics rules own the rest. Generate a convenient display name from
  the template name (`"fancy unicorn"`, `"fancy unicorn 1"`, ...), but allow
  duplicate scene display names and mint the authoritative object ID only in
  command processing.

An illustrative entry shape (field spelling is illustrative; the implementation
must use the project's stable component/property identifiers and SI values):

```yaml
apiVersion: fieldcad.catalog/v1
kind: ObjectTemplate
metadata:
  catalog: personal-physics
  name: fancy-unicorn
  labels:
    topic: demonstration
spec:
  objectKind: world-object
  shape:
    kind: point
    exclusionRadiusMetres: 0.15
  components:
    - type: { plugin: fieldcad.sources, name: inertial-mass }
      properties:
        mass: { scalar: { siValue: 1.0 } }
```

## Availability, validation, and diagnostics

- Parse every independently valid YAML document. A broken document must not
  hide other documents in the same stream or other files.
- Represent each load result explicitly: `available`, `unavailable`, or
  `invalid`. Preserve the parsed source metadata and diagnostics for entries
  that cannot be instantiated.
- An entry is **available** only when its kind, component schemas, properties,
  property value shapes, units/dimensions, and static template constraints are
  known and valid in the current application catalog.
- An entry is **unavailable** when its YAML is structurally understood but a
  needed kind/component/property/schema version is absent or unsupported.
  Show it in catalog search and management with an unavailable indicator and
  the precise reason. It cannot be added until the needed provider is present
  or the template is amended.
- An entry is **invalid** when it cannot be parsed or fails the catalog format
  contract. Surface path, YAML document ordinal, field path, and actionable
  message in the catalog UI and diagnostics view. The app still starts.
- Registered component schemas remain authorable whether the field system that
  happens to consume them is active or inactive. Availability depends on
  schema registration, not on whether a solver is currently computing a field
  (ADR 0014 and ADR 0017).
- Enforce a 1 MiB maximum per catalog file before parsing, and use bounded
  parsing/error reporting. Reject duplicate catalog/template names created
  through the UI within the same source scope. Hand-edited collisions remain
  loaded and visible with diagnostics; they must not make a scene unloadable.
- The final `CreateObject`/component transaction is validated by the existing
  runtime candidate-world path. Catalog validation is an early, explainable
  check and never replaces authoritative validation.

## Scopes, conflicts, and persistence

- Remove the compiled built-in desktop catalog as an offer source. Ship any
  desired starter templates as ordinary configuration-provided YAML, not Rust
  branches or privileged built-ins.
- Catalog scopes are additive: user/global entries plus optional
  document-scoped entries. Document scope only adds entries; it does not
  override, mask, or mutate the global source.
- The UI must prevent a user from creating a document-scoped entry that
  conflicts with an already visible entry. Direct filesystem edits may still
  produce collisions, so collision handling is a display rule, not a loader
  failure.
- When multiple available or unavailable entries have the same user-facing
  name, show every entry as a separately selectable result, with scope/source,
  labels, availability, and provenance. Warn that names conflict and offer a
  route to the catalog editor; never silently select one by load order.
- Persist document-scoped entries in the scene document. They travel with the
  scene and participate in the same additive/conflict behaviour after load.
- Persist scene-local quick-add visibility, favourites/ordering as needed, and
  last-used entries by catalog reference, not plain display name. A new scene
  starts with new preferences. Newly discovered entries use an explicit default
  visibility rule.
- Maintain the source file and YAML-document association for every editable
  entry. Editing one document in a multi-document YAML stream writes only that
  entry's semantic content while preserving the single shared file and its
  other documents. Use atomic file replacement and report write errors.
- Do not attempt concurrent shared editing. Detect changes on reload and avoid
  blindly overwriting a file changed since it was loaded; present a reload or
  conflict diagnostic instead.
- Reload at application startup and support explicit reload plus filesystem
  watching/hot reload. A reload changes the offer/catalog state only; it never
  mutates a scene object by itself.
- Use filesystem metadata to mark a source read-only. Read-only entries remain
  discoverable and instantiable when available, but their editor is read-only.

## Linked instances and portable scenes

### Instantiation

- Adding a template creates the same typed, atomic world command path used by
  manual composition. It receives a resolved object specification and returns
  the authority-minted object ID/revision.
- Alongside resolved values, store an object-level catalog-link/provenance
  record: source scope/catalog/template names, format/version/fingerprint or
  equivalent identifying description, and the resolved template revision used
  to instantiate. It is provenance, not a solver input or replacement for
  component values.
- A linked instance is read-only for template-owned shape and component values
  in the object inspector. Its inspector displays the template and source and
  links to the matching catalog editor when it is installed and editable.
  Position and other non-template instance concerns remain ordinary scene
  authoring controls.

### Propagation and unlinking

- On a catalog reload or a successful editor save, determine which linked
  objects still resolve to the changed entry. Offer a clear, explicit
  propagation action; do not silently mutate the authoritative world because a
  file changed. The confirmation states how many current-scene objects will
  change and commits all chosen updates through typed authoritative commands,
  with normal revision/undo/history behaviour.
- A linked object's `Unlink from catalog` action makes its current resolved
  shape/components editable in place. Preserve a historical note identifying
  the original catalog entry/revision, but mark it custom and stop future
  propagation.
- Do not let an inline object edit silently break a live link. Require unlink
  first (or a deliberately labelled future "unlink and edit" compound action).

### Offline portability

- A saved scene always contains the resolved authored world state. It must load
  and produce the same initial authoritative scene when the originating catalog
  is absent.
- On such a load, retain the informative catalog link and show, for example,
  `"Instance of personal-physics/fancy-unicorn (tracking; catalog unavailable)"`.
  Template-owned editing remains disabled because the instance is still marked
  tracking; the user can unlink to customize it or install/download a matching
  catalog to resume editing/propagation.
- Catalog absence, changed content, and naming collisions must be reported to
  the user but never prevent a compatible scene document from loading. The
  scene document's ordinary schema/plugin compatibility checks still apply.

## Behaviour and desktop workflow

- `Scene` → `+ Add object` shows scene-eligible quick-add entries and a
  `… Catalog` route. Consider a dedicated Catalog top-level entry once the
  management surface needs more room.
- The catalog dialog has search/filtering by name, kind, labels, source/scope,
  and availability/error state. Each result exposes source/provenance,
  availability diagnostics, Add to scene when available, and management
  actions permitted by its source.
- Reuse the inspector's schema-driven shape and component editors for catalog
  entry add/edit. The editor requires kind and name; it supports label rows and
  attach/remove/edit of any registered component. It must show validation
  errors before save without hiding the underlying YAML source diagnostic.
- Hide-from-quick-add is a scene-document preference, not a mutation of the
  source catalog entry. Catalog management remains reachable even when every
  entry is hidden.
- The linked-instance inspector provides `Open catalog entry` where resolvable,
  a clear unavailable explanation where not, and `Unlink` in both cases.

## Implementation sequence

1. Define catalog DTOs, YAML stream loading, source locations, format-version
   checks, size limits, diagnostics, and availability resolution against the
   registered kind/component schemas. Add a focused catalog crate or module;
   do not put filesystem/UI logic in `fieldcad-core`.
2. **Complete.** Add generic authoritative catalog-template instantiation and durable
   object-link/provenance data. Replace desktop-side particle-specific
   `ObjectSpec` assembly; migrate the particle values to YAML; move any
   remaining non-catalog particle logic to its appropriate shared boundary;
   then remove `fieldcad-particles` and its compiled offer/provenance model.
3. **Complete.** Extend `fieldcad-scene-document` with document-scoped entries, catalog-link
   records, and scene-local quick-add preferences. Establish round-trip and
   catalog-absent behaviour before building the UI.
4. **Complete.** Implement catalog source editing, atomic writes, source read-only handling,
   reload/conflict detection, and hot reload.
5. **Complete.** Build the catalog/search/editor and linked-instance inspector workflows.
   Run the desktop smoke check; manual in-app verification remains necessary
   for modal, reload, and linked-instance interaction.
6. **Mostly complete (2026-08-15).** Move the effective catalog (global source report,
   document-scoped entries, quick-add preferences, and source fingerprints)
   behind `fieldcad-server`, so desktop and MCP consume one source of truth.
   Expose list/reload/create/update/delete/instantiate/unlink through MCP,
   constrained to the configured catalog root for global writes. Catalog
   edits and reloads only change offers: propagation remains a separate,
   explicit authoritative operation with a preview/count and all-or-selected
   linked tracking objects. Do not create an MCP-only catalog representation
   or validation path.

   Follow-up: [server-authoritative-catalog.md](server-authoritative-catalog.md)
   closes the remaining desktop-local mirrors, centralizes propagation, and
   adds the required embedded desktop/MCP synchronization coverage.

## Tests and acceptance

- YAML loading accepts multiple documents in one file, isolates one malformed
  document, reports file/document/field diagnostics, and enforces the 1 MiB
  cap without preventing startup.
- Schema validation accepts generic registered components and rejects unknown
  kind/component/property, property-kind/dimension mismatch, and invalid
  shapes with actionable availability diagnostics.
- A component remains available when its owning/consuming field system is
  inactive, and becomes unavailable only when its schema/kind provider is not
  registered.
- UI-created same-scope/document-scope collisions are refused; filesystem
  collisions display both candidates with no implicit winner and do not block
  document load.
- Same-named quick-add choices carry a stable source/scope disambiguator, so
  each action selects its intended resolved template rather than a load-order
  winner.
- Editing an entry in a multi-document source preserves the other documents;
  write failure, external modification, and read-only source result in clear,
  non-destructive errors.
- Instantiation creates the expected generic components through the normal
  authoritative transaction, assigns an authority-owned object ID, applies the
  display-name convenience rule, and has normal undo/history semantics.
- A linked object's template-owned properties cannot be changed inline;
  unlinking permits edits and preserves historical provenance.
- A catalog edit/reload never changes linked objects until an explicit
  propagation confirmation. Propagation updates exactly the selected linked
  objects, reports its count, and is undoable as an authoritative edit.
- Scene-document round trips preserve resolved linked objects, link metadata,
  document-scoped entries, and quick-add preferences. Loading without the
  originating catalog succeeds, preserves the unavailable tracking state, and
  permits unlinking.
- Desktop automated checks pass, including the graphics smoke check. Perform
  and record manual verification of search, unavailable indicators, source
  navigation, multi-document editing, reload conflict handling, propagation
  confirmation, and offline linked-instance behaviour.

## Relevant code

- `crates/fieldcad-core/src/world.rs` and `crates/fieldcad-core/src/schema.rs`
  — generic object/component world model and schema validation.
- `crates/fieldcad-simulation/src/source.rs` and `runtime.rs` — authoritative
  command boundary, candidate-world validation, undo/history behaviour, and
  registered field/component catalog.
- `crates/fieldcad-scene-document/src/lib.rs` — durable scene, document
  compatibility, and round-trip behaviour.
- `apps/fieldcad-desktop/src/ui/panels/scene_tree.rs` — current Add Object
  presets to replace.
- `apps/fieldcad-desktop/src/profile.rs` — `ProjectDirs`-based configuration
  paths.
- `apps/fieldcad-desktop/AGENTS.md` — desktop lifecycle and smoke-check rules.
- `docs/adr/0007-validate-before-adopting-a-world-edit.md`,
  `docs/adr/0014-scene-level-field-system-activation.md`,
  `docs/adr/0017-share-physical-source-schemas-across-equation-systems.md`,
  and `docs/adr/0021-objects-are-composed-from-independent-components.md` —
  authoritative mutation, inactive-schema, shared-schema, and composition
  invariants.
