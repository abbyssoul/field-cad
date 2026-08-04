# Field CAD

Field CAD is a high-performance physics research and modelling environment for
constructing, simulating, and exploring fields, particles, and experiments in
three dimensions. It combines CAD-style scene interaction with equation-driven
simulation, field-specific visualization, quantitative observation, and local
or dedicated compute.

The long-term objective includes modelling atoms, subatomic particles, and
particle-physics experiments from the perspective of fields. Electron, proton,
positron, and neutron entries are catalog templates: each attaches the same
independent mass and charge components with published values, and the identical
object can be composed by hand. The template does not bring hidden
species-specific interactions; the active field equations and solver determine
how the particle behaves.

For example, a user can place one proton and one electron with chosen initial
velocities and run a Hydrogen experiment under the Maxwell model. It may
radiate, lose energy, or collapse instead of remaining stable. That is a
meaningful research result for the selected model. Field CAD should
make it practical to ask which explicit changes to equations, coupling,
regularization, boundaries, or numerical methods alter that behaviour—without
silently stabilizing the scene.

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

The Milestone 5 Maxwell solver is available in both CPU `f64` reference and
host-owned `wgpu f32` forms. It advances coupled electric and magnetic fields on
a periodic 3D Yee lattice, publishes energy and divergence observables, rejects
unstable Courant time steps, and is checked by grid-convergence and CPU/GPU
parity tests. The desktop defaults to a stationary-charge constraint: Maxwell E
is initialized from the same authored charge as electrostatics, B starts at
zero, and the original XY plane at the origin shows the resulting radial field.
The prescribed plane wave remains an explicit validation configuration. The
scene inspector shows the active periodic domain, precision, resolution, and
initial-condition settings alongside the system's published channels.

Milestone 6 adds generic particles and physical field feedback. Objects are built
by composition: the Scene panel adds a bare object, and the inspector attaches
charge to couple it to the electromagnetic field or mass to make it respond to
force. A `Pinned` control decides whether a solver integrates the motion or the
authored position and velocity are followed exactly. The inspector renders every
component from its registered schema, so a new plugin's property becomes
editable without a UI change. Moving particles deposit charge and current with a discrete
continuity-preserving CIC scheme, sample reconstructed Yee fields, and use a
relativistic Boris pusher before returning canonical motion through the runtime.
Periodic Poisson initialization makes the coupled E field satisfy the lattice
Gauss operator; diagnostics expose charge, the explicit neutralizing background,
particle and field energy, continuity residual, and intervention-aware drift.

## Product direction

The product target is a research workbench in which a user can define a
reproducible experiment, select validated physical models, run it efficiently
on a workstation or dedicated compute machine, and inspect both spatial fields
and quantitative observables. Interactive visualization and high-throughput
compute are two views of the same authoritative experiment.

The first useful version will let a user:

- place charged objects in a 3D scene;
- inspect the electric field on any oriented plane or throughout a 3D region;
- show or hide a reference grid and field visualization layers;
- orbit, pan, zoom, select, and move objects with CAD-style controls;
- place probes and plot sampled field values over simulation time;
- keep one or more probe recorder plots open while manipulating the scene;
- play, pause, and advance the simulation by one fixed time step; and
- edit an object's supported properties while the simulation is running.

The first field model is electrostatic. The second evolves electric and magnetic
fields together using Maxwell's equations and now couples generic moving
particles through deposited charge/current and Lorentz-force feedback. A
gravitational model remains a likely later addition.

Charge and mass authoring is shared infrastructure rather than a solver-owned
detail: each quantity owns its schema in its own Module, so electrostatics and
Maxwell consume charge without depending on one another, and a future gravity
plugin consumes mass without depending on either. The runtime remains the sole
validated world writer: the Maxwell solver claims kinematic authority only for
objects that will actually move, and returns complete transform/velocity
outcomes. Solver-produced
revisions continue the integration; authored physical changes are reported as
external interventions and reset the coupled conservation reference.

## Central design idea

The extension unit is an **equation-system plugin**, not an individual rendered
field. A plugin may expose one or more related field channels, object properties,
and solvers. For example:

- a shared electromagnetic-source Module defines authored charge, and a shared
  mass-source Module defines inertial and gravitational mass separately;
- a first-party dynamics system moves every body with inertial mass, summing the
  forces the active field systems contribute. A plugin couples to motion by
  answering what force its field exerts, never by integrating a trajectory;
- an electrostatics plugin exposes electric field `E` and electric potential;
- an electromagnetism plugin consumes charge and exposes the coupled `E` and
  `B` fields, current, and its time integrator;
- a particle catalog records where electron, proton, positron, and neutron
  values came from, over the same components any object can carry;
