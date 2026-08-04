use std::fmt;

use glam::DVec3;
use serde::{Deserialize, Serialize};

/// The speed of light in vacuum, exact by the SI definition of the metre.
///
/// Lives in the core because more than one system needs it and they must agree:
/// the relativistic integrator uses it to relate momentum to velocity, and a
/// time-domain field solver uses it for its stability limit. Two copies that
/// drifted would put the pusher and the field on different physics.
pub const SPEED_OF_LIGHT: f64 = 299_792_458.0;

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
}
