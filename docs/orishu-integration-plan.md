# Field CAD × Orishu integration plan

Status: proposed.  
Date: 2026-08-04.

## Decision in one sentence

Field CAD should become the authoring and visual-analysis client for an Orishu
workload, while Orishu remains the authoritative distributed runtime for the
loaded workload, its partitioned state, committed simulation boundaries,
checkpoints, result artifacts, and provenance.

The first deliverable is therefore **not** a remote Field CAD solver. It is a
versioned, content-addressable Field CAD initial-conditions artifact plus an
Orishu workload manifest exporter that references it. This forces the two
projects to agree on the input state before they attempt live playback.

## Evidence and boundary correction

Field CAD already has the right architectural seam: desktop code consumes a
transport-neutral `FieldDataSource`; local and loopback sources accept commands
and publish immutable, revisioned snapshots. A remote Orishu-backed source can
implement that trait without changing authoring or rendering consumers.

Orishu distinguishes two network planes:

| Plane | Purpose | Field CAD’s role |
| --- | --- | --- |
| Client plane — `docs/protocol-client.md` | HTTP/3 over QUIC, CBOR, workload control, result retrieval, and live observation | **Use this.** Field CAD is a privileged remote client for authoring/submission/control and a read-only observer where appropriate. |
| Peer plane — `docs/protocol-p2p.md` | raw QUIC/CBOR between workers: halo exchange, `StepVote`/`StepCommit`, checkpoints, gossip, membership | **Do not use this directly.** Field CAD neither joins the cluster nor participates in a distributed simulation boundary. |

The request names `protocol-p2p.md` as the client protocol. That document and
Orishu’s architecture explicitly classify it as worker-to-worker. Reusing it
would make a UI responsible for node identity, membership, partition ownership,
and hostile-peer controls. The integration must instead extend Orishu’s client
protocol; internally, Orishu will continue to use P2P to execute the submitted
workload.

## Target ownership and data flow

```text
Field CAD authoring model
  objects + components + domain + field model + run intent
             |
             | export / validate
             v
Field-CAD initial-conditions artifact  <-- content hash --> Orishu workload manifest
             |                                             |
             +-------------- upload/reference ------------+
                                                           v
                                               Orishu client API (HTTP/3 + CBOR)
                                                           |
                   query / live state                      v
Field CAD <--------------------------------- Orishu runtime / workload ABI
                                                | P2P: partitions, halos, commits
                                                v
                                  checkpoints + immutable result artifacts
```

### Ownership rules

- **Before a workload is loaded**, Field CAD is authoritative for its authored
  scene. Edits are Field CAD world transactions, with its own revision/history.
- **A submitted workload is immutable input.** Orishu’s workload manifest and
  referenced initial-conditions artifact identify the run. Editing the local
  Field CAD scene after export does not mutate a loaded workload.
- **During and after a run**, Orishu is authoritative for simulation time,
  committed state, cluster diagnostics, checkpoints, results, and runtime
  provenance. Field CAD is an observer and controller through the client API.
- **To alter physics or initial state**, Field CAD exports a new artifact and
  submits a replacement workload (or a new run from an explicit checkpoint).
  Do not initially attempt live scene edits against a running distributed
  workload; Orishu’s current contract has no transactional intervention model.
- **Camera, selection, layer styling, panel layout, and sampling preferences**
  remain per-Field-CAD-client state. They are never part of a workload’s
  physical initial conditions.

This deliberately differs from Field CAD’s local interactive runtime, which
can queue authored edits before a tick. That is a useful local workbench
semantics, but it must not be silently projected onto an Orishu run until
Orishu defines distributed intervention semantics.

## File-format first: the initial-conditions contract

### Why the current Orishu field is insufficient

Today, Orishu’s `spec.inputs.initialConditions` is only
`{ image: { uri | data } }`. It identifies bytes but says none of the following:

