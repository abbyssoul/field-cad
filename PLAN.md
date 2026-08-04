# Field CAD implementation plan

Status: **accepted; implementation underway**

Project objective: build a high-performance physics research and modelling
environment capable of reproducible field, atomic-arrangement,
subatomic-particle, and particle-physics experiments. Named particles are
catalog templates that instantiate generic mass/charge objects; the active
field equations determine their behaviour. The current electrostatic and
Maxwell milestones establish that intended modelling foundation.

Progress as of 2026-08-03:

- Milestone 0 is accepted. Its decisions are now recorded as ADRs in
  `docs/adr/`, which was the outstanding deliverable.
- Milestone 1 is complete on Linux, smoke-tested on a Vulkan adapter. The
  software-fallback adapter path and a GPU-free WGSL compilation test now exist,
  closing the two deliverables that were previously unmet. Windows and macOS are
  built and tested in CI; interactive validation on those platforms is still
  required before the exit criteria are fully satisfied.
- Milestone 2 is complete and its plugin-API review gate has been held. The
  review found and fixed: a missing `Domain` concept, an unbatched sampling path
  that would not have survived Milestone 3, `ChannelId`/`ComponentTypeId` being
  the same type, a world edit that stayed committed after a solver rejected it,
  and a clock that would have rewritten history on a `dt` change. See ADRs
  0006–0008, the [review findings](docs/reviews/2026-08-02-milestone-1-2-review.md),
  and the [remediation report](docs/reviews/2026-08-02-remediation-report.md).
- The integration increment between Milestones 2 and 3 is done: the viewport now
  draws the real world model rather than a hardcoded placeholder.
- Milestone 3 is implemented. Electrostatic point/sphere sources publish E and
  potential to probes, arbitrary planes, and sparse 3D grids; validity-aware
  colour/glyph layers consume those snapshots; authoring covers sources, probes,
  and planes; and a batched WGSL `f32` evaluator is checked against the CPU
  `f64` oracle. The visual-legibility review gate remains for manual review.
- Milestone 4 is implemented. Playback pacing remains separate from numerical
  `dt`; running edits queue to fixed-tick boundaries; probes attach to objects
  and plot bounded scalar/vector history; workbench state is explicit; and
  semantic command recordings replay deterministically. Manual UX acceptance
  of the new plot and transport controls remains.
- A code review of Milestones 3 and 4 was held and its findings applied. It
  found no failing guarantee, but three structural problems: the local and
  loopback data sources duplicated the session semantics they were meant to
  share, the electrostatics plugin existed twice and the copy had already lost
  two trait methods, and the visualization engine drew one hardcoded channel
  from one named plugin. It also found a non-unit rotation that could reach the
  world through a command, a glyph scale recomputed per glyph, and a UI that
  discarded all but the last command in a frame. See the
  [review findings](docs/reviews/2026-08-03-milestone-3-4-review.md) and the
  [remediation report](docs/reviews/2026-08-03-remediation-report.md).
- The review follow-up is implemented. Subscriptions validate and enforce a
  total source-owned sampling budget before adoption; local compute commands
  complete on an ordered background worker; and the visualizer owns independent
  layers for every published vector channel. A probe can choose its recorded
  channels and pin a persistent floating, multi-channel recorder plot while the
  user manipulates the scene.
- Scene-level field-system composition is implemented through the same local or
  remote-ready command boundary. The Inspector lists each plugin's available
  channels and can activate the coupled system as a unit; inactive solvers stop
  all compute while their object schemas and authored properties remain.
- Milestone 5 is implemented. The CPU `f64` oracle and host-injected `wgpu f32`
  backend advance `E` and `B` on periodic Yee lattices, validate the Courant
  limit before accepting `dt`, and publish energy density plus `div E` and
  `div B` residuals. CPU/GPU parity and wave-speed convergence are covered
  headlessly. The desktop composes electrostatics and Maxwell; its XY default
  plane shows a charge-constrained Maxwell E field matching electrostatics,
  while the prescribed wave remains an explicit validation configuration.
