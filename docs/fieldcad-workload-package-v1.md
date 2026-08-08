# Field CAD workload package v1

Status: **accepted shared standard for Field CAD and Orishu**.
Date: 2026-08-06.

This document freezes the package boundary for a workload that can run locally
in Field CAD or through Orishu. It does not define a second solver API. The
workload lifecycle is Orishu's existing `wl_init`, `wl_load_partition`,
`wl_restore_checkpoint`, `wl_step`, `wl_checkpoint`, `wl_query_state`, and
`wl_drop_partition` contract. The shared component world is
`orishu:workload/lifecycle@1`: it is owned by the execution-engine contract,
not by the Field CAD desktop application.

## Package identity

A package is an immutable, content-addressed directory or OCI artifact with:

```text
manifest.cbor          canonical package manifest
component.wasm         required WebAssembly Component Model binary
signatures/ed25519/    optional detached signatures over manifest.cbor
assets/                optional declared, content-addressed static assets
```

`manifest.cbor` uses deterministic CBOR (RFC 8949). Its SHA-256 digest is the
package identity. A transport URL, local path, or OCI reference is only a way
to obtain that content; it is never identity. A package may be distributed by
an Orishu artifact mechanism or a local Field CAD cache without changing its
meaning.

## Manifest schema

The canonical CBOR value is a map with these required fields:

```text
format:             "fieldcad.workload-package/v1"
id:                 reverse-DNS package id
version:            semantic version
component:
  path:             "component.wasm"
  sha256:           lowercase SHA-256 hex
  world:            "orishu:workload/lifecycle@1"
execution:
  engines:          ["wasm-component"]
  lifecycle:        "orishu.workload/v1"
  determinism:      "bitwise" | "tolerance"
  tolerance:        required only for "tolerance"
  limits:
    maxMemoryBytes: positive integer
    maxStepFuel:    positive integer
    maxWallMillis:  positive integer
physics:
  domainTypes:      non-empty ordered list
  channels:         [{ id, displayName, valueKind, dimension }]
  components:       [{ id, version }]
state:
  format:           stable state-format id
  version:          positive integer
  partitioning:     "structured-grid" | "particles" | "hybrid"
  checkpointCompatibleWithInitialConditions: true
assets:             optional [{ path, sha256, mediaType, byteLength }]
```

Unknown manifest fields are preserved by readers and ignored by V1 execution.
Unknown required `format`, `world`, `lifecycle`, `state.format`, or major version is a
hard compatibility failure. `ChannelId`, component ID, SI dimension, state
format, component digest, and declared execution profile are compatibility
identities; display names and transport locations are not.

## Execution and safety

Only a validated WebAssembly Component Model binary may execute in V1. The
host exposes the Orishu workload lifecycle and bounded logging/metric calls;
it exposes neither filesystem, network, wall-clock, ambient randomness, nor
unbounded host handles. The runtime enforces the declared memory, fuel, and
per-step wall-time limits, with host policy allowed to impose stricter limits.

GPU kernels are declarative package assets, not host-native code. A package
may name WGSL assets and a required feature profile in a future minor extension;
the host must parse, validate, compile, and resource-bind them itself. It must
fall back to the component/CPU path or reject the selected execution profile;
arbitrary shader access to device resources is never granted.

An unsigned package is usable only under an explicit development policy.
Normal Field CAD and Orishu policy requires at least one valid Ed25519 detached
signature from a configured trust root. Signature verification happens after
hashing `manifest.cbor`, verifying every declared payload digest, and before
component instantiation.

## Project documents and recovery

`fieldcad.scene/v1` is a separate, editable authored-scene document. It stores
domain, time-step, world intent, enabled package IDs/digests/configuration,
instrument definitions, and optional named views. It does not store solver
memory, snapshots, GPU resources, package bytes, or a running workload state.
Unknown package configuration is retained as an opaque, versioned blob and
shown as an unavailable placeholder; opening and saving must not discard it.

Writes use `scene.fcscene.tmp` followed by fsync and atomic replacement. Before
replacement, retain one `scene.fcscene.bak` containing the previous verified
document. On startup, the loader validates the primary, temporary, and backup
documents independently and offers the newest valid revision; it never silently
chooses a corrupt or partially written file.

The document and package formats intentionally remain separate: a scene can be
edited after an Orishu workload has been submitted, while a submitted workload
retains immutable package/input digests and its own run provenance.

## Orishu interoperability rules

- An Orishu workload manifest refers to this package by component digest,
  package manifest digest, required engine, ABI, and execution profile.
- The workload's initial-condition artifact and checkpoints use the package's
  declared `state.format` and `state.version`; this realizes Orishu decision
  002 that compatible initial conditions and checkpoints share one shape.
- Orishu supplies partition ownership, halos, barriers, checkpoints, artifact
  storage, provenance, and cancellation. Package code supplies only physics.
- Field CAD's local runner must reject exactly the same ABI, digest, state, and
  resource incompatibilities as Orishu. A local success is not permission to
  relax Orishu's cluster policy.

## V1 exclusions

Native Rust dynamic libraries, package-controlled threads, direct filesystem or
network access, arbitrary GPU API access, and live mutable edits to an Orishu
workload are excluded. Native/accelerator execution engines may be added only
as later declared engines that preserve this lifecycle and safety contract.
