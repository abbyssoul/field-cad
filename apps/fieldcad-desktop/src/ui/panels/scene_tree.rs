//! Scene tree panel (left side): list of simulation node, objects, and
//! measurement instruments.

use fieldcad_catalog::instantiate_template;
use fieldcad_core::{ObjectShape, ObjectSpec, Transform, Velocity, WorldCommand, WorldSnapshot};
use fieldcad_simulation::CommandPayload;

use crate::scene::SceneSelection;
use crate::ui::{FrameContext, UiFrameOutput, UiModel};

pub fn scene_tree(
    root: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    egui::Panel::left("scene_panel")
        .resizable(true)
        .default_size(200.0)
        .size_range(160.0..=320.0)
        .show(root, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    ui.heading("Scene");
                    ui.separator();

                    simulation_section(ui, model, frame);
                    object_section(ui, model, frame, output);
                    measurement_section(ui, model, frame, output);
                });
        });
}

fn simulation_section(ui: &mut egui::Ui, model: &mut UiModel, frame: &FrameContext<'_>) {
    let id = ui.make_persistent_id("scene_simulation_section");
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, |ui| {
            if ui
                .selectable_label(model.world_selected, "🌐  Simulation")
                .on_hover_text("Domain, active field systems, sampling, and compute status")
                .clicked()
            {
                model.select_world();
            }
        })
        .body(|ui| {
            if ui
                .selectable_label(model.variables_selected, "Variables")
                .on_hover_text("Document and reusable user constants referenced by expressions")
                .clicked()
            {
                model.select_variables();
            }
            if frame.compute.field_systems.is_empty() {
                ui.weak("No field systems available.");
            }
            for system in &frame.compute.field_systems {
                // ☑/☐, not ◈/◇: neither diamond glyph exists in egui's
                // bundled fonts and both render as tofu boxes.
                let mark = if system.enabled { "☑" } else { "☐" };
                let row = ui
                    .selectable_label(false, format!("{mark}  {}", system.plugin.display_name))
                    .on_hover_text(format!(
                        "{}\n{}",
                        system.plugin.description,
                        if system.enabled {
                            "Active. Select Simulation to configure it."
                        } else {
                            "Inactive: it does not simulate or publish. Select Simulation to \
                             enable it."
                        }
                    ));
                if row.clicked() {
                    model.select_world();
                }
            }
        });
    ui.add_space(6.0);
}

