# Field CAD project context

Status: **accepted; implementation underway**

Implementation note (2026-08-03): Milestones 1–4 are implemented. The headless
core, Rust plugin seam, deterministic local runtime, and transport-neutral field
data source passed the Milestone 2 review gate. The first real equation system
now publishes electrostatic E/potential from charged points and uniform spheres;
the desktop visualizes those snapshots on arbitrary planes and sparse 3D grids.
The local WGSL evaluator is host-owned and publishes ordinary snapshot columns,
so it does not weaken the later dedicated-compute boundary (ADR 0010).
The workbench now adds source-owned playback pacing, deterministic running-edit
boundaries, bounded probe histories with attachment, and record/replay fixtures
without moving simulation authority into the desktop (ADR 0011).

Milestone 5 is implemented. A CPU `f64` Maxwell reference and a
host-injected `wgpu f32` backend evolve staggered `E` and `B` components on a
periodic 3D Yee lattice, publish energy density and both divergence observables,
and reject a `dt` above the Courant limit before the clock adopts it (ADRs 0013
and 0015). A prescribed travelling plane wave remains the convergence oracle.
The desktop instead initializes a curl-free Yee E field from its stationary
authored charge and B=0, rebuilding that constraint after source edits (ADR
0016). A Milestone 5 review then found that this construction differences a
non-periodic Coulomb potential across the lattice wrap, fabricating the
outermost layer; that layer is now reported as undefined rather than drawn, and
excluded from conservation diagnostics. The periodic domain, resolution,
precision, and initial-condition settings are visible through the source-owned
scene catalog; independent E/B layers and the default probe consume ordinary
immutable snapshots just like a later remote visualizer will.

Milestone 6 is implemented. Its preparation removed an accidental dependency
between equation systems: charge schema and authored-source extraction live in a shared
electromagnetic-source Module consumed independently by electrostatics and
Maxwell (ADR 0017). Solver ticks can also declare exclusive kinematic authority
and return narrow transform/velocity outcomes. The runtime validates and adopts
those outcomes as the only world writer (ADR 0018). A shared generic-particle
component and versioned catalog feed mass/charge data to the solver without
species dispatch (ADR 0019). Maxwell uses periodic CIC charge, six-path
continuity-preserving current deposition, Gauss-consistent periodic Poisson
initialization, Yee-field interpolation, and a relativistic Boris pusher (ADR
0020). The GPU field update shares the CPU `f64` particle oracle for now and
reports its per-tick E/B readback explicitly as a performance limitation.

A review of Milestones 3 and 4 has been held and its findings applied. The
visualization model now owns independent layers for every published vector
channel rather than naming an equation system, so electric `E`, magnetic `B`, or
gravitational acceleration can be shown together with separate plane and 3D
settings. Subscriptions became validated commands on the data-source boundary;
the authoritative source enforces per-axis and total sample budgets before
adopting them. Local compute now runs behind a non-blocking submission/final
completion boundary on a dedicated worker (ADR 0012), matching the latency shape
of the later remote client while retaining the last complete snapshot.

The decisions that shaped the code — and the reasoning behind them — are recorded
as ADRs in `docs/adr/`. Where this document describes intent, an ADR describes a
commitment and what it cost.

This document defines the product language and the boundaries that should remain
stable as the implementation grows. It describes intent rather than a frozen
Rust API.

## Purpose

Field CAD is a high-performance physics research and modelling environment. A
user constructs a world and a reproducible experiment, chooses equation
systems, runs them locally or on dedicated compute, observes fields and
particles in 3D or on arbitrary planes, and analyses how quantitative values
evolve over time.

The long-term scientific scope includes atoms, subatomic particles, and
particle-physics experiments viewed through fields. Electron, proton, positron,
and neutron names belong to an authoring catalog: each template creates the same
generic particle object with preset mass and charge. It does not select hidden
species-specific behaviour. The active equation systems, coupling, boundaries,
and numerical methods determine the result and must declare the physical regime
they represent.

The application should serve two modes without creating two architectures:

1. **Inspection:** evaluate a static or analytic field after an edit.
2. **Simulation:** advance numerical state using a fixed time step and display
   successive field snapshots.

The emphasis is scientific legibility, reproducibility, and performance. A
result must retain enough provenance to say which model, parameters, domain,
resolution, precision, time, solver, and execution configuration produced it,
and enough diagnostics to judge whether it is valid for the experiment.