- artifact media type, schema/version, canonical encoding, or content hash;
- coordinate system, units, or whether values are cell-, face-, edge-, or
  particle-centred;
- global versus per-partition layout and how a worker extracts its portion;
- field-channel names, component ordering, numeric precision, or endianness;
- object/source definitions versus already materialised grid state;
- boundary and initial-condition interpretation;
- which model/ABI consumes the data, nor compatibility validation;
- provenance linking the scene revision/exporter/catalog to the run.

`wl_load_partition(partition_geometry, initial_state)` in
`protocol-workload.md` assumes exactly this missing contract. The first shared
artifact format closes that gap.

### Scope of version 1

Version 1 supports a structured Cartesian electromagnetic workload and is
deliberately narrow:

- a 3-D axis-aligned global domain in SI metres;
- uniform integer cell resolution;
- explicit per-axis boundary conditions;
- an initial state suitable for a chosen Maxwell/FDTD workload package;
- authored point/sphere sources and particles *only when their meaning is
  declared by that workload package*;
- grid state encoded in the staggering/layout required by that package;
- a deterministic, partition-independent global representation which Orishu
  slices into partitions for `wl_load_partition`.

It does **not** claim that every current Field CAD plugin, analytic field,
visualization plane, probe, or generic component is directly executable by
Orishu. Export should reject unsupported scene/model combinations with a
structured report rather than approximate them silently.

### Two artifacts, not one overloaded “scene” file

Separate durable authoring intent from executable initial state:

1. **Field CAD scene document** — a versioned document preserving editable
   objects, components, instrument definitions, Field CAD-specific model/view
   metadata, and source provenance. This is the round-trip file Field CAD
   opens and saves.
2. **Orishu initial-conditions artifact** — a versioned, immutable compilation
   product of a particular scene revision plus export profile. It contains the
   globally defined initial state that a specific Orishu workload package loads.

The manifest points at (2), not (1). This protects reproducibility: a human can
edit a scene document after export without retroactively changing a run, and an
Orishu worker never has to understand desktop-only concepts.

### Proposed initial-conditions artifact envelope

Use canonical CBOR for the machine artifact
(`application/vnd.fieldcad.orishu-initial-conditions+cbor;version=1`). A diagnostic JSON representation and a
human-readable YAML manifest may be supplied by tooling, but they are not the
content-hashed canonical form. Large arrays belong in length-delimited binary
sections/chunks referenced by the CBOR index rather than one gigantic CBOR
value.

```text
FieldCadOrishuInitialConditionsV1
  format:           "fieldcad.orishu.initial-conditions/v1"
  canonicalization: "cbor-rfc8949-deterministic"
  producer:
    fieldcadVersion, sceneDocumentHash, worldRevision, exportedAt,
    exporterVersion
  compatibility:
    domainType: "electromagnetic"
    workloadModel: { uri, contentHash, runtimeEngine, runtimeAbiVersion }
    exportProfile: { id, version, stateLayout }
  spatial:
    coordinateSystem: "right-handed, metres"
    bounds: { min: [f64; 3], max: [f64; 3] }
    cells: [u32; 3]
    spacing: [f64; 3]
    boundaries: { xMinus, xPlus, yMinus, yPlus, zMinus, zPlus }
  temporal:
    initialTimeSeconds: f64                 # normally 0
    timeStepSeconds: f64
  numeric:
    scalarType: "f64" | "f32"
    byteOrder: "little-endian"
  stateLayout:
    kind: "yee-fdtd-v1"                    # profile-defined, never implicit
    channels: [ChannelLayout, ...]          # E/B offsets, extents, centring
    particles: optional ParticleTableLayout
  sourceIntent: optional [SourceIntent]      # provenance/validation; no magic
  payloadIndex: [PayloadChunk]               # offsets, lengths, SHA-256 hashes
  integrity:
    artifactHash: SHA-256(canonical artifact)
```

