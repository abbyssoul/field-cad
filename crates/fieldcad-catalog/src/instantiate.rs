//! Generic authoritative instantiation: turn a resolved [`TemplateSpec`]
//! into a fully-built `fieldcad_core::ObjectSpec`, ready to hand to
//! `WorldCommand::CreateObject` — see the task doc's "Linked instances and
//! portable scenes" → "Instantiation".

use std::collections::BTreeMap;

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    CatalogEntryRef, CatalogLink, ComponentSchema, ComponentTypeId, ObjectColor, ObjectShape,
    ObjectSpec, Transform, Velocity,
};

use crate::availability::{AvailabilityReason, resolve_components, resolve_object_kind};
use crate::structure::{TemplateColor, TemplateShape, TemplateSpec};

/// Placement inputs — everything instantiation supplies that a template
/// deliberately does not carry. See the catalog document contract: "Do not
/// put position, velocity, pinning, visibility, simulation state, or a
/// world object ID in a template."
pub struct InstantiationPlacement {
    /// Resolved by the caller — either an explicit override or the output
    /// of [`crate::naming::suggest_display_name`]. Kept as a plain `String`
    /// here rather than re-deriving it, so this function stays free of any
    /// `WorldSnapshot` dependency.
    pub display_name: String,
    pub transform: Transform,
    pub velocity: Velocity,
    pub pinned: bool,
    /// The point-shape radius used only when the template declares no
    /// shape of its own.
    pub fallback_shape_radius: f64,
}

/// Instantiate `spec` as an authoritative-ready [`ObjectSpec`]: components
/// attached with schema-dimensioned values, shape converted, and
/// [`CatalogLink`] provenance stamped in.
///
/// Returns the same [`AvailabilityReason`]s [`crate::resolve_availability`]
/// would report if `spec` is not (or is no longer) available against
/// `component_schemas` — this function reuses that resolution pass rather
/// than duplicating it, so the two can never disagree about what
/// "available" means.
pub fn instantiate_template(
    spec: &TemplateSpec,
    reference: &CatalogEntryRef,
    component_schemas: &BTreeMap<ComponentTypeId, ComponentSchema>,
    placement: InstantiationPlacement,
) -> Result<ObjectSpec, Vec<AvailabilityReason>> {
    let mut reasons: Vec<AvailabilityReason> = resolve_object_kind(spec).into_iter().collect();
    let components = match resolve_components(spec, component_schemas) {
        Ok(components) => Some(components),
        Err(component_reasons) => {
            reasons.extend(component_reasons);
            None
        }
    };
    if !reasons.is_empty() {
        return Err(reasons);
    }

    let mut object = ObjectSpec::new(placement.display_name)
        .with_transform(placement.transform)
        .with_velocity(placement.velocity)
        .with_shape(to_object_shape(
            spec.shape.clone(),
            placement.fallback_shape_radius,
        ))
        .with_pinned(placement.pinned)
        .with_catalog_link(CatalogLink {
            entry: Some(reference.clone()),
            mode: fieldcad_core::CatalogLinkMode::Tracking,
            source_description: format!("{}/{}", reference.catalog, reference.template),
        });
    // A one-time seed, not a template-owned value: unlike shape/components,
    // nothing ever re-applies this from the template after instantiation —
    // the object's color is free from this point on, like its name.
    if let Some(color) = spec.default_color {
        object = object.with_color(to_object_color(color));
    }
    for (component_type, bag) in components.expect("reasons empty implies components resolved") {
        object = object.with_component(component_type, bag);
    }
    Ok(object)
}

