# Compile-time quantity type safety with `uom`

Status: **proposed for Phase 0 implementation**

Supersedes: `docs/physical-quantities-precision-plan.md` (research survey)

## Context

[ADR 0004](adr/0004-si-units-in-the-core.md) committed to SI everywhere and
runtime dimension metadata (`Dimension` as seven `i8` exponents). It explicitly
deferred compile-time dimensional arithmetic: *"No dimensional arithmetic is
provided. Multiplying a charge by a field to get a force is a plugin's job today.
If that starts producing bugs, add checked operators — until then it would be
unused machinery."*

That deferral has been exercised across seven milestones and three equation
systems. The evidence is now sufficient to act:

1. **Silent bugs are reachable.** The codebase performs `qE`, `m·g`, `v×B`,
   `courant_limit`, Poisson solves, Boris pushes — all in raw `f64`/`DVec3`.
   Each operation compiles and runs with any combination of quantities, and a
   dimensional mistake produces wrong physics, not a compile error.

2. **The boundary pattern is proven.** ADR 0004's justification — "the type
   system's guarantee is at the boundary, not on every element" — is the right
   principle. Bulk storage (`FieldColumn`) stays raw; the types enter and leave
   through named conversion points where the dimension is known.

3. **A high-quality crate exists.** `uom` (units of measurement) provides
   zero-cost compile-time dimensional analysis, full SI support, `serde`
   integration, and is actively maintained. It is the standard Rust answer to
   this problem.

## Decision

Adopt `uom` for compile-time quantity type safety, applied incrementally across
four phases. The migration starts with **Phase 0**: define and export the typed
quantity set from `fieldcad-core`.

### Architecture

```
┌──────────────────────────────────────────────────────────┐
│  uom typed quantities at API boundaries (public types)    │  ← Phases 0-3
│  MassKg, ChargeCoulombs, ForceNewtons, VelocityMps, ...  │
│  Every function signature says what it expects            │
└─────────────────────┬────────────────────────────────────┘
                      │ .value() / ::new()
┌─────────────────────▼────────────────────────────────────┐
│  Raw f64/DVec3 computation kernels (hot paths)            │  ← stays unchanged
│  FDTD Yee update, Boris pusher, superposition             │
│  WGSL shader code (always f32)                            │
└─────────────────────┬────────────────────────────────────┘
                      │
┌─────────────────────▼────────────────────────────────────┐
│  Future: pluggable precision engine                        │  ← Phase 5
│  Compensated summation, f128, double-double, interval      │
│  Swapped in at the kernel level, type boundary unchanged   │
└──────────────────────────────────────────────────────────┘
```

This layered boundary keeps three concerns independent:

- **Type safety** at the interface (where mistakes cross function boundaries).
- **Performance** in the kernel (where millions of operations run).
- **Precision** as a separable concern (swap the numeric engine without
  re-architecting the types).

### Why `uom` is compatible with future precision work

`uom` is parametric over its storage type. `uom::si::f64::Mass` stores `f64`;
`uom::si::f32::Mass` stores `f32`; and any type implementing `num_traits::Num`
(and the arithmetic trait needed for a given operation) works as storage.

| Future precision technique | `uom` compatibility |
|---|---|
| **Compensated summation** (Kahan) | Algorithmic; operates on raw `f64` inside kernels. Unwrapping/rewrapping at kernel boundaries has no cost proportional to iteration count. |
| **Double-double / `two-float`** (~106 bits) | Would need `num_traits` impls for the double-double type. `uom`'s arithmetic is generated — the trait boundary is the leverage point. Feasible but work. |
| **`f128` / `float128`** | Same as double-double. Requires `num_traits::Float` on the 128-bit type. |
| **`dashu::Float` (arbitrary precision)** | Heavyweight. Better used for CPU reference oracles outside the hot path, converting through the same typed boundary. |
| **Interval arithmetic** | Hard. Interval types break `uom`'s arithmetic assumptions (e.g., `x * x` is positive-definite for intervals). Keep intervals as a separate evaluation mode, not a `uom` storage type. |
| **GPU compute (`f32` WGSL)** | Unchanged. The GPU boundary is always raw `f32` for WGSL buffers. |

The key insight: **kernels stay on raw `f64`**. The typed boundary is around the
kernel — arguments come in typed, internals compute on `f64`, results go out
typed. A future precision engine swaps the kernel internals; the typed boundary
doesn't change. The `uom` layer is the API contract, not the execution engine.

---

## Phases