`ChannelLayout` must state the physical channel ID, SI dimension/unit, vector
component, centring (cell/face/edge/node/particle), global index extent,
strides/order, and payload reference. `SourceIntent` has a schema ID and
version, stable source ID, transform, shape, properties with units, and a
declared role such as `fixed-source` or `dynamic-particle`. It is not a licence
for a worker to invent an initial grid: the selected `exportProfile` says
whether and how intent is compiled to state.

The artifact must be independent of Orishu’s temporary partition map. Orishu
owns partitioning and extracts each partition’s state plus halo-relevant
boundary representation on load. This preserves rebalancing and late join.

### Workload-manifest extension

Keep `ExternalResource` as the transport location, but add a typed descriptor
so a worker can reject an incompatible input before instantiation. Proposed
shape (names to be ratified in Orishu):

```yaml
apiVersion: orishu.dev/v1
kind: Workload
metadata:
  name: fieldcad-maxwell-example
  labels:
    authored-by: fieldcad
spec:
  domainType: electromagnetic
  model:
    image:
      uri: oci://example.org/orishu/maxwell-fdtd@sha256:...
  domain:                                  # copied/verified from export profile
    dimensions: 3
    bounds: [10 m, 10 m, 10 m]
    discretization: { space: [0.1 m, 0.1 m, 0.1 m], time: 1e-10 s }
  inputs:
    initialConditions:
      artifact:
        uri: file:///.../initial.fcic
        mediaType: application/vnd.fieldcad.orishu-initial-conditions+cbor;version=1
        contentHash: sha256:...
        format: fieldcad.orishu.initial-conditions/v1
        exportProfile: fieldcad.maxwell.yee/v1
```

The normal Orishu `DomainSpec` remains integral: bounds, discretization, time
step, execution requirements, and model reference belong to the workload
manifest. The exporter must prove that its artifact matches the manifest’s
domain and `dt`; it must not treat those as duplicated, independently editable
settings. In V1, Field CAD authors them in a single export profile and emits
both matching values.

**Orishu discrepancy to resolve:** `workload.md` describes total duration, but
the current Rust `DomainDiscretization` encodes spatial discretization and time
step only; neither it nor `WorkloadStatus` currently models a requested end
time/step count. Decide whether duration is a workload termination policy
(`maxSteps`/`endTime`) or only an operator step budget, then specify it and
include it in the export profile if it is physical run intent.

### Field CAD scene document v1

Create a separate `fieldcad.scene/v1` document before building the Orishu
exporter. It should serialise only durable authored state:

- document metadata and format version;
- complete domain and numerical intent (bounds, resolution, boundaries,
  precision, `dt`);
- enabled field systems, selected models, plugin/configuration schemas and
  values with versions;
- objects, stable IDs, names, transforms, velocity, shape, pinning, visibility,
  components, catalogue/template provenance;
- probes and slice planes as authoring/measurement metadata;
- explicit resource references and hashes, never embedded mutable UI state;
- optional named view documents separately, not part of simulation semantics.

The loader performs migration into a candidate world/experiment and validates it
through the existing command/plugin path before adoption. It must not deserialise
private runtime structures or revive an old snapshot as current solver state.

## Mapping: Field CAD to the first Orishu workload

| Field CAD concept | First Orishu representation | Rule |
| --- | --- | --- |
| `DomainBounds`, `Resolution`, boundaries, precision | `spec.domain` plus typed initial-conditions spatial/numeric metadata | Exact equality after SI normalization; mismatch is an export/load error. |
| `TimeStep` | `spec.domain.discretization.time` and artifact temporal metadata | Exact value after canonical SI encoding; Orishu validates stability/profile. |
| Selected Maxwell field model and configuration | model image digest + `exportProfile` + requirements/execution profile | Exporter supports an allow-listed profile/version pair only. |
| Static charge/source components | source intent plus a profile-generated initial grid | Must declare treatment of periodicity, neutralising background, singular radius, and seam validity. |
| Mass + charge, unpinned object | particle records plus required grid/particle initial state | IDs and SI mass/charge/position/velocity preserved; pusher/deposition/interpolation are workload-profile semantics. |
| Pinned object | fixed source or prescribed-motion record | V1 should support fixed state only; time-varying prescribed motion needs a later intervention/trajectory contract. |
| Electrostatic analytic model | unsupported in V1 | It is a Field CAD inspection model, not automatically a partitioned Orishu workload. |
| Slice plane/probe | Field CAD client-side observation request | Never part of initial state; may be preserved in scene document or translated to a query specification later. |
| Field snapshot | Orishu query-state/result view translated to Field CAD snapshot | Must retain committed boundary, world/export hash, workload/epoch, layout, units, validity, and precision. |

