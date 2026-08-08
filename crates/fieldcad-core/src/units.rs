use std::fmt;

use glam::DVec3;
use serde::{Deserialize, Serialize};

// ── SI prefix infrastructure ────────────────────────────────────────────────

/// One SI prefix: a multiplier and the symbol(s) representing it.
#[derive(Clone, Copy, Debug)]
pub struct SiPrefix {
    pub multiplier: f64,
    pub symbol: &'static str,
}

/// All standard SI prefixes, ordered from largest (Y) to smallest (y).
///
/// The empty-symbol entry at multiplier 1.0 is the identity (no prefix).
pub const ALL_SI_PREFIXES: &[SiPrefix] = &[
    SiPrefix {
        multiplier: 1e24,
        symbol: "Y",
    },
    SiPrefix {
        multiplier: 1e21,
        symbol: "Z",
    },
    SiPrefix {
        multiplier: 1e18,
        symbol: "E",
    },
    SiPrefix {
        multiplier: 1e15,
        symbol: "P",
    },
    SiPrefix {
        multiplier: 1e12,
        symbol: "T",
    },
    SiPrefix {
        multiplier: 1e9,
        symbol: "G",
    },
    SiPrefix {
        multiplier: 1e6,
        symbol: "M",
    },
    SiPrefix {
        multiplier: 1e3,
        symbol: "k",
    },
    SiPrefix {
        multiplier: 1e2,
        symbol: "h",
    },
    SiPrefix {
        multiplier: 1e1,
        symbol: "da",
    },
    SiPrefix {
        multiplier: 1.0,
        symbol: "",
    },
    SiPrefix {
        multiplier: 1e-1,
        symbol: "d",
    },
    SiPrefix {
        multiplier: 1e-2,
        symbol: "c",
    },
    SiPrefix {
        multiplier: 1e-3,
        symbol: "m",
    },
    SiPrefix {
        multiplier: 1e-6,
        symbol: "µ",
    },
    // ASCII fallback for micro (also handles the lookalike μ via Unicode NFKD).
    SiPrefix {
        multiplier: 1e-6,
        symbol: "u",
    },
    SiPrefix {
        multiplier: 1e-9,
        symbol: "n",
    },
    SiPrefix {
        multiplier: 1e-12,
        symbol: "p",
    },
    SiPrefix {
        multiplier: 1e-15,
        symbol: "f",
    },
    SiPrefix {
        multiplier: 1e-18,
        symbol: "a",
    },
    SiPrefix {
        multiplier: 1e-21,
        symbol: "z",
    },
    SiPrefix {
        multiplier: 1e-24,
        symbol: "y",
    },
];

/// The speed of light in vacuum, exact by the SI definition of the metre.
///
/// Lives in the core because more than one system needs it and they must agree:
/// the relativistic integrator uses it to relate momentum to velocity, and a
/// time-domain field solver uses it for its stability limit. Two copies that
/// drifted would put the pusher and the field on different physics.
pub const SPEED_OF_LIGHT: f64 = 299_792_458.0;

/// The Lorentz factor, `γ = 1 / sqrt(1 − v²/c²)`.
///
/// Shared for the same reason [`SPEED_OF_LIGHT`] is: the dynamics integrator's
/// momentum-form leapfrog, the electromagnetic coupling's energy diagnostic,
/// and an inspector displaying a body's derived motion all need this to agree,
/// or "how fast is enough to matter relativistically" would be answered
/// differently by two consumers reading the same body.
///
/// Clamped rather than allowed to divide by zero or go complex: a caller
/// computing this from a value it does not otherwise control (a UI reading a
/// possibly-stale snapshot) should get a large finite number rather than
/// `NaN`.
pub fn lorentz_factor(velocity: DVec3) -> f64 {
    let beta_squared = velocity.length_squared() / (SPEED_OF_LIGHT * SPEED_OF_LIGHT);
    (1.0 - beta_squared).max(f64::MIN_POSITIVE).sqrt().recip()
}

/// Relativistic momentum, `p = γmv`.
pub fn relativistic_momentum(velocity: DVec3, mass_kg: f64) -> DVec3 {
    velocity * (mass_kg * lorentz_factor(velocity))
}

