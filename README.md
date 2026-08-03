# Field CAD

Field CAD is a desktop application for constructing, simulating, and exploring
scientific fields in three dimensions. It combines CAD-style scene interaction
with equation-driven simulation and field-specific visualization.

The first useful electrostatic vertical slice is implemented. The desktop
application draws the real world model through the field-data-source boundary:
CAD-style navigation, charged point and sphere objects, editable properties,
movable probes, arbitrary slice planes, sparse 3D glyphs, magnitude colour,
fixed-step transport controls, revisioned field snapshots with per-sample
validity, and compute diagnostics.

The simulation workbench now includes an independent playback-rate control,
explicit paused/running/solving/stale/invalid states, deterministic queued edits
while running, attachable probes, and bounded scalar or vector-component history
plots. A probe can select the channels it records and pin a persistent floating
plot that keeps updating while another scene entity is selected or moved.
Command/pacing recordings provide repeatable headless regression fixtures.

Electrostatics has an independent CPU `f64` oracle and a batched WGSL `f32`
backend for interactive probe, plane, and grid samples. Local GPU output still
becomes an ordinary immutable snapshot before visualization, preserving the
same consumer contract planned for a remote compute machine.

The first Milestone 5 reference solver also exists as a headless plugin. It
advances coupled electric and magnetic fields on a periodic 3D Yee lattice,
publishes energy and divergence residuals, rejects unstable Courant time steps,
and has a grid-convergence test against a prescribed vacuum plane wave. Its
`wgpu` port and desktop Maxwell scenario are the next implementation increment.

## Product direction

The first useful version will let a user:

- place charged objects in a 3D scene;
- inspect the electric field on any oriented plane or throughout a 3D region;
- show or hide a reference grid and field visualization layers;
- orbit, pan, zoom, select, and move objects with CAD-style controls;
- place probes and plot sampled field values over simulation time;
- keep one or more probe recorder plots open while manipulating the scene;
- play, pause, and advance the simulation by one fixed time step; and
- edit an object's supported properties while the simulation is running.

The first field model will be electrostatic. The next model will evolve the
electric and magnetic fields together using Maxwell's equations. A gravitational
model is a likely later addition.

## Central design idea

The extension unit is an **equation-system plugin**, not an individual rendered
field. A plugin may expose one or more related field channels, object properties,
and solvers. For example:

- an electrostatics plugin exposes electric field `E`, electric potential, and
  charge;
- an electromagnetism plugin exposes the coupled `E` and `B` fields, charge,
  current, and its time integrator; and
- a gravity plugin could expose gravitational acceleration, potential, and mass.

The application owns the scene, time controls, input, generic visualization,
and probes. Plugins own the physical equations and the state needed to solve
them. This keeps a new physical theory from requiring a new application.

The visualizer also consumes solver results through a **field data source**
boundary. Initially that source is an in-process simulation runtime. A later
version will connect the same desktop application to a dedicated compute service,
send authoring and time-control commands to it, and stream back versioned field
snapshots. Rendering and UI must therefore never rely on direct access to a local
solver's memory.

See [CONTEXT.md](CONTEXT.md) for the domain model and architectural boundaries.
See [PLAN.md](PLAN.md) for the proposed implementation sequence and review
questions. See [docs/adr/](docs/adr/) for the decisions that shaped the code and
the reasoning behind them, and [docs/reviews/](docs/reviews/) for review findings
and what was done about them.

## Technology