## Shared Maxwell CPU kernel and boundary semantics

The existing `fieldcad-electromagnetism` CPU `f64` reference is the preferred
first Orishu workload implementation. Sharing it gives the integration a much
stronger correctness oracle: for a supported one-partition workload, Field CAD
local execution and Orishu execution use the same numerical kernel, initial
state, `dt`, and profile; their independently produced observations should
match within the declared determinism envelope. Multi-partition tests then show
that partitioning changes placement of computation, not the result.

Do not extract the current desktop `wgpu` backend as V1 workload code. It is a
host-injected implementation tied to a graphics device and currently relies on
CPU readback for part of coupled particle work. It can later become a separately
declared accelerator execution profile. The reusable V1 implementation is the
headless CPU kernel: Yee state/layout, updates, particle coupling, deposition,
interpolation, diagnostics, initialization, and sampling reconstruction.

### Halos are not physical boundaries

No: an Orishu halo is normally an **internal partition interface**, whereas a
Field CAD boundary condition is a **physical outer-domain condition**.

| Face of a partition | Data used by a partition-local kernel | Meaning |
| --- | --- | --- |
| Adjacent to another partition | Halo copied from that neighbour’s committed state | Artificial decomposition boundary; it must be mathematically invisible. |
| Global domain edge with periodic boundary | Halo supplied from the opposite global edge | Physical periodicity; the wrap is intentional. |
| Global domain edge with Dirichlet, Neumann, absorbing, or open boundary | Profile-defined ghost/boundary update; no imaginary neighbour partition | Physical model boundary, whose policy affects the solution. |

The current Field CAD Maxwell CPU reference supports **periodic boundaries
only** and rejects other `BoundaryCondition` values. Its periodic indexing makes
the one-grid outer edges neighbours of one another. Splitting that grid across
Orishu partitions must replace only the *internal* wraps with halos from the
adjacent partition; global outer faces retain the existing periodic wrap. The
static isolated-charge initialization has an additional documented validity
seam: it is not a different boundary condition, but a sampling/diagnostic
limitation caused by representing a non-periodic Coulomb potential on this
periodic lattice.

The extracted kernel needs an explicit boundary-input abstraction, for example
`NeighbourState { halo: Interior | PeriodicWrap | PhysicalBoundary }`, rather
than direct global-array wrapping. The Orishu adapter supplies `Interior` halos
from the runtime and identifies global faces; the selected Maxwell profile owns
the physical-boundary update. This is also a gap in Orishu’s current generic
halo wording: its workload contract describes neighbour halo data but must say
how a workload declares and receives behaviour at global boundary faces.

### Extraction plan

1. Move the CPU numerical core into a headless `fieldcad-maxwell-kernel` crate
   with no UI, `wgpu`, Field CAD runtime, plugin trait, or Orishu transport
   dependency. Keep one global-grid adapter temporarily so existing Field CAD
   CPU-reference tests remain the baseline.
2. Define stable kernel DTOs for global/partition geometry, Yee layout, typed
   state buffers, particle state, halo depth/layout, physical-boundary policy,
   one-step outcome, diagnostics, checkpoint encoding, and query views. They
   must be compatible with the selected initial-conditions export profile.
