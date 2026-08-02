use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertySchema {
    pub id: PropertyId,
    pub display_name: String,
    pub kind: PropertyKind,
    pub required: bool,
}

impl PropertySchema {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentSchema {
    pub id: ComponentTypeId,
    pub display_name: String,
    pub properties: Vec<PropertySchema>,
}

impl ComponentSchema {
    pub fn validate(&self, values: &PropertyBag) -> Result<(), SchemaError> {
        validate_properties(&self.properties, values)
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
            kind: PropertyKind::Scalar(Dimension::CHARGE),
            required: true,
        }
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