fn object_section(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let object_count = frame
        .world
        .objects()
        .values()
        .filter(|object| !object.derived)
        .count();
    let title = format!("Objects ({object_count})");
    super::section(ui, "scene_objects_section", title, true, |ui| {
        let choices: Vec<(String, ObjectPreset)> = {
            let mut choices: Vec<(String, ObjectPreset)> =
                vec![("Empty".to_owned(), ObjectPreset::Empty)];
            for entry in &frame.catalog.entries {
                let is_available = matches!(
                    &entry.result,
                    fieldcad_catalog::LoadResult::Available { .. }
                );
                if let (Some(reference), true) = (&entry.reference, is_available) {
                    if frame.quick_add_hidden.contains(reference) {
                        continue;
                    }
                    choices.push((
                        quick_add_label(&frame.catalog.entries, reference),
                        ObjectPreset::Catalog(reference.clone()),
                    ));
                }
            }
            choices
        };
        let ref_choices: Vec<(&str, ObjectPreset)> = choices
            .iter()
            .map(|(label, preset)| (label.as_str(), preset.clone()))
            .collect();
        if let Some(preset) = super::split_add_button(
            ui,
            "Add object",
            "Add an object at the origin.\n\
             Give it charge or mass in the inspector to couple it to a field.",
            ObjectPreset::Empty,
            &ref_choices,
        ) {
            output.submit(match preset {
                ObjectPreset::Empty => new_object_command(frame.world),
                ObjectPreset::Catalog(reference) => catalog_object_command(frame, &reference),
            });
        }
        ui.add_space(4.0);

        if object_count == 0 {
            ui.weak("No objects yet.");
        }
        for object in frame
            .world
            .objects()
            .values()
            .filter(|object| !object.derived)
        {
            match entity_row(
                ui,
                "▣",
                &object.name,
                object.visible,
                model.selection == Some(object.id),
                "Delete object",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![WorldCommand::SetObjectVisible {
                        object: object.id,
                        visible: !object.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::Object(object.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![WorldCommand::RemoveObject(object.id)]);
                }
                None => {}
            }
        }
    });
    ui.add_space(6.0);
}

fn quick_add_label(
    entries: &[fieldcad_catalog::CatalogEntry],
    reference: &fieldcad_core::CatalogEntryRef,
) -> String {
    let collides = entries
        .iter()
        .filter_map(|entry| entry.reference.as_ref())
        .any(|other| other != reference && other.template == reference.template);
    if !collides {
        return reference.template.clone();
    }
    match &reference.origin {
        fieldcad_core::CatalogOrigin::Global {
            relative_path,
            document_ordinal,
        } => {
            format!(
                "{} — user: {} #{}",
                reference.template, relative_path, document_ordinal
            )
        }
        fieldcad_core::CatalogOrigin::Document { .. } => {
            format!("{} — document: {}", reference.template, reference.catalog)
        }
    }
}

fn measurement_section(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let instruments = frame.world.probes().len()
        + frame.world.distance_probes().len()
        + frame.world.mass_aggregate_probes().len()
        + frame.world.planes().len()
        + frame.world.boxes().len()
        + frame.world.spheres().len();
    let title = format!("Measurement ({instruments})");
    super::section(ui, "scene_measurement_section", title, true, |ui| {
        ui.weak("Not simulated").on_hover_text(
            "Probes and slice planes sample the field for you.\n\
             They carry no charge or mass and never alter the result.",
        );
        let mut objects_iter = frame.world.objects().values();
        let distance_pair = match (objects_iter.next(), objects_iter.next()) {
            (Some(first), Some(second)) => Some((first.id, second.id)),
            _ => None,
        };
        ui.menu_button("+ Measurement probe", |ui| {
            if ui
                .button("Point probe")
                .on_hover_text("Record field values at a point")
                .clicked()
            {
                let channels = frame
                    .compute
                    .field_systems
                    .iter()
                    .flat_map(|system| &system.channels)
                    .map(|channel| channel.id.clone())
                    .collect();
                output.edit(vec![fieldcad_core::WorldCommand::CreateProbe(
                    fieldcad_core::ProbeSpec::at(
                        format!("Probe {}", frame.world.probes().len() + 1),
                        glam::DVec3::new(1.0, 0.0, 0.6),
                        channels,
                    ),
                )]);
                ui.close();
            }
            if ui
                .add_enabled(distance_pair.is_some(), egui::Button::new("Distance"))
                .on_hover_text("Measure the live distance between two objects")
                .clicked()
                && let Some((first, second)) = distance_pair
            {
                output.edit(vec![fieldcad_core::WorldCommand::CreateDistanceProbe(
                    fieldcad_core::DistanceProbeSpec::new(
                        format!("Distance {}", frame.world.distance_probes().len() + 1),
                        first,
                        second,
                    ),
                )]);
                ui.close();
            }
            if ui
                .button("Plane")
                .on_hover_text("Draw the field across a slice")
                .clicked()
            {
                output.edit(vec![measurement_command(
                    frame.world,
                    MeasurementPreset::Plane,
                )]);
                ui.close();
            }
            if ui.button("Box").clicked() {
                output.edit(vec![measurement_command(
                    frame.world,
                    MeasurementPreset::Box,
                )]);
                ui.close();
            }
            if ui.button("Sphere").clicked() {
                output.edit(vec![measurement_command(
                    frame.world,
                    MeasurementPreset::Sphere,
                )]);
                ui.close();
            }
            if ui
                .button("Center of mass")
                .on_hover_text(
                    "Track the centroid of every mass-bearing object, minus an exclusion list",
                )
                .clicked()
            {
                let name = format!(
                    "Center of mass {}",
                    frame.world.mass_aggregate_probes().len() + 1
                );
                output.edit(vec![fieldcad_core::WorldCommand::CreateMassAggregateProbe(
                    fieldcad_core::MassAggregateProbeSpec::new(
                        name,
                        fieldcad_core::MassSelection::Universe {
                            excluded: std::collections::BTreeSet::new(),
                        },
                    ),
                )]);
                ui.close();
            }
        });

        for probe in frame.world.probes().values() {
            match entity_row(
                ui,
                // ◎, not ◉: the latter is missing from egui's bundled
                // fonts and renders as a tofu box.
                "◎",
                &probe.name,
                probe.visible,
                model.probe_selection == Some(probe.id),
                "Delete probe",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![fieldcad_core::WorldCommand::SetProbeVisible {
                        probe: probe.id,
                        visible: !probe.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::Probe(probe.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemoveProbe(probe.id)]);
                }
                None => {}
            }
        }

        for probe in frame.world.distance_probes().values() {
            match entity_row(
                ui,
                "↔",
                &probe.name,
                probe.visible,
                model.distance_probe_selection == Some(probe.id),
                "Delete distance probe",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![fieldcad_core::WorldCommand::SetDistanceProbeVisible {
                        probe: probe.id,
                        visible: !probe.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.select_distance_probe(probe.id);
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemoveDistanceProbe(
                        probe.id,
                    )]);
                }
                None => {}
            }
        }

        for probe in frame.world.mass_aggregate_probes().values() {
            match entity_row(
                ui,
                // ●, not the astronomical/physics "circled dot" glyph:
                // unverified against egui's bundled fonts, same tofu-box
                // risk noted elsewhere in this file.
                "●",
                &probe.name,
                probe.visible,
                model.mass_aggregate_probe_selection == Some(probe.id),
                "Delete center of mass",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![
                        fieldcad_core::WorldCommand::SetMassAggregateProbeVisible {
                            probe: probe.id,
                            visible: !probe.visible,
                        },
                    ]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::MassAggregateProbe(probe.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemoveMassAggregateProbe(
                        probe.id,
                    )]);
                }
                None => {}
            }
        }

        for plane in frame.world.planes().values() {
            match entity_row(
                ui,
                "▦",
                &plane.name,
                plane.visible,
                model.plane_selection == Some(plane.id),
                "Delete plane",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![fieldcad_core::WorldCommand::SetPlaneVisible {
                        plane: plane.id,
                        visible: !plane.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::Plane(plane.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemovePlane(plane.id)]);
                }
                None => {}
            }
        }

        for field_box in frame.world.boxes().values() {
            match entity_row(
                ui,
                "▧",
                &field_box.name,
                field_box.visible,
                model.box_selection == Some(field_box.id),
                "Delete box",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![fieldcad_core::WorldCommand::SetBoxVisible {
                        region: field_box.id,
                        visible: !field_box.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::Box(field_box.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemoveBox(field_box.id)]);
                }
                None => {}
            }
        }

        for sphere in frame.world.spheres().values() {
            match entity_row(
                ui,
                "◯",
                &sphere.name,
                sphere.visible,
                model.sphere_selection == Some(sphere.id),
                "Delete sphere",
            ) {
                Some(EntityRowAction::ToggleVisibility) => {
                    output.edit(vec![fieldcad_core::WorldCommand::SetSphereVisible {
                        sphere: sphere.id,
                        visible: !sphere.visible,
                    }]);
                }
                Some(EntityRowAction::Select) => {
                    model.set_scene_selection(Some(SceneSelection::Sphere(sphere.id)));
                }
                Some(EntityRowAction::Delete) => {
                    output.edit(vec![fieldcad_core::WorldCommand::RemoveSphere(sphere.id)]);
                }
                None => {}
            }
        }
    });
}

