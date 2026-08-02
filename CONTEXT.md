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

The decisions that shaped the code — and the reasoning behind them — are recorded
as ADRs in `docs/adr/`. Where this document describes intent, an ADR describes a
commitment and what it cost.

This document defines the product language and the boundaries that should remain
stable as the implementation grows. It describes intent rather than a frozen
Rust API.

## Purpose

Field CAD is an interactive laboratory for spatial fields. A user constructs a
world, chooses an equation system, observes its fields in 3D or on arbitrary
planes, and inspects how values evolve over time.

The application should serve two modes without creating two architectures:

1. **Inspection:** evaluate a static or analytic field after an edit.
2. **Simulation:** advance numerical state using a fixed time step and display
   successive field snapshots.

The emphasis is scientific legibility. A result must retain enough provenance to
say which model, parameters, domain, resolution, time, and solver produced it.

## Ubiquitous language

| Term | Meaning |
| --- | --- |
| **World** | The user-authored objects, common transforms and velocities, attached plugin properties, probes, and visualization planes. |
| **Object** | An identifiable entity with a transform and optional shape, velocity, and plugin-defined components such as charge or mass. |
| **Equation system** | A physical model and its coupled equations, such as electrostatics or electromagnetism. |
| **Equation-system plugin** | A module that declares fields and object properties, validates configuration, and evaluates or advances its equation system. |
| **Field channel** | One observable scalar or vector output of a plugin, such as electric field `E`, magnetic field `B`, potential, or an error residual. |
| **Domain** | The finite 3D region over which a numerical field is represented, including resolution and boundary conditions. |
| **Field snapshot** | Immutable, versioned solver output for a particular simulation time and world revision. |
| **Field data source** | A local runtime or remote compute session that accepts commands and publishes field snapshots through the same application-facing contract. |
| **Subscription** | The channels, spatial region, representation, and level of detail that a visualizer currently asks a data source to publish. Purely a visualization concern: it never changes a computed value. |
| **Sample geometry** | Where a batch of field values was taken — probe points, a lattice on a slice plane, or a lattice over the domain. Described once per batch rather than stored per sample. |
| **Sample validity** | Whether a returned value was evaluated exactly, interpolated from stored samples, or is undefined — inside a source radius, outside the domain, or unconverged. |
| **Visualization layer** | A view of one field channel using glyphs, colour, contours, streamlines, or another generic rendering technique. |
| **Slice plane** | A transformable plane on which a field is sampled and drawn. The XY plane is only a default. |
| **Probe** | A point, optionally attached to an object, that samples selected channels and records time-series values. |
| **Simulation tick** | One deterministic, fixed-duration advance of authoritative simulation state. |
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
conservation. Prescribed sources come before a particle-in-cell implementation.

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

Plugin-specific values are attachments to world objects. The electric plugin can
add `charge`; a future gravity plugin can add `mass` to the same object. A plugin
does not privately own the canonical object transform.

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

### Local and remote compute boundary

The desktop application talks to a field data source rather than directly to a
solver. In local mode an adapter wraps the in-process simulation runtime. In
remote mode a client sends commands and subscriptions to a dedicated compute
service and assembles streamed snapshot chunks. Both modes expose the same world
revision, simulation time, channels, validity, diagnostics, and connection state
to the rest of the application.

When compute is remote, the service is authoritative for the world revision and
simulation clock. The desktop may stage edits for responsiveness, but it does not
claim that they took effect until the service acknowledges the resulting
revision. Play, pause, and step are commands with correlated acknowledgements;
they are not inferred from incoming frame timing.

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
- initialization from a domain and a read-only world view;
- static evaluation and/or fixed-step advancement;
- immutable field resources or samplers for snapshots;
- probe sampling with validity/error information; and
- diagnostics such as stability limits, residuals, divergence error, and energy.

The host owns GPU device/queue access and resource budgets. Plugins request
capabilities and publish opaque, typed field resources; they do not own the
window or present frames. Generic renderers operate on declared channel layouts,
while a plugin may later contribute an optional specialized visualization.

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
use spherical source proxies, slice planes use selectable translucent rectangles
with solid corner tabs, and selected charges expose axis and plane translation
handles. A drag is constrained by the chosen handle for its entire duration;
dragging the selected object's body instead moves parallel to the camera view
plane while retaining depth. Body dragging starts only after the picking ray
intersects that object's rendered proxy, so empty-space drags cannot move it.

### Probes

A probe contains a world-space position or object attachment, a list of channel
IDs, and a bounded time-series buffer. Each sample records simulation time,
snapshot revision, value, units, and validity. GPU-only fields may require
asynchronous readback; the UI must show that latency rather than label a stale
sample as current.

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
- Local and remote data sources publish the same snapshot semantics; no renderer
  depends on direct solver memory.
- Snapshot chunks from different identities are never combined, and incomplete
  remote data is visibly identified.
- Simulation time advances only through accepted fixed ticks.
- Time-step input is normalized to seconds at the command boundary; the desktop
  accepts scientific notation and explicit SI time-unit suffixes without
  changing the authoritative clock representation.
- UI and rendering never mutate solver state directly; they submit commands.
- A field channel has explicit dimensional units and scalar/vector shape.
- Visualization sampling density does not alter simulation resolution.
- Plugins do not depend on the UI or own the presentation surface.
- A plugin failure is reported with context and must not corrupt the saved world.
- Static analytic cases remain available as regression oracles for numerical
  implementations.

## Initial non-goals

- research-grade error guarantees for arbitrary geometries;
- solid modelling, parametric constraints, or a general-purpose CAD kernel;
- conductors, dielectrics, and arbitrary material meshes in the first solver;
- relativistically correct moving particles in the electrostatic milestone;
- unrestricted third-party native code plugins;
- networking or collaborative editing; and
- hiding stability, resolution, or boundary-condition choices from the user.