- A code review of Milestone 5 was held and its findings applied. The dynamic
  solver, wave convergence, Courant enforcement, and CPU/GPU parity all held up.
  The static-charge initial condition did not: it differenced a non-periodic
  Coulomb potential across the lattice wrap and published the resulting
  fabricated outer layer as an ordinary field value — 281% error in the shipped
  default scene, and up to 30% of the reported energy. The existing test could
  not see it because it centred the charge, where symmetry makes the wrap
  accidentally correct. The seam is now reported as undefined, excluded from
  conservation diagnostics, and covered by an off-centre regression test. The
  review also removed the duplicated CPU/GPU solver scaffolding and stopped
  rebuilding the constrained field for edits that cannot change it. See the
  [review findings](docs/reviews/2026-08-03-milestone-5-review.md) and the
  [remediation report](docs/reviews/2026-08-03-milestone-5-remediation-report.md).
- Milestone 6 architecture preparation is complete. Charge is now defined by a
  shared electromagnetic-source Module rather than by the electrostatics
  Implementation; electrostatics and Maxwell compose the same schema without
  depending on one another. Solver ticks now have a narrow kinematic-outcome
  Interface: motion ownership is checked before stepping and the runtime alone
  validates, revisions, and publishes returned object transforms/velocities.
  See ADRs 0017 and 0018 and the
  [readiness assessment](docs/reviews/2026-08-03-milestone-6-readiness-assessment.md).
- Milestone 6 is implemented. Independent mass and charge components and a NIST
  CODATA catalog feed pinned or solver-integrated objects to Maxwell.
  Periodic CIC charge, six-path continuity-preserving current deposition,
  Gauss-consistent Poisson initialization, Yee interpolation, and a relativistic
  Boris pusher are shared by CPU and host-owned GPU field backends. Runtime
  interventions and solver motion are distinguished in conservation
  diagnostics. ADRs 0019 and 0020 record the model and numerical choices; the
  [implementation report](docs/reviews/2026-08-03-milestone-6-implementation-report.md)
  records the evidence and remaining review items.

The plan deliberately built a thin end-to-end product before the time-domain
Maxwell solver. Each milestone ends in something observable and testable, and
each scientific solver has an independent reference case.

## Workspace shape

Current, with planned crates marked:

```text
apps/
  fieldcad-desktop/      native visualizer and composition root      [present]
  fieldcad-compute/      later headless dedicated compute service    [planned]
crates/
  fieldcad-bench/        headless compute performance harness        [present]
  fieldcad-core/         world, domain, sampling, units, time        [present]
  fieldcad-electromagnetic-sources/ shared charge schema/sources     [present]
  fieldcad-mass-sources/ shared mass schema/sources                  [present]
  fieldcad-particles/    catalog provenance and particle view        [present]
  fieldcad-plugin-api/   equation-system contract and schemas        [present]
  fieldcad-simulation/   runtime and data-source boundary            [present]
  fieldcad-render/       wgpu renderer and visualization layers      [planned]
  fieldcad-ui/           egui panels and input mapping               [planned]
  fieldcad-protocol/     later transport-neutral session protocol    [planned]
plugins/
  test-field/            analytic contract fixture                   [present]
  electrostatics/        first real equation system                  [present]
  electromagnetism/      Maxwell/FDTD CPU reference                  [present]
```

`fieldcad-render` and `fieldcad-ui` remain modules inside `fieldcad-desktop`
until there is a second consumer. Splitting them now would produce crate
ceremony without isolation. Inside the app they are directories rather than
single files — `scene/` divides field geometry, gizmos, picking, and authoring
proxies; `ui/` divides the view model, panels, and the probe plot — which is the
right amount of structure while there is still one consumer.

Crate boundaries are dependency rules, not a promise of separate threads or
dynamic libraries. We should merge any boundary that produces ceremony without
isolation and split a crate only when ownership is clear.

## Milestone 0 — agree on the foundation

Deliverables:

- review `README.md`, `CONTEXT.md`, and this plan;
- record accepted high-impact choices as short architecture decision records;
- select initial operating-system targets and a minimum GPU feature set;
- accept the field-data-source boundary for both local and remote compute; and
- pin a mutually compatible `wgpu`, `winit`, and `egui` set.

Exit criteria:

- the review questions at the end of this document have answers or explicit
  defaults; and
- electrostatics-first followed by coupled electromagnetism is accepted.

## Milestone 1 — desktop and viewport spike

Implementation status: **complete on Linux; interactive verification on Windows
and macOS pending. Both platforms build and test in CI.**

Build the smallest native application that validates the graphics stack:

