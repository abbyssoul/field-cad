//! Registry-independent validation: does a parsed document actually satisfy
//! the catalog format contract, independent of anything currently
//! registered by the running application?
//!
//! This is the mid-tier the task doc's three-state model needs: a
//! [`TemplateSpec`] built here is `Clone + PartialEq` and re-resolvable
//! against a live component-schema registry later (see
//! [`crate::availability`]) without re-parsing the source YAML.

use std::collections::{BTreeMap, BTreeSet};

use fieldcad_core::quantities::{LengthMetres, SiScalar};
use fieldcad_core::{ComponentTypeId, PluginId, PropertyId};
use glam::DVec3;

use crate::diagnostics::{Diagnostic, InvalidReason};
use crate::document::{
    CatalogColor, CatalogComponentInstance, CatalogEntryDocument, CatalogPropertyValue,
    CatalogShape,
};
use crate::ids::{CatalogScopeName, TemplateName};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemplateSpec {
    /// Preserved verbatim, even if unrecognised — availability resolution
    /// decides instantiability, this type only decides parsability.
    pub object_kind: String,
    pub shape: Option<TemplateShape>,
    /// A one-time seed for a newly-instantiated object's display color.
    /// Not a template-owned, read-only-when-linked value like `shape` —
    /// see `instantiate::instantiate_template`, which copies this into the
    /// new object's own free `color` field and never touches it again.
    pub default_color: Option<TemplateColor>,
    pub components: Vec<TemplateComponentInstance>,
}

/// Structurally-clean mirror of [`CatalogColor`] — same shape, but every
/// channel is guaranteed finite and within `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemplateColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TemplateShape {
    Point { exclusion_radius: LengthMetres },
    Sphere { radius: LengthMetres },
    Box { half_extent: DVec3 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemplateComponentInstance {
    pub component_type: ComponentTypeId,
    pub properties: BTreeMap<PropertyId, TemplatePropertyValue>,
}

/// Structurally-clean mirror of [`CatalogPropertyValue`] — same shape, but
/// `si_value` is guaranteed finite.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TemplatePropertyValue {
    Scalar { si_value: f64 },
    Vector { si_value: DVec3 },
    Boolean(bool),
    Text(String),
    Choice(String),
}

fn positive_finite(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn unit_range_finite(value: f64) -> Option<f32> {
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value as f32)
}

/// Structurally validate a parsed document, collecting every problem found
/// rather than stopping at the first one.
///
/// Deliberately does *not* check whether `object_kind` is known, whether a
/// `ComponentTypeId`/`PropertyId` is registered, or whether a property's
/// value-kind matches its schema — those depend on a live registry and
/// belong to [`crate::availability::resolve_availability`].
pub fn validate_structure(
    document: &CatalogEntryDocument,
) -> Result<TemplateSpec, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();

    if let Err(source) = CatalogScopeName::new(document.metadata.catalog.clone()) {
        diagnostics.push(Diagnostic {
            field_path: Some("metadata.catalog".to_owned()),
            reason: InvalidReason::InvalidCatalogName { source },
        });
    }
    if let Err(source) = TemplateName::new(document.metadata.name.clone()) {
        diagnostics.push(Diagnostic {
            field_path: Some("metadata.name".to_owned()),
            reason: InvalidReason::InvalidTemplateName { source },
        });
    }

    let shape = validate_shape(&document.spec.shape, &mut diagnostics);
    let default_color = validate_color(&document.spec.default_color, &mut diagnostics);
    let components = validate_components(&document.spec.components, &mut diagnostics);

    if diagnostics.is_empty() {
        Ok(TemplateSpec {
            object_kind: document.spec.object_kind.clone(),
            shape,
            default_color,
            components,
        })
    } else {
        Err(diagnostics)
    }
}

