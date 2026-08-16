//! Inspector section for editing a distance probe: the two measured objects,
//! visibility, live reading, and history plot.

use fieldcad_core::{DistanceProbe, ObjectId, WorldCommand, WorldObject, WorldSnapshot};
use fieldcad_simulation::DistanceHistory;

use super::name_editor;
use crate::ui::plot::distance_history_plot;
use crate::ui::{UiFrameOutput, UiModel};

pub(super) fn distance_probe_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    probe: &DistanceProbe,
    world: &WorldSnapshot,
    history: &DistanceHistory,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("distance_probe_name", probe.id), &probe.name) {
        output.edit(vec![WorldCommand::SetDistanceProbeName {
            probe: probe.id,
            name,
        }]);
    }
    super::section(
        ui,
        "inspector_distance_probe_objects",
        "Objects",
        true,
        |ui| distance_probe_object_pickers(ui, probe, world, output),
    );
    super::section(
        ui,
        "inspector_distance_probe_history",
        "History",
        false,
        |ui| {
            let series = model.distance_probe_series.entry(probe.id).or_default();
            distance_history_plot(ui, probe.id, history, series);
        },
    );

    ui.add_space(10.0);
    if ui.button("Open floating plot").clicked() {
        model.open_distance_probe_plot(probe.id);
    }
    if ui
        .button("Export…")
        .on_hover_text(
            "Save this distance probe's recorded history to a standalone \
             fieldcad.observation-export/v1 file, independent of the scene.",
        )
        .clicked()
    {
        output.app_action = Some(crate::ui::AppAction::ExportObservations(
            fieldcad_server::ObservationExportScope {
                distance_probes: vec![probe.id],
                ..Default::default()
            },
        ));
    }
    if ui.button("Remove distance probe").clicked() {
        output.edit(vec![WorldCommand::RemoveDistanceProbe(probe.id)]);
    }
}

fn distance_probe_object_pickers(
    ui: &mut egui::Ui,
    probe: &DistanceProbe,
    world: &WorldSnapshot,
    output: &mut UiFrameOutput,
) {
    match world.resolve_distance(probe) {
        Ok(distance) => {
            ui.label(format!("{distance:.4} m"));
        }
        Err(_) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 150, 60),
                "One object no longer exists",
            );
        }
    }

    let object_name = |id: fieldcad_core::ObjectId| {
        world
            .object(id)
            .map_or_else(|| id.to_string(), |object| object.name.clone())
    };

    for (label, current, other, is_a) in [
        ("Object A", probe.object_a, probe.object_b, true),
        ("Object B", probe.object_b, probe.object_a, false),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            egui::ComboBox::from_id_salt(("distance_probe_object", probe.id, is_a))
                .selected_text(object_name(current))
                .show_ui(ui, |ui| {
                    for object in distance_probe_candidates(world, current, other) {
                        if ui.selectable_label(false, &object.name).clicked() {
                            let (object_a, object_b) = if is_a {
                                (object.id, probe.object_b)
                            } else {
                                (probe.object_a, object.id)
                            };
                            output.edit(vec![WorldCommand::SetDistanceProbeObjects {
                                probe: probe.id,
                                object_a,
                                object_b,
                            }]);
                        }
                    }
                });
        });
    }

    ui.add_space(6.0);
    let mut show_line = probe.show_line;
    if ui
        .checkbox(&mut show_line, "Show line between objects")
        .changed()
    {
        output.edit(vec![WorldCommand::SetDistanceProbeShowLine {
            probe: probe.id,
            show_line,
        }]);
    }
}

/// The objects offered as candidates for one endpoint's picker: every object
/// except `current` (this endpoint's own value) and `other` (the far end of
/// the same measurement) — offering `other` here would let a user point both
/// endpoints at the same object.
fn distance_probe_candidates(
    world: &WorldSnapshot,
    current: ObjectId,
    other: ObjectId,
) -> impl Iterator<Item = &WorldObject> {
    world
        .objects()
        .values()
        .filter(move |object| object.id != current && object.id != other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picker_excludes_both_its_own_value_and_the_other_endpoints() {
        let mut world = fieldcad_core::World::new();
        world
            .commit([
                fieldcad_core::WorldCommand::CreateObject(fieldcad_core::ObjectSpec::new("a")),
                fieldcad_core::WorldCommand::CreateObject(fieldcad_core::ObjectSpec::new("b")),
                fieldcad_core::WorldCommand::CreateObject(fieldcad_core::ObjectSpec::new("c")),
            ])
            .unwrap();
        let snapshot = world.snapshot();

        let names: Vec<&str> =
            distance_probe_candidates(&snapshot, ObjectId::new(0), ObjectId::new(1))
                .map(|object| object.name.as_str())
                .collect();
        assert_eq!(names, vec!["c"]);
    }

    #[test]
    fn a_picker_offers_nothing_once_every_other_object_is_the_far_endpoint() {
        let mut world = fieldcad_core::World::new();
        world
            .commit([
                fieldcad_core::WorldCommand::CreateObject(fieldcad_core::ObjectSpec::new("a")),
                fieldcad_core::WorldCommand::CreateObject(fieldcad_core::ObjectSpec::new("b")),
            ])
            .unwrap();
        let snapshot = world.snapshot();

        assert_eq!(
            distance_probe_candidates(&snapshot, ObjectId::new(0), ObjectId::new(1)).count(),
            0
        );
    }
}