3. Add an Orishu workload adapter which implements `wl_init`,
   `wl_load_partition`, `wl_step`, `wl_checkpoint`, `wl_restore_checkpoint`,
   and `wl_query_state` by calling the kernel exactly once per runtime request.
   The adapter never owns its own time loop, partition map, peer connection, or
   wall-clock pacing.
4. Prove equivalence in increasing scope: global Field CAD reference versus
   one-partition Orishu kernel; one partition versus multiple partitions;
   uninterrupted run versus checkpoint/resume/rebalance; and CPU reference
   versus any later accelerator profile.

## Phased delivery plan

### Phase 0 — agree contracts before code

1. Review this plan in both repositories and record two joint ADRs/specs:
   `fieldcad.scene/v1` ownership and `fieldcad.orishu.initial-conditions/v1`.
2. Choose one reference workload package: Maxwell/Yee, one ABI version, one
   numeric precision, periodic-domain policy, and one particle/source profile.
   Record the existing Field CAD CPU `f64` reference as the shared numerical
   kernel and define its partition/halo boundary contract before extraction.
3. Resolve initial-state authority: whether Field CAD compiles object intent to
   grid/particle bytes, or the workload package does so deterministically from
   an intent payload. For V1, prefer Field CAD compilation plus a workload
   validator, because the byte layout entering `wl_load_partition` is explicit.
4. Define canonical CBOR, hashing scope, artifact media type, schema evolution,
   source-ID rules, and maximum resource sizes.
5. Specify end-time/maximum-step semantics in Orishu.

Exit: both sides can validate a small golden artifact without a desktop or
cluster; no field in the initial state is left to informal interpretation.

### Phase 1 — Field CAD document model and offline export

Field CAD changes:

1. Add a headless `fieldcad-document` crate (or equivalent focused module)
   owning public document DTOs, migrations, canonical serialization, validation
   reports, and hashes. Do not serialise `WorldSnapshot` internals directly.
2. Add a scene document importer/exporter that reconstructs world changes as
   validated transactions, preserving IDs and provenance where permitted.
3. Add an `OrishuExportProfile` registry to the Maxwell plugin boundary. Each
   profile declares supported components, numerical layout, constraints, and
   deterministic lowering rules.
4. Implement exporter validation with field paths and actionable failures:
   unselected model, unsupported component, invalid domain, incompatible
   boundaries, unstable `dt`, unknown plugin/catalog version, oversized grid,
   non-representable value.
5. Implement canonical initial-conditions artifact generation, content hashing,
   and manifest generation. Export writes atomically and returns the scene hash,
   artifact hash, selected profile, and a generated manifest.
6. Add a CLI first (`fieldcad export-orishu scene.fcscene --profile ...`) and
   then a desktop export/validation workflow. File-first keeps the initial
   integration debuggable and usable without a live cluster.

Tests:

- round-trip scene-document fixtures and migration tests;
- deterministic byte-for-byte export for a fixed scene/profile;
- golden Maxwell artifact decoded by an independent test reader;
- rejection fixtures for each unsupported/ambiguous mapping;
- Field CAD CPU reference versus exported initial grid at declared sample points,
  including the periodic-seam rule.

Exit: a Field CAD-authored Maxwell scene yields a manifest plus hashed initial
conditions file that Orishu can validate and partition deterministically.

### Phase 2 — Orishu workload-input support

Orishu changes (required separately):

1. Replace/extend the untyped `InitialConditionsSpec.image` with a typed
   artifact descriptor: URI, media type, format version, content hash, byte
   size, and optional declared export profile. Retain legacy `image` only as a
   deprecated compatibility form.
2. Add manifest validation that binds `domainType`, dimensions, bounds,
   discretization, `dt`, runtime engine/ABI, and model digest to the input
   descriptor. Require a content hash for remote artifacts under normal policy.