/// Relativistic kinetic energy, `K = (γ−1)mc²`. Reduces to `½mv²` at low speed,
/// and excludes rest energy (`mc²`) deliberately: for a body far from
/// relativistic speeds, rest energy is many orders of magnitude larger than
/// the kinetic energy actually changing as it moves, and would swamp the one
/// number a display of "how much energy does this motion have" is for.
///
/// Computed as `β²mc² / (√(1−β²)(1+√(1−β²)))` rather than `(γ−1)mc²` directly.
/// Below relativistic speeds `γ` rounds to exactly `1.0` in `f64` — the
/// `β²/2` correction is smaller than an ULP of `1.0` for anything under
/// roughly 60 m/s — so subtracting `1.0` from it returns exactly zero no
/// matter the mass. This form multiplies through by the conjugate instead,
/// leaving a `β²` that is computed directly rather than recovered from a
/// near-cancelled subtraction, and it agrees with `(γ−1)mc²` everywhere both
/// are numerically meaningful.
pub fn relativistic_kinetic_energy(velocity: DVec3, mass_kg: f64) -> f64 {
    let beta_squared = velocity.length_squared() / (SPEED_OF_LIGHT * SPEED_OF_LIGHT);
    let root = (1.0 - beta_squared).max(f64::MIN_POSITIVE).sqrt();
    mass_kg * SPEED_OF_LIGHT * SPEED_OF_LIGHT * beta_squared / (root * (1.0 + root))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dimension {
    pub mass: i8,
    pub length: i8,
    pub time: i8,
    pub current: i8,
    pub temperature: i8,
    pub amount: i8,
    pub luminous_intensity: i8,
}

impl Dimension {
    pub const DIMENSIONLESS: Self = Self::new(0, 0, 0, 0, 0, 0, 0);
    pub const MASS: Self = Self::new(1, 0, 0, 0, 0, 0, 0);
    pub const LENGTH: Self = Self::new(0, 1, 0, 0, 0, 0, 0);
    pub const TIME: Self = Self::new(0, 0, 1, 0, 0, 0, 0);
    pub const CURRENT: Self = Self::new(0, 0, 0, 1, 0, 0, 0);
    pub const CHARGE: Self = Self::new(0, 0, 1, 1, 0, 0, 0);
    pub const VELOCITY: Self = Self::new(0, 1, -1, 0, 0, 0, 0);
    pub const ACCELERATION: Self = Self::new(0, 1, -2, 0, 0, 0, 0);
    /// Volts per metre.
    pub const ELECTRIC_FIELD: Self = Self::new(1, 1, -3, -1, 0, 0, 0);
    /// Volts.
    pub const ELECTRIC_POTENTIAL: Self = Self::new(1, 2, -3, -1, 0, 0, 0);
    /// Tesla.
    pub const MAGNETIC_FLUX_DENSITY: Self = Self::new(1, 0, -2, -1, 0, 0, 0);
    /// Joules per cubic metre.
    pub const ENERGY_DENSITY: Self = Self::new(1, -1, -2, 0, 0, 0, 0);
    /// Electric field divergence, volts per square metre.
    pub const ELECTRIC_FIELD_DIVERGENCE: Self = Self::new(1, 0, -3, -1, 0, 0, 0);
    /// Magnetic flux-density divergence, tesla per metre.
    pub const MAGNETIC_FIELD_DIVERGENCE: Self = Self::new(1, -1, -2, -1, 0, 0, 0);

    /// The root unit symbol that SI prefixes attach to, together with the
    /// conversion factor from that root unit to the SI base unit.
    ///
    /// Returns `None` for compound dimensions where prefix decomposition is
    /// ambiguous (VELOCITY, ACCELERATION, ELECTRIC_FIELD, …). For simple
    /// dimensions the root differs from the display [`unit_symbol`] only for
    /// [`MASS`](Self::MASS): the display is `"kg"` (the SI base unit with its
    /// conventional prefix), while the prefix root is `"g"` (gram) with a
    /// conversion of `0.001` — a value expressed in grams must be divided by
    /// 1000 to obtain the stored kilogram value.
    ///
    /// ```
    /// # use fieldcad_core::Dimension;
    /// assert_eq!(Dimension::MASS.si_prefix_root(), Some(("g", 0.001)));
    /// assert_eq!(Dimension::LENGTH.si_prefix_root(), Some(("m", 1.0)));
    /// assert_eq!(Dimension::VELOCITY.si_prefix_root(), None);
    /// ```
    pub fn si_prefix_root(self) -> Option<(&'static str, f64)> {
        match self {
            Self::MASS => Some(("g", 0.001)),
            Self::LENGTH => Some(("m", 1.0)),
            Self::TIME => Some(("s", 1.0)),
            Self::CURRENT => Some(("A", 1.0)),
            Self::CHARGE => Some(("C", 1.0)),
            Self::ELECTRIC_POTENTIAL => Some(("V", 1.0)),
            Self::MAGNETIC_FLUX_DENSITY => Some(("T", 1.0)),
            _ => None,
        }
    }

    /// The familiar symbol for this dimension, where SI names one.
    ///
    /// [`Display`](fmt::Display) always spells a dimension out in base units,
    /// which is unambiguous but reads badly in a property editor: charge shows
    /// as `s A` rather than `C`. A generic, schema-driven editor has only the
    /// dimension to work from, so the lookup lives here — this is knowledge
    /// about SI, not about any one plugin's component.
    pub fn unit_symbol(self) -> String {
        let named = [
            (Self::DIMENSIONLESS, ""),
            (Self::MASS, "kg"),
            (Self::LENGTH, "m"),
            (Self::TIME, "s"),
            (Self::CURRENT, "A"),
            (Self::CHARGE, "C"),
            (Self::VELOCITY, "m/s"),
            (Self::ACCELERATION, "m/s²"),
            (Self::ELECTRIC_FIELD, "V/m"),
            (Self::ELECTRIC_POTENTIAL, "V"),
            (Self::MAGNETIC_FLUX_DENSITY, "T"),
            (Self::ENERGY_DENSITY, "J/m³"),
        ];
        named
            .into_iter()
            .find(|(dimension, _)| *dimension == self)
            .map_or_else(|| self.to_string(), |(_, symbol)| symbol.to_owned())
    }

    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        mass: i8,
        length: i8,
        time: i8,
        current: i8,
        temperature: i8,
        amount: i8,
        luminous_intensity: i8,
    ) -> Self {
        Self {
            mass,
            length,
            time,
            current,
            temperature,
            amount,
            luminous_intensity,
        }
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let units = [
            ("kg", self.mass),
            ("m", self.length),
            ("s", self.time),
            ("A", self.current),
            ("K", self.temperature),
            ("mol", self.amount),
            ("cd", self.luminous_intensity),
        ];
        let mut wrote_unit = false;
        for (unit, exponent) in units {
            if exponent == 0 {
                continue;
            }
            if wrote_unit {
                formatter.write_str(" ")?;
            }
            formatter.write_str(unit)?;
            if exponent != 1 {
                write!(formatter, "^{exponent}")?;
            }
            wrote_unit = true;
        }
        if !wrote_unit {
            formatter.write_str("1")?;
        }
        Ok(())
    }
}

