//! Inspector sections for editing an object's properties: placement, shape,
//! motion, components, and derived values.

use std::collections::{BTreeMap, HashSet};

use fieldcad_core::{
    CatalogLinkMode, ComponentTypeId, Dimension, ObjectId, ObjectShape, PropertyBag, PropertyKind,
    PropertySchema, PropertyValue, Quantity, Transform, VectorQuantity, Velocity, WorldCommand,
    WorldObject, WorldSnapshot, relativistic_kinetic_energy, relativistic_momentum,
};
use fieldcad_gravity_sources::{inertial_mass_component_id, mass_property_id};
use glam::DVec3;

use super::scene_tree::DEFAULT_AUTHORING_RADIUS;
use super::{color_editor, coordinate_editor, name_editor, note_held_edit};
use crate::scene::TrajectoryDisplay;
use crate::ui::compute::{ComputeView, format_engineering};
use crate::ui::{CameraAction, CatalogAction, UiFrameOutput};

#[allow(clippy::too_many_arguments)]
pub(super) fn object_properties(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    catalog: &fieldcad_catalog::CatalogLoadReport,
    compute: &ComputeView,
    object: &WorldObject,
    following: Option<ObjectId>,
    object_trajectories: &mut BTreeMap<ObjectId, TrajectoryDisplay>,
    user_constants: &fieldcad_expressions::UserConstantLibrary,
    output: &mut UiFrameOutput,
) {
    let tracking = object
        .catalog_link
        .as_ref()
        .is_some_and(|link| link.mode == CatalogLinkMode::Tracking);
    super::section(
        ui,
        "inspector_catalog_link",
        "Catalog template",
        object.catalog_link.is_some(),
        |ui| {
            if let Some(link) = &object.catalog_link {
                let mode = if tracking {
                    "tracking"
                } else {
                    "custom (unlinked)"
                };
                ui.label(format!("{} — {mode}", link.source_description));
                if let Some(reference) = &link.entry {
                    let state = match catalog.resolve_link(reference) {
                        fieldcad_catalog::LinkResolution::Exact(_) => "available",
                        fieldcad_catalog::LinkResolution::RelinkCandidate(_) => {
                            "moved; relink available"
                        }
                        fieldcad_catalog::LinkResolution::Unavailable => {
                            "catalog unavailable or changed"
                        }
                        fieldcad_catalog::LinkResolution::Ambiguous => "ambiguous catalog match",
                    };
                    ui.weak(state);
                    if ui.button("Open catalog entry").clicked() {
                        output.catalog_action = Some(CatalogAction::Open(Some(reference.clone())));
                    }
                    if tracking {
                        let current = catalog.entries.iter().find(|entry| {
                            entry
                                .reference
                                .as_ref()
                                .is_some_and(|candidate| candidate.same_source(reference))
                        });
                        let apply =
                            current.and_then(|entry| match (&entry.reference, &entry.result) {
                                (
                                    Some(current_reference),
                                    fieldcad_catalog::LoadResult::Available { spec, .. },
                                ) => {
                                    let placement = fieldcad_catalog::InstantiationPlacement {
                                        display_name: object.name.clone(),
                                        transform: object.transform,
                                        velocity: object.velocity,
                                        pinned: object.pinned,
                                        fallback_shape_radius: DEFAULT_AUTHORING_RADIUS,
                                    };
                                    fieldcad_catalog::instantiate_template(
                                        spec,
                                        current_reference,
                                        world.component_schemas(),
                                        placement,
                                    )
                                    .ok()
                                }
                                _ => None,
                            });
                        if ui
                            .add_enabled(
                                apply.is_some(),
                                egui::Button::new("Apply current template"),
                            )
                            .on_disabled_hover_text("The matching catalog entry is unavailable.")
                            .clicked()
                            && let Some(spec) = apply
                        {
                            output.edit(vec![WorldCommand::ApplyCatalogTemplate {
                                object: object.id,
                                expected_entry: reference.clone(),
                                shape: spec.shape,
                                components: spec.components,
                                link: spec
                                    .catalog_link
                                    .expect("catalog instantiation stamps provenance"),
                            }]);
                        }
                    }
                }
                if tracking && ui.button("Unlink from catalog").clicked() {
                    output.edit(vec![WorldCommand::UnlinkCatalogTemplate {
                        object: object.id,
                    }]);
                }
            } else {
                ui.weak("Not linked to a catalog entry.");
            }
            let link_label = if object.catalog_link.is_some() {
                "Change catalog link..."
            } else {
                "Link to catalog..."
            };
            if ui
                .button(link_label)
                .on_hover_text(
                    "Choose a catalog entry to attach this object to. Its declared shape/\
                     components are set on this object; anything else you've already \
                     attached is left untouched.",
                )
                .clicked()
            {
                output.catalog_action = Some(CatalogAction::BeginLink { object: object.id });
            }
        },
    );
    if let Some(name) = name_editor(ui, ("object_name", object.id), &object.name) {
        output.edit(vec![WorldCommand::SetObjectName {
            object: object.id,
            name,
        }]);
    }
    // Cosmetic-only, so — unlike `shape_editor` in `placement_editors` below
    // — deliberately not gated behind `!tracking`: an object's color is
    // never re-synced from its catalog template and stays freely editable
    // even while linked, the same way `name` already does above.
    if let Some(color) = color_editor(ui, object.color, &mut output.scene_edit_in_progress) {
        output.edit(vec![WorldCommand::SetObjectColor {
            object: object.id,
            color,
        }]);
    }
    super::section(ui, "inspector_placement", "Placement", true, |ui| {
        placement_editors(ui, object, tracking, output);
    });
    super::section(ui, "inspector_components", "Components", true, |ui| {
        object_components(ui, world, compute, object, tracking, user_constants, output);
    });
    if let Some(mass_kg) = inertial_mass_kg(object) {
        super::section(ui, "inspector_derived", "Derived values", true, |ui| {
            derived_values(ui, compute, object, mass_kg);
        });
    }
    super::section(ui, "inspector_trajectory", "Trajectory", false, |ui| {
        let display = object_trajectories.entry(object.id).or_default();
        super::trajectory_display_controls(
            ui,
            display,
            "Show trail",
            "Trace this object's recent motion as a flow-line-style trail, built \
             from its recorded position/velocity history.",
            compute,
        );
    });

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui.button("Focus selection  [F]").clicked() {
            output.camera_action = Some(CameraAction::FocusSelection);
        }
        let following_this = following == Some(object.id);
        let follow_label = if following_this {
            "Stop following"
        } else {
            "Follow"
        };
        if ui
            .selectable_label(following_this, follow_label)
            .on_hover_text(
                "Lock the camera onto this object so it appears motionless \
                 while the rest of the scene moves around it. Distance and \
                 angle stay adjustable with the mouse, or from the View \
                 window, while following.",
            )
            .clicked()
        {
            output.camera_action = Some(CameraAction::ToggleFollow(object.id));
        }
    });
    if ui.button("Remove object").clicked() {
        output.edit(vec![WorldCommand::RemoveObject(object.id)]);
    }
}

