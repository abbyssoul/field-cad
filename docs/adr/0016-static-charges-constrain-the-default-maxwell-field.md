# 0016 — stationary charges constrain the default Maxwell field

## Context

The first Maxwell desktop composition opened in the source-free prescribed
plane-wave validation case. That case is valuable for convergence and CPU/GPU
parity, but it ignored the authored charge and presented a travelling wave as
the default physical result. Maxwell's lossless curl equations do not make such
a wave “settle” into Coulomb's field; without a source constraint or damping it
continues indefinitely.

The desktop also moved its established XY measurement plane to a YZ wave
antinode and enabled both E and B layers. This made the validation fixture more
prominent than the user's scene and caused coincident visualization layers to
look overpainted.

## Decision

- Maxwell has an explicit initial-condition setting. `Static charges` is the
  default; `Prescribed plane wave` remains available for numerical validation.
- Static-charge initialization reads the same point/sphere charge components as
  electrostatics. It samples a radius-regularized Coulomb potential on periodic
  grid nodes and initializes Yee E as its discrete negative gradient. B starts
  at zero.
- The discrete-gradient construction is curl-free under the solver's periodic
  difference operators, so stationary sources remain stationary across Maxwell
  steps. Source position, shape, or charge edits rebuild the constrained state
  on both CPU and GPU backends. Edits that cannot change the charge
  configuration do not.
- A Coulomb potential is not periodic, so the outermost lattice layer on each
  axis differences two opposite faces of the box. Those values are fabricated,
  not approximate, and are reported as
  `SampleValidity::Undefined(AcrossPeriodicSeam)` rather than published as
  measurements. Conservation diagnostics exclude them too.
- Static-charge mode rejects charged objects with nonzero linear or angular
  velocity until charge-conserving current deposition exists.
- The desktop restores the XY plane at the origin. Maxwell E is visible by
  default; B remains independently selectable but starts hidden because it is
  zero for this scenario. Field-system details are collapsed and wrapped in the
  narrow inspector.

## Consequences

Maxwell and electrostatics now show the same stationary-charge field direction
and magnitude within grid/interpolation tolerance. The result is initialized
immediately rather than expected to converge through nonexistent dissipation.
The prescribed plane wave still covers time-domain propagation and backend
parity.

This is not moving-source electromagnetism. An authored position edit replaces
the static constraint instantaneously; a future milestone must deposit charge
and current consistently and evolve their retarded fields.

A periodic box cannot contain uncompensated net charge. The 2026-08-03
Milestone 5 review measured what that costs. Two constructions were compared
against the electrostatic oracle over the desktop's default slice plane:

| construction | median error | p90 | max |
| --- | --- | --- | --- |
| sampled potential (this ADR) | 0.003 | 0.014 | 0.146 |
| discrete periodic Poisson solve | 0.226 | 0.473 | 1.364 |

The Poisson alternative is self-consistent — it satisfies `div E = rho/eps0` to
machine precision — but it reproduces the periodic *lattice* field, charge plus
images, which is not what an isolated authored charge is meant to show. The
sampled potential buys its far better interior agreement by concentrating the
inconsistency into a single lattice layer. That trade is accepted, and the
layer is declared undefined rather than drawn.

The consequence is that the constrained static state does not satisfy Gauss's
law globally: summing `div E · dV` over the lattice gives zero, not `q/eps0`.
The published `div E` channel is a local residual, not enclosed charge. Before
the review, the fabricated layer was drawn as an ordinary field value — 281%
error in the shipped default scene — and contributed up to 30% of the reported
energy.

Status: accepted; amended 2026-08-03 by the Milestone 5 review.