- a `winit` event loop and resizable `wgpu` surface;
- an `egui` menu/inspector region composed with a custom 3D render pass;
- perspective camera, orbit, pan, dolly, focus, and axis views;
- world axes, ground/reference grid, and a selectable placeholder object;
- correct input arbitration when the pointer or keyboard is captured by UI;
- adapter information, frame time, and recoverable surface-error diagnostics;
- shader compilation in tests or CI where practical; and
- a software/fallback-adapter path with a clear error if no usable GPU exists.

Exit criteria:

- the app opens, resizes, and exits cleanly on the initial target platforms;
- camera controls remain stable across frame rates and high-DPI scaling;
- the grid can be toggled and does not z-fight at ordinary camera distances; and
- UI interaction does not also move the camera.

Review gate: assess `egui` ergonomics and the direct `wgpu` integration before
building domain code on top of them.

## Milestone 2 — domain core and plugin seam

Implementation status: **complete; review gate held and its findings applied.**

Create the non-rendering foundation:

- object IDs, transforms, velocities, shapes, plugin components, bounded slice
  planes, and probes;
- dimensional property/channel schemas and SI-backed values;
- world commands committed at explicit boundaries, including schema
  registration;
- `Paused`/`Running` fixed-step simulation state machine with an editable `dt`;
- a `Domain` — bounds, resolution, boundary conditions, precision — carried on
  every snapshot;
- immutable, revisioned field snapshots of columnar batches with per-sample
  validity;
- a field data source interface with two implementations, local and loopback;
- scene-level field-system composition exposed through that data-source
  boundary: available channels remain discoverable while inactive systems stop
  simulation without unregistering their object properties;
- Rust plugin traits with configuration validation, world validation, and
  diagnostics; and
- a tiny test plugin that exposes a known scalar/vector function.

Keep the core headless. It must be possible to test world edits, stepping, and
sampling without creating a window or GPU device.

Exit criteria:

- play, pause, and single-step have deterministic unit tests;
- an edit is seen atomically by a plugin on the correct revision, and a rejected
  edit leaves the world and every solver untouched;
- stale snapshots can be identified and never masquerade as current results;
- channels and properties reject dimensionally invalid values;
- neither `fieldcad-core` nor `fieldcad-plugin-api` depends on UI crates; and
- swapping a local source for a remote source does not change probe or
  visualization consumers — driven by one script through both, asserting
  identical observations.

Review gate: **held.** The API was inspected before a real physical model
implemented it. Findings and their resolutions are recorded in ADRs 0006, 0007,
and 0008; the concrete changes were a `Domain` type, columnar batched sampling
with interned channel handles, distinct channel and component identifier types,
per-sample validity, validate-before-adopt for world edits, and an epoch-based
clock.

## Milestone 3 — useful electrostatic vertical slice

Implementation status: **complete; final manual UX acceptance pending.**

The local WGSL backend is injected by the application host and publishes
ordinary snapshots rather than solver GPU handles (ADR 0010). GPU/CPU parity is
defined as `abs(error) <= 2e-3 + 5e-4 * max(abs(cpu), abs(gpu))`; the looser
tolerance is explicit because the interactive backend is `f32` and the oracle
is `f64`.

Implement the first real equation system and connect the whole product:

- charged point/sphere objects with editable charge and position;
- analytic Coulomb-field and potential evaluation with superposition;
- explicit treatment of the source singularity/radius;
- a CPU `f64` reference evaluator;
- a batched WGSL evaluator for interactive plane and 3D samples;
- electric-vector glyphs and magnitude colour on the default XY plane;
- arbitrary translated/rotated slice planes;
- sparse whole-domain 3D glyphs;
- point selection, transform gizmo, property inspector, and scene tree; and
- movable probes showing value, magnitude, units, and snapshot revision.

The 2026-08-03 authoring refinement adds spherical charge proxies, selectable
plane/probe authoring geometry, Blender-style X/Y/Z and XY/YZ/ZX constrained
translation handles, per-plane magnitude/vector density, in-plane vector
projection by default with an explicit full-3D option, and per-item viewport
visibility/delete controls. Plane presentation remains visualizer-owned so it
can consume the same snapshots from a future remote compute source.

The 2026-08-03 follow-up restores camera-plane dragging from the selected charge
body, makes grid and world-axis helpers independently visible, and replaces
integer-index glyph decimation with uniform interpolated placement at any
non-negative display density. Analytic electrostatic planes are sampled over
their complete requested extent rather than clipped to the numerical grid
domain.