fn placement_editors(
    ui: &mut egui::Ui,
    object: &WorldObject,
    tracking: bool,
    output: &mut UiFrameOutput,
) {
    let mut position = object.transform.translation;
    let mut position_changed = false;
    egui::Grid::new("object_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Position");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                position_changed |=
                    coordinate_editor(ui, "x", &mut position.x, Dimension::LENGTH, editing);
                position_changed |=
                    coordinate_editor(ui, "y", &mut position.y, Dimension::LENGTH, editing);
                position_changed |=
                    coordinate_editor(ui, "z", &mut position.z, Dimension::LENGTH, editing);
            });
            ui.end_row();

            let mut velocity = object.velocity.linear;
            let mut velocity_changed = false;
            ui.label("Velocity");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                velocity_changed |=
                    coordinate_editor(ui, "vx", &mut velocity.x, Dimension::VELOCITY, editing);
                velocity_changed |=
                    coordinate_editor(ui, "vy", &mut velocity.y, Dimension::VELOCITY, editing);
                velocity_changed |=
                    coordinate_editor(ui, "vz", &mut velocity.z, Dimension::VELOCITY, editing);
            });
            ui.end_row();
            if velocity_changed
                && let Ok(velocity) = Velocity::new(velocity, object.velocity.angular)
            {
                output.edit(vec![WorldCommand::SetVelocity {
                    object: object.id,
                    velocity,
                }]);
            }

            ui.label("Extent");
            ui.add_enabled_ui(!tracking, |ui| shape_editor(ui, object, output));
            ui.end_row();

            ui.label("Motion");
            motion_editor(ui, object, output);
            ui.end_row();
        });

    if position_changed && let Ok(transform) = Transform::new(position, object.transform.rotation) {
        output.edit(vec![WorldCommand::SetTransform {
            object: object.id,
            transform,
        }]);
    }
}

