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

## Working conventions

- Desktop UI and rendering changes have no driven GUI harness. Run the smoke
  check as well as automated checks, and state when manual in-app verification
  remains necessary. See `apps/fieldcad-desktop/AGENTS.md` for lifecycle rules.
- Capture a legitimate but out-of-scope follow-up in `docs/tasks/` using its
  established goal, limitation, behaviour, tests, and relevant-code structure.
  Re-check file references in older tasks before relying on them.
- Treat `docs/perf/` findings as a revalidated backlog, not permanent truth:
  confirm each claim against current code before acting on it.

## Code conventions

- **Errors:** `thiserror` derive with `#[error("...")]` / `#[error(transparent)]`.
- **Serialization:** `#[derive(Serialize, Deserialize)]` on every persisted or
  command-carried type, via `serde`.
- **Math:** `glam::DVec3`, `DVec2`, `DQuat` for authoritative `f64` work;
  `glam::UVec2`/`UVec3` for grid dimensions. Use `DMat3` for 3×3 matrices.
- **Units:** `uom` SI quantities (`LengthMetres`, `MassKg`, etc.) on every
  schema boundary. Dimensions are checked at the command boundary.
- **Common derives:** `#[derive(Clone, Debug, PartialEq)]` on most value types;
  add `Serialize, Deserialize` when the type crosses a persist or transport seam.
- **Channel handles:** use `ChannelHandle(u16)` (not `ChannelId`, which is a
  string) wherever a channel is looked up per sample — the hot path compares
  `u16`, not `String`.
- **Shared immutable data:** `Arc<[T]>` for buffers shared across snapshot
  consumers. Use `SampleCache` in plugins to reuse allocations across ticks
  and channels.
- **Crate naming:** `fieldcad-{noun}` for library crates; plugin crates in
  `plugins/` follow the `fieldcad-{adjective}` pattern.
- **Workspace deps:** pin every shared dependency version in the root
  `Cargo.toml` `[workspace.dependencies]` table. Never duplicate a version
  in a child `Cargo.toml`.
- **Test placement:** `#[cfg(test)] mod tests { ... }` at the bottom of the
  source file containing the code being tested.
- **Doc comments:** `///` on every public item. Internal comments inside
  function bodies are sparse — prefer expressive types and naming.
- **WGSL:** validated at compile time by a Naga unit test in
  `fieldcad-desktop`. Add a test case when a new shader or entry point is
  introduced.
- **No-alloc hot paths:** structure tight loops to reuse buffers. Accept a
  `&mut [T]` or `&mut Vec<T>` parameter rather than returning a newly
  allocated collection.

See `CONTRIBUTING.md` for setup, verification commands, and documentation
conventions. See `docs/architecture.md` for the client/server model.
