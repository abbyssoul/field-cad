//! Availability resolution: is a structurally-valid [`TemplateSpec`]
//! instantiable against what this build currently has registered?
//!
//! This is a pure function of a live component-schema map, never solver or
//! field-system activation state — a component whose owning field system is
//! currently inactive stays available as long as its schema is registered
//! (ADR 0014, ADR 0017). The same [`TemplateSpec`] produced once at load
//! time can be re-resolved here later against a fresh
//! `WorldSnapshot::component_schemas()` without re-parsing the source YAML.

use std::collections::BTreeMap;

use fieldcad_core::{
    ComponentSchema, ComponentTypeId, PropertyBag, PropertyKind, PropertyValue, Quantity,
    SchemaError, VectorQuantity,
};

use crate::structure::{TemplatePropertyValue, TemplateSpec};

/// Object kinds this build knows how to instantiate. Everything else stays
/// parsable but unavailable, per the task's extensibility requirement for
/// future kinds (sources, emitters, ...).
pub const KNOWN_OBJECT_KINDS: &[&str] = &["world-object"];

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum AvailabilityReason {
    #[error("object kind '{kind}' is not recognised by this build")]
    UnknownObjectKind { kind: String },
    #[error("component '{component}' is not registered by this build")]
    UnknownComponent { component: ComponentTypeId },
    #[error("component '{component}': {source}")]
    ComponentSchema {
        component: ComponentTypeId,
        #[source]
        source: SchemaError,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum AvailabilityOutcome {
    Available,
    /// Never empty: constructed only when at least one reason was found.
    Unavailable(Vec<AvailabilityReason>),
}

impl AvailabilityOutcome {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Resolve whether `spec` can be instantiated given the component schemas
/// currently registered on the world (typically
/// `WorldSnapshot::component_schemas()`).
pub fn resolve_availability(
    spec: &TemplateSpec,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
) -> AvailabilityOutcome {
    let mut reasons: Vec<AvailabilityReason> = resolve_object_kind(spec).into_iter().collect();
    if let Err(component_reasons) = resolve_components(spec, component_schemas) {
        reasons.extend(component_reasons);
    }

    if reasons.is_empty() {
        AvailabilityOutcome::Available
    } else {
        AvailabilityOutcome::Unavailable(reasons)
    }
}

/// Whether `spec`'s object kind is one this build knows how to instantiate.
pub(crate) fn resolve_object_kind(spec: &TemplateSpec) -> Option<AvailabilityReason> {
    (!KNOWN_OBJECT_KINDS.contains(&spec.object_kind.as_str())).then(|| {
        AvailabilityReason::UnknownObjectKind {
            kind: spec.object_kind.clone(),
        }
    })
}

/// Resolve every declared component's property bag against the live
/// registry. `Ok` carries the fully-converted, dimensioned bags in template
/// order — shared by [`crate::instantiate::instantiate_template`] so
/// availability checking and instantiation can never disagree about what
/// "available" means.
pub(crate) fn resolve_components(
    spec: &TemplateSpec,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
) -> Result<Vec<(ComponentTypeId, PropertyBag)>, Vec<AvailabilityReason>> {
    let mut reasons = Vec::new();
    let mut resolved = Vec::new();

    for component in &spec.components {
        let Some(schema) = component_schemas.get(&component.component_type) else {
            reasons.push(AvailabilityReason::UnknownComponent {
                component: component.component_type.clone(),
            });
            continue;
        };

        match template_properties_to_bag(schema, &component.properties) {
            Ok(bag) => match schema.validate(&bag) {
                Ok(()) => resolved.push((component.component_type.clone(), bag)),
                Err(source) => reasons.push(AvailabilityReason::ComponentSchema {
                    component: component.component_type.clone(),
                    source,
                }),
            },
            Err(errors) => reasons.extend(errors.into_iter().map(|source| {
                AvailabilityReason::ComponentSchema {
                    component: component.component_type.clone(),
                    source,
                }
            })),
        }
    }

    if reasons.is_empty() {
        Ok(resolved)
    } else {
        Err(reasons)
    }
}

/// Convert a template's raw properties into a schema-checkable
/// [`PropertyBag`], reusing `fieldcad_core::SchemaError`'s vocabulary rather
/// than inventing a parallel one.
/// Convert a catalog property's raw, dimensionless representation into the
/// runtime bag prescribed by `schema`. This is public so authoring clients use
/// exactly the same conversion and error vocabulary as availability checks.
pub fn template_properties_to_bag(
    schema: &ComponentSchema,
    raw: &BTreeMap<fieldcad_core::PropertyId, TemplatePropertyValue>,
) -> Result<PropertyBag, Vec<SchemaError>> {
    let mut errors = Vec::new();
    let mut bag = PropertyBag::default();

    for (property_id, value) in raw {
        let Some(property_schema) = schema.properties.iter().find(|p| p.id == *property_id) else {
            errors.push(SchemaError::UnknownProperty {
                property: property_id.clone(),
            });
            continue;
        };

        match convert_value(value, &property_schema.kind) {
            Some(value) => {
                bag.insert(property_id.clone(), value);
            }
            None => errors.push(SchemaError::ValueMismatch {
                property: property_id.clone(),
                expected: property_schema.kind.clone(),
            }),
        }
    }

    if errors.is_empty() {
        Ok(bag)
    } else {
        Err(errors)
    }
}

/// `si_value` finiteness is already guaranteed by
/// `crate::structure::validate_structure`, which always runs before
/// availability resolution — so `Quantity::new`/`VectorQuantity::new` can
/// only fail here on a logic error in that invariant, hence the `expect`.
fn convert_value(value: &TemplatePropertyValue, kind: &PropertyKind) -> Option<PropertyValue> {
    match (value, kind) {
        (TemplatePropertyValue::Scalar { si_value }, PropertyKind::Scalar(dimension)) => {
            Some(PropertyValue::Scalar(
                Quantity::new(*si_value, *dimension)
                    .expect("finiteness already checked by validate_structure"),
            ))
        }
        (TemplatePropertyValue::Vector { si_value }, PropertyKind::Vector(dimension)) => {
            Some(PropertyValue::Vector(
                VectorQuantity::new(*si_value, *dimension)
                    .expect("finiteness already checked by validate_structure"),
            ))
        }
        (TemplatePropertyValue::Boolean(flag), PropertyKind::Boolean) => {
            Some(PropertyValue::Boolean(*flag))
        }
        (TemplatePropertyValue::Text(text), PropertyKind::Text) => {
            Some(PropertyValue::Text(text.clone()))
        }
        (TemplatePropertyValue::Choice(choice), PropertyKind::Choice(_)) => {
            Some(PropertyValue::Choice(choice.clone()))
        }
        _ => None,
    }
}

/// Convert schema-backed runtime values to the raw catalog representation.
/// Values not described by `schema` are deliberately omitted; callers that
/// edit a loaded template should overlay this result onto its original bag so
/// unavailable raw properties survive an otherwise ordinary edit.
pub fn property_bag_to_template(
    schema: &ComponentSchema,
    bag: &PropertyBag,
) -> Result<BTreeMap<fieldcad_core::PropertyId, TemplatePropertyValue>, Vec<SchemaError>> {
    let mut result = BTreeMap::new();
    let mut errors = Vec::new();
    for property in &schema.properties {
        let Some(value) = bag.get(&property.id) else {
            continue;
        };
        let converted = match (value, &property.kind) {
            (PropertyValue::Scalar(value), PropertyKind::Scalar(_)) => {
                Some(TemplatePropertyValue::Scalar {
                    si_value: value.si_value(),
                })
            }
            (PropertyValue::Vector(value), PropertyKind::Vector(_)) => {
                Some(TemplatePropertyValue::Vector {
                    si_value: value.si_value(),
                })
            }
            (PropertyValue::Boolean(value), PropertyKind::Boolean) => {
                Some(TemplatePropertyValue::Boolean(*value))
            }
            (PropertyValue::Text(value), PropertyKind::Text) => {
                Some(TemplatePropertyValue::Text(value.clone()))
            }
            (PropertyValue::Choice(value), PropertyKind::Choice(_)) => {
                Some(TemplatePropertyValue::Choice(value.clone()))
            }
            _ => None,
        };
        if let Some(value) = converted {
            result.insert(property.id.clone(), value);
        } else {
            errors.push(SchemaError::ValueMismatch {
                property: property.id.clone(),
                expected: property.kind.clone(),
            });
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::TemplateComponentInstance;
    use fieldcad_sources::{
        inertial_mass_component_id, inertial_mass_component_schema, mass_property_id,
    };

    fn registry_with_mass() -> BTreeMap<ComponentTypeId, ComponentSchema> {
        [(
            inertial_mass_component_id(),
            inertial_mass_component_schema(),
        )]
        .into_iter()
        .collect()
    }

    fn spec_with_mass(si_value: f64) -> TemplateSpec {
        TemplateSpec {
            object_kind: "world-object".to_owned(),
            shape: None,
            components: vec![TemplateComponentInstance {
                component_type: inertial_mass_component_id(),
                properties: [(
                    mass_property_id(),
                    TemplatePropertyValue::Scalar { si_value },
                )]
                .into_iter()
                .collect(),
            }],
        }
    }

    #[test]
    fn a_known_component_with_a_matching_property_is_available() {
        let outcome = resolve_availability(&spec_with_mass(1.0), &registry_with_mass());
        assert_eq!(outcome, AvailabilityOutcome::Available);
    }

    #[test]
    fn availability_does_not_depend_on_any_solver_or_activation_state() {
        // The registry here simulates "a plugin registered this schema at
        // startup" with nothing else constructed at all — no runtime, no
        // solver, no field-system activation flag. If this resolves as
        // available, availability is provably a function of registration
        // alone (ADR 0014 / ADR 0017).
        let registry = registry_with_mass();
        let outcome = resolve_availability(&spec_with_mass(2.5), &registry);
        assert_eq!(outcome, AvailabilityOutcome::Available);
    }

    #[test]
    fn unknown_object_kind_is_unavailable() {
        let mut spec = spec_with_mass(1.0);
        spec.object_kind = "emitter".to_owned();

        let outcome = resolve_availability(&spec, &registry_with_mass());
        assert!(matches!(
            outcome,
            AvailabilityOutcome::Unavailable(reasons)
                if reasons.iter().any(|r| matches!(r, AvailabilityReason::UnknownObjectKind { .. }))
        ));
    }

    #[test]
    fn unregistered_component_is_unavailable() {
        let outcome = resolve_availability(&spec_with_mass(1.0), &BTreeMap::new());
        assert!(matches!(
            outcome,
            AvailabilityOutcome::Unavailable(reasons)
                if matches!(reasons.as_slice(), [AvailabilityReason::UnknownComponent { .. }])
        ));
    }

    #[test]
    fn unknown_property_on_a_known_component_is_unavailable() {
        let mut spec = spec_with_mass(1.0);
        spec.components[0].properties.insert(
            fieldcad_core::PropertyId::new("not-a-real-property").unwrap(),
            TemplatePropertyValue::Boolean(true),
        );

        let outcome = resolve_availability(&spec, &registry_with_mass());
        assert!(matches!(
            outcome,
            AvailabilityOutcome::Unavailable(reasons)
                if reasons.iter().any(|r| matches!(
                    r,
                    AvailabilityReason::ComponentSchema {
                        source: SchemaError::UnknownProperty { .. },
                        ..
                    }
                ))
        ));
    }

    #[test]
    fn a_property_kind_mismatch_is_unavailable() {
        let mut spec = spec_with_mass(1.0);
        spec.components[0]
            .properties
            .insert(mass_property_id(), TemplatePropertyValue::Boolean(true));

        let outcome = resolve_availability(&spec, &registry_with_mass());
        assert!(matches!(
            outcome,
            AvailabilityOutcome::Unavailable(reasons)
                if reasons.iter().any(|r| matches!(
                    r,
                    AvailabilityReason::ComponentSchema {
                        source: SchemaError::ValueMismatch { .. },
                        ..
                    }
                ))
        ));
    }

    #[test]
    fn a_missing_required_property_is_unavailable() {
        let spec = TemplateSpec {
            object_kind: "world-object".to_owned(),
            shape: None,
            components: vec![TemplateComponentInstance {
                component_type: inertial_mass_component_id(),
                properties: BTreeMap::new(),
            }],
        };

        let outcome = resolve_availability(&spec, &registry_with_mass());
        assert!(matches!(
            outcome,
            AvailabilityOutcome::Unavailable(reasons)
                if reasons.iter().any(|r| matches!(
                    r,
                    AvailabilityReason::ComponentSchema {
                        source: SchemaError::MissingProperty { .. },
                        ..
                    }
                ))
        ));
    }
}
