//! Compile-time typed physical quantities built on `uom`.
//!
//! Each quantity wrapper names its SI unit so that a function signature
//! communicates what it expects without requiring a comment. A function taking
//! `MassKg` expects kilograms; one taking `ChargeCoulombs` expects coulombs.
//! Arithmetic between different dimensions is caught at compile time.
//!
//! Vector quantities use [`Quantity3<Q>`], a 3‑vector of scalar `uom` types
//! with a `to_dvec3()` / `from_dvec3()` conversion boundary. Computation
//! kernels stay on raw `DVec3`/`f64`; this module owns the typed boundary
//! around them.

use glam::DVec3;

// ---------------------------------------------------------------------------
// Scalar quantity aliases
// ---------------------------------------------------------------------------

pub use uom::si::f64::{
    Acceleration as AccelMps2, Capacitance as CapacitanceFarads, ElectricCharge as ChargeCoulombs,
    ElectricCurrent as CurrentAmperes, ElectricField as ElectricFieldStrength,
    ElectricPotential as Voltage, ElectricalConductance as ConductanceSiemens,
    ElectricalResistance as ResistanceOhms, Energy as EnergyJoules, Force as ForceNewtons,
    Frequency as FrequencyHertz, Inductance as InductanceHenrys, Length as LengthMetres,
    MagneticFluxDensity as MagneticFieldStrength, Mass as MassKg, Momentum as MomentumKgMps,
    Power as PowerWatts, Pressure as PressurePascals, ThermodynamicTemperature as Kelvin,
    Time as TimeQuantity, Velocity as VelocityMps,
};

// ---------------------------------------------------------------------------
// Unit re-exports for construction:  `MassKg::new::<kilogram>(5.0)`
// ---------------------------------------------------------------------------

pub use uom::si::acceleration::meter_per_second_squared;
pub use uom::si::capacitance::farad;
pub use uom::si::electric_charge::coulomb;
pub use uom::si::electric_current::ampere;
pub use uom::si::electric_field::volt_per_meter;
pub use uom::si::electric_potential::volt;
pub use uom::si::electrical_conductance::siemens;
pub use uom::si::electrical_resistance::ohm;
pub use uom::si::energy::joule;
pub use uom::si::force::newton;
pub use uom::si::frequency::hertz;
pub use uom::si::inductance::henry;
pub use uom::si::length::meter;
pub use uom::si::magnetic_flux_density::tesla;
pub use uom::si::mass::kilogram;
pub use uom::si::momentum::kilogram_meter_per_second;
pub use uom::si::power::watt;
pub use uom::si::pressure::pascal;
pub use uom::si::thermodynamic_temperature::kelvin;
pub use uom::si::time::second;
pub use uom::si::velocity::meter_per_second;

// Also re-export the base `uom::si` modules so downstream crates can use
// .get::<Unit>() with the same units (e.g. `mass.get::<kilogram>()`).
pub use uom::si::acceleration;
pub use uom::si::capacitance;
pub use uom::si::electric_charge;
pub use uom::si::electric_current;
pub use uom::si::electric_field;
pub use uom::si::electric_potential;
pub use uom::si::electrical_conductance;
pub use uom::si::electrical_resistance;
pub use uom::si::energy;
pub use uom::si::force;
pub use uom::si::frequency;
pub use uom::si::inductance;
pub use uom::si::length;
pub use uom::si::magnetic_flux_density;
pub use uom::si::mass;
pub use uom::si::momentum;
pub use uom::si::power;
pub use uom::si::pressure;
pub use uom::si::thermodynamic_temperature;
pub use uom::si::time;
pub use uom::si::velocity;

// ---------------------------------------------------------------------------
// SiScalar — extract / construct the SI f64 value
// ---------------------------------------------------------------------------

/// A scalar quantity whose SI value can be extracted as `f64` and
/// reconstructed from one.
///
/// Every `uom` quantity with an SI base unit implements this. The
/// `.value` field on a `uom::si::f64::*` quantity is already the SI
/// magnitude; this trait just names that access consistently.
pub trait SiScalar: Sized + Copy {
    /// The SI magnitude (e.g. for a `MassKg`, the value in kilograms).
    fn into_si(self) -> f64;

