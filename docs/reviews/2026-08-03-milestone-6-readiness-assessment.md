# Milestone 5 remediation assessment and Milestone 6 readiness

Date: 2026-08-03

## Assessment

The Milestone 5 review was correct to accept the dynamic Maxwell claim while
rejecting the periodic static seam as an ordinary measurement. The remediation
implements all five requested actions coherently:

- seam validity is channel-specific and matches each staggered operator's read
  stencil;
- conservation diagnostics exclude values the constrained state cannot defend;
- off-centre tests exercise the defect hidden by the original symmetric case;
- `MaxwellCore` centralizes backend-neutral behaviour without moving GPU storage
  into the plugin; and
- source equality prevents probe and plane edits from rebuilding the static
  lattice.

The constrained charge state remains intentionally local rather than a global
periodic Gauss-law solution. That limitation is now explicit in validity,
diagnostics, ADR 0016, and the plan. No additional Milestone 5 scientific fix
is required before the next physics increment.

## Readiness gaps found

Two architecture gaps would otherwise have forced Milestone 6 to cut across
existing ownership rules:

1. The charge schema and source extraction were owned by the electrostatics
   Implementation, so Maxwell depended on another equation system to represent
   a world property.
2. `EquationSystemSolver::step` could evolve only private state. There was no
   narrow Interface by which a coupled solver could publish canonical object
   motion while the runtime remained the only world writer.

## Adjustments made

- Added the `fieldcad-electromagnetic-sources` Module. It owns the charge schema
  and extracts object identity, position, velocity, charge, and distribution.
  Electrostatics and Maxwell consume it independently.
- Changed runtime schema composition so identical shared contributions register
  once and incompatible definitions fail before solver creation.
- Added declared per-object kinematic authority and `SolverStepOutcome` with
  complete transform/velocity updates. Ownership conflicts and missing objects
  are checked before any solver advances; returned updates use the runtime's
  validated world-adoption path.
- Recorded the decisions in ADRs 0017 and 0018 and updated `README.md`,
  `CONTEXT.md`, and `PLAN.md`.

Focused regressions prove shared-schema composition, Maxwell without an
electrostatics runtime dependency, conflicting-schema rejection, solver-produced
world motion, and competing motion-owner rejection.

## Verification

- `cargo test --workspace` — **197 passed**.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `git diff --check` — clean.

## Remaining gate and implementation order

Milestone 5's named-hardware performance review remains open. It should measure
GPU step submission, readback, snapshot publication, scene extraction, and
render time independently; this is a performance budget decision, not a
correctness blocker hidden by the remediation.

After that review, Milestone 6 can proceed without another ownership refactor:

1. introduce generic mass/charge particles, initial electron/proton/positron/
   neutron catalog templates, and fixed, prescribed, and dynamically integrated
   motion modes;
2. prove charge/current deposition satisfies discrete continuity and preserves
   the stationary-source case;
3. add Yee-field interpolation and a tested Lorentz pusher;
4. define domain exit and external edit/reinitialization semantics; and
5. expose charge, particle/field energy, and intervention-aware drift
   diagnostics before enabling particle coupling in the desktop.