fn visibility_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    // ☑/☐ (not ◉/○) because egui's bundled fonts lack a glyph for ◉ and
    // render it as a tofu box.
    ui.small_button(if visible { "☑" } else { "☐" })
        .on_hover_text(if visible {
            "Hide in viewport"
        } else {
            "Show in viewport"
        })
}

enum EntityRowAction {
    ToggleVisibility,
    Select,
    Delete,
}

fn entity_row(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    visible: bool,
    selected: bool,
    delete_hover: &str,
) -> Option<EntityRowAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if visibility_button(ui, visible).clicked() {
            action = Some(EntityRowAction::ToggleVisibility);
        }
        if ui
            .selectable_label(selected, format!("{icon}  {name}"))
            .on_hover_text(name)
            .clicked()
        {
            action = Some(EntityRowAction::Select);
        }
        if ui.small_button("×").on_hover_text(delete_hover).clicked() {
            action = Some(EntityRowAction::Delete);
        }
    });
    action
}

pub(super) fn entity_actions(
    ui: &mut egui::Ui,
    output: &mut UiFrameOutput,
    kind: &str,
    duplicate: impl FnOnce() -> fieldcad_core::WorldCommand,
    remove: impl FnOnce() -> fieldcad_core::WorldCommand,
) {
    ui.add_space(10.0);
    if ui.button(format!("Duplicate {kind}")).clicked() {
        output.edit(vec![duplicate()]);
    }
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(super::CameraAction::FocusSelection);
    }
    if ui.button(format!("Remove {kind}")).clicked() {
        output.edit(vec![remove()]);
    }
}

/// The default radius for an object whose shape was just chosen.
pub(super) const DEFAULT_AUTHORING_RADIUS: f64 = 0.15;

#[derive(Clone, PartialEq)]
enum ObjectPreset {
    Empty,
    Catalog(fieldcad_core::CatalogEntryRef),
}

// TODO: Ensure that position is finite and within bounds
fn next_object_position(world: &WorldSnapshot) -> glam::DVec3 {
    let index = world.objects().len();
    glam::DVec3::new(index as f64 * 0.6, 0.0, 0.6)
}