The top-panel time step is also an editable, magnitude-sensitive drag value. It
parses unitless seconds, scientific notation, and explicit SI time suffixes,
then submits the existing validated `SetTimeStep` command.

The unified-selection refinement applies the same origin marker, X/Y/Z axis
handles, XY/YZ/ZX plane handles, free camera-plane drag, and focus action to
objects, probes, and slice planes. Selected planes additionally expose a dashed,
labelled, size-proportional normal handle whose arcball drag preserves in-plane
orientation while rotating the plane.

Exit criteria:

- a single-charge result matches the analytic formula at selected points;
- superposition tests cover multiple positive and negative charges;
- GPU samples agree with the CPU oracle within a documented tolerance;
- moving or editing a charge invalidates and recomputes displayed samples;
- a user can create, orient, duplicate, hide, and remove slice planes; and
- visualization density can change without changing the physical result.

This milestone is static physics. If objects are animated for demonstration,
their motion is prescribed and the field is an instantaneous electrostatic
re-evaluation; the UI must label that approximation clearly.

Review gate: use the vertical slice to decide which visualization layers are
actually legible before investing in more techniques.

## Milestone 4 — time controls, probes, and edit workflow

Implementation status: **complete; final manual UX acceptance pending.**

Turn the vertical slice into a coherent simulation workbench:

- transport controls with editable fixed `dt`, speed multiplier, and current
  simulation time;
- exact one-tick stepping while paused;
- a bounded probe history plot with vector components and magnitude;
- a persistent floating recorder plot that survives selection changes and can
  show several recorded channels without blocking viewport interaction;
- attach/detach probes from moving objects;
- command queuing for edits made while running;
- clear solving, stale, invalid, and paused states in the UI; and
- record/replay fixtures for deterministic command sequences.

Running world edits are acknowledged as queued and applied in submission order
immediately before the next fixed tick. Pausing flushes accepted edits at the
current boundary. Playback speed scales fixed-tick demand from wall-clock time;
it does not alter `dt` (ADR 0011).

Exit criteria:

- repeated runs of a deterministic test scenario produce the same core state;
- probe samples carry the correct tick time and snapshot revision;
- an edit during `Running` becomes visible at a documented tick boundary; and
- a solver that falls behind does not silently increase its numerical time step.

## Milestone 5 — coupled Maxwell solver and magnetic visualization

Implementation status: **complete; performance review gate pending.**

Add an electromagnetism plugin rather than extending electrostatics in place:

1. **Done:** implement a small CPU `f64` reference finite-difference time-domain
   solver;
2. **Done:** place `E` and `B` components on a three-dimensional Yee lattice;
3. **Done:** enforce the Courant stability limit when choosing `dt`, using the
   plugin's validate-before-adopt time-step hook;
4. **Done for the reference slice:** periodic boundaries and a prescribed
   travelling plane-wave initial condition;
   the desktop default instead builds a curl-free E constraint from stationary
   authored charges with B=0, whose outermost lattice layer is undefined
   because a Coulomb potential is not periodic;
5. **Done:** port field updates to host-owned `wgpu` compute buffers with
   asynchronous submission/readback and cooperative shutdown cancellation;
6. **Done:** publish electric, magnetic, energy-density, and both divergence
   residual channels; and
7. **Done:** the desktop composes the Maxwell scenario; planes, glyphs, colours,
   and probe recorders show E and B as independent layers.

Absorbing boundaries should follow the basic solver; perfectly matched layers
are valuable but should not obscure validation of the interior update scheme.

Exit criteria:

- a vacuum plane wave propagates at the expected numerical wave speed;
- a convergence test improves as spatial and temporal resolution increase;
- the plugin reports unstable parameter choices instead of running them;
- divergence and energy diagnostics are visible and covered by regression tests;
- CPU and GPU solvers agree on a small deterministic grid within tolerance; and
- electric and magnetic channels can be inspected together on independent
  visualization layers; and
- a stationary authored charge produces the electrostatic field direction and
  magnitude within grid tolerance and remains stationary across Maxwell steps.

Review gate: profile representative grids and choose performance budgets from
measurements on named hardware rather than from speculative cell counts.

## Milestone 6 — moving charged objects and field coupling

