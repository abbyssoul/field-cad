# Field CAD

Field CAD is a high-performance research workbench for constructing physics
experiments, choosing explicit field models, and inspecting their results in
three dimensions. It combines CAD-style scene authoring with equation-driven
simulation, field visualisation, and quantitative measurement.

The project treats a result as evidence for the chosen model, numerical method,
domain, and initial conditions—not as a promise that a familiar arrangement
will behave as expected. For example, a proton and electron under the active
Maxwell model may radiate, decay, or collapse; Field CAD should make that result
reproducible and explainable rather than silently stabilise it.

## What you can do today

- Compose objects from independent properties such as charge and mass.
- Explore electrostatic, Maxwell, and gravity equation systems.
- Inspect field channels on planes and sparse 3D regions.
- Run, pause, and step deterministic simulations.
- Record selected probe channels as bounded time-series plots.
- Inspect numerical and conservation diagnostics.
- Save and load reproducible scene documents.

The application is intended for research and model exploration. It does not yet
claim to be a validated atomic- or particle-physics package; representation,
precision, domain, boundary conditions, and sample validity remain visible
parts of every experiment.

## Run the desktop application

With a current Rust toolchain and platform graphics/window dependencies:

```shell
cargo run -p fieldcad-desktop
```

To check the graphics stack without opening a window:

```shell
cargo run -p fieldcad-desktop -- --smoke 120
```

See the [desktop user guide](docs/user-guide.md) for controls, scene authoring,
measurements, and graphics troubleshooting.

## One authoritative experiment, many clients

Field CAD follows a client/server architecture even for local desktop use. A
headless server owns the authoritative scene, simulation state, command queue,
and published observations. The desktop UI is a client of that server; it does
not own separate simulation state.

MCP is a thin tool and transport wrapper around the same server. It can drive a
shared desktop session or a standalone headless session through the same
validated commands and reads that define an experiment. This is intentional:
automation and AI agents should be able to construct, run, and inspect an
experiment by meaning, rather than emulate mouse gestures. Camera, layout, and
other presentation-only preferences remain client-local.

Current MCP transports are restricted to local IPC or loopback HTTP. See the
[architecture overview](docs/architecture.md), [MCP implementation and
security notes](docs/mcp-plan.md), and [capability inventory](docs/user-stories/README.md)
for the current surface and remaining parity work.

## Documentation

- [Desktop user guide](docs/user-guide.md) — build, run, navigate, observe.
- [Architecture overview](docs/architecture.md) — client/server responsibilities
  and data flow.
- [Project context](CONTEXT.md) — detailed domain language and invariants.
- [Architecture decisions](docs/adr/README.md) — durable design decisions.
- [Contributing](CONTRIBUTING.md) — setup, verification, and project navigation.
- [Performance benchmarks](crates/fieldcad-bench/README.md) — workloads and
  reporting.
