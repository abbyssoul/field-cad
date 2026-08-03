# Milestone 5 remediation report

Date: 2026-08-03
Companion to [the review findings](2026-08-03-milestone-5-review.md).

All five remediations are implemented. Verification after the change:

- `cargo test --workspace` — **190 passed**, 15 suites (185 before; 5 added).
- `cargo clippy --workspace --all-targets` — clean.
- `cargo fmt --all --check` — clean.
- The GPU parity test ran against a real adapter rather than skipping, so the
  backend refactor is checked against the `f64` oracle on hardware.

## R1 — the periodic seam is declared, not fabricated

`UndefinedReason::AcrossPeriodicSeam` was added to `fieldcad-core`. It carries
the reason a value is missing rather than reusing `OutsideDomain`, which would
have been untrue: the sample is inside the domain, and the solver simply has no
defensible value there.

`MaxwellInitialCondition::periodicity()` now reports whether a state is
genuinely periodic. A prescribed plane wave is, so nothing changes for it. A
charge-constrained state is not, so `sample_yee_fields` marks any sample whose
trilinear stencil reads the seam as undefined.

The seam is derived per channel from the operators rather than guessed:

| channel | seam indices per axis | why |
| --- | --- | --- |
| `E`, energy density | last | `E` is a forward difference of the potential |
| `div E` | first and last | a backward difference of `E` reads the seam from both sides of the wrap |
| `B`, `div B` | none | `B` is zero for a constrained static state |

`yee_conservation` takes the same periodicity and excludes seam cells per
channel, so the reported energy is an integral over cells the lattice can
defend.

**What this deliberately does not do.** The textbook fix is a discrete periodic
Poisson solve. I prototyped it, and it is *worse* for this product: it is
self-consistent (`div E = rho/eps0` to `4e-13`) but reproduces the periodic
lattice field, measured at a 22.6% median error over the desktop's default plane
against 0.3% for the construction already in place. The review records both
measurements. The interior construction was kept and the solver made honest
about where it stops being valid.

## R2 — regression tests that can actually see the defect

Four tests were added, plus shared fixtures (`desktop_domain`, `charged_world`,
`static_charge_solver`, `vectors`) that removed the setup duplication in the
existing static-charge test.

- `an_off_centre_static_charge_reports_its_periodic_seam_as_undefined` — uses
  the charge position the desktop actually ships, `(0, 0, 0.6)`.
- `a_prescribed_wave_is_genuinely_periodic_and_has_no_seam` — guards against
  over-marking; a real periodic state must keep its outer layer.
- `static_charge_interior_tracks_the_electrostatic_oracle` — samples the shipped
  slice plane and asserts direction and magnitude against the oracle.
- `conservation_diagnostics_exclude_the_fabricated_seam`.

The seam test was mutation-checked: reverting `periodicity()` to
`Periodic` makes it fail and leaves the other eleven passing, which is the
property the original centred-charge test lacked.

## R3 — one implementation of the backend-neutral solver logic

`MaxwellCore` now owns what both backends previously duplicated: the Courant
check, the tick-sequence guard, world validation, the constrained-state rebuild
decision, and diagnostics formatting. `MaxwellSolver` and `GpuMaxwellSolver`
delegate to it and keep only their storage and update mechanics.

The `"CPU f64"` / `"GPU f32"` diagnostic strings were the clearest symptom of
the copy: the precision now comes from the domain and only the backend label is
injected, so the two cannot disagree about the lattice they are describing.
`GpuMaxwellSolver` lost four fields (`domain`, `initial_condition`, `tick`,
`world_revision`) that shadowed the core's.

Net: roughly 90 duplicated lines removed, and ADR 0015's "CPU/GPU backends
expose identical plugin metadata and channels" is now structural rather than a
convention.

## R4 — the constrained field is rebuilt only when the charges change

`MaxwellCore::constrained_state_for` compares the collected `ChargeSource` list
against the one the resident state was built from and returns `None` when they
match. `MaxwellSolverSetup` gained `initial_sources` so the core starts out
knowing what its state was derived from.

Dragging a probe or a slice plane no longer triggers a full `cells × sources`
potential rebuild, a gradient pass, and — on GPU — a complete grid upload. This
is the most frequent interaction in the app, and the runtime calls
`on_world_changed` for every accepted commit.

## R5 — documentation reconciled with measured behaviour

- **ADR 0016** now states the seam explicitly, records the measured comparison
  against the Poisson alternative, and says plainly that the constrained state
  does not satisfy Gauss's law globally — `div E` is a local residual, not
  enclosed charge. Status amended rather than superseded.
- **CONTEXT.md** updates the *Sample validity* glossary entry and the
  electromagnetism section.
- **PLAN.md** records the review under Milestone 5.

## Residual risk

The performance review gate for Milestone 5 is still open and unchanged by this
work; R4 reduces edit-path cost but no budgets have been set from measurements
on named hardware. R1 narrows where the static field is defined but does not
change what a periodic box can represent — an isolated charge remains
approximated, and the honest fix for that is a non-periodic boundary condition,
which Milestone 5 deferred along with absorbing boundaries.