Implementation status: **complete.** The shared charge-source Module retains
object identity, position, velocity, charge, and distribution. The solver-step
Interface publishes canonical object kinematics through the authoritative
runtime with pre-step motion-ownership conflict checks. ADRs 0017–0020 record
the ownership, particle-model, and numerical coupling decisions.

Support physical feedback between objects and fields:

- define one generic particle object with mass, charge, transform, velocity, and
  motion mode;
- provide initial electron, proton, positron, and neutron templates that fill
  mass/charge values without introducing template-specific solver behaviour;
- distinguish fixed, prescribed-motion, and dynamically integrated objects;
- deposit charge and current onto the grid using a charge-conserving scheme;
- interpolate fields back to particles;
- integrate Lorentz-force motion with an appropriate particle pusher;
- define collision/domain-exit behaviour; and
- expose total charge, field energy, particle energy, and conservation drift.

This is a particle-in-cell feature, not merely `position += velocity * dt`.
Manual object edits during a run create a discontinuity; they must be logged as
external interventions and may require solver reinitialization.

The reference implementation uses periodic CIC charge deposition and averages
six exact-continuity axis paths for current. It subtracts and diagnoses the
uniform background required by net charge in a periodic Poisson solve,
interpolates reconstructed Yee fields, and advances dynamic velocity with a
relativistic Boris pusher. Domain exits wrap periodically and particles pass
through one another; collisions are not part of this increment. The host-owned
GPU backend advances E/B and current on GPU while using one full-state readback
per tick for the shared CPU `f64` particle oracle. Diagnostics name that
reference-path cost; eliminating it is a performance follow-up, not a hidden
claim of fully resident GPU particle coupling.

Exit criteria:

- charge conservation and deposition have focused numerical tests;
- every initial catalog template creates the same inspectable generic particle
  representation with versioned source values;
- known single-particle trajectories behave within documented error bounds;
- a two-particle proton/electron baseline runs reproducibly and reports its
  stability or decay without asserting that Maxwell must produce a stable atom;
- pause/edit/resume has explicit and tested reinitialization semantics; and
- diagnostics distinguish numerical drift from user-injected changes.

## Milestone 7 — prove extensibility with gravity

Implement a minimal gravity plugin to test that abstractions are not secretly
electromagnetic:

- mass as an independently declared object component — **done** ahead of this
  milestone by ADR 0021: `fieldcad-mass-sources` owns the schema, and one object
  may already carry both charge and mass;
- gravitational potential and acceleration channels;
- analytic point/sphere sources first; and
- reuse of generic planes, glyphs, colour maps, probes, and property editing.
  Property editing is **done**: the inspector is schema-driven, so gravity's
  component becomes authorable with no desktop change.

Exit criteria:

- gravity adds no dependency to electromagnetism;
- one object may carry both charge and mass components — **met**;
- the application can enable and inspect both equation systems without channel
  ID or unit ambiguity; and
- any plugin API change is documented with the concrete need that forced it.

## Milestone 8 — runtime plugin and project format

Only after at least two real equation systems:

- version and freeze a minimal serializable plugin contract;
- spike WebAssembly Component Model loading and resource limits;
- define signed package manifests, capability requests, and compatibility errors;
- decide how host-validated WGSL modules are packaged and constrained;
- design a versioned project file with missing-plugin placeholders; and
- add autosave/recovery before allowing third-party extensions to mutate a
  project session.

Exit criteria:

- an example out-of-tree plugin can be installed and removed without rebuilding;
- incompatible or malformed plugins fail without losing the world;
- project files retain unknown plugin data for recovery; and
- CPU time, memory, GPU resources, and host capabilities are bounded.

## Milestone 9 — dedicated remote compute

Turn the headless simulation runtime into an independently deployable service
while keeping the desktop application usable in local mode:

1. define a transport-neutral, versioned protocol for sessions, world commands,
   time controls, subscriptions, acknowledgements, diagnostics, and chunked field
   snapshots;
2. build a loopback client/server harness before selecting a network transport;
3. measure representative plane, probe, sparse-3D, and full-grid payloads, then
   spike reliable-stream transports and compression options;
4. add subscriptions by channel, region, plane, representation, and level of
   detail so invisible data is not transferred;
5. implement backpressure, cancellation, progressive completeness, integrity
   checks, and strict snapshot assembly;
6. upload completed remote chunks into the same local `wgpu` resource forms used
   by visualization layers;