fn shape_editor(ui: &mut egui::Ui, object: &WorldObject, output: &mut UiFrameOutput) {
    ui.horizontal(|ui| {
        let mut selected = ShapeKind::of(object.shape);
        let before = selected;
        egui::ComboBox::from_id_salt(("object_shape", object.id))
            .selected_text(selected.label())
            .width(110.0)
            .show_ui(ui, |ui| {
                for candidate in ShapeKind::ALL {
                    ui.selectable_value(&mut selected, candidate, candidate.label());
                }
            });
        if selected != before {
            let radius = match object.shape {
                Some(ObjectShape::Point { radius } | ObjectShape::Sphere { radius }) => radius,
                _ => DEFAULT_AUTHORING_RADIUS,
            };
            if let Ok(shape) = selected.build(radius) {
                output.edit(vec![WorldCommand::SetShape {
                    object: object.id,
                    shape,
                }]);
            }
        }

        match object.shape {
            Some(ObjectShape::Point { mut radius }) => {
                if radius_editor(ui, &mut radius, 0.0, &mut output.scene_edit_in_progress)
                    && let Ok(shape) = ObjectShape::point(radius)
                {
                    output.edit(vec![WorldCommand::SetShape {
                        object: object.id,
                        shape: Some(shape),
                    }]);
                }
            }
            Some(ObjectShape::Sphere { mut radius }) => {
                if radius_editor(ui, &mut radius, 1.0e-4, &mut output.scene_edit_in_progress)
                    && let Ok(shape) = ObjectShape::sphere(radius)
                {
                    output.edit(vec![WorldCommand::SetShape {
                        object: object.id,
                        shape: Some(shape),
                    }]);
                }
            }
            Some(ObjectShape::Box { half_extent }) => {
                ui.label(format!(
                    "{:.2} × {:.2} × {:.2} m",
                    half_extent.x * 2.0,
                    half_extent.y * 2.0,
                    half_extent.z * 2.0
                ));
            }
            None => {}
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShapeKind {
    None,
    Point,
    Sphere,
    Box,
}

impl ShapeKind {
    const ALL: [Self; 3] = [Self::None, Self::Point, Self::Sphere];

    fn of(shape: Option<ObjectShape>) -> Self {
        match shape {
            None => Self::None,
            Some(ObjectShape::Point { .. }) => Self::Point,
            Some(ObjectShape::Sphere { .. }) => Self::Sphere,
            Some(ObjectShape::Box { .. }) => Self::Box,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::None => "Marker",
            Self::Point => "Point",
            Self::Sphere => "Sphere",
            Self::Box => "Box",
        }
    }

    fn build(self, radius: f64) -> Result<Option<ObjectShape>, fieldcad_core::WorldError> {
        let shape = match self {
            Self::None => None,
            Self::Point => Some(ObjectShape::point(radius)?),
            Self::Sphere => Some(ObjectShape::sphere(radius.max(1.0e-4))?),
            Self::Box => Some(ObjectShape::boxed(glam::DVec3::splat(radius.max(1.0e-4)))?),
        };
        Ok(shape)
    }
}

fn motion_editor(ui: &mut egui::Ui, object: &WorldObject, output: &mut UiFrameOutput) {
    ui.horizontal(|ui| {
        let mut pinned = object.pinned;
        if ui
            .checkbox(&mut pinned, "Pinned")
            .on_hover_text(
                "Pinned: this object follows the position and velocity you set.\n\
                 Unpinned: a solver moves it, if it has the mass to be pushed.",
            )
            .changed()
        {
            output.edit(vec![WorldCommand::SetObjectPinned {
                object: object.id,
                pinned,
            }]);
        }
        ui.weak(motion_summary(object));
    });
}

pub(super) fn motion_summary(object: &WorldObject) -> &'static str {
    if object.pinned {
        if object.velocity.linear == DVec3::ZERO {
            "held in place"
        } else {
            "carried at the velocity you set"
        }
    } else if object
        .components
        .contains_key(&inertial_mass_component_id())
    {
        "moved by the forces acting on it"
    } else {
        "no inertia — add Inertial mass to make it movable"
    }
}

pub(super) fn inertial_mass_kg(object: &WorldObject) -> Option<f64> {
    object
        .components
        .get(&inertial_mass_component_id())
        .and_then(|properties| properties.scalar(&mass_property_id()))
        .filter(|mass| mass.is_finite() && *mass > 0.0)
}

fn derived_values(ui: &mut egui::Ui, compute: &ComputeView, object: &WorldObject, mass_kg: f64) {
    let velocity = object.velocity.linear;
    let kinetic_energy = relativistic_kinetic_energy(velocity, mass_kg);
    let momentum = relativistic_momentum(velocity, mass_kg);
    let force = compute.body_forces.get(&object.id).copied();

    egui::Grid::new("object_derived_values")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Kinetic energy").on_hover_text(
                "(γ−1)mc², the relativistic kinetic energy. Reduces to ½mv² well below \
                 light speed; excludes rest energy, which would otherwise swamp this number \
                 for anything not moving relativistically.",
            );
            ui.label(format!("{} J", format_engineering(kinetic_energy)));
            ui.end_row();

            ui.label("Momentum")
                .on_hover_text("p = γmv, the relativistic momentum.");
            ui.label(format_vector(momentum, "kg·m/s"));
            ui.end_row();

            ui.label("Force").on_hover_text(
                "What every active field system's coupling summed onto this body at the most \
                 recent simulation tick. Only meaningful while the body is one the dynamics \
                 system advances — pinned, and a solver's own pusher (rather than a summed \
                 force) both leave this unavailable.",
            );
            match force {
                Some(force) => ui.label(format_vector(force, "N")),
                None => ui.weak("not available"),
            };
            ui.end_row();
        });
}