## Ubiquitous language

| Term | Meaning |
| --- | --- |
| **World** | The user-authored objects, common transforms and velocities, attached plugin properties, probes, and visualization planes. |
| **Experiment** | A reproducible specification of a world, active physical models, parameters, initial and boundary conditions, interventions, run controls, and requested observations. |
| **Object** | An identifiable entity with a transform and optional shape, velocity, and plugin-defined components such as charge or mass. |
| **Particle template** | A catalog entry, such as electron, proton, positron, or neutron, that creates a generic particle with preset physical properties such as mass and charge. It is an authoring convenience and provenance record, not a separate runtime behaviour. |
| **Equation system** | A physical model and its coupled equations, such as electrostatics or electromagnetism. |
| **Equation-system plugin** | A module that declares fields and object properties, validates configuration, and evaluates or advances its equation system. |
| **Physical-source schema** | A stable object-property schema, such as charge, that may be consumed by several independent equation systems and is not owned by one solver Implementation. |
| **Field system** | One equation-system plugin composed into a scene, with an authoritative active/inactive state. Its field channels are coupled at this activation boundary. |
| **Field channel** | One observable scalar or vector output of a plugin, such as electric field `E`, magnetic field `B`, potential, or an error residual. |
| **Observation** | A versioned experiment output with model and run provenance. Field samples are one kind; particle trajectories, probe/energy histories, integrated quantities, and statistical distributions are others. |
| **Domain** | The finite 3D region over which a numerical field is represented, including resolution and boundary conditions. |
| **Field snapshot** | Immutable, versioned solver output for a particular simulation time and world revision. |
| **Field data source** | A local runtime or remote compute session that accepts commands and publishes field snapshots through the same application-facing contract. |
| **Subscription** | The channels, spatial region, representation, and level of detail that a visualizer currently asks a data source to publish. Purely a visualization concern: it never changes a computed value. |
| **Sample geometry** | Where a batch of field values was taken — probe points, a lattice on a slice plane, or a lattice over the domain. Described once per batch rather than stored per sample. |
| **Sample validity** | Whether a returned value was evaluated exactly, interpolated from stored samples, or is undefined — inside a source radius, outside the domain, unconverged, overflowed, or read across a periodic seam the solver's state does not satisfy. |
| **Visualization layer** | A view of one field channel using glyphs, colour, contours, streamlines, or another generic rendering technique. |
| **Slice plane** | A transformable plane on which a field is sampled and drawn. The XY plane is only a default. |
| **Probe** | A recorder, currently point-shaped and optionally attached to an object, that samples selected channels and stores bounded time-series values. |
| **Simulation tick** | One deterministic, fixed-duration advance of authoritative simulation state. |
| **Kinematic authority** | The one active solver permitted to publish the canonical transform and velocity of a particular dynamically integrated object during a tick. |
| **Frame** | One screen presentation. Frames and simulation ticks are intentionally independent. |

“Plugin” below always means an equation-system plugin. It does not imply that
the first version loads untrusted binary extensions at runtime.

## Physics model

### Continuous intent, discrete representation

The product language talks about a field at every point in space. An actual
solver supplies a representation with a stated validity region:

- an analytic evaluator can sample arbitrary points;
- a uniform or adaptive grid stores discrete samples;
- a staggered grid may store different components at different locations; and
- a probe may interpolate nearby samples and report the interpolation method.

The renderer must consume the plugin's field description rather than assume
that every field is a cell-centred 3D texture.

### Electric first, electromagnetism second

The first vertical slice is electrostatics: charged sources produce an electric
field through an analytic Coulomb evaluator. This is fast to validate and is a
good test of authoring, sampling, visualization, and plugin boundaries.

Time-domain electromagnetism is a distinct second equation system. Maxwell's
curl equations couple electric and magnetic fields:

```text
curl(E) = -dB/dt
curl(B) = mu0 J + mu0 epsilon0 dE/dt
```

Consequently, a physically meaningful Maxwell solver cannot add magnetic
dynamics as an optional visualization detail after independently evolving only
the electric field. The electromagnetism plugin will introduce `E` and `B`
together, initially using a finite-difference time-domain method on a staggered
Yee grid.

