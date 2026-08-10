# Target Physics Experiments for Field CAD

This document serves as the target benchmark list of foundational physics experiments in **Electromagnetism** and **Gravitation** that Field CAD solvers, integrators, and field models aim to numerically reproduce or match.

> See [`target-experiments-prerequisites.md`](file:///home/soultaker/workspace/field-cad/docs/target-experiments-prerequisites.md) for the minimal primitive object types, solver capabilities, and feature gap matrix required to implement these target experiments.

---

## 1. Classical Electromagnetism & Electrostatics

### Coulomb's Law & Charge Interaction
- **Description**: Electrostatic force between stationary point charges and continuous charge distributions, demonstrating $1/r^2$ decay and superposition principles.
- **Reference**: Griffiths, D. J., *Introduction to Electrodynamics*, 4th ed., Pearson, 2013, Ch. 2.

### Millikan Oil Drop Experiment
- **Description**: Quantization of electric charge and trajectory balancing of charged micro-particles in uniform electrostatic and gravitational forces.
- **Reference**: Millikan, R. A. (1913), "On the Elementary Electrical Charge and the Avogadro Constant", *Physical Review*, 2(2), 109-143.

### Oersted & Ampère Force Law Experiments
- **Description**: Creation of magnetic fields by moving charges/currents and mutual magnetic forces between parallel/perpendicular current elements.
- **Reference**: Ampère, A. M. (1826), *Mémoire sur la théorie mathématique des phénomènes électrodynamiques uniquement déduite de l'expérience*.

### Faraday's Law of Induction
- **Description**: Generation of induced electric fields and electromotive forces (EMF) in loops or conductors by time-varying magnetic fields.
- **Reference**: Faraday, M. (1831), *Experimental Researches in Electricity*, Philosophical Transactions of the Royal Society.

---

## 2. Electrodynamics, Wave Propagation & Optics

### Hertzian Dipole Antenna Radiation
- **Description**: Oscillating charge dipole emitting spherical electromagnetic waves; demonstrates near-field reactive fields, far-field Poynting vector flux, and angular radiation pattern.
- **Reference**: Hertz, H. (1887), "Ueber sehr schnelle elektrischen Schwingungen", *Annalen der Physik*, 267(7), 421-448.

### Young's Double-Slit Experiment (EM Wave Interference)
- **Description**: Spatial wave interference and diffraction fringes formed by coherent EM waves passing through double-slit apertures.
- **Reference**: Young, T. (1804), "Experiments and Calculations Relative to Physical Optics", *Philosophical Transactions of the Royal Society*, 94, 1-16.

### Fresnel Reflection & Snell's Law (Dielectric Boundaries)
- **Description**: Reflection, refraction, total internal reflection, and Brewster angle polarization effects at dielectric boundaries.
- **Reference**: Born, M., & Wolf, E., *Principles of Optics*, 7th ed., Cambridge University Press, 1999, Ch. 1.

### Waveguide Dispersion & Cavity Resonant Modes
- **Description**: Propagation of TE and TM electromagnetic wave modes, cutoff frequencies, and standing wave resonance inside bounded domains.
- **Reference**: Jackson, J. D., *Classical Electrodynamics*, 3rd ed., Wiley, 1998, Ch. 8.

---

## 3. Particle-Field Coupling & Relativistic Dynamics

### Thomson's $e/m$ Measurement (Crossed E and B Fields)
- **Description**: Motion of charged particle beams in perpendicular electric and magnetic fields (Lorentz force velocity selection and trajectory curvature).
- **Reference**: Thomson, J. J. (1897), "Cathode Rays", *Philosophical Magazine*, 44(269), 293-316.

### Cyclotron Motion & Synchrotron Radiation
- **Description**: Relativistic trajectory of charged particles in homogeneous/inhomogeneous magnetic fields and power radiated by accelerated charges (Liénard-Wiechert fields).
- **Reference**: Jackson, J. D., *Classical Electrodynamics*, 3rd ed., Wiley, 1998, Ch. 14.

### Rutherford Scattering (Coulomb Deflection)
- **Description**: Classical and relativistic trajectory scattering of energetic charged particles ($\alpha$-particles) by a central electrostatic force field.
- **Reference**: Rutherford, E. (1911), "The Scattering of $\alpha$ and $\beta$ Particles by Matter and the Structure of the Atom", *Philosophical Magazine*, 21(125), 669-688.

### Compton Scattering
- **Description**: Relativistic energy and momentum exchange between high-frequency EM wave packets/photons and electrons, causing wavelength shift.
- **Reference**: Compton, A. H. (1923), "A Quantum Theory of the Scattering of X-rays by Light Elements", *Physical Review*, 21(5), 483-502.

---

## 4. Semi-Classical & Quantum EM Frontiers

### Photoelectric Effect
- **Description**: Ionization and electron emission from bound states when exposed to EM radiation above a specific frequency threshold, independent of wave amplitude.
- **Reference**: Einstein, A. (1905), "Über einen die Erzeugung und Verwandlung des Lichtes betreffenden heuristischen Gesichtspunkt", *Annalen der Physik*, 322(6), 132-148.

### Bohr Model & Hydrogen Atom Stability
- **Description**: Orbit stability, discrete energy levels, and radiation collapse behavior of orbiting electron-proton pairs under electrodynamic models.
- **Reference**: Bohr, N. (1913), "On the Constitution of Atoms and Molecules", *Philosophical Magazine*, 26(151), 1-25.

### Stern-Gerlach Experiment
- **Description**: Spatial splitting of atomic/particle beams with magnetic dipole moments traversing an inhomogeneous magnetic field gradient.
- **Reference**: Gerlach, W., & Stern, O. (1922), "Der experimentelle Nachweis der Richtungsquantelung im Magnetfeld", *Zeitschrift für Physik*, 9(1), 349-352.

### Aharonov-Bohm Effect
- **Description**: Quantum phase shift of charged particles moving through regions with non-zero vector potential $A$ but zero magnetic field $B$.
- **Reference**: Aharonov, Y., & Bohm, D. (1959), "Significance of Electromagnetic Potentials in the Quantum Theory", *Physical Review*, 115(3), 485-491.

---

## 5. Newtonian Gravitation & Celestial Mechanics

### Cavendish Experiment (Measuring $G$)
- **Description**: Measurement of weak gravitational attraction forces between masses using a torsion balance to establish Newton's constant $G$.
- **Reference**: Cavendish, H. (1798), "Experiments to Determine the Density of the Earth", *Philosophical Transactions of the Royal Society*, 88, 469-499.

### Keplerian Orbits & Two-Body Mechanics
- **Description**: Closed elliptical trajectories, conservation of angular momentum, areal velocity, and energy in inverse-square central gravity fields.
- **Reference**: Newton, I. (1687), *Philosophiae Naturalis Principia Mathematica*.

### Lagrange Points & Three-Body Libration
- **Description**: Gravitational potential equipotential topology, stable ($L_4, L_5$) and unstable ($L_1, L_2, L_3$) equilibrium points in rotating two-body fields.
- **Reference**: Lagrange, J. L. (1772), *Essai sur le Problème des Trois Corps*.

### Tidal Forces & Roche Limit Disruption
- **Description**: Differential gravitational field gradients deforming and disrupting self-gravitating or fluid bodies approaching a primary mass.
- **Reference**: Roche, É. (1849), "La figure d'une masse fluide soumise à l'attraction d'un point éloigné", *Académie des Sciences de Montpellier*.

---

## 6. Relativistic Gravitation & General Relativity Benchmarks

### Michelson-Morley Experiment
- **Description**: Precision interferometric measurement confirming light speed invariance across perpendicular directions relative to Earth's velocity.
- **Reference**: Michelson, A. A., & Morley, E. W. (1887), "On the Relative Motion of the Earth and the Luminiferous Ether", *American Journal of Science*, 34(203), 333-345.

### Perihelion Precession of Mercury
- **Description**: Anomalous non-Keplerian orbital precession of planets caused by post-Newtonian relativistic potential corrections.
- **Reference**: Einstein, A. (1915), "Erklärung der Perihelbewegung des Merkur aus der allgemeinen Relativitätstheorie", *Sitzungsberichte der Preußischen Akademie der Wissenschaften*, 831-839.

### Gravitational Light Deflection (Eddington 1919 Eclipse)
- **Description**: Curvature and angular deflection of electromagnetic rays propagating through strong gravitational fields near massive stellar objects.
- **Reference**: Dyson, F. W., Eddington, A. S., & Davidson, C. (1920), "A Determination of the Deflection of Light by the Sun's Gravitational Field", *Philosophical Transactions of the Royal Society A*, 220, 291-333.

### Gravitational Redshift (Pound-Rebka Experiment)
- **Description**: Frequency shift and photon energy alteration when light moves vertically along a gravitational potential gradient.
- **Reference**: Pound, R. V., & Rebka, G. A. (1959), "Apparent Weight of Photons", *Physical Review Letters*, 3(9), 439-441.

### Shapiro Time Delay
- **Description**: Relativistic propagation delay of electromagnetic signals passing near massive gravitating bodies.
- **Reference**: Shapiro, I. I. (1964), "Fourth Test of General Relativity", *Physical Review Letters*, 13(26), 789-791.

---

## 7. Gravitational Waves & Coupled EM-Gravity Dynamics

### Hulse-Taylor Binary Pulsar Decay
- **Description**: Secular orbital decay rate of compact binary star systems resulting from quadrupolar gravitational wave energy loss.
- **Reference**: Hulse, R. A., & Taylor, J. H. (1975), "Discovery of a pulsar in a binary system", *Astrophysical Journal*, 195, L51-L53.

### LIGO Gravitational Wave Transient Detection
- **Description**: Quadrupolar strain spacetime metric perturbations generated by compact binary inspirals passing through 3D spatial domains.
- **Reference**: Abbott, B. P., et al. (LIGO Scientific Collaboration & Virgo Collaboration) (2016), "Observation of Gravitational Waves from a Binary Black Hole Merger", *Physical Review Letters*, 116(6), 061102.

### Frame-Dragging & Lense-Thirring Precession (Gravity Probe B)
- **Description**: Geodetic and frame-dragging (gravitomagnetic) precession of spinning test bodies orbiting a massive rotating object.
- **Reference**: Everitt, C. W. F., et al. (2011), "Gravity Probe B: Final Results of a Space Experiment to Test General Relativity", *Physical Review Letters*, 106(22), 221101.