| Phase | Scope | Risk |
|---|---|---|
| **0 (this plan)** | Core: add `uom`, define quantity type set, replace scalar signature types in `fieldcad-core` | Low — self-contained addition, no dependent crate breaks |
| **1** | Data boundary: `PropertyValue`, `TimeStep`, `CoupledSource`, particle constants | Medium — touches serialization, schemas |
| **2** | Computation: dynamics, superposition, physical constants in plugins | Medium-high — changes kernel signatures |
| **3** | Plugin API: trait signatures, SampledColumn construction | Medium — public trait changes need coordinated crate releases |
| **4** | UI: unit display from typed quantities | Low — presentation only |
| **5** | Precision engine: compensated summation, higher-precision kernel oracles | Independent — builds on Phase 0-3 boundaries |

Each phase is independently testable and revertible.

---

## Phase 0 — Core quantity definition set

### What this phase does

1. Add `uom` as a workspace dependency.
2. Define and re-export a curated set of quantity types and unit aliases from
   `fieldcad_core::quantities`.
3. Add vector quantity wrappers (`Quantity3<T>`) for 3D physical quantities that
   currently use `DVec3`.
4. Replace raw `f64` in key scalar types and function signatures in
   `fieldcad-core` — only where the quantity identity is unambiguous and the
   replacement is local (no cascading downstream breakage yet).
5. Add conversion constructors to the existing `Quantity`/`VectorQuantity`
   runtime types so they can be built from and decomposed into `uom` types.
6. Re-export `uom`'s existing `si::f64::*` types under dimension-revealing names
   (e.g., `MassKg`, `ChargeCoulombs`) for use by downstream crates.

### Files to create

| File | Purpose |
|---|---|
| `crates/fieldcad-core/src/quantities.rs` | Quantity type aliases, `Quantity3<T>`, `Vec3<Q>` conversion helpers |
| `crates/fieldcad-core/src/quantities/vector.rs` | 3D vector quantity wrapper |
| `crates/fieldcad-core/src/quantities/macros.rs` | (optional) macros for quantity operations |

### Files to modify

| File | Change |
|---|---|
| `Cargo.toml` (workspace root) | Add `uom = { version = "0.38", features = ["autoconvert", "serde"] }` to `[workspace.dependencies]` |
| `crates/fieldcad-core/Cargo.toml` | Add `uom = { workspace = true }` |
| `crates/fieldcad-core/src/lib.rs` | Add `pub mod quantities;` |
| `crates/fieldcad-core/src/units.rs` | Add `From`/`Into` impls between `Quantity`/`VectorQuantity` and `uom` types. No deletions yet. |
| `crates/fieldcad-core/src/time.rs` | `TimeStep(f64)` → `TimeStep(TimeQuantity)`. Validated wrapper preserved. |

### Quantity type set to define

```rust
// — Base SI quantities (re-exported from uom::si::f64 with domain names) —
pub use uom::si::f64::{
    Length as LengthMetres,      // m
    Mass as MassKg,              // kg
    Time as TimeQuantity,        // s
    ElectricCurrent as Amperes,  // A
    ThermodynamicTemperature as Kelvin,  // K
};

// — Derived mechanical quantities —
pub use uom::si::f64::{
    Velocity as VelocityMps,         // m/s
    Acceleration as AccelMps2,       // m/s²
    Force as ForceNewtons,           // N = kg·m/s²
    Energy as EnergyJoules,          // J = N·m
    Momentum as MomentumKgMps,       // kg·m/s
    Power as PowerWatts,             // W = J/s
    Frequency as Hertz,              // Hz = 1/s
    Pressure as PressurePascals,     // Pa = N/m²
};

// — Electromagnetic quantities —
pub use uom::si::f64::{
    ElectricCharge as ChargeCoulombs,  // C = A·s
    ElectricPotential as Voltage,      // V = J/C
    ElectricField as ElectricFieldStrength,  // V/m
    MagneticFluxDensity as MagneticFieldStrength,  // T = V·s/m²
    Capacitance as Farads,             // F = C/V
    Resistance as Ohms,                // Ω = V/A
    Conductance as Siemens,            // S = A/V
    Inductance as Henrys,              // H = Wb/A
};

// — Unit re-exports for construction —
pub use uom::si::length::meter;
pub use uom::si::mass::kilogram;
pub use uom::si::time::second;
pub use uom::si::electric_charge::coulomb;
pub use uom::si::electric_potential::volt;
pub use uom::si::force::newton;
pub use uom::si::energy::joule;
pub use uom::si::velocity::meter_per_second;
pub use uom::si::frequency::hertz;
// etc.
```

