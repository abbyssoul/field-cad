# Task: safe snapshot sizing and force-integration eligibility reporting

## Status: resolved

Both gaps are closed in `crates/fieldcad-mcp/src/lib.rs`:

- `get_latest_snapshot` now takes `channels` (an optional allow-list of
  `{plugin, name}` refs) and `max_samples` (default 2,000 total samples,
  `DEFAULT_MAX_SNAPSHOT_SAMPLES`). A read that would exceed the limit is
  refused with a structured error naming the total, the limit, and a
  per-channel sample-count breakdown sorted largest-first, instead of
  serializing an oversized payload. `get_subscription` and `set_subscription`
  now document the sampling knobs' approximate effect on response size.
- `get_body_forces`'s description now enumerates every reason a body can be
  absent — no mass component, pinned, solver-owned, or no tick yet — mirroring
  `FieldDataSource::body_forces`'s own doc comment
  (`crates/fieldcad-simulation/src/source.rs`), which already had this right;
  only the MCP-facing description was out of date.

Left open, as an optional follow-on (was a "consider" in the original
required behavior, not a hard requirement): a diagnostic-level signal that
distinguishes "not dynamics-eligible" from "no tick has run yet" for a body
missing from `get_body_forces`, rather than both presenting as an absent
entry.

## Origin

Found during black-box MCP exploration (agent driving `field-cad-server`
tools with no source access): create a negative point charge, confirm it
appears in the field, inspect body forces. Two gaps surfaced that are outside
the scope of `typed-world-mutation-dsl.md` — that task only covers the
`commit_world` mutation envelope and component-property schemas. These are
read-path/observation issues instead.

## Gap 1 — `get_latest_snapshot` has no safe default and no size guard

### Current limitation

`get_latest_snapshot` returns whatever the current `set_subscription` state
asks for, with no independent size control. The session's default
subscription (plane 33x33, domain stride 8, boxes/spheres sampled) produced a
116,812-character single-line response and the call failed outright with a
transport/token-limit error. There was no truncation, pagination, or partial
result — the read was simply unusable until `set_subscription` was called
separately to narrow it.

Nothing in `get_latest_snapshot`'s or `get_subscription`'s tool description
warns a caller that the current subscription may be too large to read back,
or that shrinking it is a prerequisite for a successful call. A client has no
way to discover this short of hitting the failure once.

### Required behavior

- `get_latest_snapshot` (and any future paginated variant) should either:
  - accept its own sampling/size override independent of the durable
    `set_subscription` state (a read-scoped "give me at most N samples"
    parameter), or
  - report an estimated/actual payload size and support offset-based or
    per-channel partial reads when the full snapshot would exceed a
    documented safe response size.
- `get_subscription`/`set_subscription` descriptions should state the
  approximate response-size implications of each sampling knob (plane/box/
  sphere sample density, domain stride) so a client can reason about the
  tradeoff before subscribing.
- A failed-due-to-size read should return a structured error identifying
  which channels/geometries were oversized, not just a generic transport
  failure.

## Gap 2 — `get_body_forces` omits a common reason for empty results

### Current limitation

`get_body_forces` returns `[]` for a freshly created charged object (a
`fieldcad.electromagnetic-sources:charge-source` component with no mass
component attached). Its description explains that pinned or solver-owned
bodies are excluded, but does not mention that a body with no mass component
is never force-integrated at all. A client that just authored a charge source
and expects to observe a force has no way to learn from the tool surface
alone that a mass component is the missing prerequisite.

### Required behavior

- Extend `get_body_forces`'s description to enumerate all reasons an object
  can be absent from the result, including missing mass-source component.
- Consider a diagnostic-level signal (via `get_diagnostics`, or an explicit
  field on the `get_body_forces` response) distinguishing "no force computed
  because this object isn't dynamics-eligible" from "no tick has run yet" —
  today both present identically as an empty/missing entry.

## Tests and acceptance

- A snapshot read under a subscription that would exceed the documented safe
  response size returns a structured, actionable error or a valid partial
  result — never an opaque transport failure.
- Tool descriptions for `get_latest_snapshot`, `get_subscription`, and
  `set_subscription` document response-size tradeoffs.
- `get_body_forces`'s description lists every exclusion reason, including
  missing mass component.
- A regression test creates a charge-only (massless) object, calls
  `get_body_forces`, and asserts the result/documentation makes the exclusion
  reason discoverable without reading source.

## Relevant code

- `crates/fieldcad-mcp/src/lib.rs` — `get_latest_snapshot`, `get_subscription`,
  `set_subscription`, `get_body_forces` tool definitions and descriptions.
- `crates/fieldcad-server/src/lib.rs` — snapshot sampling/subscription
  application, force-integration eligibility.
