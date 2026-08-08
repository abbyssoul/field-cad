//! Inspector sections for editing an object's properties: placement, shape,
//! motion, components, and derived values.

use fieldcad_core::{
    Dimension, ObjectId, ObjectShape, PropertyBag, PropertyKind, PropertySchema, PropertyValue,
    Quantity, Transform, VectorQuantity, Velocity, WorldCommand, WorldObject, WorldSnapshot,
    relativistic_kinetic_energy, relativistic_momentum,
};
use fieldcad_sources::{inertial_mass_component_id, mass_property_id};
use glam::DVec3;

use super::scene_tree::DEFAULT_AUTHORING_RADIUS;
use super::{coordinate_editor, name_editor, note_held_edit};
use crate::ui::compute::{ComputeView, format_engineering};
use crate::ui::{CameraAction, UiFrameOutput};

pub(super) fn object_properties(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    compute: &ComputeView,
    object: &WorldObject,
    following: Option<ObjectId>,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("object_name", object.id), &object.name) {
        output.edit(vec![WorldCommand::SetObjectName {
            object: object.id,
            name,
        }]);
    }
    super::section(ui, "inspector_placement", "Placement", true, |ui| {
        placement_editors(ui, object, output);
    });
    super::section(ui, "inspector_components", "Components", true, |ui| {
        object_components(ui, world, object, output);
    });
    if let Some(mass_kg) = inertial_mass_kg(object) {
        super::section(ui, "inspector_derived", "Derived values", true, |ui| {
            derived_values(ui, compute, object, mass_kg);
        });
    }

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

fn placement_editors(ui: &mut egui::Ui, object: &WorldObject, output: &mut UiFrameOutput) {
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

            ui.label("Extent");
            shape_editor(ui, object, output);
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

fn object_components(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    object: &WorldObject,
    output: &mut UiFrameOutput,
) {
    let schemas = world.component_schemas();
    add_component_menu(ui, world, object, output);

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

        ui.add_space(4.0);
        egui::CollapsingHeader::new(&schema.display_name)
            .id_salt(("component", object.id, id))
            .default_open(true)
            .show(ui, |ui| {
                let mut edited = properties.clone();
                let mut changed = false;
                for property in &schema.properties {
                    changed |= property_editor(
                        ui,
                        object.id,
                        property,
                        &mut edited,
                        &mut output.scene_edit_in_progress,
                    );
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
    }
}

fn add_component_menu(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
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
    ui.add_enabled_ui(relevant, |ui| {
        let response = ui.horizontal(|ui| {
            ui.label(&schema.display_name);
            changed = property_widget(ui, object, schema, values, editing);
        });
        if let Some(condition) = schema.relevant_when.as_ref().filter(|_| !relevant) {
            response
                .response
                .on_disabled_hover_text(condition.because.clone());
        }
    });
    changed
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

fn radius_editor(ui: &mut egui::Ui, radius: &mut f64, minimum: f64, editing: &mut bool) -> bool {
    let response = ui.add(
        egui::DragValue::new(radius)
            .speed(0.01)
            .range(minimum..=f64::INFINITY)
            .prefix("r: ")
            .suffix(" m"),
    );
    note_held_edit(&response, editing)
}
