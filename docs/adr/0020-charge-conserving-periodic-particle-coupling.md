# 0020 — moving particles use charge-conserving periodic coupling

## Context

Updating an authored position and then sampling Coulomb's law would animate a
charge but would not couple it to Maxwell's equations. A Yee update needs charge
and current on the same discrete operators, and a particle pusher needs fields
interpolated back from the staggered representation. Violating discrete
continuity immediately creates a Gauss-law error that no plausible rendering
can diagnose reliably.

## Decision

- Charge uses periodic cloud-in-cell deposition on the nodes read by the Yee
  backward-divergence operator.
- One move deposits current along each of the six coordinate-order paths from
  its old to its new position. Each one-dimensional segment is the cumulative
  flux of the same CIC weight change; it therefore satisfies
  `(rho_new-rho_old)/dt + div(J) = 0` to roundoff. Averaging all six paths avoids
  choosing a preferred axis ordering.
- Coupled initial state solves the periodic discrete Poisson equation, then uses
  the matching forward gradient for E and B=0. Net charge is represented with
  an explicit uniform neutralizing background because periodic Poisson has no
  net-charge solution.
- Reconstructed cell-centred Yee fields are trilinearly interpolated to each
  particle. Dynamic velocity uses a relativistic Boris momentum pusher;
  prescribed velocity is authored external motion; fixed particles do not move.
- Particle motion wraps through the periodic domain. Particles pass through one
  another in this increment; no collision or short-range model is implied.
- Each tick pushes particles from the synchronized current E/B state, deposits
  their old-to-new current, advances Maxwell E/B, and returns complete
  kinematics to the runtime. External physical edits rebuild the Gauss-consistent
  field, reset the energy-drift reference, and increment an intervention count.

## Consequences

Continuity, seam crossing, Gauss initialization, pusher orbits, runtime
interventions, a proton/electron baseline, and CPU/GPU current-update parity all
have focused regressions. Diagnostics report total charge, the neutralizing
background, field and particle energy, intervention-aware drift, continuity
residual, and the periodic pass-through boundary policy.

The host-owned GPU backend presently keeps Yee advancement on GPU but uses the
shared CPU `f64` particle oracle, requiring one E/B readback per coupled tick.
That reference path is labelled in diagnostics. Moving interpolation,
deposition, and pushing onto GPU is a measured performance follow-up; it must
retain the CPU oracle and the same continuity/parity guarantees.

Status: accepted.