7. surface latency, bandwidth, server tick rate, snapshot age, and connection
   state in diagnostics; and
8. add authenticated, encrypted sessions and explicit server resource limits
   before use outside a trusted network.

Exit criteria:

- the same recorded scenario is visually and numerically equivalent through the
  local and loopback-remote data sources;
- play, pause, step, and edits are correlated with authoritative server
  acknowledgements and world revisions;
- a slow client does not slow simulation unless it explicitly requests that
  policy, and obsolete presentation data can be dropped safely;
- disconnect retains only the last complete snapshot and marks it stale;
- reconnect reconciles the session and subscriptions without mixing revisions;
- corrupted, duplicate, out-of-order, and incomplete chunks have protocol tests;
  and
- field transfer is bounded by active subscriptions rather than total solver
  state.

Review gate: choose and commit to the production transport only after the payload
and latency measurements above. The snapshot and command semantics must not
depend on that choice.

## Milestone 10 — reproducible research experiments

Turn an interactive scene into a durable, inspectable research experiment:

- define a versioned experiment specification containing the world, active
  models and versions, parameters, initial/boundary conditions, run controls,
  random seeds, interventions, and requested observations;
- support checkpoint/restart and headless batch execution;
- add parameter sweeps and statistical ensembles without coupling them to the
  viewport lifecycle;
- export quantitative observations, diagnostics, and provenance in documented
  machine-readable forms;
- generalize the current field-only result stream to typed trajectories,
  probe and energy histories, integrated quantities, and statistical
  distributions with the same identity and backpressure semantics; and
- record convergence studies, uncertainty estimates, and comparisons with
  accepted reference data as first-class run outputs.

Exit criteria:

- another machine can reproduce a deterministic experiment from its saved
  specification or report exactly why it cannot;
- batch and interactive execution use the same model configuration and result
  identity;
- a published result retains solver, hardware/execution, precision, parameter,
  and intervention provenance; and
- parameter sweeps and ensembles can run on dedicated compute without streaming
  every intermediate field to the desktop.

## Milestone 11 — atomic-arrangement and particle experiment suite

Build research comparisons on the generic particle/catalog foundation while
keeping behaviour entirely attributable to active field models:

- expand the versioned template catalog and retain template identity only as
  provenance and UI metadata;
- publish reproducible experiment presets, beginning with a proton/electron
  Hydrogen-under-Maxwell arrangement across selected initial velocities;
- record particle trajectories together with E/B fields, probe histories,
  radiation/energy measures, and conservation diagnostics; and
- compare explicit changes to equations, coupling, regularization, boundaries,
  resolution, and integration to determine why an arrangement is stable or
  unstable under each model.

Exit criteria:

- a proton/electron experiment runs reproducibly through Maxwell particle
  coupling and reports its observed orbit, radiation, energy transfer, and any
  collapse or decay without hidden stabilization;
- any modification that changes stability is named in experiment provenance and
  can be enabled, disabled, and compared against the baseline; and
- workloads execute through the same local/dedicated data-source architecture
  with visualization subscriptions independent of solver scale.

Review gate: specify the first proton/electron experiment's initial conditions,
measured observables, acceptable numerical error, and comparison protocol before
choosing or tuning any stabilizing model change.

## Cross-cutting engineering work

### Verification

- Unit-test dimensions, commands, time state, sampling, and interpolation.
- Compare every GPU solver with a small CPU oracle.
- Keep analytic fixtures for symmetry, superposition, falloff, and wave cases.
- Add convergence tests; a visually plausible image is not a correctness test.
- Separate tolerances for `f32` GPU results and `f64` reference calculations.
- Capture deterministic world/solver fixtures for regression testing.
- Treat reproducible experiment definitions, convergence evidence, uncertainty,
  and comparisons with external reference results as research deliverables, not
  optional documentation.

### Observability and performance

- Instrument simulation ticks, GPU dispatches, snapshot publication, render
  passes, probe readback, command latency, snapshot assembly, and data-source
  latency from the first vertical slice.
- Define representative research benchmarks on named hardware and record
  throughput, memory use, numerical precision, CPU/GPU transfer cost, and
  strong/weak scaling where a backend supports parallel execution.
- Keep authoritative solver state on the compute resource and avoid mandatory
  full-state readback; interactive subscriptions and research observations
  should transfer only what their consumers request.
