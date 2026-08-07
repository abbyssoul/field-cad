# Physical Quantities & High-Precision Numerical Framework Plan

## Executive Summary & Problem Context

Field CAD is a high-performance physics modeling and simulation environment designed for physical phenomena and new-physics model evaluations. Accurate representation and computation of physical quantities—such as mass, electric charge, position/distance, velocity, force, and energy—are fundamental to the scientific validity of the simulation engine.

Currently, physical quantities are represented across the codebase using primitive IEEE 754 floating-point types (`f32` / `f64`) and 3D vectors (`glam::DVec3`). Primitive floats suffer from two fundamental weaknesses in scientific computing:

1. **Numeric Precision Limitations & Catastrophic Cancellation**:
   - Simulation domains often involve vastly different physical scales, such as subatomic particles (e.g., electron mass $m_e \approx 9.1093837 \times 10^{-31}\text{ kg}$, elementary charge $e \approx 1.60217663 \times 10^{-19}\text{ C}$) interacting across macroscopic or astronomical distances ($10^3\text{ m}$ to $10^{15}\text{ m}$). Standard `f64` (53-bit significand, ~15–17 decimal digits) loses precision when combining scales.
   - Relativistic computations (e.g., Lorentz factor $\gamma = 1/\sqrt{1 - v^2/c^2}$ and kinetic energy subtractive forms $K = (\gamma - 1)mc^2$) suffer from catastrophic cancellation when $v \ll c$ or $v \to c$.
   - Numerical integration over many simulation steps leads to accumulative rounding drift.

2. **Absence of Type-Level Dimensional Safety**:
   - Primitive floats do not prevent dimensional misuse. Accidental addition of Mass to Charge, or passing Velocity where Force is expected, compiles without compiler errors or warnings.

---

## Research & Solution Evaluation

### 1. Dimensional Analysis & Type Safety

| Crate / Approach | Type Safety Level | Performance & Ergonomics | Recommendation |
| :--- | :--- | :--- | :--- |
| **`uom` (Units of Measurement)** | Full compile-time SI dimensional analysis | Zero runtime cost for standard types; type-level exponents; highly customizable backing scalars | **Recommended**. The standard, mature Rust crate for zero-overhead dimensional safety. |
| **`dimensioned`** | Compile-time dimensional analysis | Less active maintenance, more complex generic trait bounds | Not recommended. |
| **Custom Const-Generic Wrapper** | Custom compile-time checking | Requires building unit conversions, SI systems, and derived dimensions from scratch | Not recommended over `uom`. |

### 2. High-Precision & Arbitrary-Precision Scalar Engine

| Representation | Precision / Significand | Transcendentals ($\sqrt{x}$, $\sin$, $\exp$) | External C Dependencies | Recommendation |
| :--- | :--- | :--- | :--- | :--- |
| **`dashu::Float` / `num_bigfloat`** | Arbitrary (configurable bits) | Supported | None (Pure Rust) | **Recommended for CPU Oracles & Integrators**. |
| **`two-float` / `qd`** | Double-double (~106 bits / ~31 decimal digits) | High performance | None (Pure Rust) | Excellent for hardware-accelerated extended precision. |
| **`rug::Float`** | Arbitrary (MPFR binding) | Fully supported | Requires MPFR / GMP C libraries | High performance, but adds C build toolchain requirements. |
| **`num-rational::Ratio`** | Exact rational arithmetic | Requires approximations | None | Ideal for exact rational constants, limited for non-rational math. |

---

## Architectural Proposal: Dual-Layered Quantity Architecture

The proposed architecture introduces a dual-layered design separating **Dimensional Type Guarantees** from **Numerical Precision**:

```
┌──────────────────────────────────────────────────────────────────────────┐
│                         Field CAD Engine                                 │
├──────────────────────────────────────────────────────────────────────────┤
│ Layer 1: Dimensional Type Safety (`uom`)                                 │
│   • Mass, Length, Time, Charge, Velocity, Acceleration, Force, Energy,   │
│     Electric Field, Magnetic Flux Density, etc.                          │
│   • Enforces dimensional correctness at compile time.                    │
│   • Interoperates with `fieldcad-core` dynamic `Dimension` schema.       │
├──────────────────────────────────────────────────────────────────────────┤
│ Layer 2: Pluggable Precision Scalar Engine                               │
│   • Standard `f64`: High-throughput WGPU compute & rendering pipelines.  │
│   • Arbitrary/Extended Precision (`dashu::Float` / `num_bigfloat`):      │
│     CPU Reference Oracles, Relativistic Integrators, and Diagnostic      │
│     Conservation Auditors.                                              │
└──────────────────────────────────────────────────────────────────────────┘
```

1. **Dimensional Typing Layer (`uom`)**:
   - Strong compile-time type wrappers: `Mass`, `Length`, `Time`, `ElectricCharge`, `Velocity`, `Force`, `Energy`, `ElectricField`, `MagneticFluxDensity`, etc.
   - Vector quantities represented via `Vector3Quantity<Q>`.
   - Automatic dimensional arithmetic (e.g., `Length / Time` automatically yields `Velocity`; `Mass * Acceleration` automatically yields `Force`).

2. **Precision Scalar Engine**:
   - Generic scalar abstraction layer (`FloatScalar`) enabling solvers to execute either with `f64` (for fast interactive previews and GPU pipelines) or arbitrary precision (for exact CPU reference solutions and verification).

---

## Affected Codebase Locations

