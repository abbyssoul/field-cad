# Prerequisites and Feature Gaps for Target Physics Experiments

This document identifies the missing primitive object types, solver capabilities, boundary conditions, and detector infrastructure needed in Field CAD to numerically reproduce the target experiments listed in [`target-experiments.md`](file:///home/soultaker/workspace/field-cad/docs/target-experiments.md).

---

## 1. Current Simulator Baseline

Field CAD currently supports:
- **Point Mass & Point Charge Particles**: Discrete particles with mass $m$ and charge $q$, integrated dynamically or pinned.
- **Spherical Sources**: Point and spherical electrostatic/gravitational charge/mass distributions.
- **Solvers & Kernels**: Analytic electrostatics, FDTD Yee-lattice Maxwell equations (coupled $E$ and $B$ fields with particle-in-cell CIC charge/current deposition), and Newtonian gravity.
- **Observables & Recorders**: Single-point probes, slice plane field visualizers, and energy/divergence diagnostics.

---

## 2. Minimal Set of Object Types Needed

To cover the full range of target EM and Gravitational experiments, Field CAD requires the following primitive object types and components:

### A. Background Field Generators
* **Uniform Field Component / Entity**:
  * Uniform Gravitational Acceleration field ($\vec{g}_0$).
  * Uniform Electrostatic field ($\vec{E}_0$).
  * Uniform Magnetostatic field ($\vec{B}_0$).
  * *Required for*: Millikan oil drop, Thomson $e/m$ velocity selector, Cyclotron motion.

### B. Current & Magnetic Field Sources
* **Line Current / Polyline / Wire Loop Segment**:
  * Carrying steady or time-varying current $I$.
  * *Required for*: Ampère force laws, Oersted effect, induction coils (Faraday's Law), and Aharonov-Bohm solenoids.
* **Magnetic Dipole / Intrinsic Spin Component**:
  * Attachable particle component with magnetic dipole moment $\vec{\mu}$ and spin angular momentum $\vec{S}$.
  * *Required for*: Stern-Gerlach spin splitting in magnetic gradients ($\nabla B$), Larmor precession, and torque on magnetic bodies.

### C. EM Wave & Radiation Emitters
* **Wave Source / Emitter Primitive**:
  * Field source generating directed monochromatic plane waves, Gaussian beams, or localized wave packets with controllable frequency $\omega$, wavevector $\vec{k}$, polarization $\vec{E}_0$, and pulse envelope $\tau$.
  * *Required for*: Hertzian dipole antenna radiation, Young's double-slit wave source, Compton scattering wave packets, Photoelectric monochromatic radiation, and Michelson-Morley interferometry.

### D. Material Domains & Boundary Geometries
* **Boundary Surface / Mask (PEC, Absorber, Slit Plate)**:
  * Planar or box primitives with explicit boundary conditions: Perfect Electric Conductor (PEC: $E_\parallel = 0$), Absorbing boundary (PML), or opaque aperture masks.
  * *Required for*: Young's double-slit apertures, Waveguide & resonant cavity boundaries, Millikan capacitor plates, and electrostatic shielding.
* **Dielectric / Material Volume**:
  * 3D geometry (box, sphere, cylinder) with physical properties: relative permittivity $\varepsilon_r$, relative permeability $\mu_r$, and electrical conductivity $\sigma$.
  * *Required for*: Snell's law refraction, Fresnel reflection/transmission, total internal reflection, and Brewster angle polarization.

### E. Extended Rigid Bodies & Mechanical Constraints
* **Extended Mass / Charge Body (Sphere, Cylinder, Ring)**:
  * Finite volume geometry with continuous or discretized mass density $\rho_m$ / charge density $\rho_q$ and inertia tensor $\mathbf{I}$.
  * *Required for*: Non-point mass gravitational multipoles, Roche limit tidal deformation, and Cavendish experiment source spheres.
* **Rigid Linkage / Torsion Spring Constraint**:
  * Mechanical constraint connecting two entities (distance rod, fixed pivot, or angular torsion spring).
  * *Required for*: Cavendish torsion balance deflection, pendulum setups, and physical gyroscope mounts (Gravity Probe B).

### F. Spatial Observation & Detector Screens
* **2D Spatial Impact & Field Intensity Detector Screen**:
  * Planar collector measuring spatial distribution of accumulated particle impacts, wave intensity flux ($|E|^2$), or energy deposition over time into a 2D grid/image.
  * *Required for*: Double-slit interference fringe recording, Thomson $e/m$ beam spots, Stern-Gerlach spin splitting profiles, and Rutherford scattering angular distributions.

---

## 3. Feature & Solver Gap Matrix

| Target Experiment | Required Object Types / Primitives | Solver & Physics Requirements |
| :--- | :--- | :--- |
| **Coulomb's Law** | Point Charge, Sphere Charge *(Existing)* | Static E-field evaluator *(Existing)* |
| **Millikan Oil Drop** | Point Charge *(Existing)*, Uniform $\vec{E}_0$ & $\vec{g}_0$ | Particle dynamics under coupled static fields |
| **Oersted & Ampère Laws** | Line Current / Loop, Point Charge | Magnetostatic field / Biot-Savart or FDTD current coupling |
| **Faraday's Law of Induction** | Line Current Loop, Moving Magnet / Dipole | Time-varying $B$-field generating induced $E$-field (Maxwell FDTD) |
| **Hertzian Dipole Antenna** | Oscillating Dipole / Wave Source | Maxwell FDTD wave propagation & Poynting flux |
| **Young's Double-Slit** | Wave Source, PEC Slit Mask, 2D Detector Screen | EM wave diffraction & spatial interference accumulation |
| **Fresnel & Snell's Laws** | Wave Source, Dielectric Volume, 2D Detector | Dielectric boundary conditions ($\varepsilon_r, \mu_r$) in FDTD |
| **Waveguides & Cavities** | Wave Source, PEC Box/Tube Boundary | PEC boundary conditions on grid |
| **Thomson $e/m$** | Point Charge *(Existing)*, Uniform $\vec{E}_0$ & $\vec{B}_0$, 2D Detector | Lorentz force particle integration & impact logging |
| **Cyclotron Motion** | Point Charge *(Existing)*, Uniform/Inhomogeneous $\vec{B}_0$ | Relativistic Boris pusher *(Existing)* + inhomogeneous $B$ |
| **Rutherford Scattering** | Point Charge *(Existing)*, 2D Detector | Relativistic Coulomb trajectory deflection |
| **Compton Scattering** | Wave Packet Emitter, Free Electron *(Existing)* | Wave packet interaction with charged particle |
| **Photoelectric Effect** | Wave Emitter, Bound Charge Surface | Energy absorption threshold / ionization model |
| **Bohr Atom Stability** | Proton & Electron *(Existing)* | Electrodynamic radiation damping / collapse check |
| **Stern-Gerlach** | Particle with Spin $\vec{\mu}$, $\nabla B$ Field Region, 2D Detector Screen | Torque & force on dipole in magnetic field gradient $\vec{F} = \nabla(\vec{\mu} \cdot \vec{B})$ |
| **Aharonov-Bohm** | Particle with Charge *(Existing)*, Shielded Solenoid (Line Current) | Vector potential $\vec{A}$ coupling to phase / wavefunction |
| **Cavendish Experiment** | Extended Mass Spheres, Torsion Constraint | Mass-mass gravitational force + torsion spring integration |
| **Keplerian Orbits** | Point Mass *(Existing)* | Newtonian Gravity *(Existing)* |
| **Lagrange Points** | Point Mass *(Existing)* | Multi-body Newtonian Gravity *(Existing)* |
| **Tidal Forces / Roche** | Composite Deformable Mass Body | Multi-particle / self-gravitating extended body mechanics |
| **Michelson-Morley** | Coherent Wave Source, Beam Splitter (Dielectric), Mirror (PEC), 2D Detector Screen | EM wave beam splitting & interferometric phase shift |
| **Perihelion Precession** | Central Mass & Planet | Post-Newtonian GR potential correction $U(r) = -GM/r (1 + 3L^2/m^2c^2r^2)$ |
| **Gravitational Light Deflection** | EM Wave Source, Heavy Central Mass | GR curved spacetime geodesic or effective index refraction $n(r) \approx 1 + 2GM/c^2r$ |
| **Gravitational Redshift** | EM Wave Source, Gravitational Potential Gradient | Gravitational frequency shift in wave solver |
| **Shapiro Time Delay** | EM Wave Source, Central Mass | Effective Shapiro delay in gravitational metric |
| **Binary Pulsar Decay** | Compact Binary Masses | Quadrupolar gravitational wave energy loss |
| **LIGO GW Detection** | Metric Wave Source / Boundary, 2D Interferometer Screen | Spacetime strain metric perturbation $\mathbf{h}_{ij}$ wave solver |
| **Frame-Dragging (GP-B)** | Rotating Mass, Gyroscope Dipole / Spin | Gravitomagnetic vector potential $\vec{A}_g$ / Lense-Thirring force |

---

## 4. Next Session Action Items

1. **Prioritize Primitive Implementation Phases**:
   - **Phase 1 (Immediate)**: Uniform Fields ($\vec{E}_0, \vec{B}_0, \vec{g}_0$), 2D Detector Screen, Line Current primitive.
   - **Phase 2 (Optics & Waves)**: Wave Packet Emitter, PEC Boundary Mask, Dielectric Volume.
   - **Phase 3 (Extended Mechanics & Spin)**: Magnetic Dipole/Spin component, Torsion/Rigid Constraint, Extended Mass/Charge body.
2. **Design Data Schemas & Plugins**: Define object component schemas in `fieldcad-plugin-api` for uniform fields, wave emitters, PEC/dielectric boundaries, and detector screens.
