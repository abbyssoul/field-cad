# Contributing to Field CAD

## Orientation

Read [CONTEXT.md](CONTEXT.md) before making domain or architecture changes, and
read the relevant [ADR](docs/adr/README.md) before crossing an established
boundary. The [architecture overview](docs/architecture.md) explains how the
desktop, server, and MCP transport share one authoritative experiment model.

The workspace is organised by responsibility:

- `fieldcad-core`: domain, world, units, sampling, snapshots.
- `fieldcad-simulation`: runtime, command handling, and `FieldDataSource`.
- `fieldcad-dynamics`: first-party force integration for dynamic bodies.
- `fieldcad-*-sources` and `fieldcad-particles`: shared physical-source
  schemas and particle catalogue data.
- `fieldcad-server`: headless authoritative session owner.
- `fieldcad-mcp`: MCP transport over the server.
- `fieldcad-scene-document`: persisted scene documents.
- `plugins/*`: physical equation systems and source schemas.
- `fieldcad-desktop`: native client, renderer, and UI.
- `fieldcad-bench`: repeatable headless performance workloads.

## Verify changes

Run focused tests for the crate you changed. Before handing off a broad change,
run:

```shell
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Desktop changes should also use the headless graphics smoke check:

```shell
cargo run -p fieldcad-desktop -- --smoke 120
```

## Engineering standards

- Keep the runtime/server as the sole validated world writer; clients consume
  `FieldDataSource` rather than solver memory.
- Keep simulation ticks deterministic and independent from presentation frames.
- Store authoritative quantities in SI. Prefer `f64` for reference work; make
  GPU precision and approximation visible in output metadata.
- Treat particle templates as authored data/provenance, not special behaviour.
- For numerical changes, add analytic, reference, convergence, or CPU/GPU
  parity evidence.
- For hot paths, identify expected complexity and avoid per-tick, per-sample,
  or per-frame allocation. Use `fieldcad-bench` for meaningful performance
  claims.

## Documentation

Keep the top-level README concise and user-facing. Put desktop workflows in
`docs/user-guide.md`, architecture summaries in `docs/architecture.md`, detailed
domain rules in `CONTEXT.md`, and costly-to-reverse decisions in `docs/adr/`.
Update documentation whenever user-visible behaviour or an architectural
invariant changes.