- future field systems may test alternative couplings, correction terms, or
  approximations against the same particle arrangement; and
- a gravity plugin could consume the same mass and expose gravitational
  acceleration and potential, adding no dependency on electromagnetism.

The application owns the scene, time controls, input, generic visualization,
and probes. Plugins own the physical equations and the state needed to solve
them. This keeps a new physical theory from requiring a new application.

Equation systems are composed per scene. Selecting the **Simulation** node at the
top of the Scene panel lists every available system in the Inspector, together
with the scalar/vector field channels it provides. Disabling a system
stops its solver and removes its channels from new snapshots, while objects keep
the plugin-contributed properties—such as charge or mass—that were authored on
them. Coupled channels such as Maxwell `E` and `B` are activated together.

The visualizer also consumes solver results through a **field data source**
boundary. Initially that source is an in-process simulation runtime. A later
version will connect the same desktop application to a dedicated compute service,
send authoring and time-control commands to it, and stream back versioned field
snapshots. Rendering and UI must therefore never rely on direct access to a local
solver's memory.

The current Interface is named `FieldDataSource` because fields are the first
vertical slice. The research roadmap must generalize its versioned result stream
to typed field-and-particle observations—without making trajectories, energy
histories, or distributions masquerade as field channels.

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

Research use raises the standard beyond visual plausibility. Each solver must
state its physical assumptions and validity regime, retain complete provenance,
provide reference or convergence evidence, and make precision and conservation
diagnostics inspectable. Early milestones are foundations and are not yet a
validated atomic or particle-physics package. A familiar template name such as
“electron” or “Hydrogen under Maxwell” identifies initial values and
arrangement, not a claim that the active equations include every effect known
for that system.

“High performance” is likewise a measured requirement, not a branding claim.
Representative experiments will have named-hardware benchmarks for throughput,
memory, scaling, and result-transfer cost. Solver state should remain on the
most suitable CPU, GPU, or dedicated compute resource, with only subscribed
observations transferred for interaction and analysis.

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
- the Scene panel is the scene's contents: a Simulation node holding the domain
  and its field systems, then the objects, then probes and slice planes under a
  Measurement heading that marks them as instruments rather than physics. One
  button adds an object; each row has viewport visibility and delete controls;
- the Inspector edits position, charge, source radius, probe position, and plane
  orientation/extent. Each plane independently controls magnitude and arrow
  density with non-negative numeric inputs, and whether vectors are projected
  into the plane (the default) or shown in full 3D. A selected probe can attach
  to an object, detach without jumping, edit its local offset, and show bounded
  scalar or x/y/z/magnitude history; and
- the Inspector describes exactly one selected thing and nothing else. With the
  Simulation node selected it lists the fields contributed by each composed
  equation system, shows its authoritative settings, and enables or disables that
  system for the scene. Inactive field names and settings remain available for
  authoring, but are not simulated or published. The Compute section there
  reports domain resolution, precision, and boundary conditions;
- the View window, floating over the 3D view, holds the six axis viewpoints,
  focus and camera reset, and what is drawn — grid, origin axes, objects, probes,
  slice planes, and which field channels are shown. Nothing in it changes the
  physics. The top bar is the simulation transport: Play, Pause, Step, `dt`, and
  playback speed;
- a Getting started window opens on a first run and is reachable from `? Help`.
  It covers building a scene by composition, where each panel's responsibility
  lies, and the navigation and drag controls; and
- under a selected probe, Recorded channels controls which published fields enter
  its bounded history; Open floating plot pins a non-blocking recorder window
  that can display several unit-safe channel plots at once.

The initial desktop scene composes Electrostatics and Electromagnetism.
Maxwell uses the renderer's existing GPU device through the plugin backend seam;
its default `dt` is 80% of the Yee Courant limit. Maxwell E is the initial
visible layer; B is independently available and remains zero for the stationary
charge. Energy density and both divergence channels are available to the initial
probe, recorder plots, and Diagnostics window. Moving-charge current deposition
is still the following physics milestone.

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
│   ├── fieldcad-bench/       # Headless performance workloads and reports
│   ├── fieldcad-core/        # Units, world, domain, sampling, clock, snapshots
│   ├── fieldcad-electromagnetic-sources/ # Shared charge schema/source Adapter
│   ├── fieldcad-particles/   # Generic particles and versioned catalog templates
│   ├── fieldcad-plugin-api/  # Headless equation-system plugin contract
│   └── fieldcad-simulation/  # Runtime, data-source boundary, probe history
└── plugins/
    ├── electrostatics/       # Coulomb CPU oracle and host-injected evaluator
    ├── electromagnetism/     # Yee/Maxwell fields and particle-coupling oracle
    └── test-field/           # Known analytic scalar/vector contract fixture
```