pub(super) fn new_object_command(world: &WorldSnapshot) -> CommandPayload {
    let index = world.objects().len() + 1;
    CommandPayload::CommitWorld(vec![fieldcad_core::WorldCommand::CreateObject(
        ObjectSpec::new(format!("Object {index}"))
            .with_transform(Transform::at_finite(next_object_position(world)))
            .with_shape(
                ObjectShape::point(DEFAULT_AUTHORING_RADIUS).expect("static radius is valid"),
            ),
    )])
}

pub(super) fn catalog_object_command(
    frame: &FrameContext<'_>,
    reference: &fieldcad_core::CatalogEntryRef,
) -> CommandPayload {
    let entry = frame
        .catalog
        .entries
        .iter()
        .find(|e| e.reference.as_ref() == Some(reference));
    let Some(entry) = entry else {
        return new_object_command(frame.world);
    };
    let spec = match &entry.result {
        fieldcad_catalog::LoadResult::Available { spec, .. } => spec,
        _ => return new_object_command(frame.world),
    };
    let existing_names = frame.world.objects().values().map(|o| o.name.as_str());
    let display_name = fieldcad_catalog::suggest_display_name(&reference.template, existing_names);
    let pos = next_object_position(frame.world);
    let placement = fieldcad_catalog::InstantiationPlacement {
        display_name,
        transform: Transform::at_finite(pos),
        velocity: Velocity::default(),
        pinned: false,
        fallback_shape_radius: DEFAULT_AUTHORING_RADIUS,
    };
    let spec =
        match instantiate_template(spec, reference, frame.world.component_schemas(), placement) {
            Ok(spec) => spec,
            Err(_) => return CommandPayload::CommitWorld(Vec::new()),
        };
    CommandPayload::CommitWorld(vec![fieldcad_core::WorldCommand::CreateObject(spec)])
}

#[derive(Clone, Copy, PartialEq)]
enum MeasurementPreset {
    Plane,
    Box,
    Sphere,
}

fn measurement_command(
    world: &WorldSnapshot,
    preset: MeasurementPreset,
) -> fieldcad_core::WorldCommand {
    use fieldcad_core::{FieldBoxSpec, FieldSphereSpec, SlicePlaneSpec};
    match preset {
        MeasurementPreset::Plane => fieldcad_core::WorldCommand::CreatePlane(
            SlicePlaneSpec::new(
                format!("XY plane {}", world.planes().len() + 1),
                glam::DVec3::ZERO,
                glam::DVec3::Z,
            )
            .and_then(|plane| plane.with_half_extent(glam::DVec2::splat(4.0)))
            .expect("static plane parameters are valid"),
        ),
        MeasurementPreset::Box => fieldcad_core::WorldCommand::CreateBox(
            FieldBoxSpec::new(
                format!("Box {}", world.boxes().len() + 1),
                glam::DVec3::ZERO,
                glam::DVec3::splat(1.0),
            )
            .expect("static box parameters are valid"),
        ),
        MeasurementPreset::Sphere => fieldcad_core::WorldCommand::CreateSphere(
            FieldSphereSpec::new(
                format!("Sphere {}", world.spheres().len() + 1),
                glam::DVec3::ZERO,
                0.75,
            )
            .expect("static sphere parameters are valid"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fieldcad_catalog::{CatalogEntry, DocumentOrdinal, LoadResult, SourceLocation};
    use fieldcad_core::{CatalogEntryRef, CatalogOrigin};

    use super::quick_add_label;

    fn entry(reference: CatalogEntryRef) -> CatalogEntry {
        CatalogEntry {
            source: SourceLocation {
                file: PathBuf::from("catalog.yaml"),
                document_ordinal: DocumentOrdinal::new(0),
            },
            reference: Some(reference),
            identity: None,
            result: LoadResult::Invalid {
                diagnostics: Vec::new(),
            },
        }
    }

    fn global_reference(path: &str) -> CatalogEntryRef {
        CatalogEntryRef {
            catalog: "personal".to_owned(),
            template: "particle-a".to_owned(),
            origin: CatalogOrigin::Global {
                relative_path: path.to_owned(),
                document_ordinal: 1,
            },
            api_version: "fieldcad.catalog/v1".to_owned(),
            fingerprint: path.to_owned(),
        }
    }

    #[test]
    fn colliding_quick_add_entries_include_their_source() {
        let first = global_reference("one.yaml");
        let second = global_reference("two.yaml");
        let entries = vec![entry(first.clone()), entry(second.clone())];

        assert_eq!(
            quick_add_label(&entries, &first),
            "particle-a — user: one.yaml #1"
        );
        assert_eq!(
            quick_add_label(&entries, &second),
            "particle-a — user: two.yaml #1"
        );
    }
}