Moving point charges add another level of coupling: object charge and velocity
must become charge/current density on the grid while respecting charge
conservation. Before that dynamic coupling, stationary authored charges provide
a constrained electrostatic initialization: E is a discrete curl-free gradient
and B is zero, so Maxwell agrees with the electrostatic picture instead of
inventing a source-free wave. A periodic lattice can only carry a field whose
potential is periodic, and an isolated charge's is not, so the outermost layer
is undefined rather than fabricated. The legacy stationary-charge initialization
remains available for non-particle sources; a moving generic particle switches
Maxwell to the periodic coupled state.

### Particle templates and field-model experiments

Milestone 6 introduces charged-particle/field feedback. The simulated entity is
a generic particle carrying ordinary world properties such as mass, charge,
position, and velocity. Electron, proton, positron, and neutron templates fill
those values from a versioned catalog; after creation they do not dispatch to
species-specific solver code. A neutral template simply has zero charge and is
unaffected by an electromagnetic field unless another enabled field system
couples to one of its properties.

A proton and electron with selected initial positions and velocities form a
Hydrogen experiment under the active Maxwell/Lorentz model. The application does
not promise a stable atom. If the arrangement radiates, loses energy,
collapses, or otherwise decays, that is an observable result of the stated
equations, regularization, discretization, boundaries, and time integration.

The research workflow is to reproduce that result, inspect fields, trajectories,
energy and conservation diagnostics, then compare explicit model or solver
changes. A stabilizing change must be named and attributable—for example a
different coupling term, approximation, regularization, boundary treatment, or
integrator. The solver must never introduce hidden forces merely because the
catalog template or scene preset is called “electron”, “proton”, or
“Hydrogen under Maxwell”.

Particle-in-cell macro-particles remain distinct from individually authored
particles because their mass and charge may represent a distribution rather
than one physical particle. The active representation must be visible in
experiment provenance.

### Units and singularities

- Authoritative physical quantities use SI internally.
- The UI may format values in convenient derived units without changing stored
  values.
- Dimensions belong to property and channel schemas; `charge = 2` without a unit
  is not a valid serialized value.
- Point-source singularities are marked as undefined inside a declared source
  radius. Rendering may clamp colour and glyph length, but the solver must not
  silently change the physics merely to make the image attractive.
- CPU-side scene state and reference solvers favour `f64`. GPU grids may use
  `f32` when adapter support or throughput makes that appropriate, and the choice
  is part of snapshot metadata.

## Conceptual architecture

```text
keyboard / mouse / UI
          |
          v
     world commands  ----->  undo/history (later)
          |
          v
    field data source <-----------------------+
     /             \                          |
    v               v                         |
local runtime   remote compute client         |
    |               |                         |
    v               v                         |
authoritative world + simulation clock        |
    |                                         |
    v                                         |
equation-system plugins + object coupling ----+
          |
          v
 immutable field snapshots
          |
       +--+------------------+
       |                     |
       v                     v
 visualization engine    probes/diagnostics
       |
       v
  wgpu viewport + egui
```

### Application shell

Owns the native window, event loop, high-level application state, keyboard
shortcuts, selection, command routing, and lifecycle. It translates input into
world or view commands; it does not calculate fields.

### World model

Owns stable object identifiers and common authoring state:

- transform and optional geometry;
- linear velocity initially, with angular motion later if needed;
- plugin components described by typed schemas;
- probes, slice planes, and visualization-layer configuration; and
- a monotonically increasing revision.

Physical values are attachments to world objects. A shared source Module owns
the stable `charge` schema consumed by electrostatics and electromagnetism; a
future gravity Module can add `mass` to the same object. Multiple plugins may
contribute one identical schema, which the runtime registers once, while
incompatible definitions with the same identity are rejected before solver
creation. A plugin does not privately own the canonical object transform.

Field-system activation is scene/session state, distinct from schema
registration. Inactive systems remain discoverable and their component schemas
stay registered, so authored charge, mass, and other properties survive. They do
not validate, step, sample, diagnose, or publish. Re-enabling constructs solver
state at the current scene time and validates the current world, `dt`, and
sampling budget before the new composition is adopted.

An edit becomes a command and is committed at a tick boundary. While paused it
can commit immediately at the current boundary. While running it is queued in
submission order and committed immediately before the next fixed tick; a pause
flushes accepted edits at the current boundary. This avoids a solver observing
half of a UI edit and creates a future seam for undo/redo and record/replay.