3. Implement a trusted parser/validator for
   `fieldcad.orishu.initial-conditions/v1` before workload instantiation. Treat it as hostile input:
   bounded CBOR nesting/string/array sizes, checked arithmetic for extents and
   offsets, exact hashes, finite values, and allocation limits.
4. Define the artifact-to-partition reader: compute a partition’s global index
   range, fetch only required chunks/ranges where possible, decode the profile
   layout, and pass a self-contained `initial_state` to `wl_load_partition`.
   The reader must work for a late partition assignment without relying on the
   original Field CAD process.
5. Extend the workload contract/profile to distinguish neighbour halos at
   internal partition interfaces from physical global-boundary faces, including
   how a workload declares periodic wrapping and applies any future non-periodic
   policy. The runtime must never substitute an absent neighbour halo for a
   physical boundary condition.
6. Extend workload-package validation so `wl_init` declares whether it accepts
   the profile/layout/precision/boundary combination. A package rejects before
   `Ready`, with a structured compatibility error.
7. Add explicit workload termination semantics (`endTime` or `maxSteps`) if
   accepted in Phase 0, and include it in status/provenance.
7. Add initial-condition provenance to load status, checkpoint records, and
   result records: artifact hash/format/profile, scene document hash, exporter
   version, and the full workload/model identity.

Tests:

- golden Field CAD artifact parser and partition extraction tests (including
  non-zero origin and partitions on every axis);
- corrupted/truncated/oversized/hash-mismatched input rejection;
- manifest/artifact mismatch rejection;
- one-node and multi-node runs reaching identical committed states within the
  declared execution profile;
- global-grid Field CAD CPU reference versus one-partition shared kernel, then
  one-partition versus multi-partition runs, including a partition face on every
  axis and periodic global wrapping;
- checkpoint/rejoin/rebalance proving initial-state bytes do not depend on
  Field CAD availability.

Exit: `PUT /cluster/workload` can load the generated manifest, all eligible
workers validate the same input artifact, and `wl_load_partition` receives the
correct partition state.

### Phase 2b — import supported Orishu workloads into Field CAD

This phase establishes authoring equivalence for the supported intersection of
the two systems. It is deliberately placed after the artifact reader and before
remote playback: Field CAD must be able to independently decode the exact input
that Orishu will load before it tries to display Orishu-produced state.

Field CAD changes:

1. Add `open-orishu-workload` for a local manifest file, and later an equivalent
   operation for a manifest retrieved through Orishu’s client API. Parse the
   manifest using the Orishu-compatible schema, verify its declared model/input
   identities, and resolve its initial-conditions artifact without executing
   the workload.
2. Verify the artifact’s content hash, media type, format, export profile,
   model digest, runtime ABI, domain bounds/discretization, `dt`, precision,
   boundaries, and dimensional units before importing any editable state.
3. Implement profile-owned decoders from a supported global initial-state
   artifact to a Field CAD scene document/world. The V1 Maxwell profile restores
   the domain, model configuration, particles/sources, and any source
   provenance that was retained and validated during export.
4. Distinguish two open modes:
   - **Editable round-trip** — every physical value has an unambiguous Field CAD
     representation under a supported profile. The imported scene can be edited
     and re-exported as a new workload/input artifact.
   - **Inspection-only** — Field CAD can verify and inspect manifest/artifact
     metadata or sampled state, but cannot faithfully reconstruct its authoring
     intent. The document is read-only and labels unsupported models, state
     layouts, components, or lossy fields explicitly.
5. Preserve the original manifest, input hash, model identity, workload UID
   (when present), and import report in scene provenance. Saving an editable
   scene must create a Field CAD scene document; it must never overwrite an
   immutable Orishu artifact in place.

Orishu changes (required separately):

1. Make workload manifests and initial-condition artifacts retrievable through
   the client API with their canonical bytes, media type, content hash, and
   compatibility/profile metadata. A Field CAD client must not scrape worker
   storage or use P2P chunk transfer.
2. Publish/maintain profile specifications and golden artifacts independently
   of Field CAD so import compatibility is testable by both projects.