- Display domain resolution, memory estimate, solver `dt`, tick rate, and stale
  snapshot age in a diagnostics panel.
- Degrade visualization density before changing solver resolution.
- Avoid synchronous GPU readback in the render loop.

### User experience

- Keep navigation available while paused or solving.
- Make units, channel identity, simulation time, and approximation mode visible.
- Make all destructive scene operations undoable once project persistence lands.
- Prefer manipulators plus numeric entry; scientific placement needs both.
- Treat invalid solver configuration as editable state with useful diagnostics,
  not as an application crash.

## Proposed review defaults

These choices materially affect the architecture. The recommendation column is
the default if no contrary requirement emerges during review.

| Question | Recommended starting answer |
| --- | --- |
| Primary audience and accuracy | Physics research and advanced modelling, with explicit validity regimes, reproducible provenance, convergence evidence, and transparent numerical error. Early solvers remain exploratory until individually validated. |
| Initial platforms | Linux, Windows, and macOS desktop, with Linux as the first development target. |
| Runtime third-party plugins | Defer; use first-party Cargo crates until electrostatics and electromagnetism stabilize the contract. |
| First electric model | Analytic electrostatics with point/sphere charges. |
| First Maxwell sources | Constrained stationary charges plus a prescribed-wave validation mode; defer moving-charge current deposition to the coupling milestone. |
| Internal units | SI, with display-unit conversion in the UI. |
| Whole-field 3D view | Sparse vector glyphs and seeded streamlines; do not attempt one glyph per solver cell. |
| Initial numerical domain | User-visible finite box with uniform resolution and explicit boundary conditions. |
| Initial object geometry | Point/sphere source representation; arbitrary material meshes later. |
| Persistence | In-memory first, versioned project format after the world and plugin schemas have survived the vertical slice. |
| Compute placement | One field-data-source contract; in-process runtime first and a dedicated remote service after solver data layouts are measured. |
| Remote transport | Defer the transport choice; require versioned commands, subscriptions, and chunked immutable snapshots independently of it. |

## Next implementation increment

Close Milestone 5's performance review gate on named hardware. Profile GPU step
submission, full-grid readback, snapshot publication, scene extraction, and
render time separately over representative grid and visualization densities.
Use those measurements to establish desktop budgets and decide when the current
single 32³ readback should give way to GPU-side sparse sampling or chunked
publication. Preserve the CPU/GPU parity fixture while optimizing.

The headless half of that measurement now exists: `crates/fieldcad-bench` sweeps
named scenes, reports cost per cell/source/sample, and fits the observed growth
against each operation's declared complexity. First observations on named
hardware are recorded in
[docs/perf](docs/perf/2026-08-03-first-compute-observations.md); the largest
finding is that a default-scene tick spends 26% of its time advancing fields and
76% publishing and diagnosing them. Two gaps remain for the gate itself: GPU
backends are not reachable from the harness because `GpuMaxwellBackend` is
crate-private to the desktop, and scene extraction and render time are
deliberately unmeasured because visualization is not the current target.

After that gate, begin Milestone 6 on the prepared Interfaces in this order:

1. add generic particle mass/charge properties, initial catalog templates, and
   explicit fixed, prescribed, and dynamically integrated motion modes; assign
   kinematic authority only for dynamic objects;
2. specify and test a charge-conserving charge/current deposition scheme,
   including discrete continuity and stationary-source compatibility;
3. interpolate Yee fields back to charged objects and validate a Lorentz-force
   pusher against known single-particle trajectories;
4. connect accepted transform/velocity outcomes to the runtime and define
   domain-exit, authored-edit, and pause/resume reinitialization semantics; and
5. publish total charge, particle/field energy, and intervention-aware drift
   diagnostics before exposing the new mode as a desktop default.

This extends the current instantaneous stationary-charge constraint into causal
moving-source electromagnetism rather than replacing the prescribed-wave
validation case.

The manual acceptance pass should now include the floating recorder while moving
a charge/probe, simultaneous layer legibility, sampling-budget rejection, and
continued camera responsiveness while the compute worker is solving. The older
Milestone 1 cross-platform interactive checks remain pending.

Cross-platform Milestone 1 verification runs in parallel — CI builds and tests
Windows and macOS on every push, leaving only interactive checks (window
lifecycle, high-DPI camera behaviour, surface recovery) to be done by hand.
