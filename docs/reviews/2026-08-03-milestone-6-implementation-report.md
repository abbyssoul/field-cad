# Milestone 6 implementation report

Date: 2026-08-03

## Outcome

Milestone 6 is implemented end to end: catalog authoring creates generic
particles, Maxwell couples their motion to its E/B lattice, the runtime adopts
the solver's canonical kinematics, and the desktop exposes the new authoring and
inspection controls.

## Particle model

- Added the shared `fieldcad-particles` crate and component schema.
- Added fixed, prescribed, and dynamically integrated motion modes.
- Added electron, proton, positron, and neutron catalog templates using NIST
  2022 CODATA mass and elementary-charge values. Template identity is provenance
  and UI metadata; every entry creates the same generic representation.
- Added catalog buttons to the Scene panel and mass, motion-mode, and velocity
  editing to the selected-object inspector.

## Numerical coupling

- Periodic cloud-in-cell charge deposition integrates to the authored charge.
- Current deposition averages all six coordinate-order paths. Each path uses
  the cumulative flux of the same CIC weight change and satisfies discrete
  continuity to roundoff, including a periodic seam crossing.
- Coupled fields initialize through a periodic discrete Poisson solve. Its
  forward-gradient E satisfies the existing backward-divergence Gauss operator;
  net charge receives an explicit, diagnosed neutralizing background.
- Particle forces use trilinearly interpolated reconstructed Yee fields and a
  relativistic Boris momentum pusher. The magnetic reference trajectory
  preserves speed and matches the Boris rotation angle within `1e-9` radians.
- The Maxwell electric update now includes `-J/epsilon0` on CPU and GPU.
- Motion wraps at periodic boundaries. Particles pass through one another;
  collision and short-range models are explicitly absent.

## Runtime and diagnostics

- Fixed particles have no motion authority. Prescribed and dynamic particles
  declare authority through the existing solver-outcome Interface.
- Solver-produced revisions continue resident coupled state without rebuilding
  it or incrementing the intervention counter.
- An authored physical edit rebuilds the Gauss-consistent field, resets the
  combined-energy reference, and increments the intervention counter exactly
  once.
- Diagnostics expose total charge, neutralizing background, field energy,
  particle kinetic energy, intervention-aware combined-energy drift, maximum
  continuity residual, and the boundary/collision policy. Prescribed motion is
  labelled as potentially exchanging untracked external work.

## Backend status

The CPU backend is the `f64` numerical oracle. The host-owned `wgpu f32` backend
executes Yee E/B/J advancement and passes a moving-particle parity regression.
Its particle interpolation, deposition, and pusher presently reuse the CPU
oracle, requiring one full E/B readback for every coupled tick. The desktop
diagnostics state that cost explicitly. A fully GPU-resident particle path is a
performance follow-up and must preserve the same reference tests.

## Verification

- `cargo test --workspace` — **231 passed**.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `git diff --check` — clean.

Focused evidence covers catalog representation/provenance, integrated charge,
same-cell and periodic-seam continuity, discrete Gauss initialization, magnetic
orbit and subluminal acceleration, runtime intervention semantics, a
deterministic proton/electron baseline, UI catalog authoring, WGSL validation,
and CPU/GPU particle-current parity.

## Remaining review items

- Manually assess the catalog/inspector workflow and useful visualization scales
  for elementary charges on the current metre-scale default scene.
- Profile the diagnosed GPU readback and choose a named-hardware budget before
  implementing a resident GPU pusher/depositor.
- Review the periodic-background, no-collision baseline before adding any new
  boundary, collision, regularization, or stabilizing model.