Tests:

- Field CAD scene → V1 manifest/artifact → Field CAD import produces an
  equivalent editable scene (allowing only documented generated metadata such
  as new document IDs and timestamps).
- A re-export of the imported scene produces byte-identical canonical initial
  conditions when the profile and exporter version are unchanged.
- Unsupported but structurally valid Orishu workloads open inspection-only;
  corrupt, incompatible, or hash-mismatched inputs are rejected before display.
- Importing a partitioned/checkpoint result is not treated as importing initial
  authoring state; it remains a result/inspection workflow until a separate
  checkpoint-to-scene contract exists.

Exit: for every supported profile, Field CAD can round-trip the authored
workload representation:

```text
Field CAD scene ⇄ Orishu workload manifest + initial-conditions artifact
```

This is equivalence of *authoring representation*, not of runtime authority:
Orishu still owns distributed execution, committed state, and checkpoint
semantics once the workload is loaded.

### Phase 3 — submission and playback through the client plane

Field CAD changes:

1. Add `fieldcad-orishu-client`, implementing a remote `FieldDataSource` adapter
   over Orishu’s `ClusterApi`/HTTP client. Keep HTTP/3/CBOR and credentials out
   of `fieldcad-core` and the renderer.
2. Provide explicit UI/API operations: configure endpoint/authentication,
   preflight compatibility check, upload/publish or reference artifacts, submit
   manifest, load/replace/unload, start, stop, step N boundaries, and reset to
   checkpoint.
3. Map Orishu `Ready`/`Running`/`Stopped`/`Error`, current committed step/time,
   workload epoch, compatibility errors, and cluster metrics to an expanded
   Field CAD remote status. Do not pretend that Orishu has Field CAD’s local
   queued-edit or undo semantics.
4. Subscribe to Orishu’s workload event stream; reconnect from a full state
   snapshot and discard stale/out-of-epoch deltas.
5. Initially show status and diagnostics in Field CAD even before field payloads
   are rendered. This validates authentication, lifecycle, and reconnection
   separately from data-layout translation.

Orishu changes (required separately):

1. Ensure the documented client API is implemented for workload `check`, load,
   run/stop/step/reset/unload and emits machine-readable compatibility/load
   errors with the field/worker that rejected them.
2. Make the stream’s snapshot/delta schema normative and include workload UID,
   epoch, committed step, simulation time, manifest ETag/content hash, and
   structured diagnostics. This is required for Field CAD to reject stale data.
3. Define artifact publication/upload on the client plane. A URI alone is not a
   usable workflow for a desktop authoring client: add either an authenticated,
   size-limited resumable upload endpoint returning a content-addressed URI, or
   a pre-signed external-store flow. Verify the supplied hash after upload.
4. Expose checkpoint/result listing, metadata, and download through the same
   client abstraction, preserving content-hash and availability status.

Exit: Field CAD can submit a V1 artifact, control a run, survive a stream
reconnect, and accurately show authoritative lifecycle/provenance.

### Phase 4 — field/result observation adapter

1. Define an Orishu **state-query schema**, separate from the generic event
   stream: requested profile channels, spatial box/plane/points, decimation,
   maximum bytes, representation, and desired committed boundary.
2. Have Orishu pass an approved query to `wl_query_state`, assemble
partition-aligned responses, and return/stream typed field batches with global
geometry, layout, units, precision, validity, workload UID/epoch, committed
step/time, and source artifact/model hashes.
3. Translate these batches to `FieldSnapshot` in `fieldcad-orishu-client`.
   Retain all raw provenance/validity; no fabricated interpolation across a
   partition seam or unreported downsampling.
4. Map Field CAD probes and slice planes to queries/subscriptions. Treat them
   as client observations subject to Orishu budgets, not workload mutations.
5. Retrieve sealed result artifacts for historical inspection and distinguish
   their durable record from their current availability.