### Simulation runtime

Owns simulation time, fixed-step scheduling, plugin instances, and publication of
snapshots. Its state machine is:

```text
Paused --Play--> Running --Pause--> Paused
Paused --Step--> Paused (after exactly one accepted tick)
```

Rendering continues in either state. A slow solver may cause fewer screen
updates, but elapsed wall-clock time must not silently change the numerical
`dt`. A playback multiplier changes only how many unchanged fixed ticks are
requested per wall-clock interval. Edits invalidate a static solution or enter
a running simulation at the next fixed-tick boundary.

The exact scheduling of source deposition, field advance, force evaluation, and
object integration remains inside the equation system where numerical coupling
requires it. The core runtime coordinates phases without assuming every theory
uses the same integrator.

A time-stepped solver may declare exclusive kinematic authority over selected
objects and return complete transform/velocity outcomes from a tick. The runtime
checks missing objects and competing claims before any solver advances, then
validates and adopts all results through the authoritative world-command
Interface. Plugins never receive mutable world access. A tick that moves an
object republishes analytic systems as well, because their fields may depend on
the new pose.

A candidate numerical time step is validated by every active solver before the
runtime adopts it. This makes a Courant or other method-specific stability error
part of the rejected command, rather than a failure discovered after the next
tick has begun.

The runtime exposes the full field-system catalog independently of snapshot
provenance. Only active systems appear in provenance; otherwise a system that
correctly publishes nothing could not be re-enabled from a local or remote UI.

### Local and remote compute boundary

The desktop application talks to a field data source rather than directly to a
solver. In local mode an adapter wraps the in-process simulation runtime. In
remote mode a client sends commands and subscriptions to a dedicated compute
service and assembles streamed snapshot chunks. Both modes expose the same world
revision, simulation time, channels, validity, diagnostics, and connection state
to the rest of the application.

`FieldDataSource` is the current Interface name because field snapshots are the
implemented result type. It is not a claim that every research observation is a
field. Particle experiments must extend or generalize the typed result stream
for trajectories, probe and energy histories, integrated quantities, and
statistical distributions while preserving the same authoritative session,
provenance, subscription, completeness, and backpressure semantics.

When compute is remote, the service is authoritative for the world revision and
simulation clock. The desktop may stage edits for responsiveness, but it does not
claim that they took effect until the service acknowledges the resulting
revision. Play, pause, and step are commands with correlated acknowledgements;
they are not inferred from incoming frame timing.

Local compute has the same asynchronous shape at the application boundary. A
submission is provisional and carries no promised snapshot sequence; a later
completion event reports whether the ordered worker applied, queued, or rejected
it. While work is pending the visualizer remains interactive and shows the last
complete snapshot as solving/stale rather than borrowing mutable solver memory.

A remote snapshot envelope includes at least:

- protocol and schema versions, session ID, monotonically increasing sequence,
  world revision, simulation time, and plugin/model versions;
- domain and channel descriptors, numerical representation, precision, and
  boundary-condition metadata;
- subscription identity and the spatial extent/resolution of each payload;
- chunk ordering, completeness, compression, and integrity information; and
- solver diagnostics and whether the result is complete, progressive, or stale.

Full-resolution 3D arrays are not assumed to fit interactive network budgets.
The visualizer requests only the channels, region, planes, probe samples, or
whole-domain level of detail needed by visible layers. Field chunks are uploaded
into client-side `wgpu` resources; renderers never depend on server GPU handles.

Control traffic must be reliable and ordered. The bulk-data transport will be
selected after measuring representative field sizes and update rates, but the
protocol model must allow chunking, compression, cancellation, and independent
progress for subscriptions. Under backpressure the client may supersede an old
unpresented snapshot with a newer complete one. It must not combine chunks from
different snapshot identities.

On disconnect, the viewport may retain the last complete snapshot with a visible
stale/disconnected state. Reconnection establishes or resumes a session,
reconciles the authoritative world revision, and renews subscriptions before new
data is labelled current.

### Plugin contract

A plugin will conceptually provide:

- identity, version, description, and compatibility metadata;
- field-channel descriptors: scalar/vector shape, dimensions, sampling support,
  and suggested visual mappings;