pub(super) fn format_vector(vector: DVec3, unit: &str) -> String {
    format!(
        "({}, {}, {}) {unit}",
        format_engineering(vector.x),
        format_engineering(vector.y),
        format_engineering(vector.z),
    )
}

/// Component type IDs backed by at least one plugin in the current session's
/// composition — independent of `enabled`, matching `FieldSystemStatus`'s own
/// doc comment ("Component schemas... remain available while the system is
/// inactive"). A schema in `world.component_schemas()` but absent here has no
/// current plugin that can declare, validate, or act on it — most commonly,
/// data restored from a scene saved by a build with a plugin this one no
/// longer has (see `docs/tasks/prune-orphaned-component-schemas.md`).
fn active_component_schemas(compute: &ComputeView) -> HashSet<ComponentTypeId> {
    compute
        .field_systems
        .iter()
        .flat_map(|status| status.component_schemas.iter().cloned())
        .collect()
}

fn object_components(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    compute: &ComputeView,
    object: &WorldObject,
    tracking: bool,
    user_constants: &fieldcad_expressions::UserConstantLibrary,
    output: &mut UiFrameOutput,
) {
    let schemas = world.component_schemas();
    let active = active_component_schemas(compute);
    if tracking {
        ui.weak(
            "Template-owned components are read-only while this object tracks its catalog entry.",
        );
    }
    ui.add_enabled_ui(!tracking, |ui| {
        add_component_menu(ui, world, &active, object, output)
    });

    if object.components.is_empty() {
        ui.weak("None. This object has a position but no physics.");
    }

    for (id, properties) in &object.components {
        let Some(schema) = schemas.get(id) else {
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 60),
                format!("{id} (schema unavailable)"),
            );
            continue;
        };
        let orphaned = !active.contains(id);

        ui.add_space(4.0);
        egui::CollapsingHeader::new(if orphaned {
            format!("(!) {}", schema.display_name)
        } else {
            schema.display_name.clone()
        })
        .id_salt(("component", object.id, id))
        .default_open(true)
        .show(ui, |ui| {
            if orphaned {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 160, 60),
                    "No active plugin declares this component — it was restored from the \
                         document as-is. Values are preserved but nothing computes with them.",
                );
            }
            ui.add_enabled_ui(!tracking, |ui| {
                let mut edited = properties.clone();
                let mut changed = false;
                for property in &schema.properties {
                    if !property.is_relevant(&edited) {
                        continue;
                    }
                    changed |= if matches!(property.kind, PropertyKind::Scalar(_)) {
                        expression_property_editor(
                            ui,
                            object.id,
                            id,
                            property,
                            &mut edited,
                            world,
                            &compute.expressions,
                            &compute.expression_state,
                            user_constants,
                            &compute.global_variables,
                            output,
                        )
                    } else {
                        property_editor(
                            ui,
                            object.id,
                            property,
                            &mut edited,
                            &mut output.scene_edit_in_progress,
                        )
                    };
                }
                if changed && schema.validate(&edited).is_ok() {
                    output.edit(vec![WorldCommand::AttachComponent {
                        object: object.id,
                        component: id.clone(),
                        properties: edited,
                    }]);
                }
                if ui
                    .small_button("Remove component")
                    .on_hover_text(format!(
                        "Detach {} from this object",
                        schema.display_name.to_lowercase()
                    ))
                    .clicked()
                {
                    output.edit(vec![WorldCommand::DetachComponent {
                        object: object.id,
                        component: id.clone(),
                    }]);
                }
            });
        });
    }
}