// ── SI prefix parsing and formatting ────────────────────────────────────────

/// Parse a string containing a number optionally followed by an SI prefix and
/// the dimension's prefix root unit, returning the value in SI base units.
///
/// Returns `None` for compound dimensions (where [`Dimension::si_prefix_root`]
/// returns `None`) — callers should fall back to plain `f64` parsing.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(parse_si_value("1.2mm", Dimension::LENGTH), Some(0.0012));
/// assert_eq!(parse_si_value("1.2kg", Dimension::MASS), Some(1.2));
/// assert_eq!(parse_si_value("1.2g", Dimension::MASS), Some(0.0012));
/// assert_eq!(parse_si_value("1.23ns", Dimension::TIME), Some(1.23e-9));
/// assert_eq!(parse_si_value("4.43e-3", Dimension::TIME), Some(4.43e-3));
/// ```
///
/// The kg case works because the prefix root for [`Dimension::MASS`] is `"g"`
/// (gram): `"kg"` is decomposed as prefix `"k"` (×1000) + root `"g"`, giving
/// `1.2 × 1000 × 0.001 = 1.2 kg`.
pub fn parse_si_value(input: &str, dimension: Dimension) -> Option<f64> {
    let input = input.trim();
    let (root, conversion) = dimension.si_prefix_root()?;

    // Try plain number first (no suffix at all).
    if let Ok(value) = input.parse::<f64>() {
        return value.is_finite().then_some(value);
    }

    // Try each prefix. Build the suffix as `prefix_symbol + root` and check
    // whether the input ends with it.  Also try a spaced variant
    // `prefix_symbol + " " + root` so "2.5 µs" works.
    for prefix in ALL_SI_PREFIXES {
        let suffix = format!("{}{}", prefix.symbol, root);
        if let Some(number) = input.strip_suffix(&suffix)
            && let Ok(value) = number.trim().parse::<f64>()
        {
            return Some(value * prefix.multiplier * conversion);
        }
        // Check `prefix_symbol + " " + root` for readability (e.g. "2.5 mm").
        if !prefix.symbol.is_empty()
            && !root.is_empty()
            && let Some(number) = input.strip_suffix(&format!("{} {}", prefix.symbol, root))
            && let Ok(value) = number.trim().parse::<f64>()
        {
            return Some(value * prefix.multiplier * conversion);
        }
    }

    // Try bare root (no prefix) — e.g. "5 g" → 0.005 kg.
    if !root.is_empty()
        && let Some(number) = input
            .strip_suffix(root)
            .or_else(|| input.strip_suffix(&format!(" {root}")))
        && let Ok(value) = number.trim().parse::<f64>()
    {
        return Some(value * conversion);
    }

    None
}