- object-component and solver-configuration schemas;
- configuration and world validation;
- candidate time-step validation for numerical stability;
- initialization from a domain and a read-only world view;
- static evaluation and/or fixed-step advancement;
- immutable field resources or samplers for snapshots;
- probe sampling with validity/error information; and
- diagnostics such as stability limits, residuals, divergence error, and energy.

Future particle-coupled field plugins may additionally declare non-field
observation schemas. These must be typed and versioned; encoding a trajectory or
energy history as an invented spatial field merely to reuse the current
Interface is not acceptable.

The host owns GPU device/queue access and resource budgets. Plugins request
capabilities and publish opaque, typed field resources; they do not own the
window or present frames. Generic renderers operate on declared channel layouts,
while a plugin may later contribute an optional specialized visualization.

The Maxwell plugin exercises this rule with a backend factory: its CPU `f64`
oracle is the default, while the desktop injects a `wgpu f32` implementation
using the host's existing device and queue. Yee state stays in storage buffers
between ticks, but complete results are asynchronously read back and published
as ordinary snapshot columns. A session cancellation token interrupts pending
GPU completion during compute-worker shutdown. Renderer code never sees solver
buffers, and CPU/GPU backends expose identical plugin metadata and channels.
Both backends accept the same charge-constrained or prescribed-wave initial
state through that factory.

The first-party contract is expressed as Rust traits and serializable data types.
It should avoid leaking `egui`, `winit`, or application state into plugin crates.

### Visualization engine

The visualization engine does not solve equations. It renders world geometry and
samples a field snapshot through layers such as:

- vector glyphs with independently configurable density and scale;
- magnitude colour maps on arbitrary slice planes, with presentation density
  stored independently for each plane;
- contours for scalar channels;
- seeded streamlines; and
- sparse 3D glyphs or streamlines for a whole-domain view.

Displaying a glyph at every numerical cell is neither legible nor generally
affordable. Sampling density is a visualization setting separate from simulation
resolution. Volume rendering can be added for suitable scalar channels after the
core layer API is proven.

Slice-plane vector glyphs project values into the plane by default so a 2D view
does not imply depth it cannot depict. A per-plane full-3D mode exposes the
normal component when that is useful. These density and projection choices are
visualizer-owned presentation state; they do not change the simulation or the
field values published by local or remote compute.

Each vector channel owns an independent presentation layer, including plane
visibility/density/projection and sparse whole-domain glyph visibility. Several
layers may be active at once; this is how coupled `E` and `B` are compared
without a renderer that understands Maxwell's equations.

Plane display density is a non-negative glyph/mesh count per axis. The
visualizer distributes those presentation samples uniformly across the complete
plane and bilinearly interpolates immutable snapshot columns when the requested
density differs from the published lattice. This avoids clustered integer-index
decimation and allows display density above the transport sampling density
without claiming additional solver accuracy.

The analytic electrostatic model evaluates requested plane samples beyond the
finite grid domain used by future numerical representations: Coulomb's law is
defined throughout space except at the explicitly excluded point-source radius.
Grid-backed equation systems may instead report `OutsideDomain` validity.

The initial camera is perspective with orbit, pan, dolly/zoom, focus-selection,
and view-axis shortcuts. Input bindings will be remappable. Scene grid, world
XYZ axes, and field sampling grids are distinct, independently visible layers.

The viewport also has an authoring layer independent of field rendering. Charges
use spherical source proxies and slice planes use selectable translucent
rectangles with solid corner tabs. Objects, probes, and planes share one selected
entity language: selection highlighting, three wire circles marking the origin,
world-axis arrows, and world-plane translation squares. A drag is constrained by
the chosen handle for its entire duration; dragging the selected entity's body
instead moves parallel to the camera view plane while retaining depth. Body
dragging starts only after the picking ray intersects that entity's rendered
proxy, so empty-space drags cannot move it.

A selected slice plane also draws its normal from the plane centre as a dashed
purple arrow, proportional to the plane extent and labelled `N` in screen space.
Dragging the outer normal handle uses a virtual 3D arcball to rotate the normal
while carrying the plane's in-plane `u` orientation with it. The dashed colour,
label, and outer-only hit region distinguish rotation from solid RGB translation
axes and from sampled field-vector arrows.

### Probes