- **Rust** for the application and first-party plugins.
- **[wgpu](https://docs.rs/wgpu/latest/wgpu/)** for cross-platform 3D rendering
  and GPU compute.
- **[winit](https://docs.rs/winit/latest/winit/)** for native windows and input.
- **[egui](https://docs.rs/egui/latest/egui/)**, integrated through `egui-winit`
  and `egui-wgpu`, for inspectors, simulation controls, and diagnostics.
- **WGSL** for GPU visualization and numerical kernels.

`egui` is proposed instead of Dear ImGui because it is pure Rust and has a
maintained direct integration with both `winit` and `wgpu`. We will use the lower
level integration crates rather than make the 3D viewport an `eframe` detail.

The initial plugins will be normal crates in a Cargo workspace. Runtime-loaded
third-party plugins are intentionally deferred until the contract has been
tested by at least two equation systems. The current candidate for that later
boundary is the WebAssembly Component Model, with host-validated WGSL assets for
GPU work, rather than Rust dynamic libraries with an unstable ABI.

## Scientific stance

A continuous field has a value at every point; software represents it using an
analytic function, a finite numerical grid, or another approximation. Field CAD
will make the active representation, domain, resolution, boundary conditions,
units, and numerical error visible instead of presenting sampled results as
exact.

CPU reference implementations and analytic cases will act as correctness
oracles for GPU solvers. Interactive rendering may use `f32`, while authoritative
scene values, units, time, and reference calculations use `f64` where practical.

Large remote fields will be requested by channel, region, representation, and
level of detail. The compute service remains authoritative for simulation time;
the visualizer may discard obsolete presentation frames under backpressure, but
must identify stale or incomplete data instead of presenting it as current.

## Run the application

With a current Rust toolchain and platform graphics/window dependencies:

```shell
cargo run -p fieldcad-desktop
```

Controls:

- Play, Pause, and Step control the deterministic local simulation clock. The
  `dt` value can be dragged or entered directly; unitless values are seconds and
  scientific notation may be combined with `s`, `ms`, `us`/`µs`, `ns`, `ps`,
  `fs`, `min`, or `h` (for example `1.23ns` or `7.3213e-4ms`);
- `speed` changes wall-clock playback pacing without changing `dt`; the current
  simulation time and queued-edit/state indicators sit beside it;
- Grid, XYZ axes, and Diagnostics independently show or hide viewport helpers;
- middle-button drag orbits;
- Shift + middle-button drag pans;
- mouse wheel dollies;
- `1`, `3`, and `7` select the +X, +Y, and +Z views;
- clicking a charge, probe, or slice plane highlights it; `F` frames the selected
  entity and `Esc` clears the selection;
- every selected entity shows its origin as three RGB wire circles and shares the
  same transform gizmo: drag a red, green, or blue arrow to move only along X,
  Y, or Z, or drag a coloured unit square to move only in XY, YZ, or ZX. The
  active constraint turns yellow and is named at the top of the viewport;
- dragging directly on the selected entity's body moves it freely in the
  camera-oriented view plane; dragging empty space never moves it;
- a selected slice plane additionally shows a proportional dashed purple normal
  arrow labelled `N`. Drag its outer tip to rotate and reorient the plane;
- the Scene panel creates positive/negative point charges, uniformly charged
  spheres, probes, and slice planes. Each row has viewport visibility and delete
  controls; and
- the Inspector edits position, charge, source radius, probe position, and plane
  orientation/extent. Each plane independently controls magnitude and arrow
  density with non-negative numeric inputs, and whether vectors are projected
  into the plane (the default) or shown in full 3D. A selected probe can attach
  to an object, detach without jumping, edit its local offset, and show bounded
  scalar or x/y/z/magnitude history; and
- under a selected probe, Recorded channels controls which published fields enter
  its bounded history; Open floating plot pins a non-blocking recorder window
  that can display several unit-safe channel plots at once.

Use `RUST_LOG=fieldcad_desktop=debug` for more application diagnostics.

To check the graphics stack without opening a window — useful when a windowed
run misbehaves, since it cannot involve a compositor:

```shell
cargo run -p fieldcad-desktop -- --smoke 120
```

The backend, present mode, and adapter class are selectable at runtime
(`WGPU_BACKEND=gl`, `FIELDCAD_PRESENT_MODE=no-vsync`,
`FIELDCAD_FORCE_FALLBACK=1`). See
[docs/troubleshooting-graphics.md](docs/troubleshooting-graphics.md) if the
viewport freezes or the app crashes on exit.

The domain core is headless by construction. Working on the model, the plugin
contract, or the runtime needs no GPU and no window system:

```shell
cargo test -p fieldcad-core -p fieldcad-plugin-api -p fieldcad-simulation \
  -p fieldcad-electromagnetism
```

## Repository

```text
.
├── README.md       # Product entry point and technology direction
├── CONTEXT.md      # Domain language, architecture, and invariants
├── PLAN.md         # Milestones and review gates
├── Cargo.toml      # Virtual workspace; shared dependency versions
├── docs/
│   ├── adr/        # Architecture decision records
│   └── reviews/    # Review findings and remediation reports
├── apps/
│   └── fieldcad-desktop/     # Native visualizer and composition root
│       └── src/
│           ├── app.rs        # winit lifecycle, input routing, composition
│           ├── camera.rs     # Orbit camera, physical viewport, picking rays
│           ├── scene/        # Field layers, gizmos, picking, authoring proxies
│           ├── renderer.rs   # wgpu surface, instanced scene, egui composition
│           ├── scene.wgsl    # Grid/axes and instanced object shader
│           └── ui/           # Panels, compute view, inline/floating plots
├── crates/
│   ├── fieldcad-core/        # Units, world, domain, sampling, clock, snapshots
│   ├── fieldcad-plugin-api/  # Headless equation-system plugin contract
│   └── fieldcad-simulation/  # Runtime, data-source boundary, probe history
└── plugins/
    ├── electrostatics/       # Coulomb CPU oracle and host-injected evaluator
    ├── electromagnetism/     # Periodic CPU f64 Yee/Maxwell reference
    └── test-field/           # Known analytic scalar/vector contract fixture
```