Exit: a remote Maxwell run can render a selected plane and probe history in
Field CAD, while values can be traced to a committed Orishu boundary and the
exact exported scene/input artifact.

### Phase 5 — later capabilities (explicitly deferred)

- runtime scene interventions and branchable interactive experiments;
- live collaborative editing/multiple Field CAD authoring clients;
- generic plugin/component lowering across arbitrary scientific domains;
- arbitrary meshes/adaptive grids and non-Cartesian layouts;
- bidirectional import of a running/checkpointed Orishu state into an editable
  Field CAD scene;
- using Field CAD as a workload-code/WASM authoring environment.

Each needs a separate contract. None should be smuggled into the file-format
or playback milestones.

## Orishu-side changes, consolidated

These are the required changes on the Orishu side; Field CAD cannot safely
paper over them:

1. Specify and implement typed, hashed initial-conditions descriptors.
2. Adopt/own the versioned Field CAD initial-conditions format as a supported
   workload-input profile, including secure parsing and partition extraction.
3. Close the duration/termination-policy gap in the workload model.
4. Add canonical profile/model/ABI/boundary/precision compatibility validation
   at manifest load time and surface structured rejection reasons.
5. Implement the documented workload client API and stable lifecycle event
   schema on the HTTP/3 client plane — not the peer protocol.
6. Provide authenticated content-addressed artifact publication or a sanctioned
   external-store reference workflow.
7. Define and implement typed query-state/result-view payloads, query budgets,
   subscriptions, and committed-boundary provenance so a visual client can
   obtain fields rather than opaque result bytes only.
8. Persist source/export provenance in workload status, checkpoint records, and
   result artifact records.

## Compatibility and safety invariants

- A content hash identifies exactly the canonical initial-state bytes used by
  every worker in a workload epoch.
- A worker rejects an artifact before code execution if any structural,
  integrity, profile, manifest, or resource limit check fails.
- Partitioning/rebalancing changes who computes a region, never what initial
  state that region means.
- A Field CAD renderer never presents a remote value without its committed
  boundary, workload epoch, channel/layout, units, precision, and validity.
- A local scene edit never changes an already submitted workload; a remote run
  is changed only through an explicit, auditable Orishu lifecycle operation.
- Field CAD does not hold P2P credentials or impersonate a worker.
- Plugin/profile versions are pinned in exported inputs and workload
  requirements; “best effort” conversion is forbidden.

## Open decisions to resolve in Phase 0

1. Does Orishu standardise one global artifact container with profiles, or does
   each workload package register its own initial-state media type? This plan
   recommends one bounded container plus profile-defined payload layouts.
2. Should source intent be retained in the V1 executable artifact, or only in
   the Field CAD scene document/provenance? Retain it only when it is validated
   against compiled state; otherwise keep it out of the executable input.
3. Which initial Maxwell policy is authoritative for periodic charge: permitted
   net charge/background, Poisson initialisation, seam handling, and field
   centring? This must be specified before golden fixtures exist.
4. Is V1 `f64` reference-first, or is `f32` accepted for practical GPU/WASM
   execution? The choice belongs in the profile and determinism envelope.
5. Who hosts field queries: each worker, an elected/reachable aggregator, or a
   result-artifact service? The client contract must make the choice invisible
   while preserving backpressure and provenance.
6. Is an Orishu workload replacement the intended first “apply edit” UX, or
   should field authoring first target new workloads only? This plan recommends
   new/replaced workloads until intervention semantics exist.

## Definition of success for the first vertical slice

A user can create a periodic Maxwell scene in Field CAD containing the agreed
supported sources/particles; save it; export deterministic `fcic/v1` initial
conditions and an Orishu manifest; run Orishu’s compatibility check; submit and
step the workload on one or more workers; observe committed time/diagnostics;
then retrieve an electric/magnetic plane or probe result in Field CAD. A second
machine can reproduce the run from the saved scene, model digest, and hashed
artifact without the first Field CAD process being present.
