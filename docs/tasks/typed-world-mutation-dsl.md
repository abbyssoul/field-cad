# Task: schema-guided typed world-mutation DSL for MCP

## Goal

Give an MCP/AI client a discoverable, typed way to author every supported
world mutation without embedding JSON inside strings or reverse-engineering
Rust `WorldCommand` serde shapes. Preserve the existing atomic world-command
transaction as the single authoritative mutation path.

## Current state

`commit_world` has already moved past its first transport limitation: it
accepts a native MCP JSON array of command objects instead of
`commands_json`, a JSON string containing an array. The server deserializes
each entry to `fieldcad_core::WorldCommand` and submits one atomic
`CommandPayload::CommitWorld` transaction.

This is better for clients, but it is not yet a useful typed authoring API:

- `WorldCommand` and its nested Rust types do not expose a `schemars`
  `JsonSchema` through MCP.
- Command serde names and nested shapes are implementation details rather than
  agent-facing vocabulary.
- Component properties are intentionally plugin-defined and cannot honestly be
  frozen into one static core schema.
- An agent currently needs to infer component/property payloads from raw
  serialized world/schema output and receives only authority-side rejection.

## Required public surface

### Transaction envelope

- Keep one atomic `commit_world(expected_revision?, commands)` operation. The
  command list remains ordered and all-or-nothing.
- Add explicit optimistic-concurrency support: an optional expected world
  revision; reject mismatch with structured current-revision information.
- Return the existing commit receipt plus allocated IDs and committed revision
  for every creation in the transaction.

### Typed command DSL

- Define MCP-facing request types separate from core `WorldCommand` serde.
  Use stable, documented operation names and SI-unit field names.
- Cover all user-authorable operations: create/edit/remove objects; transform,
  velocity, shape, visibility and pinning; component attach/edit/detach;
  create/edit/remove/show-hide planes, field boxes, field spheres, and probes;
  probe placement/attachment and recorded-channel changes.
- Use explicit scalar/vector/rotation request shapes rather than exposing
  `glam` serialization. Entity references are numeric stable IDs; field and
  component references are `{ plugin, name }` pairs.
- Convert requests to `WorldCommand` inside the MCP adapter, validate simple
  identifiers/finite values at the boundary, then rely on the authority for
  the final atomic validation and adoption.

### Dynamic component properties

- Do not invent a fixed schema for plugin-defined components. Add a dedicated
  `list_component_schemas`/`get_component_schema` read returning each property
  ID, display name, kind, SI dimension, required flag, condition, choices, and
  default value.
- Represent component property input as a typed tagged value:
  `scalar { si_value }`, `vector { x, y, z }`, `boolean`, `text`, or `choice`.
  The component schema supplies dimensions and allowed choices; clients do not
  submit dimensions that could disagree with the declared property.
- Validate component-property shapes against the discovered schema before
  constructing a `PropertyBag`; return field-path-specific errors. The runtime
  remains the final validator for world/plugin composition.

## Implementation sequence

1. Introduce MCP request/response types and schema tests without changing the
   core model or `WorldCommand` serialization.
2. Implement command-to-`WorldCommand` conversion for objects and components,
   then planes/boxes/spheres/probes. Keep legacy native-JSON `commit_world`
   temporarily as a compatibility alias while the typed operation reaches full
   coverage.
3. Add component-schema reads and typed property conversion/validation.
4. Add expected-revision handling in the authoritative command boundary, not
   as an MCP-only check, so desktop and future HTTP clients share the rule.
5. Deprecate and then remove the raw-core-command MCP input after parity tests
   demonstrate every supported command is representable by the DSL.

## Constraints

- No MCP command may mutate the world except through the existing validated
  atomic runtime transaction.
- Names are labels; all edits/references use stable entity IDs.
- Keep field-system configuration and numerical-domain commands outside this
  DSL; they are session commands with their own MCP tools.
- Preserve local/remote equivalence, queued-at-tick-boundary semantics, undo
  behavior, and snapshot provenance.

## Tests and acceptance

- Generate/inspect MCP schemas showing operation discriminators, SI fields,
  entity references, and dynamic-property tagged values.
- Test one mixed atomic transaction creating/editing objects and instruments,
  asserting allocated IDs and one committed revision.
- Test invalid identifiers, non-finite geometry, property-kind mismatch,
  unknown/missing properties, unsupported choice, and stale expected revision
  return structured tool errors without mutation.
- Test every `WorldCommand` user-authorable variant has a DSL representation.
- Run parity tests submitting equivalent DSL and direct `WorldCommand`
  transactions and compare resulting world/revision/snapshot state.

## Relevant code

- `crates/fieldcad-mcp/src/lib.rs` — current native-array `commit_world` and
  MCP request schemas.
- `crates/fieldcad-core/src/world.rs` — authoritative command variants.
- `crates/fieldcad-core/src/schema.rs` — component/property schema model.
- `crates/fieldcad-simulation/src/source.rs` — authoritative command boundary.
