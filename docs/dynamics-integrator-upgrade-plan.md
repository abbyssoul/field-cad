# Dynamics Integrator Upgrade Plan: Relativistic Velocity Verlet & Energy-Conserving Solvers

## 1. Context & Motivation

FieldCAD aims to provide high-fidelity physics modeling and interactive virtual experimentation. In virtual experimentation (e.g., electrostatic particle traps, gravitational orbits, harmonic resonators, charged beam optics), long-term numerical stability, energy conservation, and accurate trajectory integration take precedence over raw execution speed.

The current dynamics integration in [`crates/fieldcad-dynamics/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs) uses a single-stage momentum update. While the module documentation refers to it as a *"momentum-form leapfrog"*, mathematically it is a **1st-order Relativistic Symplectic Euler (Euler-Cromer)** integrator.

Although 1st-order Symplectic Euler preserves phase-space volume for conservative position-only force fields $F(x)$, its $O(\Delta t)$ local truncation error introduces phase lag, artificial precession in orbital dynamics, and significant trajectory degradation unless exceedingly small time steps are used. Furthermore, for velocity-dependent forces like magnetic Lorentz forces ($q \mathbf{v} \times \mathbf{B}$), 1st-order explicit force sampling introduces non-physical kinetic energy growth over time ([ADR 0022](file:///home/soultaker/workspace/field-cad/docs/adr/0022-dynamics-is-a-first-party-system.md#L67-L74)).

This document outlines the next step for the dynamics solver: upgrading `fieldcad-dynamics` to a **2nd-Order Relativistic Velocity Verlet** integrator and establishing an optional path for energy-conserving electromagnetic particle pushing (Boris Push).

---

## 2. Review of Integrator Alternatives

| Integrator | Accuracy Order | Symplectic / Energy Conserving | Force Calls / Step | Historical State Required | Relativistic Formulation | Suitability for FieldCAD |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Symplectic Euler** *(Current)* | 1st Order $O(\Delta t)$ | Phase-space bound | 1 | None | $\mathbf{p}$-space | Baseline preview only |
| **Velocity Verlet** *(Target)* | **2nd Order $O(\Delta t^2)$** | **Exact Hamiltonian bound** | **1 (cached)** | $\mathbf{F}_n$ (forces) | **Relativistic $\mathbf{p}$-Verlet** | **Primary choice** |
| **Staggered Leapfrog** | 2nd Order $O(\Delta t^2)$ | Symplectic | 1 | Half-step velocity $\mathbf{v}_{n-1/2}$ | Relativistic $\mathbf{p}$-space | Good, but $x, v$ staggered |
| **Beeman's Algorithm** | 3rd Order ($x$), 2nd ($v$) | No (long-term drift) | 1 (cached) | Accelerations $\mathbf{a}_n, \mathbf{a}_{n-1}$ | Complex | Non-symplectic |
| **Boris Push** | 2nd Order $O(\Delta t^2)$ | Exact $\|v\|$ in static $\mathbf{B}$ | 1 | $\mathbf{v}_{n-1/2}$ | Native | EM particle sidecar |
| **Yoshida 4th-Order** | 4th Order $O(\Delta t^4)$ | Symplectic | 3 | None | Multi-stage $\mathbf{p}$-space | High-cost gravitation |
| **Explicit RK4** | 4th Order $O(\Delta t^4)$ | **No** (dissipative) | 4 | None | Standard | Unstable for long orbits |

---

## 3. Recommended Design: Relativistic Velocity Verlet

Velocity Verlet provides 2nd-order trajectory accuracy $O(\Delta t^2)$, time-reversibility, and symplectic energy preservation while maintaining synchronous positions $\mathbf{x}_n$ and velocities $\mathbf{v}_n$ at integer tick boundaries $t_n$.

### Mathematical Formulation in Relativistic Momentum Space

Given body state $(\mathbf{x}_n, \mathbf{v}_n)$ at tick $n$, rest mass $m$, and total force $\mathbf{F}_n$ evaluated at $t_n$:

1. **Half-Step Momentum Push**:
   $$\mathbf{p}_{n+1/2} = \mathbf{p}(\mathbf{v}_n, m) + \mathbf{F}_n \frac{\Delta t}{2}$$

2. **Half-Step Velocity Update**:
   $$\mathbf{v}_{n+1/2} = \frac{\mathbf{p}_{n+1/2}}{m \sqrt{1 + \left(\frac{\mathbf{p}_{n+1/2}}{m c}\right)^2}}$$

3. **Full-Step Position Advance**:
   $$\mathbf{x}_{n+1} = \mathbf{x}_n + \mathbf{v}_{n+1/2} \, \Delta t$$

4. **Evaluate Forces at New State**:
   Plugins evaluate forces $\mathbf{F}_{n+1}$ at position $\mathbf{x}_{n+1}$ and velocity $\mathbf{v}_{n+1/2}$.

5. **Final Half-Step Momentum & Velocity Push**:
   $$\mathbf{p}_{n+1} = \mathbf{p}_{n+1/2} + \mathbf{F}_{n+1} \frac{\Delta t}{2}$$
   $$\mathbf{v}_{n+1} = \frac{\mathbf{p}_{n+1}}{m \sqrt{1 + \left(\frac{\mathbf{p}_{n+1}}{m c}\right)^2}}$$

---

## 4. Implementation & Architectural Changes

### A. [`crates/fieldcad-dynamics/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs)
- Update `advance_body` (or provide a new `advance_body_verlet` / stateful integrator helper) to accept half-step momentum state or previous force $\mathbf{F}_n$.
- Expose half-step kinematics helpers `half_push` and `final_push` to allow two-stage execution during a runtime tick.

### B. [`crates/fieldcad-simulation/src/runtime.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-simulation/src/runtime.rs)
- Retain `last_forces` across ticks (already partially tracked at [`runtime.rs:L1530`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-simulation/src/runtime.rs#L1530)).
- Execution sequence per tick:
  1. Advance position $\mathbf{x}_n \to \mathbf{x}_{n+1}$ using cached forces $\mathbf{F}_n$ and half-step velocity $\mathbf{v}_{n+1/2}$.
  2. Sample plugin forces $\mathbf{F}_{n+1} = \sum \text{plugin.forces}(\mathbf{x}_{n+1}, \mathbf{v}_{n+1/2})$.
  3. Compute final velocity $\mathbf{v}_{n+1}$ using $\mathbf{F}_{n+1}$.
  4. Store $\mathbf{F}_{n+1}$ as `last_forces` for the next tick.

### C. Electromagnetic Boris Push Extension (Phase 2)
- For velocity-dependent magnetic fields $q \mathbf{v} \times \mathbf{B}$, explicit force evaluation introduces energy drift.
- Phase 2 will introduce an optional `EquationSystemSolver` extension method `lorentz_push` allowing Maxwell / Electromagnetism solvers to handle magnetic vector rotations exactly via Boris push while delegating scalar potential forces to `fieldcad-dynamics`.

---

## 5. Verification & Testing Plan

1. **Kepler Orbital Test**:
   - Simulate a 2-body gravitational orbit over 10,000 time steps.
   - Measure total energy drift $|E(t) - E(0)| / |E(0)|$ and perihelion precession.
   - Confirm 2nd-order convergence rate $O(\Delta t^2)$ vs 1st-order $O(\Delta t)$.

2. **Harmonic Oscillator Test**:
   - Model a 1D spring-mass system. Verify phase error reduction and bounded amplitude.

3. **Relativistic Speed Limit Guard**:
   - Ensure subluminal speed clamp ($v < c$) remains strictly enforced under extreme forces.
