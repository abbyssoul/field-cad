//! Inspector sections for editing a probe: position, recorded channels,
//! attachment, and history plots.

use fieldcad_core::{ObjectId, ProbeId, ProbePosition, WorldCommand, WorldSnapshot};
use glam::DVec3;

use super::{coordinate_editor, name_editor};
use crate::ui::compute::ComputeView;
use crate::ui::plot::probe_history_plots;
use crate::ui::{CameraAction, UiFrameOutput, UiModel};
use fieldcad_simulation::ProbeHistory;

pub(super) fn probe_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    probe: &fieldcad_core::Probe,
    world: &WorldSnapshot,
    compute: &ComputeView,
    history: &ProbeHistory,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("probe_name", probe.id), &probe.name) {
        output.edit(vec![WorldCommand::SetProbeName {
            probe: probe.id,
            name,
        }]);
    }
    super::section(ui, "inspector_probe_position", "Position", true, |ui| {
        probe_position_editors(ui, probe, world, output);
    });
    super::section(
        ui,
        "inspector_probe_channels",
        "Recorded channels",
        false,
        |ui| probe_channel_picker(ui, model, probe, compute, output),
    );
    super::section(ui, "inspector_probe_history", "History", false, |ui| {
        probe_history_plots(ui, probe, compute, history);
    });

    ui.add_space(10.0);
    if ui.button("Open floating plot").clicked() {
        model.open_probe_plot(probe);
    }
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove probe").clicked() {
        output.edit(vec![WorldCommand::RemoveProbe(probe.id)]);
    }
}

fn probe_position_editors(
    ui: &mut egui::Ui,
    probe: &fieldcad_core::Probe,
    world: &WorldSnapshot,
    output: &mut UiFrameOutput,
) {
    match probe.position {
        ProbePosition::World(mut position) => {
            let mut changed = false;
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut position.x, " m", editing);
                changed |= coordinate_editor(ui, "y", &mut position.y, " m", editing);
                changed |= coordinate_editor(ui, "z", &mut position.z, " m", editing);
            });
            if changed {
                output.edit(vec![WorldCommand::SetProbePosition {
                    probe: probe.id,
                    position: ProbePosition::World(position),
                }]);
            }
            probe_attachment_picker(ui, probe.id, position, None, world, output);
        }
        ProbePosition::Attached { object, mut offset } => {
            let object_name = world
                .object(object)
                .map_or_else(|| object.to_string(), |object| object.name.clone());
            ui.label(format!("Attached to {object_name}"));
            let mut changed = false;
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut offset.x, " m", editing);
                changed |= coordinate_editor(ui, "y", &mut offset.y, " m", editing);
                changed |= coordinate_editor(ui, "z", &mut offset.z, " m", editing);
            });
            if changed {
                output.edit(vec![WorldCommand::SetProbePosition {
                    probe: probe.id,
                    position: ProbePosition::Attached { object, offset },
                }]);
            }

            if ui.button("Detach at current position").clicked()
                && let Ok(position) = world.resolve_probe_position(probe)
            {
                output.edit(vec![WorldCommand::SetProbePosition {
                    probe: probe.id,
                    position: ProbePosition::World(position),
                }]);
            }
            if let Ok(position) = world.resolve_probe_position(probe) {
                probe_attachment_picker(ui, probe.id, position, Some(object), world, output);
            }
        }
    }
}

fn probe_channel_picker(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    probe: &fieldcad_core::Probe,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    let mut channels = probe.channels.clone();
    let mut changed = false;
    for (channel, name) in &compute.channel_names {
        let mut records = channels.contains(channel);
        if ui.checkbox(&mut records, name).changed() {
            changed = true;
            if records {
                channels.push(channel.clone());
                if let Some(plot) = model.probe_plots.get_mut(&probe.id) {
                    plot.channels.insert(channel.clone());
                }
            } else {
                channels.retain(|recorded| recorded != channel);
            }
        }
    }
    if changed {
        output.edit(vec![WorldCommand::SetProbeChannels {
            probe: probe.id,
            channels,
        }]);
    }
}

fn probe_attachment_picker(
    ui: &mut egui::Ui,
    probe: ProbeId,
    world_position: DVec3,
    attached_to: Option<ObjectId>,
    world: &WorldSnapshot,
    output: &mut UiFrameOutput,
) {
    if world.objects().is_empty() {
        return;
    }

    ui.horizontal(|ui| {
        ui.label(if attached_to.is_some() {
            "Reattach to"
        } else {
            "Attach to"
        });
        egui::ComboBox::from_id_salt(("probe_attachment", probe))
            .selected_text("Choose object…")
            .show_ui(ui, |ui| {
                for object in world.objects().values() {
                    if Some(object.id) == attached_to {
                        continue;
                    }
                    if ui.selectable_label(false, &object.name).clicked() {
                        let offset = attachment_offset(world_position, object);
                        output.edit(vec![WorldCommand::SetProbePosition {
                            probe,
                            position: ProbePosition::Attached {
                                object: object.id,
                                offset,
                            },
                        }]);
                    }
                }
            });
    });
}

pub(super) fn attachment_offset(
    world_position: DVec3,
    object: &fieldcad_core::WorldObject,
) -> DVec3 {
    object.transform.rotation.inverse() * (world_position - object.transform.translation)
}

// note_held_edit, coordinate_editor, name_editor provided by super::
