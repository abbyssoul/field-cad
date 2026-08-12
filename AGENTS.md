# Field CAD — agent guide

Field CAD is a research workbench for composing physical models, simulating
experiments, and inspecting/exporting reproducible observations.

## Start here

1. Read `README.md` for the product and how to run it.
2. Read `CONTEXT.md` for the domain glossary and architectural boundaries.
3. Read the relevant record in `docs/adr/` before changing a boundary.
4. Follow the nearest crate's public types and tests.

## Package map

- `apps/fieldcad-desktop`: native desktop client, rendering, UI, and input.
- `crates/fieldcad-core`: domain types, world, units, sampling, and snapshots.
- `crates/fieldcad-simulation`: authoritative runtime, clock, commands, and
  `FieldDataSource` contract.
- `crates/fieldcad-dynamics`: first-party force integration for dynamic bodies.
- `crates/fieldcad-*-sources` and `fieldcad-particles`: shared source schemas
  and particle catalog data used independently by equation systems.
- `crates/fieldcad-server`: headless owner of a session and its authoritative
  state; desktop and network transports drive the same server.
- `crates/fieldcad-mcp`: thin MCP transport over `fieldcad-server`.
- `crates/fieldcad-scene-document`: versioned experiment persistence.
- `plugins/*`: equation systems and their solver implementations.
- `crates/fieldcad-bench`: headless performance workloads and reports.

## Non-negotiable boundaries

- The UI consumes `FieldDataSource`; it never reads solver-owned memory.
- The runtime/server is the sole validated writer of the authoritative world.
- Publish immutable, versioned observations with validity and provenance.
- Store physical quantities in SI. Prefer `f64` for authoritative/reference
  work; GPU `f32` use must be explicit in metadata.
- Objects are independent components. Particle templates are data and
  provenance, never hidden species-specific physics.

## Quality bar

- Preserve deterministic simulation and keep presentation frames independent
  from simulation ticks.
- For numerical changes, add analytic, reference, convergence, or CPU/GPU
  parity evidence.
- State expected complexity for hot paths. Do not allocate per tick, sample, or
  render-loop iteration without measured justification.
- Measure meaningful performance changes with `fieldcad-bench`.
- Record expensive-to-reverse architecture decisions in `docs/adr/`.

See `CONTRIBUTING.md` for setup, verification commands, and documentation
conventions. See `docs/architecture.md` for the client/server model.
