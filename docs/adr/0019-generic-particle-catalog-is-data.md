# 0019 — the particle catalog creates one generic representation

Status: **accepted**, revisited by
[0021](0021-objects-are-composed-from-independent-components.md)

## Context

Milestone 6 needs familiar electron, proton, positron, and neutron authoring
choices without turning those names into hidden solver dispatch. Future field
models must be able to run against the same authored arrangement and attribute
different behaviour to their equations rather than to an opaque “species”
branch.

## Decision

- One shared particle component stores positive mass, motion mode, and catalog
  provenance. Charge remains the independently shared electromagnetic-source
  component; transform and velocity remain ordinary world state.
  *(Superseded by [0021](0021-objects-are-composed-from-independent-components.md):
  mass moved to its own shared component so a body can be massive without being a
  catalog particle, and the particle component now records provenance only.)*
- `Fixed`, `Prescribed`, and `Dynamic` are explicit motion modes. Only the latter
  two grant a solver kinematic authority.
  *(Superseded by [0021](0021-objects-are-composed-from-independent-components.md):
  replaced by `WorldObject::pinned`, which applies to any object rather than only
  to particles. Pinned with zero velocity is `Fixed`; pinned with authored
  velocity is `Prescribed`.)*
- Electron, proton, positron, and neutron templates create the same component
  layout. Their only runtime inputs are the authored numerical properties.
- Catalog release 1 records the NIST 2022 CODATA values: exact elementary
  charge, electron mass `9.1093837139e-31 kg`, proton mass
  `1.67262192595e-27 kg`, and neutron mass `1.67492750056e-27 kg`. A positron
  uses the electron mass and positive elementary charge; a neutron has zero
  charge. Sources: [NIST elementary charge](https://physics.nist.gov/cuu/Constants/Value/e.html)
  and the [2022 CODATA recommended values](https://physics.nist.gov/cuu/pdf/wall_2022.pdf).
- Editing a catalog mass changes its provenance to `Custom`; no UI edit may
  continue claiming the catalog value after changing it.
  *(Since [0021](0021-objects-are-composed-from-independent-components.md) this is
  enforced by checking the claim against the authored values rather than by asking
  the editor to reset it — a generic property editor cannot know to.)*

## Consequences

Equation systems inspect mass, charge, pose, velocity, and motion mode. They do
not receive an electron/proton enum on which to select forces. Catalog identity
remains useful for UI and experiment provenance, but a Hydrogen-under-Maxwell
arrangement is still only a proton-valued generic particle and an
electron-valued generic particle evolved by the declared field model.

Status: accepted.