### Vector quantity wrapper

`uom` wraps scalars, not 3D vectors. Every field quantity in the codebase
travels as `DVec3` (electric field, magnetic field, force, velocity,
acceleration, momentum). We need a type that pairs a dimension with a `DVec3`.

```rust
/// A 3D vector carrying compile-time unit information.
///
/// `Q` is a `uom` quantity (e.g., `ForceNewtons`, `ElectricFieldStrength`).
/// `Inner` only exists to distinguish `Vec3<ForceNewtons>` from `Vec3<VelocityMps>`
/// at the type level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantity3<Q> {
    pub x: Q,
    pub y: Q,
    pub z: Q,
}
```

With conversion to/from `DVec3`:

```rust
impl<Q> Quantity3<Q>
where
    Q: uom::typenum::Copy + Into<f64>,
{
    pub fn to_dvec3(self) -> DVec3 {
        DVec3::new(self.x.into(), self.y.into(), self.z.into())
    }

    pub fn from_dvec3(v: DVec3, make: impl Fn(f64) -> Q) -> Self {
        Self { x: make(v.x), y: make(v.y), z: make(v.z) }
    }
}
```

Domain-specific accessors:

```rust
pub type ForceVector = Quantity3<ForceNewtons>;
pub type ElectricFieldVector = Quantity3<ElectricFieldStrength>;
pub type MagneticFieldVector = Quantity3<MagneticFieldStrength>;
pub type VelocityVector = Quantity3<VelocityMps>;
pub type MomentumVector = Quantity3<MomentumKgMps>;
```

### `Quantity`/`VectorQuantity` conversion bridge

The existing runtime types are used for serialization and schema validation.
They remain unchanged, but gain constructors from `uom` types:

```rust
// In units.rs
impl Quantity {
    pub fn from_mass(mass: MassKg) -> Result<Self, QuantityError> {
        Self::new(mass.value, Dimension::MASS)
    }
    pub fn to_mass(self) -> Option<MassKg> {
        (self.dimension == Dimension::MASS)
            .then(|| MassKg::new::<kilogram>(self.si_value))
    }
    // ... same for Charge, Length, Time, Force, Energy, Velocity, Acceleration
}
```

### Scalar type replacement in `time.rs`

```rust
// Preserve the validated newtype, but wrap uom TimeQuantity inside:
pub struct TimeStep(TimeQuantity);

impl TimeStep {
    pub fn from_seconds(seconds: f64) -> Result<Self, TimeStepError> {
        let quantity = TimeQuantity::new::<second>(seconds);
        if !quantity.is_finite() || quantity <= TimeQuantity::new::<second>(0.0) {
            return Err(TimeStepError::Invalid { seconds });
        }
        Ok(Self(quantity))
    }

    pub fn seconds(self) -> f64 {
        self.0.get::<second>()
    }

    pub fn quantity(self) -> TimeQuantity {
        self.0
    }
}
```

### What stays raw in Phase 0

- `units.rs`: `lorentz_factor`, `relativistic_momentum`, `relativistic_kinetic_energy`
  — these take and return `DVec3`/`f64`. They accept typed arguments through
  accessor methods but keep internal `f64` arithmetic. Phase 2 converts them.
- `sampling.rs`: `FieldColumn { Scalar(Arc<[f64]>), Vector(Arc<[DVec3]>) }`
  — bulk storage stays raw. Typed accessors are added later when consumers
  request typed slices.
- `schema.rs`: `PropertyValue`, `PropertyKind`, `FieldValue` — keep existing
  `Quantity`/`VectorQuantity` runtime types. Add `uom`-aware constructors.
- `source_geometry.rs`: `CoupledSource<T = f64>` — Phase 1 addition of
  `CoupledSource<ChargeCoulombs>` etc. alongside existing impls.

### Downstream crate impact

**Phase 0 breaks zero downstream crates.** All changes are additive:
- New types are defined and exported.
- No existing type is removed or changed in signature.
- `TimeStep` gains a `TimeQuantity` inside; its public API (`from_seconds`,
  `seconds()`, `FromStr`) is unchanged.

Downstream crates can begin adopting the new types in their own time — which is
Phase 1.

### Verification

- `cargo build -p fieldcad-core` compiles.
- Existing tests pass unchanged.
- New tests verify:
  - `Quantity3::to_dvec3`/`from_dvec3` round trips.
  - `TimeStep` wraps a `TimeQuantity` and rejects non-finite/zero values.
  - Quantity aliases construct correctly from SI values.
  - `Quantity::from_mass(mass).unwrap().to_mass()` round-trips for valid
    dimension.