### 1. `crates/fieldcad-core`
- [`crates/fieldcad-core/src/units.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/units.rs): Refactor `Quantity`, `VectorQuantity`, and `Dimension`. Introduce `uom` type definitions, `Vector3Quantity<Q>`, and high-precision relativistic math (`lorentz_factor`, `relativistic_momentum`, `relativistic_kinetic_energy`).
- [`crates/fieldcad-core/src/schema.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/schema.rs): Update `PropertyValue::Scalar` and `PropertyValue::Vector` to wrap strongly-typed quantities with unit metadata.
- [`crates/fieldcad-core/src/sampling.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/sampling.rs): Support typed quantity extraction and conversions in columnar sampling.
- [`crates/fieldcad-core/src/time.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/time.rs): Upgrade `TimeStep` and simulation clock calculations to typed `Time` quantities.

### 2. `crates/fieldcad-particles`
- [`crates/fieldcad-particles/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-particles/src/lib.rs): Represent fundamental particle templates (Electron, Proton, Positron, Neutron) with exact high-precision mass and charge constants.

### 3. `crates/fieldcad-electromagnetic-sources`
- [`crates/fieldcad-electromagnetic-sources/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-electromagnetic-sources/src/lib.rs): Update `ChargeSource`, electric charge properties, and field channel schemas to use typed quantities.

### 4. `crates/fieldcad-dynamics`
- [`crates/fieldcad-dynamics/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs): Upgrade `DynamicBody` (position, velocity, mass) and the relativistic momentum-form leapfrog integrator (`collect_bodies`, `accumulate_forces`, `integrate`, `advance_body`) to high-precision typed math.

### 5. `crates/fieldcad-superposition`
- [`crates/fieldcad-superposition/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-superposition/src/lib.rs): Inverse-square law evaluation with numerical safeguards against cancellation and distance underflow.

### 6. Plugins (`plugins/`)
- [`plugins/electrostatics/src/lib.rs`](file:///home/soultaker/workspace/field-cad/plugins/electrostatics/src/lib.rs): Re-implement Coulomb field and potential CPU evaluator using typed quantities and high-precision scalars.
- [`plugins/electromagnetism/src/lib.rs`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/lib.rs): Upgrade Yee lattice field updates and Boris pusher to typed quantities.
- [`plugins/gravity/src/lib.rs`](file:///home/soultaker/workspace/field-cad/plugins/gravity/src/lib.rs): Upgrade Newtonian gravitational solver to typed quantities.

### 7. Plugin API, Desktop App & MCP Server
- [`crates/fieldcad-plugin-api/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-plugin-api/src/lib.rs): Update solver interfaces (`DynamicBody`, `ObjectKinematicsUpdate`, `SampledColumn`).
- [`apps/fieldcad-desktop/src/ui/panels.rs`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/ui/panels.rs): Update UI property editors and inspector panels to render unit symbols (`kg`, `C`, `m/s`, `V/m`, `T`) from `uom` types.
- [`crates/fieldcad-mcp/src/typed_world.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-mcp/src/typed_world.rs): Update JSON-RPC serialization for typed quantities.

---

## Detailed Step-by-Step Implementation Breakdown

### Phase 1: Core Type System & High-Precision Infrastructure
1. Add `uom` and `dashu` / `num-bigfloat` dependencies to workspace [`Cargo.toml`](file:///home/soultaker/workspace/field-cad/Cargo.toml).
2. Refactor [`crates/fieldcad-core/src/units.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/units.rs):
   - Define type-safe quantity aliases using `uom::si::f64` (and generic scalar variants).
   - Implement `Vector3Quantity<Q>` wrapping 3D physical quantities.
   - Implement conversion traits between static `uom` types and dynamic `fieldcad_core::Dimension` / `Quantity`.
   - Update `lorentz_factor`, `relativistic_momentum`, and `relativistic_kinetic_energy` to use high-precision numerical operations.
3. Update `PropertyValue` in [`crates/fieldcad-core/src/schema.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-core/src/schema.rs).

### Phase 2: Solvers, Dynamics & Particle Catalog
1. Upgrade [`crates/fieldcad-dynamics/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-dynamics/src/lib.rs) to use typed `Mass`, `Velocity`, `Force`, `TimeStep` in momentum leapfrog updates.
2. Upgrade [`crates/fieldcad-particles/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-particles/src/lib.rs) particle templates with exact typed mass and charge constants.
3. Update [`crates/fieldcad-superposition/src/lib.rs`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-superposition/src/lib.rs) inverse-square calculation routines.

### Phase 3: Equation System Plugins
1. Re-implement CPU reference evaluators in [`plugins/electrostatics`](file:///home/soultaker/workspace/field-cad/plugins/electrostatics/src/lib.rs), [`plugins/electromagnetism`](file:///home/soultaker/workspace/field-cad/plugins/electromagnetism/src/lib.rs), and [`plugins/gravity`](file:///home/soultaker/workspace/field-cad/plugins/gravity/src/lib.rs) using typed quantities and high-precision scalars.
2. Ensure GPU backends explicitly convert snapshot inputs/outputs between high-precision typed quantities and `f32`/`f64` render buffers.

### Phase 4: UI Presentation, MCP & Verification
1. Update UI panels in [`apps/fieldcad-desktop`](file:///home/soultaker/workspace/field-cad/apps/fieldcad-desktop/src/ui/panels.rs) to display unit symbols automatically derived from `uom` quantity types.
2. Update MCP tool handlers in [`crates/fieldcad-mcp`](file:///home/soultaker/workspace/field-cad/crates/fieldcad-mcp/src/typed_world.rs).
3. Add unit tests for compile-time dimensional checking, extreme physical scale evaluation (subatomic vs astronomical), relativistic momentum stability near light speed, and long-run energy conservation.