    /// Construct from an SI magnitude.
    fn from_si(value: f64) -> Self;
}

macro_rules! impl_si_scalar {
    ($ty:ty, $unit:path) => {
        impl SiScalar for $ty {
            fn into_si(self) -> f64 {
                self.value
            }
            fn from_si(value: f64) -> Self {
                <$ty>::new::<$unit>(value)
            }
        }
    };
}

impl_si_scalar!(AccelMps2, meter_per_second_squared);
impl_si_scalar!(CapacitanceFarads, farad);
impl_si_scalar!(ChargeCoulombs, coulomb);
impl_si_scalar!(CurrentAmperes, ampere);
impl_si_scalar!(ElectricFieldStrength, volt_per_meter);
impl_si_scalar!(Voltage, volt);
impl_si_scalar!(ConductanceSiemens, siemens);
impl_si_scalar!(ResistanceOhms, ohm);
impl_si_scalar!(EnergyJoules, joule);
impl_si_scalar!(ForceNewtons, newton);
impl_si_scalar!(FrequencyHertz, hertz);
impl_si_scalar!(InductanceHenrys, henry);
impl_si_scalar!(LengthMetres, meter);
impl_si_scalar!(MagneticFieldStrength, tesla);
impl_si_scalar!(MassKg, kilogram);
impl_si_scalar!(MomentumKgMps, kilogram_meter_per_second);
impl_si_scalar!(PowerWatts, watt);
impl_si_scalar!(PressurePascals, pascal);
impl_si_scalar!(Kelvin, kelvin);
impl_si_scalar!(TimeQuantity, second);
impl_si_scalar!(VelocityMps, meter_per_second);

// ---------------------------------------------------------------------------
// Quantity3 — a 3‑vector of typed quantities
// ---------------------------------------------------------------------------

/// A 3‑dimensional vector of a typed physical quantity.
///
/// Each component is the same `uom` scalar type, so the dimension is uniform
/// across the vector — an `ElectricFieldVector` has `V/m` in every axis.
///
/// # Boundary pattern
///
/// ```ignore
/// // Typed entry point
/// fn compute_force(charge: ChargeCoulombs, field: ElectricFieldVector) -> ForceVector { … }
///
/// // Inside the kernel, operate on raw DVec3
/// let f: DVec3 = field.to_dvec3() * charge.into_si();
/// ForceVector::from_dvec3(f)
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantity3<Q> {
    pub x: Q,
    pub y: Q,
    pub z: Q,
}

// Re-use the glam vector type for dimensionless and dimension-annotated
// spatial types that are already well-known.
pub type PositionVector = Quantity3<LengthMetres>;
pub type VelocityVector = Quantity3<VelocityMps>;
pub type AccelerationVector = Quantity3<AccelMps2>;
pub type ForceVector = Quantity3<ForceNewtons>;
pub type MomentumVector = Quantity3<MomentumKgMps>;
pub type ElectricFieldVector = Quantity3<ElectricFieldStrength>;
pub type MagneticFieldVector = Quantity3<MagneticFieldStrength>;
pub type CurrentDensityVector = Quantity3<CurrentAmperes>;

impl<Q> Quantity3<Q> {
    pub const fn new(x: Q, y: Q, z: Q) -> Self {
        Self { x, y, z }
    }
}

impl<Q: SiScalar> Quantity3<Q> {
    /// Decompose into a raw `DVec3` with each component in SI units.
    pub fn to_dvec3(self) -> DVec3 {
        DVec3::new(self.x.into_si(), self.y.into_si(), self.z.into_si())
    }

    /// Build from a raw `DVec3` whose components are in SI units.
    pub fn from_dvec3(v: DVec3) -> Self {
        Self {
            x: Q::from_si(v.x),
            y: Q::from_si(v.y),
            z: Q::from_si(v.z),
        }
    }

    /// Apply a scalar function element-wise.
    pub fn map(self, f: impl Fn(Q) -> Q) -> Self {
        Self::new(f(self.x), f(self.y), f(self.z))
    }
}

impl<Q> From<Quantity3<Q>> for DVec3
where
    Q: SiScalar,
{
    fn from(v: Quantity3<Q>) -> Self {
        v.to_dvec3()
    }
}