fn add_component_menu(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    active: &HashSet<ComponentTypeId>,
    object: &WorldObject,
    output: &mut UiFrameOutput,
) {
    let available: Vec<_> = world
        .component_schemas()
        .values()
        .filter(|schema| !object.components.contains_key(&schema.id))
        .collect();

    ui.add_enabled_ui(!available.is_empty(), |ui| {
        ui.menu_button("+ Add", |ui| {
            for schema in available {
                if !active.contains(&schema.id) {
                    ui.add_enabled(
                        false,
                        egui::Button::new(format!("{} (!)", schema.display_name)),
                    )
                    .on_disabled_hover_text(
                        "No active plugin declares this component — it's data restored \
                             from the document, not something the current build can offer.",
                    );
                    continue;
                }
                let Ok(properties) = schema.default_properties() else {
                    ui.add_enabled(false, egui::Button::new(&schema.display_name))
                        .on_disabled_hover_text(
                            "This component has no default value and cannot be added here.",
                        );
                    continue;
                };
                if ui.button(&schema.display_name).clicked() {
                    output.edit(vec![WorldCommand::AttachComponent {
                        object: object.id,
                        component: schema.id.clone(),
                        properties,
                    }]);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(if object.components.is_empty() {
            "Give this object a physical property"
        } else {
            "Add another physical property"
        });
    });
}

pub(super) fn property_editor(
    ui: &mut egui::Ui,
    object: ObjectId,
    schema: &PropertySchema,
    values: &mut PropertyBag,
    editing: &mut bool,
) -> bool {
    let relevant = schema.is_relevant(values);
    let mut changed = false;
    if !relevant {
        // The property is hidden from the generic editor when its condition is
        // unsatisfied — that's the caller's choice. We don't render it here.
        return changed;
    }
    ui.horizontal(|ui| {
        let label = ui.label(&schema.display_name);
        if let Some(description) = &schema.description {
            let _ = label.on_hover_text(description);
        }
        changed = property_widget(ui, object, schema, values, editing);
    });
    changed
}

#[derive(Clone, Debug)]
struct ExpressionDraft {
    active: bool,
    formula: crate::ui::expression_draft::PropertyFormulaDraft,
}

#[allow(clippy::too_many_arguments)]
fn expression_property_editor(
    ui: &mut egui::Ui,
    object: ObjectId,
    component: &ComponentTypeId,
    schema: &PropertySchema,
    values: &mut PropertyBag,
    world: &WorldSnapshot,
    expressions: &fieldcad_expressions::ExpressionDocument,
    expression_state: &fieldcad_expressions::ExpressionState,
    user_constants: &fieldcad_expressions::UserConstantLibrary,
    global_variables: &[(
        fieldcad_core::PluginId,
        fieldcad_plugin_api::ExportedVariable,
    )],
    output: &mut UiFrameOutput,
) -> bool {
    let PropertyKind::Scalar(dimension) = schema.kind else {
        return false;
    };
    let target = fieldcad_expressions::PropertyTarget {
        object,
        component: component.clone(),
        property: schema.id.clone(),
    };
    let binding = expressions
        .bindings
        .iter()
        .find(|binding| binding.target == target);
    let magnitude = values
        .get(&schema.id)
        .and_then(|value| match value {
            PropertyValue::Scalar(quantity) => Some(quantity.si_value()),
            _ => None,
        })
        .unwrap_or_default();
    let editor_id = ui.make_persistent_id(("property-expression", &target));
    let authoritative_source = binding.map_or_else(
        || literal_expression_source(magnitude, dimension),
        |binding| binding.source.as_str().to_owned(),
    );
    let mut draft = ui.data_mut(|data| {
        data.get_temp::<ExpressionDraft>(editor_id)
            .unwrap_or_else(|| ExpressionDraft {
                active: binding.is_some(),
                formula: crate::ui::expression_draft::PropertyFormulaDraft(
                    crate::ui::expression_draft::AuthorityDraft::new(
                        authoritative_source.clone(),
                        expression_state.graph_hash.clone(),
                    ),
                ),
            })
    });
    draft
        .formula
        .0
        .reconcile(authoritative_source, expression_state.graph_hash.clone());
    if binding.is_some() && !draft.active {
        draft.active = true;
    }

    let mut literal_changed = false;
    let mut commit_requested = false;
    ui.horizontal(|ui| {
        let label = ui.label(&schema.display_name);
        if let Some(description) = &schema.description {
            let _ = label.on_hover_text(description);
        }
        if draft.active {
            let source_text = draft.formula.0.edited_mut();
            let response = ui.add(
                egui::TextEdit::singleline(source_text)
                    .id(editor_id.with("source"))
                    .desired_width(150.0),
            );
            output.scene_edit_in_progress |= response.has_focus();
            commit_requested =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            super::expression_editor::insert_constant_menu(
                ui,
                source_text,
                expressions,
                user_constants,
                global_variables,
                world,
                schema.live_binding,
                output,
            );
            if ui
                .small_button("Freeze")
                .on_hover_text("Replace the formula with its current resolved literal value")
                .clicked()
            {
                output.submit(fieldcad_simulation::CommandPayload::CommitExpressions(
                    vec![
                        fieldcad_expressions::ExpressionCommand::ClearPropertyExpression(
                            target.clone(),
                        ),
                    ],
                ));
                draft.active = false;
                draft.formula.0.reset();
            }
        } else {
            let mut edited = magnitude;
            literal_changed = scalar_editor(
                ui,
                &mut edited,
                dimension,
                &mut output.scene_edit_in_progress,
            );
            if literal_changed && let Ok(quantity) = Quantity::new(edited, dimension) {
                values.insert(schema.id.clone(), PropertyValue::Scalar(quantity));
            }
            if ui
                .small_button("fx")
                .on_hover_text("Author a dimension-checked expression")
                .clicked()
            {
                draft.active = true;
                draft.formula.0.reset();
            }
        }
    });

    if draft.active {
        ui.indent(editor_id.with("details"), |ui| {
            let drafted_source = draft.formula.0.edited().clone();
            let candidate = expressions.apply([
                fieldcad_expressions::ExpressionCommand::SetPropertyExpression(
                    fieldcad_expressions::PropertyBinding {
                        target: target.clone(),
                        source: drafted_source.as_str().into(),
                    },
                ),
            ]);
            let preview = match candidate {
                Ok(document) => crate::ui::expression_draft::preview_document(
                    &document,
                    world,
                    fieldcad_expressions::ExpressionSubject::Property(target.clone()),
                    expression_state,
                ),
                Err(error) => crate::ui::expression_draft::DraftPreview {
                    values: None,
                    diagnostic: Some(fieldcad_expressions::ExpressionDiagnostic {
                        subject: fieldcad_expressions::ExpressionSubject::Property(target.clone()),
                        error,
                    }),
                    dependents: Vec::new(),
                },
            };
            if let Some(result) = &preview.values
                && let Some(value) = result.properties.get(&target)
            {
                ui.weak(format!(
                    "Resolved: {}",
                    fieldcad_core::format_si_value(value.si_value(), dimension).unwrap_or_else(
                        || format!(
                            "{} {}",
                            format_engineering(value.si_value()),
                            dimension.unit_symbol()
                        )
                    )
                ));
            }
            let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            let valid_dirty =
                crate::ui::expression_draft::should_submit(&draft.formula.0, &preview, true);
            if (commit_requested
                || ui
                    .add_enabled(valid_dirty, egui::Button::new("Apply"))
                    .clicked())
                && valid_dirty
            {
                output.submit(fieldcad_simulation::CommandPayload::CommitExpressions(
                    vec![
                        fieldcad_expressions::ExpressionCommand::SetPropertyExpression(
                            fieldcad_expressions::PropertyBinding {
                                target: target.clone(),
                                source: drafted_source.as_str().into(),
                            },
                        ),
                    ],
                ));
                draft.formula.0.mark_submitted();
            }
            if ui.small_button("Cancel").clicked() || escape {
                draft.formula.0.reset();
            }
            if let Some(diagnostic) = &preview.diagnostic {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 100, 90),
                    diagnostic.error.to_string(),
                );
                if let Some(span) = diagnostic.error.span {
                    ui.weak(format!("Source bytes {}..{}", span.start, span.end));
                }
            }
            if !preview.dependents.is_empty() {
                ui.weak(format!(
                    "Affected: {}",
                    preview
                        .dependents
                        .iter()
                        .map(|subject| crate::ui::expression_draft::subject_label(
                            subject,
                            expressions
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if draft.formula.0.authority_changed() {
                ui.weak(
                    "Authoritative value changed; Cancel restores the latest accepted formula.",
                );
            }
            if let Some(node) = expression_state.nodes.iter().find(|node| {
                node.subject == fieldcad_expressions::ExpressionSubject::Property(target.clone())
            }) && node.status != fieldcad_expressions::ExpressionNodeStatus::Resolved
            {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 100, 90),
                    format!("Authoritative dependency status: {:?}", node.status),
                );
            }
        });
    }
    ui.data_mut(|data| data.insert_temp(editor_id, draft));
    literal_changed
}

fn literal_expression_source(value: f64, dimension: Dimension) -> String {
    let unit = dimension.unit_symbol();
    if unit.is_empty() {
        format_engineering(value)
    } else {
        format!("{} {unit}", format_engineering(value))
    }
}

fn property_widget(
    ui: &mut egui::Ui,
    object: ObjectId,
    schema: &PropertySchema,
    values: &mut PropertyBag,
    editing: &mut bool,
) -> bool {
    let mut changed = false;
    {
        match (&schema.kind, values.get(&schema.id).cloned()) {
            (PropertyKind::Scalar(dimension), value) => {
                let mut magnitude = match value {
                    Some(PropertyValue::Scalar(quantity)) => quantity.si_value(),
                    _ => 0.0,
                };
                if scalar_editor(ui, &mut magnitude, *dimension, editing)
                    && let Ok(quantity) = Quantity::new(magnitude, *dimension)
                {
                    values.insert(schema.id.clone(), PropertyValue::Scalar(quantity));
                    changed = true;
                }
            }
            (PropertyKind::Vector(dimension), value) => {
                let mut vector = match value {
                    Some(PropertyValue::Vector(quantity)) => quantity.si_value(),
                    _ => DVec3::ZERO,
                };
                let mut vector_changed = false;
                vector_changed |= coordinate_editor(ui, "x", &mut vector.x, *dimension, editing);
                vector_changed |= coordinate_editor(ui, "y", &mut vector.y, *dimension, editing);
                vector_changed |= coordinate_editor(ui, "z", &mut vector.z, *dimension, editing);
                if vector_changed && let Ok(quantity) = VectorQuantity::new(vector, *dimension) {
                    values.insert(schema.id.clone(), PropertyValue::Vector(quantity));
                    changed = true;
                }
            }
            (PropertyKind::Boolean, value) => {
                let mut flag = matches!(value, Some(PropertyValue::Boolean(true)));
                if ui.checkbox(&mut flag, "").changed() {
                    values.insert(schema.id.clone(), PropertyValue::Boolean(flag));
                    changed = true;
                }
            }
            (PropertyKind::Text, value) => {
                let mut text = match value {
                    Some(PropertyValue::Text(text)) => text,
                    _ => String::new(),
                };
                let response = ui.text_edit_singleline(&mut text);
                if note_held_edit(&response, editing) {
                    values.insert(schema.id.clone(), PropertyValue::Text(text));
                    changed = true;
                }
            }
            (PropertyKind::Choice(options), value) => {
                let mut selected = match value {
                    Some(PropertyValue::Choice(choice)) => choice,
                    _ => options.first().cloned().unwrap_or_default(),
                };
                let before = selected.clone();
                egui::ComboBox::from_id_salt(("property", object, &schema.id))
                    .selected_text(&selected)
                    .show_ui(ui, |ui| {
                        for option in options {
                            ui.selectable_value(&mut selected, option.clone(), option);
                        }
                    });
                if selected != before {
                    values.insert(schema.id.clone(), PropertyValue::Choice(selected));
                    changed = true;
                }
            }
        }
    }
    changed
}

fn scalar_editor(
    ui: &mut egui::Ui,
    value: &mut f64,
    dimension: Dimension,
    editing: &mut bool,
) -> bool {
    let speed = (value.abs() * 0.01).max(f64::MIN_POSITIVE);
    let mut drag = egui::DragValue::new(value)
        .speed(speed)
        .update_while_editing(false);

    if dimension.si_prefix_root().is_some() {
        // SI prefix supported — formatter includes the unit, no suffix needed.
        drag = drag
            .custom_formatter(move |val, _| fieldcad_core::format_si_value(val, dimension).unwrap())
            .custom_parser(move |text| {
                fieldcad_core::parse_si_value(text, dimension).or_else(|| text.trim().parse().ok())
            });
    } else {
        // Compound dimension — fall back to engineering format + unit suffix.
        let suffix = format!(" {}", dimension.unit_symbol());
        drag = drag
            .custom_formatter(|val, _| format_engineering(val))
            .custom_parser(|text| text.trim().parse().ok())
            .suffix(suffix);
    }

    let response = ui.add(drag);
    note_held_edit(&response, editing)
}

/// `minimum` is advisory only, same as everywhere else `coordinate_editor` is
/// used for a shape/position field: `ObjectShape::point`/`sphere`'s own
/// validation is what actually rejects a non-positive radius (silently, by
/// returning `Err` at the call site rather than submitting), so a UI-level
/// range clamp would only duplicate that check while blocking the SI-prefix
/// parser (`coordinate_editor`) from accepting "6400 km" the same way every
/// other length field in this inspector already does.
fn radius_editor(ui: &mut egui::Ui, radius: &mut f64, _minimum: f64, editing: &mut bool) -> bool {
    coordinate_editor(ui, "r", radius, Dimension::LENGTH, editing)
}