fn validate_color(
    color: &Option<CatalogColor>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<TemplateColor> {
    let color = color.as_ref()?;
    let channels = [
        ("r", color.r),
        ("g", color.g),
        ("b", color.b),
        ("a", color.a),
    ];
    let mut valid = [0.0f32; 4];
    let mut all_valid = true;
    for (index, (name, value)) in channels.into_iter().enumerate() {
        match unit_range_finite(value) {
            Some(channel) => valid[index] = channel,
            None => {
                diagnostics.push(Diagnostic {
                    field_path: Some(format!("spec.defaultColor.{name}")),
                    reason: InvalidReason::ColorChannelOutOfRange { value },
                });
                all_valid = false;
            }
        }
    }
    all_valid.then_some(TemplateColor {
        r: valid[0],
        g: valid[1],
        b: valid[2],
        a: valid[3],
    })
}

fn validate_shape(
    shape: &Option<CatalogShape>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<TemplateShape> {
    match shape {
        None => None,
        Some(CatalogShape::Point {
            exclusion_radius_metres,
        }) => match positive_finite(*exclusion_radius_metres) {
            Some(value) => Some(TemplateShape::Point {
                exclusion_radius: LengthMetres::from_si(value),
            }),
            None => {
                diagnostics.push(Diagnostic {
                    field_path: Some("spec.shape.exclusionRadiusMetres".to_owned()),
                    reason: InvalidReason::NonPositiveOrNonFiniteExtent {
                        value: *exclusion_radius_metres,
                    },
                });
                None
            }
        },
        Some(CatalogShape::Sphere { radius_metres }) => match positive_finite(*radius_metres) {
            Some(value) => Some(TemplateShape::Sphere {
                radius: LengthMetres::from_si(value),
            }),
            None => {
                diagnostics.push(Diagnostic {
                    field_path: Some("spec.shape.radiusMetres".to_owned()),
                    reason: InvalidReason::NonPositiveOrNonFiniteExtent {
                        value: *radius_metres,
                    },
                });
                None
            }
        },
        Some(CatalogShape::Box { half_extent_metres }) => {
            let mut all_valid = true;
            for (axis, value) in ["x", "y", "z"].iter().zip(half_extent_metres.iter()) {
                if positive_finite(*value).is_none() {
                    diagnostics.push(Diagnostic {
                        field_path: Some(format!("spec.shape.halfExtentMetres.{axis}")),
                        reason: InvalidReason::NonPositiveOrNonFiniteExtent { value: *value },
                    });
                    all_valid = false;
                }
            }
            all_valid.then(|| TemplateShape::Box {
                half_extent: DVec3::new(
                    half_extent_metres[0],
                    half_extent_metres[1],
                    half_extent_metres[2],
                ),
            })
        }
    }
}

fn validate_components(
    components: &[CatalogComponentInstance],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<TemplateComponentInstance> {
    let mut seen = BTreeSet::new();
    let mut validated = Vec::new();

    for (index, component) in components.iter().enumerate() {
        let component_type = validate_component_type(component, index, diagnostics);

        if let Some(component_type) = &component_type
            && !seen.insert(component_type.clone())
        {
            diagnostics.push(Diagnostic {
                field_path: Some(format!("spec.components[{index}].type")),
                reason: InvalidReason::DuplicateComponentInEntry {
                    component: component_type.to_string(),
                },
            });
        }

        let properties = validate_properties(component, index, diagnostics);

        if let Some(component_type) = component_type {
            validated.push(TemplateComponentInstance {
                component_type,
                properties,
            });
        }
    }

    validated
}

fn validate_component_type(
    component: &CatalogComponentInstance,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ComponentTypeId> {
    let plugin = match PluginId::new(component.component_type.plugin.clone()) {
        Ok(plugin) => plugin,
        Err(source) => {
            diagnostics.push(Diagnostic {
                field_path: Some(format!("spec.components[{index}].type.plugin")),
                reason: InvalidReason::InvalidPluginId { source },
            });
            return None;
        }
    };

    match ComponentTypeId::new(plugin, component.component_type.name.clone()) {
        Ok(id) => Some(id),
        Err(source) => {
            diagnostics.push(Diagnostic {
                field_path: Some(format!("spec.components[{index}].type.name")),
                reason: InvalidReason::InvalidComponentName { source },
            });
            None
        }
    }
}

fn validate_properties(
    component: &CatalogComponentInstance,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<PropertyId, TemplatePropertyValue> {
    let mut properties = BTreeMap::new();

    for (key, value) in &component.properties {
        let property_id = match PropertyId::new(key.clone()) {
            Ok(id) => Some(id),
            Err(source) => {
                diagnostics.push(Diagnostic {
                    field_path: Some(format!("spec.components[{index}].properties.{key}")),
                    reason: InvalidReason::InvalidPropertyId { source },
                });
                None
            }
        };

        let template_value = validate_property_value(value, index, key, diagnostics);

        if let (Some(property_id), Some(template_value)) = (property_id, template_value) {
            properties.insert(property_id, template_value);
        }
    }

    properties
}

fn validate_property_value(
    value: &CatalogPropertyValue,
    index: usize,
    key: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<TemplatePropertyValue> {
    match value {
        CatalogPropertyValue::Scalar { si_value } => {
            if si_value.is_finite() {
                Some(TemplatePropertyValue::Scalar {
                    si_value: *si_value,
                })
            } else {
                diagnostics.push(Diagnostic {
                    field_path: Some(format!(
                        "spec.components[{index}].properties.{key}.scalar.siValue"
                    )),
                    reason: InvalidReason::NonFiniteValue { value: *si_value },
                });
                None
            }
        }
        CatalogPropertyValue::Vector { si_value } => {
            let mut all_finite = true;
            for (axis, component_value) in ["x", "y", "z"].iter().zip(si_value.iter()) {
                if !component_value.is_finite() {
                    diagnostics.push(Diagnostic {
                        field_path: Some(format!(
                            "spec.components[{index}].properties.{key}.vector.siValue.{axis}"
                        )),
                        reason: InvalidReason::NonFiniteValue {
                            value: *component_value,
                        },
                    });
                    all_finite = false;
                }
            }
            all_finite.then(|| TemplatePropertyValue::Vector {
                si_value: DVec3::new(si_value[0], si_value[1], si_value[2]),
            })
        }
        CatalogPropertyValue::Boolean(flag) => Some(TemplatePropertyValue::Boolean(*flag)),
        CatalogPropertyValue::Text(text) => Some(TemplatePropertyValue::Text(text.clone())),
        CatalogPropertyValue::Choice(choice) => {
            if choice.is_empty() {
                diagnostics.push(Diagnostic {
                    field_path: Some(format!("spec.components[{index}].properties.{key}.choice")),
                    reason: InvalidReason::EmptyChoiceValue,
                });
                None
            } else {
                Some(TemplatePropertyValue::Choice(choice.clone()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::CatalogComponentTypeRef;

    fn base_document() -> CatalogEntryDocument {
        CatalogEntryDocument {
            api_version: crate::document::API_VERSION.to_owned(),
            kind: crate::document::KIND.to_owned(),
            metadata: crate::document::CatalogMetadata {
                catalog: "personal-physics".to_owned(),
                name: "fancy-unicorn".to_owned(),
                description: None,
                author: None,
                labels: BTreeMap::new(),
                annotations: BTreeMap::new(),
            },
            spec: crate::document::CatalogSpec {
                object_kind: "world-object".to_owned(),
                shape: None,
                default_color: None,
                components: Vec::new(),
            },
        }
    }

    #[test]
    fn a_well_formed_document_validates_with_an_empty_registry_dependency() {
        let mut document = base_document();
        document.spec.shape = Some(CatalogShape::Point {
            exclusion_radius_metres: 0.15,
        });
        document.spec.components.push(CatalogComponentInstance {
            component_type: CatalogComponentTypeRef {
                plugin: "fieldcad.mass-sources".to_owned(),
                name: "inertial-mass".to_owned(),
            },
            properties: [(
                "mass".to_owned(),
                CatalogPropertyValue::Scalar { si_value: 1.0 },
            )]
            .into_iter()
            .collect(),
        });

        let spec = validate_structure(&document).expect("well-formed document must validate");
        assert_eq!(spec.object_kind, "world-object");
        assert!(matches!(spec.shape, Some(TemplateShape::Point { .. })));
        assert_eq!(spec.components.len(), 1);
    }

    #[test]
    fn rejects_non_positive_radius() {
        let mut document = base_document();
        document.spec.shape = Some(CatalogShape::Sphere { radius_metres: 0.0 });

        let diagnostics = validate_structure(&document).unwrap_err();
        assert!(matches!(
            diagnostics[0].reason,
            InvalidReason::NonPositiveOrNonFiniteExtent { .. }
        ));
    }

    #[test]
    fn a_declared_default_color_validates_into_the_template() {
        let mut document = base_document();
        document.spec.default_color = Some(crate::document::CatalogColor {
            r: 0.2,
            g: 0.56,
            b: 0.88,
            a: 1.0,
        });

        let spec = validate_structure(&document).expect("in-range color validates");
        assert_eq!(
            spec.default_color,
            Some(TemplateColor {
                r: 0.2,
                g: 0.56,
                b: 0.88,
                a: 1.0,
            })
        );
    }

    #[test]
    fn rejects_an_out_of_range_color_channel() {
        let mut document = base_document();
        document.spec.default_color = Some(crate::document::CatalogColor {
            r: 1.5,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        });

        let diagnostics = validate_structure(&document).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| matches!(d.reason, InvalidReason::ColorChannelOutOfRange { .. }))
        );
    }

    #[test]
    fn rejects_non_finite_property_value() {
        let mut document = base_document();
        document.spec.components.push(CatalogComponentInstance {
            component_type: CatalogComponentTypeRef {
                plugin: "fieldcad.mass-sources".to_owned(),
                name: "inertial-mass".to_owned(),
            },
            properties: [(
                "mass".to_owned(),
                CatalogPropertyValue::Scalar { si_value: f64::NAN },
            )]
            .into_iter()
            .collect(),
        });

        let diagnostics = validate_structure(&document).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| matches!(d.reason, InvalidReason::NonFiniteValue { .. }))
        );
    }

    #[test]
    fn rejects_invalid_identifiers() {
        let mut document = base_document();
        document.metadata.catalog = "has a space".to_owned();
        document.spec.components.push(CatalogComponentInstance {
            component_type: CatalogComponentTypeRef {
                plugin: "not valid!".to_owned(),
                name: "inertial-mass".to_owned(),
            },
            properties: BTreeMap::new(),
        });

        let diagnostics = validate_structure(&document).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| matches!(d.reason, InvalidReason::InvalidCatalogName { .. }))
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| matches!(d.reason, InvalidReason::InvalidPluginId { .. }))
        );
    }

    #[test]
    fn rejects_a_duplicate_component_within_one_entry() {
        let mut document = base_document();
        for _ in 0..2 {
            document.spec.components.push(CatalogComponentInstance {
                component_type: CatalogComponentTypeRef {
                    plugin: "fieldcad.mass-sources".to_owned(),
                    name: "inertial-mass".to_owned(),
                },
                properties: BTreeMap::new(),
            });
        }

        let diagnostics = validate_structure(&document).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|d| matches!(d.reason, InvalidReason::DuplicateComponentInEntry { .. }))
        );
    }

    #[test]
    fn collects_every_problem_rather_than_stopping_at_the_first() {
        let mut document = base_document();
        document.metadata.catalog = "".to_owned();
        document.metadata.name = "".to_owned();
        document.spec.shape = Some(CatalogShape::Sphere {
            radius_metres: -1.0,
        });

        let diagnostics = validate_structure(&document).unwrap_err();
        assert_eq!(diagnostics.len(), 3);
    }
}
