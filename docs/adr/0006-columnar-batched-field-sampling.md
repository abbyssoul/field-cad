# 0006 — Field values are sampled in columnar batches over a declared geometry

Status: **accepted** (Milestone 2 review gate)

## Context

The first plugin contract offered:

```rust
fn sample(&self, channel: &ChannelId, position: DVec3) -> Result<FieldValue, PluginError>;
```

One point, one channel, one virtual call, one `Result`. It fitted the test
fixture, which sampled a handful of probes.

Milestone 3 does not sample a handful of probes. A magnitude colour map on a
slice plane is tens of thousands of samples per frame, and a decimated
whole-domain glyph field is more. At that scale the signature has three problems:

- `ChannelId` is a plugin-namespaced `String`. The test plugin compared it by
  *constructing two heap strings per sample* — allocation in the hottest loop.
- Every returned `FieldValue` re-wraps a `Dimension` the channel already
  declares, and the runtime re-validated it per value.
- A per-point virtual call cannot be handed to a GPU, batched, or chunked over a
  network. Milestone 3 requires all three.

## Decision

Sample a whole batch at once, against a geometry that describes positions
implicitly:

```rust
fn sample(&self, channel: ChannelHandle, geometry: &SampleGeometry)
    -> Result<SampledColumn, PluginError>;
```

- `SampleGeometry` is `Probes { ids, positions }`, `Plane(PlaneLattice)`, or
  `Grid(GridLattice)`. A plane or grid batch carries three vectors and a count
  instead of one position per cell.
- `SampledColumn` is a `FieldColumn` — `Arc<[f64]>` or `Arc<[DVec3]>` — plus one
  `SampleValidity` per element. The dimension is carried once, by the channel.
- `ChannelHandle` is an index into the plugin's declared channel list, resolved
  once. `ChannelId` remains the identity for serialization, persistence, and UI.
- Shape and length are checked **once per batch** by the runtime, not once per
  value.

`SampleValidity` is part of this decision, not an addition to it. A point-source
field is undefined inside its source radius, and a value read off a grid is
interpolated rather than evaluated. Both must be reported per sample or the
renderer will draw a singularity as though it were a measurement.

## Consequences

- The columnar layout is what a `wgpu` buffer upload and a network chunk both
  want. No repacking at either boundary.
- Plugin authors must keep `channels()` order in sync with their handle
  constants. Mitigated by asserting the correspondence in a plugin's own tests;
  the test plugin demonstrates the pattern.
- `Quantity`/`FieldValue` remain the display- and property-facing types, and
  `FieldBatch::sample` reconstructs one on demand. Dimensional safety lives at
  the boundary; the interior is numbers.
- Sampling density is a `Subscription`, entirely separate from the `Domain`. That
  separation is what makes "visualization density does not change the physical
  result" a testable claim rather than a convention.
