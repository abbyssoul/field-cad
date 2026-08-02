# 0005 — Defer runtime-loaded plugins; first-party crates until two solvers exist

Status: **accepted** (Milestone 0)

## Context

"Plugin" means two different things here, and conflating them would be costly:

1. **Architectural plugin** — an independently testable equation system behind a
   narrow contract. Needed immediately; it is the product's central idea.
2. **Runtime plugin** — a package installed without rebuilding the application.

Freezing a contract before any real physical model has used it produces a
contract shaped by the mock. This project already has evidence for that: the
first plugin API grew a per-point `sample()` that fitted the test fixture and
would not have survived electrostatics
([0006](0006-columnar-batched-field-sampling.md)).

Native Rust dynamic libraries are not a candidate. Rust has no stable ABI, and an
in-process plugin can corrupt or crash the host — unacceptable for something a
user installs.

## Decision

Requirement 1 now, requirement 2 deferred. Plugins are ordinary crates in the
workspace so the contract can change with a compiler-checked refactor. The
contract will not be versioned or frozen until at least two real equation systems
— electrostatics and time-domain electromagnetism — have implemented it.

The candidate for the eventual boundary is the WebAssembly Component Model for
control code, plus declarative host-validated WGSL for GPU kernels. That is a
direction, not a commitment; it is evaluated in Milestone 8.

## Consequences

- Adding an equation system requires rebuilding the application. Acceptable
  while the audience is us.
- The contract is expressed as Rust traits over serializable data types and
  deliberately avoids `egui`, `winit`, and application state, so a future
  out-of-process boundary is a port rather than a redesign.
- `ChannelHandle` is an in-process interning detail; `ChannelId` is the stable,
  serializable identity. Only `ChannelId` may cross a package boundary.
- Milestone 7's gravity plugin is a test of the abstraction, not a feature: if it
  cannot be written without touching the contract, the contract is still
  secretly electromagnetic.