---

## Phase 1 (outline) — data boundary migration

Replace raw `f64` with typed quantities at data-description boundaries:

- **`particles/lib.rs`**: Constants become `MassKg`, `ChargeCoulombs`.
  `Particle{ mass_kg, charge_coulombs }` fields typed.
- **`sources/lib.rs`**: `inertial_mass_of` → returns `MassKg`.
  `gravitational_mass_of` → returns `MassKg`.
- **`electromagnetic-sources/lib.rs`**: `charge_coulombs` extraction returns
  `ChargeCoulombs`. `ChargeSource.coupling_value` typed.
- **`source_geometry.rs`**: `CoupledSource<MassKg>`, `CoupledSource<ChargeCoulombs>`
  impls.
- **`plugin-api/lib.rs`**: `DynamicBody{ inertial_mass_kg: MassKg }`, etc.
- **`schema.rs`**: `PropertyBag::typed_mass()` accessor that returns `Option<MassKg>`.

**Risk**: Changes serializable struct fields. Mitigation: `uom` with `autoconvert`
feature serialises as the underlying `f64`, so wire format is unchanged for
types like `TimeStep(f64)` where the inner type is a `uom` quantity.

---

## Phase 2 (outline) — computation migration

Typed signatures for computational functions:

- **`dynamics/lib.rs`**: `integrate(&[DynamicBody], &[ForceVector], TimeQuantity) → Vec<ObjectKinematicsUpdate>`. Force accumulation sums typed vectors.
- **`superposition/lib.rs`**: Generic `InverseSquareSource<T>` where `T = MassKg | ChargeCoulombs`. Field and potential results typed.
- **`electrostatics/lib.rs`**: `COULOMB_CONSTANT` as `uom` constant. `evaluate_sources` returns typed samples.
- **`electromagnetism/lib.rs`**: Vacuum constants, Yee field arrays, Courant limit, diagnostics all typed.
- **`newtonian-gravity/lib.rs`**: `GRAVITATIONAL_CONSTANT` typed. Acceleration and potential typed.

**Risk**: Changes function signatures throughout. Mitigation: parallel old+new
impls during transition; deprecate old signatures.

---

## Phase 3 (outline) — plugin trait boundary

- **`EquationSystemSolver::forces()`** returns `Vec<ForceVector>` instead of
  `Vec<DVec3>`.
- **`EquationSystemSolver::sample()`** constructs `SampledColumn` from typed
  evaluations.
- **`FieldBrushStroke::strength`** stays `Quantity` (serialization type) but is
  converted to typed on use.

**Risk**: Plugin trait changes break all implementations. Mitigation: done once
at a milestone boundary, coordinated across all equation-system crates.

---

## Phase 4 (outline) — unit display

- `units.rs`: `Dimension::unit_symbol()` augmented or replaced with `uom`'s
  formatting. UI panels show `kg`, `C`, `V/m`, `T` from typed quantity
  information rather than matching magic strings.
- Property editor derives unit annotations from typed quantity metadata.

---

## Phase 5 (outline) — precision engine

After the type boundary is established:

- Identify accumulation hot spots (force sum, energy diagnostics, probe history)
  and apply compensated (Kahan) summation.
- Evaluate double-double (`two-float`) or `f128` for CPU reference oracles that
  need lower drift than `f64` alone provides.
- The typed boundary absorbs the engine swap: the kernel operates on `f64` or
  `DoubleDouble`, but the surrounding code sees `ForceNewtons` either way.

---

## Timeline and dependencies

```
Phase 0 ─────────────────────► (self-contained, ~1 session)
  │
  ├──► Phase 1 ─────────────► (requires Phase 0, ~2 sessions)
  │     │
  │     ├──► Phase 2 ───────► (requires Phases 0-1, ~2 sessions)
  │     │
  │     ├──► Phase 3 ───────► (requires Phase 0-2, ~1 session)
  │     │
  │     └──► Phase 4 ───────► (requires Phase 0-3, ~1 session)
  │
  └──► Phase 5 ─────────────► (independent of 1-4, ongoing)
```

---

## References

- [ADR 0004](adr/0004-si-units-in-the-core.md): SI in the core, conversion only
  at display (the existing dimension runtime).
- [ADR 0006](adr/0006-columnar-batched-field-sampling.md): Columnar sampling
  (the reason bulk storage stays raw).
- `docs/physical-quantities-precision-plan.md`: Prior research survey
  (superseded by this plan for actionable scope).
- `docs/adr/README.md`: ADR format and index.
