use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::quantities::{ChargeCoulombs, LengthMetres, MassKg, SiScalar, TimeQuantity};
use crate::{ChannelId, ComponentTypeId, Dimension, PropertyId, Quantity, VectorQuantity};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    Scalar(Quantity),
    Vector(VectorQuantity),
    Boolean(bool),
    Text(String),
    Choice(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyKind {
    Scalar(Dimension),
    Vector(Dimension),
    Boolean,
    Text,
    Choice(Vec<String>),
}

impl PropertyKind {
    /// A schema-valid neutral value of this kind.
    ///
    /// Attaching a component from a generic editor needs *some* value for every
    /// required property before the world will accept the edit. Zero is the
    /// honest neutral for a physical quantity: a body given mass but not yet a
    /// charge is uncharged, not invalid. A `Choice` with no options has no
    /// representable value, which is why this returns an `Option` rather than
    /// inventing an out-of-schema string.
    pub fn default_value(&self) -> Option<PropertyValue> {
        let value = match self {
            Self::Scalar(dimension) => PropertyValue::Scalar(Quantity::new(0.0, *dimension).ok()?),
            Self::Vector(dimension) => {
                PropertyValue::Vector(VectorQuantity::new(glam::DVec3::ZERO, *dimension).ok()?)
            }
            Self::Boolean => PropertyValue::Boolean(false),
            Self::Text => PropertyValue::Text(String::new()),
            Self::Choice(options) => PropertyValue::Choice(options.first()?.clone()),
        };
        Some(value)
    }
}

/// A condition on a sibling property within the same component.
///
/// Some properties only mean anything in a particular configuration of the
/// others: a gravitational mass is inert while it is declared equal to the
/// inertial one. Saying so in the schema lets a generic editor disable the
/// value rather than letting a user type a number that will be ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropertyCondition {
    /// The sibling whose value decides this.
    pub property: PropertyId,
    /// The value that sibling must hold.
    pub equals: PropertyValue,
    /// Why the property is inert, phrased for a tooltip.
    pub because: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropertySchema {
    pub id: PropertyId,
    pub display_name: String,
    /// Explanatory text shown as a tooltip on the property label in a generic
    /// inspector. Use this to clarify what a control means or how it relates
    /// to other properties in the same component.
    #[serde(default)]
    pub description: Option<String>,
    pub kind: PropertyKind,
    pub required: bool,
    /// When set, this property only takes effect while the named sibling holds
    /// the given value.
    ///
    /// This is presentation-relevance, not validity: an inert property is still
    /// stored and still validated, because turning the condition back on must
    /// return the value the user last chose rather than a blank.
    pub relevant_when: Option<PropertyCondition>,
    /// The value to attach before the user has edited anything.
    ///
    /// `None` falls back to [`PropertyKind::default_value`]. Declare one
    /// whenever the kind's neutral value is outside the range a consumer will
    /// accept: a mass of zero satisfies every dimension check and then fails the
    /// solver that has to divide by it, so the property that carries a
    /// constraint is the property that must carry a default satisfying it.
    pub default_value: Option<PropertyValue>,
}

impl PropertySchema {
    /// The value to use when attaching this property unedited.
    pub fn initial_value(&self) -> Option<PropertyValue> {
        self.default_value
            .clone()
            .or_else(|| self.kind.default_value())
    }

    /// Whether this property currently takes effect, given its siblings.
    ///
    /// A property with no condition is always relevant. A condition whose
    /// sibling is missing counts as unmet, so an incomplete bag disables the
    /// dependent field rather than offering an edit that may be discarded.
    pub fn is_relevant(&self, values: &PropertyBag) -> bool {
        self.relevant_when
            .as_ref()
            .is_none_or(|condition| values.get(&condition.property) == Some(&condition.equals))
    }

    pub fn validate(&self, value: &PropertyValue) -> Result<(), SchemaError> {
        let valid = match (&self.kind, value) {
            (PropertyKind::Scalar(expected), PropertyValue::Scalar(actual)) => {
                actual.dimension() == *expected
            }
            (PropertyKind::Vector(expected), PropertyValue::Vector(actual)) => {
                actual.dimension() == *expected
            }
            (PropertyKind::Boolean, PropertyValue::Boolean(_))
            | (PropertyKind::Text, PropertyValue::Text(_)) => true,
            (PropertyKind::Choice(options), PropertyValue::Choice(selected)) => {
                options.contains(selected)
            }
            _ => false,
        };

        if valid {
            Ok(())
        } else {
            Err(SchemaError::ValueMismatch {
                property: self.id.clone(),
                expected: self.kind.clone(),
            })
        }
    }
}

/// Check a bag of values against a set of property schemas.
///
/// Object components and plugin configuration are the same problem — a set of
/// declared, dimensioned, optionally-required properties — so they share one
/// implementation rather than two that drift apart.
pub fn validate_properties(
    schemas: &[PropertySchema],
    values: &PropertyBag,
) -> Result<(), SchemaError> {
    for (id, value) in values.iter() {
        let schema = schemas
            .iter()
            .find(|schema| schema.id == *id)
            .ok_or_else(|| SchemaError::UnknownProperty {
                property: id.clone(),
            })?;
        schema.validate(value)?;
    }
    for schema in schemas {
        if schema.required && values.get(&schema.id).is_none() {
            return Err(SchemaError::MissingProperty {
                property: schema.id.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyBag(BTreeMap<PropertyId, PropertyValue>);

impl PropertyBag {
    pub fn insert(&mut self, id: PropertyId, value: PropertyValue) -> Option<PropertyValue> {
        self.0.insert(id, value)
    }

    pub fn get(&self, id: &PropertyId) -> Option<&PropertyValue> {
        self.0.get(id)
    }

    /// The SI magnitude of a scalar property, if present and scalar.
    pub fn scalar(&self, id: &PropertyId) -> Option<f64> {
        match self.get(id) {
            Some(PropertyValue::Scalar(value)) => Some(value.si_value()),
            _ => None,
        }
    }

    /// The value as a typed mass, if present and has the mass dimension.
    pub fn typed_mass(&self, id: &PropertyId) -> Option<MassKg> {
        self.scalar(id).map(MassKg::from_si)
    }

    /// The value as a typed electric charge, if present and has the charge dimension.
    pub fn typed_charge(&self, id: &PropertyId) -> Option<ChargeCoulombs> {
        self.scalar(id).map(ChargeCoulombs::from_si)
    }

    /// The value as a typed length, if present and has the length dimension.
    pub fn typed_length(&self, id: &PropertyId) -> Option<LengthMetres> {
        self.scalar(id).map(LengthMetres::from_si)
    }

    /// The value as a typed time, if present and has the time dimension.
    pub fn typed_time(&self, id: &PropertyId) -> Option<TimeQuantity> {
        self.scalar(id).map(TimeQuantity::from_si)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PropertyId, &PropertyValue)> {
        self.0.iter()
    }
}

impl FromIterator<(PropertyId, PropertyValue)> for PropertyBag {
    fn from_iter<T: IntoIterator<Item = (PropertyId, PropertyValue)>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

// Not `Eq`: a declared default may carry an `f64` magnitude, and comparing
// schemas for conflict detection only ever needs `PartialEq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentSchema {
    pub id: ComponentTypeId,
    pub display_name: String,
    pub properties: Vec<PropertySchema>,
}

impl ComponentSchema {
    pub fn validate(&self, values: &PropertyBag) -> Result<(), SchemaError> {
        validate_properties(&self.properties, values)
    }

    /// A bag that satisfies this schema, for attaching the component before the
    /// user has typed anything into it.
    ///
    /// Only required properties are populated; an optional property left absent
    /// is what "not set" means. Fails only if a required property has no
    /// representable default, which the caller must surface rather than attach
    /// a bag the world will reject.
    pub fn default_properties(&self) -> Result<PropertyBag, SchemaError> {
        self.properties
            .iter()
            .filter(|property| property.required)
            .map(|property| {
                let value =
                    property
                        .initial_value()
                        .ok_or_else(|| SchemaError::NoDefaultValue {
                            property: property.id.clone(),
                        })?;
                Ok((property.id.clone(), value))
            })
            .collect::<Result<PropertyBag, SchemaError>>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValueKind {
    Scalar(Dimension),
    Vector(Dimension),
}

impl FieldValueKind {
    pub const fn dimension(self) -> Dimension {
        match self {
            Self::Scalar(dimension) | Self::Vector(dimension) => dimension,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSchema {
    pub id: ChannelId,
    pub display_name: String,
    pub value_kind: FieldValueKind,
}

impl ChannelSchema {
    pub const fn dimension(&self) -> Dimension {
        self.value_kind.dimension()
    }

    pub fn validate(&self, value: &FieldValue) -> Result<(), SchemaError> {
        let valid = match (self.value_kind, value) {
            (FieldValueKind::Scalar(expected), FieldValue::Scalar(actual)) => {
                actual.dimension() == expected
            }
            (FieldValueKind::Vector(expected), FieldValue::Vector(actual)) => {
                actual.dimension() == expected
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(SchemaError::ChannelValueMismatch {
                channel: self.id.clone(),
                expected: self.value_kind,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    Scalar(Quantity),
    Vector(VectorQuantity),
}

impl FieldValue {
    pub const fn dimension(self) -> Dimension {
        match self {
            Self::Scalar(value) => value.dimension(),
            Self::Vector(value) => value.dimension(),
        }
    }

    /// Magnitude in SI units. Colour maps and probe plots need this for both
    /// shapes without caring which they were given.
    pub fn magnitude(self) -> f64 {
        match self {
            Self::Scalar(value) => value.si_value().abs(),
            Self::Vector(value) => value.si_value().length(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("property '{property}' is not declared by the schema")]
    UnknownProperty { property: PropertyId },
    #[error("required property '{property}' is missing")]
    MissingProperty { property: PropertyId },
    #[error("required property '{property}' has no representable default value")]
    NoDefaultValue { property: PropertyId },
    #[error("property '{property}' does not match expected kind {expected:?}")]
    ValueMismatch {
        property: PropertyId,
        expected: PropertyKind,
    },
    #[error("channel '{channel}' does not match expected value kind {expected:?}")]
    ChannelValueMismatch {
        channel: ChannelId,
        expected: FieldValueKind,
    },
    #[error(
        "channel '{channel}' expected {expected:?} values but the solver produced another shape"
    )]
    ChannelColumnMismatch {
        channel: ChannelId,
        expected: FieldValueKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginId;

    fn charge_schema() -> PropertySchema {
        PropertySchema {
            id: PropertyId::new("charge").unwrap(),
            display_name: "Charge".to_owned(),
            description: None,
            kind: PropertyKind::Scalar(Dimension::CHARGE),
            required: true,
            default_value: None,
            relevant_when: None,
        }
    }

    #[test]
    fn a_conditional_property_is_inert_until_its_sibling_agrees() {
        let switch = PropertyId::new("linked").unwrap();
        let governed = PropertySchema {
            id: PropertyId::new("value").unwrap(),
            display_name: "Value".to_owned(),
            description: None,
            kind: PropertyKind::Scalar(Dimension::MASS),
            required: true,
            relevant_when: Some(PropertyCondition {
                property: switch.clone(),
                equals: PropertyValue::Boolean(false),
                because: "linked to something else".to_owned(),
            }),
            default_value: None,
        };

        let linked: PropertyBag = [(switch.clone(), PropertyValue::Boolean(true))]
            .into_iter()
            .collect();
        let unlinked: PropertyBag = [(switch, PropertyValue::Boolean(false))]
            .into_iter()
            .collect();

        assert!(!governed.is_relevant(&linked));
        assert!(governed.is_relevant(&unlinked));
        // A missing sibling cannot satisfy the condition, so the dependent
        // property stays inert rather than offering an edit that may be lost.
        assert!(!governed.is_relevant(&PropertyBag::default()));
    }

    #[test]
    fn an_unconditional_property_is_always_relevant() {
        assert!(charge_schema().is_relevant(&PropertyBag::default()));
    }

    /// Relevance is presentation, not validity. An inert value is still stored
    /// and still required, so turning the condition back on returns the value
    /// the user last chose rather than a blank.
    #[test]
    fn an_inert_property_is_still_required_by_validation() {
        let schemas = vec![charge_schema()];

        assert!(matches!(
            validate_properties(&schemas, &PropertyBag::default()),
            Err(SchemaError::MissingProperty { .. })
        ));
    }

    #[test]
    fn property_schema_rejects_wrong_dimensions() {
        let mass = PropertyValue::Scalar(Quantity::new(2.0, Dimension::MASS).unwrap());

        assert!(matches!(
            charge_schema().validate(&mass),
            Err(SchemaError::ValueMismatch { .. })
        ));
    }

    #[test]
    fn shared_validation_reports_unknown_and_missing_properties() {
        let schemas = vec![charge_schema()];
        let unknown: PropertyBag = [(
            PropertyId::new("mass").unwrap(),
            PropertyValue::Scalar(Quantity::new(1.0, Dimension::MASS).unwrap()),
        )]
        .into_iter()
        .collect();

        assert!(matches!(
            validate_properties(&schemas, &unknown),
            Err(SchemaError::UnknownProperty { .. })
        ));
        assert!(matches!(
            validate_properties(&schemas, &PropertyBag::default()),
            Err(SchemaError::MissingProperty { .. })
        ));
    }

    #[test]
    fn channel_schema_rejects_scalar_for_vector_channel() {
        let channel = ChannelSchema {
            id: ChannelId::new(PluginId::new("test").unwrap(), "vector").unwrap(),
            display_name: "Vector".to_owned(),
            value_kind: FieldValueKind::Vector(Dimension::LENGTH),
        };
        let scalar = FieldValue::Scalar(Quantity::new(1.0, Dimension::LENGTH).unwrap());

        assert!(channel.validate(&scalar).is_err());
        assert_eq!(channel.dimension(), Dimension::LENGTH);
    }
}