/// Format a value in SI base units as a human-readable string using an
/// appropriate SI prefix.
///
/// Returns [`None`] for compound dimensions — callers should fall back to
/// [`format_engineering`]-style display with the unit symbol appended.
///
/// The algorithm converts from SI base to the dimension's prefix root, then
/// selects the prefix that brings the numeric part into the range `[1, 1000)`.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(format_si_value(0.0012, Dimension::LENGTH), "1.2 mm");
/// assert_eq!(format_si_value(1.2, Dimension::MASS), "1.2 kg");
/// assert_eq!(format_si_value(0.0012, Dimension::MASS), "1.2 g");
/// ```
pub fn format_si_value(value: f64, dimension: Dimension) -> Option<String> {
    let (root, conversion) = dimension.si_prefix_root()?;

    let root_value = value / conversion;

    if root_value == 0.0 {
        return Some(if root.is_empty() {
            "0".to_owned()
        } else {
            format!("0 {root}")
        });
    }

    let abs = root_value.abs();

    // Find the first (largest) SI prefix whose multiplier is ≤ abs.
    // This brings the displayed numeric part into [1, 1000).
    for prefix in ALL_SI_PREFIXES {
        if abs >= prefix.multiplier {
            let scaled = root_value / prefix.multiplier;
            let formatted = if scaled.fract() == 0.0 {
                format!("{}", scaled as i64)
            } else {
                // Use default Display (reasonably short, matches format_time_step).
                format!("{}", scaled)
            };
            return Some(format!("{formatted} {}{root}", prefix.symbol));
        }
    }

    // Too small for any prefix — use scientific notation on the root value.
    Some(format!("{:.6e} {root}", root_value))
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum QuantityError {
    #[error("physical quantity must be finite, received {value}")]
    NonFiniteScalar { value: f64 },
    #[error("physical vector must be finite, received {value:?}")]
    NonFiniteVector { value: DVec3 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quantity {
    si_value: f64,
    dimension: Dimension,
}

impl Quantity {
    pub fn new(si_value: f64, dimension: Dimension) -> Result<Self, QuantityError> {
        if !si_value.is_finite() {
            return Err(QuantityError::NonFiniteScalar { value: si_value });
        }
        Ok(Self {
            si_value,
            dimension,
        })
    }

    pub const fn si_value(self) -> f64 {
        self.si_value
    }

    pub const fn dimension(self) -> Dimension {
        self.dimension
    }
}

// --- From impls: typed uom quantities → runtime Quantity --------------------

macro_rules! impl_quantity_from {
    ($uom_ty:ty, $dim:expr) => {
        impl From<$uom_ty> for Quantity {
            fn from(value: $uom_ty) -> Self {
                Self::new(value.value, $dim).expect("uom value is always finite")
            }
        }
    };
}

impl_quantity_from!(crate::quantities::MassKg, Dimension::MASS);
impl_quantity_from!(crate::quantities::LengthMetres, Dimension::LENGTH);
impl_quantity_from!(crate::quantities::TimeQuantity, Dimension::TIME);
impl_quantity_from!(crate::quantities::ChargeCoulombs, Dimension::CHARGE);
impl_quantity_from!(crate::quantities::VelocityMps, Dimension::VELOCITY);
impl_quantity_from!(crate::quantities::AccelMps2, Dimension::ACCELERATION);
impl_quantity_from!(crate::quantities::ForceNewtons, Dimension::MASS);
impl_quantity_from!(crate::quantities::EnergyJoules, Dimension::MASS);

// --- From impls: typed vector quantities → runtime VectorQuantity ----------

macro_rules! impl_vector_quantity_from {
    ($vec_ty:ty, $dim:expr) => {
        impl From<$vec_ty> for VectorQuantity {
            fn from(value: $vec_ty) -> Self {
                Self::new(value.to_dvec3(), $dim).expect("uom vector value is always finite")
            }
        }
    };
}

impl_vector_quantity_from!(
    crate::quantities::ElectricFieldVector,
    Dimension::ELECTRIC_FIELD
);
impl_vector_quantity_from!(
    crate::quantities::MagneticFieldVector,
    Dimension::MAGNETIC_FLUX_DENSITY
);
impl_vector_quantity_from!(crate::quantities::ForceVector, Dimension::MASS);
impl_vector_quantity_from!(crate::quantities::VelocityVector, Dimension::VELOCITY);

// --- Try-into helpers: runtime Quantity → typed uom quantity ----------------

impl Quantity {
    pub fn try_into_mass(self) -> Option<crate::quantities::MassKg> {
        (self.dimension == Dimension::MASS)
            .then(|| crate::quantities::MassKg::new::<crate::quantities::kilogram>(self.si_value))
    }

    pub fn try_into_length(self) -> Option<crate::quantities::LengthMetres> {
        (self.dimension == Dimension::LENGTH).then(|| {
            crate::quantities::LengthMetres::new::<crate::quantities::meter>(self.si_value)
        })
    }

    pub fn try_into_time(self) -> Option<crate::quantities::TimeQuantity> {
        (self.dimension == Dimension::TIME).then(|| {
            crate::quantities::TimeQuantity::new::<crate::quantities::second>(self.si_value)
        })
    }

    pub fn try_into_charge(self) -> Option<crate::quantities::ChargeCoulombs> {
        (self.dimension == Dimension::CHARGE).then(|| {
            crate::quantities::ChargeCoulombs::new::<crate::quantities::coulomb>(self.si_value)
        })
    }

    pub fn try_into_velocity(self) -> Option<crate::quantities::VelocityMps> {
        (self.dimension == Dimension::VELOCITY).then(|| {
            crate::quantities::VelocityMps::new::<crate::quantities::meter_per_second>(
                self.si_value,
            )
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VectorQuantity {
    si_value: DVec3,
    dimension: Dimension,
}

impl VectorQuantity {
    pub fn new(si_value: DVec3, dimension: Dimension) -> Result<Self, QuantityError> {
        if !si_value.is_finite() {
            return Err(QuantityError::NonFiniteVector { value: si_value });
        }
        Ok(Self {
            si_value,
            dimension,
        })
    }

    pub const fn si_value(self) -> DVec3 {
        self.si_value
    }

    pub const fn dimension(self) -> Dimension {
        self.dimension
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_uses_si_current_times_time_dimensions() {
        assert_eq!(Dimension::CHARGE.current, 1);
        assert_eq!(Dimension::CHARGE.time, 1);
    }

    #[test]
    fn non_finite_quantities_are_rejected() {
        assert!(Quantity::new(f64::NAN, Dimension::MASS).is_err());
        assert!(VectorQuantity::new(DVec3::INFINITY, Dimension::LENGTH).is_err());
    }

    #[test]
    fn dimensions_have_compact_si_labels() {
        assert_eq!(Dimension::VELOCITY.to_string(), "m s^-1");
        assert_eq!(Dimension::DIMENSIONLESS.to_string(), "1");
        assert_eq!(Dimension::ELECTRIC_FIELD.to_string(), "kg m s^-3 A^-1");
    }

    #[test]
    fn lorentz_factor_is_one_at_rest_and_grows_toward_light_speed() {
        assert_eq!(lorentz_factor(DVec3::ZERO), 1.0);
        assert!(lorentz_factor(DVec3::X * SPEED_OF_LIGHT * 0.99) > 7.0);
    }

    #[test]
    fn relativistic_kinetic_energy_matches_the_classical_limit_at_low_speed() {
        let velocity = DVec3::X * 1.0e6;
        let mass_kg = 2.0;
        let classical = 0.5 * mass_kg * velocity.length_squared();

        let relativistic = relativistic_kinetic_energy(velocity, mass_kg);
        assert!(
            ((relativistic - classical) / classical).abs() < 1.0e-3,
            "relativistic {relativistic} vs classical {classical}"
        );
    }

    #[test]
    fn relativistic_kinetic_energy_is_nonzero_at_everyday_speed() {
        // At v = 1 m/s, γ rounds to exactly 1.0 in f64 — well below the
        // ~60 m/s where (γ−1) still has a representable bit. A naive
        // `(γ−1)mc²` returns exactly zero here for any mass; a walking-pace
        // body must still show a nonzero kinetic energy.
        let velocity = DVec3::Y;
        let mass_kg = 1.0;
        let classical = 0.5 * mass_kg * velocity.length_squared();

        let relativistic = relativistic_kinetic_energy(velocity, mass_kg);
        assert!(
            ((relativistic - classical) / classical).abs() < 1.0e-9,
            "relativistic {relativistic} vs classical {classical}"
        );
    }

    #[test]
    fn relativistic_momentum_reduces_to_mv_at_low_speed() {
        let velocity = DVec3::new(3.0, 0.0, 0.0);
        let mass_kg = 5.0;

        let momentum = relativistic_momentum(velocity, mass_kg);

        assert!((momentum - velocity * mass_kg).length() < 1.0e-9);
    }

    #[test]
    fn named_units_read_the_way_a_physicist_writes_them() {
        assert_eq!(Dimension::CHARGE.unit_symbol(), "C");
        assert_eq!(Dimension::MASS.unit_symbol(), "kg");
        assert_eq!(Dimension::DIMENSIONLESS.unit_symbol(), "");
        // An unnamed combination still has to render as something correct.
        assert_eq!(
            Dimension::ELECTRIC_FIELD_DIVERGENCE.unit_symbol(),
            Dimension::ELECTRIC_FIELD_DIVERGENCE.to_string()
        );
    }

    // ── SI prefix tests ────────────────────────────────────────────────────

    #[test]
    fn si_prefix_root_returns_root_for_simple_dimensions_and_none_for_compound() {
        assert_eq!(Dimension::MASS.si_prefix_root(), Some(("g", 0.001)));
        assert_eq!(Dimension::LENGTH.si_prefix_root(), Some(("m", 1.0)));
        assert_eq!(Dimension::TIME.si_prefix_root(), Some(("s", 1.0)));
        assert_eq!(Dimension::CHARGE.si_prefix_root(), Some(("C", 1.0)));
        assert_eq!(Dimension::VELOCITY.si_prefix_root(), None);
        assert_eq!(Dimension::ACCELERATION.si_prefix_root(), None);
        assert_eq!(Dimension::ELECTRIC_FIELD.si_prefix_root(), None);
    }

    #[test]
    fn parse_si_value_handles_plain_numbers() {
        assert!((parse_si_value("432", Dimension::TIME).unwrap() - 432.0).abs() < 1.0e-14);
        assert!((parse_si_value("4.43e-3", Dimension::TIME).unwrap() - 4.43e-3).abs() < 1.0e-17);
    }

    #[test]
    fn parse_si_value_handles_si_prefix_on_length() {
        let result = parse_si_value("1.2mm", Dimension::LENGTH).unwrap();
        assert!((result - 0.0012).abs() < 1.0e-17, "1.2mm → {result}");
        let result = parse_si_value("5 km", Dimension::LENGTH).unwrap();
        assert!((result - 5000.0).abs() < 1.0e-12, "5 km → {result}");
    }

    #[test]
    fn parse_si_value_handles_kg_as_prefix_plus_root() {
        // "kg" = prefix "k" (1000) + root "g" → 1.2 × 1000 × 0.001 = 1.2 kg
        let result = parse_si_value("1.2kg", Dimension::MASS).unwrap();
        assert!((result - 1.2).abs() < 1.0e-15, "1.2kg → {result}");
        // "g" = bare root → 5 × 0.001 = 0.005 kg
        let result = parse_si_value("5g", Dimension::MASS).unwrap();
        assert!((result - 0.005).abs() < 1.0e-17, "5g → {result}");
        // "mg" = prefix "m" (1e-3) + root "g" → 500 × 1e-3 × 0.001 = 0.0005 kg
        let result = parse_si_value("500mg", Dimension::MASS).unwrap();
        assert!((result - 0.0005).abs() < 1.0e-17, "500mg → {result}");
    }

    #[test]
    fn parse_si_value_handles_time_prefixes() {
        let result = parse_si_value("1.23ns", Dimension::TIME).unwrap();
        assert!((result - 1.23e-9).abs() < 1.0e-22, "1.23ns → {result}");
        let result = parse_si_value("2.5 µs", Dimension::TIME).unwrap();
        assert!((result - 2.5e-6).abs() < 1.0e-16, "2.5µs → {result}");
        let result = parse_si_value("7.3213e-4ms", Dimension::TIME).unwrap();
        assert!(
            (result - 7.3213e-7).abs() < 1.0e-17,
            "7.3213e-4ms → {result}"
        );
    }

    #[test]
    fn parse_si_value_returns_none_for_compound_dimensions() {
        assert_eq!(parse_si_value("1.2m/s", Dimension::VELOCITY), None);
        assert!(parse_si_value("1.2", Dimension::VELOCITY).is_none());
    }

    #[test]
    fn parse_si_value_returns_none_for_unparseable_input() {
        assert_eq!(parse_si_value("abc", Dimension::LENGTH), None);
        assert_eq!(parse_si_value("NaN", Dimension::LENGTH), None);
    }

    #[test]
    fn format_si_value_formats_simple_dimensions_with_prefix() {
        assert_eq!(
            format_si_value(0.0012, Dimension::LENGTH).unwrap(),
            "1.2 mm"
        );
        assert_eq!(format_si_value(1.0, Dimension::LENGTH).unwrap(), "1 m");
        assert_eq!(format_si_value(5000.0, Dimension::LENGTH).unwrap(), "5 km");
    }

    #[test]
    fn format_si_value_handles_mass_round_trip() {
        // 1.2 kg → root = 1200 g → prefix k → "1.2 kg"
        assert_eq!(format_si_value(1.2, Dimension::MASS).unwrap(), "1.2 kg");
        // 0.0012 kg → root = 1.2 g → no prefix → "1.2 g"
        assert_eq!(format_si_value(0.0012, Dimension::MASS).unwrap(), "1.2 g");
        // 0.0000012 kg → root = 0.0012 g → prefix m → "1.2 mg"
        assert_eq!(
            format_si_value(0.0000012, Dimension::MASS).unwrap(),
            "1.2 mg"
        );
    }

    #[test]
    fn format_si_value_returns_none_for_compound_dimensions() {
        assert_eq!(format_si_value(1.0, Dimension::VELOCITY), None);
        assert_eq!(format_si_value(1.0, Dimension::ACCELERATION), None);
        assert_eq!(format_si_value(1.0, Dimension::ELECTRIC_FIELD), None);
    }

    #[test]
    fn format_si_value_handles_zero() {
        assert_eq!(format_si_value(0.0, Dimension::LENGTH).unwrap(), "0 m");
        assert_eq!(format_si_value(0.0, Dimension::MASS).unwrap(), "0 g");
    }
}
