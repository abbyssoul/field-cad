# 0023 — Runtime workload packages share Orishu's lifecycle ABI

Status: **accepted**  
Date: 2026-08-06

## Context

Field CAD now has electrostatics, Maxwell, and Newtonian gravity implementations,
so the runtime-plugin boundary has evidence from more than one physical domain.
Orishu already defines a workload lifecycle and artifact/provenance model but
does not define a package format. If Field CAD invented a desktop-specific
plugin format, the eventual distributed path would have incompatible package,
state, trust, and resource semantics.

## Decision

Field CAD defines `fieldcad.workload-package/v1` in
[the package standard](../fieldcad-workload-package-v1.md). It is the shared
package format proposed for Orishu adoption. V1 packages are content-addressed,
deterministically encoded WebAssembly Component Model packages with signed
manifests, declared resource limits, typed physics/state compatibility, and
host-validated optional GPU assets.

The package uses Orishu's existing workload lifecycle; it does not introduce a
Field CAD-specific stepping ABI. Scene documents are separate editable intent,
and unknown package data is retained rather than discarded.

## Consequences

- First-party Rust crates remain the current local implementations while a
  component host is spiked; no Rust dynamic-library ABI is introduced.
- A future package loader must validate signatures, payload hashes, manifest
  compatibility, and resource limits before instantiation.
- Orishu can adopt the same package identity and state compatibility rules
  without depending on desktop UI/runtime code.
- The remaining executable work is a component-host spike, package
  install/remove tests, scene-document serialization/recovery, and an Orishu
  manifest/artifact adapter.