fn to_object_color(color: TemplateColor) -> ObjectColor {
    ObjectColor {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn to_object_shape(shape: Option<TemplateShape>, fallback_radius: f64) -> ObjectShape {
    match shape {
        None => ObjectShape::point(fallback_radius)
            .expect("caller-supplied fallback radius is a static valid default"),
        Some(TemplateShape::Point { exclusion_radius }) => {
            ObjectShape::point(exclusion_radius.into_si())
                .expect("template shape already validated by validate_structure")
        }
        Some(TemplateShape::Sphere { radius }) => ObjectShape::sphere(radius.into_si())
            .expect("template shape already validated by validate_structure"),
        Some(TemplateShape::Box { half_extent }) => ObjectShape::boxed(half_extent)
            .expect("template shape already validated by validate_structure"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::resolve_availability;
    use crate::ids::{CatalogScopeName, TemplateName};
    use crate::source::{DocumentOrdinal, SourceLocation, TemplateIdentity, global_entry_ref};
    use crate::structure::TemplateComponentInstance;
    use fieldcad_core::quantities::LengthMetres;
    use fieldcad_gravity_sources::{
        inertial_mass_component_id, inertial_mass_component_schema, mass_property_id,
    };
    use glam::DVec3;
    use std::path::PathBuf;

    fn registry_with_mass() -> BTreeMap<ComponentTypeId, ComponentSchema> {
        [(
            inertial_mass_component_id(),
            inertial_mass_component_schema(),
        )]
        .into_iter()
        .collect()
    }

    fn identity() -> TemplateIdentity {
        TemplateIdentity {
            catalog: CatalogScopeName::new("personal-physics").unwrap(),
            template: TemplateName::new("fancy-unicorn").unwrap(),
        }
    }

    fn reference() -> CatalogEntryRef {
        let source = SourceLocation {
            file: PathBuf::from("catalog.yaml"),
            document_ordinal: DocumentOrdinal::new(0),
        };
        global_entry_ref(
            std::path::Path::new(""),
            &source,
            &identity(),
            "test-fingerprint".to_owned(),
        )
    }

    fn placement() -> InstantiationPlacement {
        InstantiationPlacement {
            display_name: "fancy-unicorn".to_owned(),
            transform: Transform::default(),
            velocity: Velocity::default(),
            pinned: false,
            fallback_shape_radius: 0.15,
        }
    }

    fn spec_with_mass(shape: Option<TemplateShape>) -> TemplateSpec {
        TemplateSpec {
            object_kind: "world-object".to_owned(),
            shape,
            default_color: None,
            components: vec![TemplateComponentInstance {
                component_type: inertial_mass_component_id(),
                properties: [(
                    mass_property_id(),
                    crate::structure::TemplatePropertyValue::Scalar { si_value: 1.0 },
                )]
                .into_iter()
                .collect(),
            }],
        }
    }

    #[test]
    fn instantiates_a_mass_template_into_a_ready_object_spec() {
        let object = instantiate_template(
            &spec_with_mass(None),
            &reference(),
            &registry_with_mass(),
            placement(),
        )
        .unwrap();

        assert_eq!(object.name, "fancy-unicorn");
        assert!(
            object
                .components
                .contains_key(&inertial_mass_component_id())
        );
        let link = object.catalog_link.unwrap();
        assert_eq!(link.entry.unwrap(), reference());
        assert_eq!(
            object.shape,
            Some(ObjectShape::point(0.15).unwrap()),
            "no template shape falls back to the placement radius"
        );
        assert_eq!(
            object.color, None,
            "a template with no declared default color instantiates unset"
        );
    }

    #[test]
    fn a_declared_default_color_seeds_the_instantiated_objects_color() {
        let mut spec = spec_with_mass(None);
        spec.default_color = Some(TemplateColor {
            r: 0.2,
            g: 0.56,
            b: 0.88,
            a: 1.0,
        });

        let object =
            instantiate_template(&spec, &reference(), &registry_with_mass(), placement()).unwrap();

        assert_eq!(
            object.color,
            Some(ObjectColor {
                r: 0.2,
                g: 0.56,
                b: 0.88,
                a: 1.0,
            })
        );
    }

    #[test]
    fn instantiates_each_declared_shape_variant() {
        let point = instantiate_template(
            &spec_with_mass(Some(TemplateShape::Point {
                exclusion_radius: LengthMetres::from_si(0.2),
            })),
            &reference(),
            &registry_with_mass(),
            placement(),
        )
        .unwrap();
        assert_eq!(point.shape, Some(ObjectShape::point(0.2).unwrap()));

        let sphere = instantiate_template(
            &spec_with_mass(Some(TemplateShape::Sphere {
                radius: LengthMetres::from_si(0.3),
            })),
            &reference(),
            &registry_with_mass(),
            placement(),
        )
        .unwrap();
        assert_eq!(sphere.shape, Some(ObjectShape::sphere(0.3).unwrap()));

        let boxed = instantiate_template(
            &spec_with_mass(Some(TemplateShape::Box {
                half_extent: DVec3::new(1.0, 2.0, 3.0),
            })),
            &reference(),
            &registry_with_mass(),
            placement(),
        )
        .unwrap();
        assert_eq!(
            boxed.shape,
            Some(ObjectShape::boxed(DVec3::new(1.0, 2.0, 3.0)).unwrap())
        );
    }

    #[test]
    fn instantiation_reports_the_same_reasons_resolve_availability_would() {
        let spec = spec_with_mass(None);
        let empty_registry = BTreeMap::new();

        let instantiate_err =
            instantiate_template(&spec, &reference(), &empty_registry, placement()).unwrap_err();
        let availability_reasons = match resolve_availability(&spec, &empty_registry) {
            crate::availability::AvailabilityOutcome::Unavailable(reasons) => reasons,
            crate::availability::AvailabilityOutcome::Available => panic!("expected unavailable"),
        };

        assert_eq!(instantiate_err, availability_reasons);
    }

    #[test]
    fn unknown_object_kind_is_reported_by_instantiation_too() {
        let mut spec = spec_with_mass(None);
        spec.object_kind = "emitter".to_owned();

        let error = instantiate_template(&spec, &reference(), &registry_with_mass(), placement())
            .unwrap_err();

        assert!(
            error
                .iter()
                .any(|reason| matches!(reason, AvailabilityReason::UnknownObjectKind { .. }))
        );
    }
}