A probe contains a world-space position or object attachment, a list of recorded
channel IDs, and a bounded time-series buffer. Each sample records simulation
time, snapshot revision, value, units, and validity. The selected-probe inspector
keeps its inline history, while any probe can pin a persistent, non-blocking
floating recorder window. A window remains visible while the user selects and
moves other scene entities and can show several recorded channels as separate,
unit-safe plots. GPU-only fields may require asynchronous readback; the UI shows
that latency rather than labelling a stale sample as current.

Point probes are the implemented geometry. A later spatial probe will add an
explicit region and reduction/statistic to the sample schema; its plotted
series will still use this recorder/history contract rather than expose solver
storage to the UI.

## Technology direction

| Concern | Proposed choice | Reason |
| --- | --- | --- |
| Language | Rust 2024 edition | Strong modelling, predictable native performance, and one language across application and first-party plugins. |
| Window/input | `winit` | Cross-platform native event loop without committing to a game engine. |
| Render/compute | `wgpu` + WGSL | One abstraction over Vulkan, Metal, Direct3D 12, and compatible fallback backends; supports compute and rendering with shared GPU resources. |
| UI | `egui-winit` + `egui-wgpu` | Immediate-mode tools UI with direct integration into the selected event and render stack. |
| Math | `glam` | GPU-friendly vector, matrix, and quaternion types. |
| Data | `serde` with a versioned project schema | Explicit persistence boundary and test fixtures. The on-disk format is selected later. |
| Diagnostics | `tracing` plus in-app metrics | Structured CPU spans and visible solver/render timings. |

We will not start with a full game engine or ECS. The product needs a controlled
render graph, scientific data resources, and a modest object model; adding an ECS
before those access patterns are known would make the first plugin contract less
clear. This can be revisited if object and system counts justify it.

## Plugin delivery strategy

There are two separate requirements:

1. **Architectural plugins:** independently testable equation systems behind a
   narrow contract.
2. **Runtime plugins:** packages installed without rebuilding the application.

The first is required immediately; the second is deferred. Initial plugins live
as crates in the repository so interfaces can change safely. After electrostatics
and electromagnetism exercise the contract, a design spike will evaluate the
WebAssembly Component Model for portable control code plus declarative,
host-validated WGSL kernels. Native Rust dynamic libraries are not the default
because Rust does not promise a stable ABI and an in-process plugin can crash or
compromise the host.

## Invariants

- A rendered value is attributable to a world revision, plugin version,
  simulation time, domain, and numerical configuration.
- A research result is attributable to a reproducible experiment definition,
  solver Implementation and version, precision, execution configuration, and
  recorded interventions.
- Local and remote data sources publish the same snapshot semantics; no renderer
  depends on direct solver memory.
- Snapshot chunks from different identities are never combined, and incomplete
  remote data is visibly identified.
- Non-field observations retain the same authoritative experiment/run identity,
  completeness, provenance, and unit discipline as field snapshots.
- Simulation time advances only through accepted fixed ticks.
- Time-step input is normalized to seconds at the command boundary; the desktop
  accepts scientific notation and explicit SI time-unit suffixes without
  changing the authoritative clock representation.
- UI and rendering never mutate solver state directly; they submit commands.
- Candidate worlds, subscriptions, and numerical time steps are validated before
  their authoritative values are replaced.
- Solver-produced object motion has one declared owner per object and reaches
  the canonical world only through the runtime's validated kinematic outcome
  Interface.
- A field channel has explicit dimensional units and scalar/vector shape.
- Visualization sampling density does not alter simulation resolution.
- Presentation subscriptions are bounded by authoritative source budgets, not
  only by widget ranges.
- Plugins do not depend on the UI or own the presentation surface.
- A plugin failure is reported with context and must not corrupt the saved world.
- Static analytic cases remain available as regression oracles for numerical
  implementations.

## Initial non-goals

- claiming research-grade validity outside the explicitly tested regime of a
  solver;
- claiming that a proton/electron arrangement includes unmodelled effects, or
  silently adding species-specific forces to make it stable;
- solid modelling, parametric constraints, or a general-purpose CAD kernel;
- conductors, dielectrics, and arbitrary material meshes in the first solver;
- relativistically correct moving particles in the electrostatic milestone;
- unrestricted third-party native code plugins;
- networking or collaborative editing; and
- hiding stability, resolution, or boundary-condition choices from the user.
