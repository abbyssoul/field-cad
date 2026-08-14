//! Inspector sections for editing a mass-aggregate ("center of mass") probe:
//! name, membership mode, live totals, and history plots.

use std::collections::BTreeSet;

use fieldcad_core::{MassAggregateProbe, MassSelection, ObjectId, WorldCommand, WorldSnapshot};
use fieldcad_simulation::MassAggregateHistory;

use super::name_editor;
use crate::ui::compute::ComputeView;
use crate::ui::plot::mass_aggregate_history_plot;
use crate::ui::{CameraAction, UiFrameOutput, UiModel};

pub(super) fn mass_aggregate_probe_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    probe: &MassAggregateProbe,
    world: &WorldSnapshot,
    compute: &ComputeView,
    history: &MassAggregateHistory,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("mass_aggregate_probe_name", probe.id), &probe.name) {
        output.edit(vec![WorldCommand::SetMassAggregateProbeName {
            probe: probe.id,
            name,
        }]);
    }
    super::section(
        ui,
        "inspector_mass_aggregate_membership",
        "Membership",
        true,
        |ui| {
            membership_editor(ui, probe, world, output);
            ui.add_space(6.0);
            let mut show_member_lines = probe.show_member_lines;
            if ui
                .checkbox(
                    &mut show_member_lines,
                    "Show lines to members when selected",
                )
                .changed()
            {
                output.edit(vec![WorldCommand::SetMassAggregateProbeShowMemberLines {
                    probe: probe.id,
                    show_member_lines,
                }]);
            }
        },
    );
    super::section(
        ui,
        "inspector_mass_aggregate_values",
        "Live values",
        true,
        |ui| live_values_panel(ui, probe, compute),
    );
    super::section(
        ui,
        "inspector_mass_aggregate_history",
        "History",
        false,
        |ui| {
            let series = model
                .mass_aggregate_probe_series
                .entry(probe.id)
                .or_default();
            mass_aggregate_history_plot(ui, probe.id, history, series);
        },
    );

    ui.add_space(10.0);
    if ui.button("Open floating plot").clicked() {
        model.open_mass_aggregate_probe_plot(probe.id);
    }
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove center of mass").clicked() {
        output.edit(vec![WorldCommand::RemoveMassAggregateProbe(probe.id)]);
    }
}

fn membership_editor(
    ui: &mut egui::Ui,
    probe: &MassAggregateProbe,
    world: &WorldSnapshot,
    output: &mut UiFrameOutput,
) {
    let universe_mode = matches!(probe.selection, MassSelection::Universe { .. });
    ui.horizontal(|ui| {
        if ui.selectable_label(universe_mode, "Universe").clicked() && !universe_mode {
            output.edit(vec![WorldCommand::SetMassAggregateProbeSelection {
                probe: probe.id,
                selection: MassSelection::Universe {
                    excluded: BTreeSet::new(),
                },
            }]);
        }
        if ui.selectable_label(!universe_mode, "Selection").clicked() && universe_mode {
            output.edit(vec![WorldCommand::SetMassAggregateProbeSelection {
                probe: probe.id,
                selection: MassSelection::Selection {
                    included: BTreeSet::new(),
                },
            }]);
        }
    });

    match &probe.selection {
        MassSelection::Universe { excluded } => {
            ui.weak("Every mass-bearing object, except any checked below:");
            object_checklist(
                ui,
                probe,
                world,
                excluded,
                |excluded| MassSelection::Universe { excluded },
                output,
            );
        }
        MassSelection::Selection { included } => {
            ui.weak("Only the objects checked below:");
            object_checklist(
                ui,
                probe,
                world,
                included,
                |included| MassSelection::Selection { included },
                output,
            );
        }
    }
}

/// One checkbox per authored object (derived anchors are never offered —
/// they carry no mass and aren't a meaningful member of anyone's
/// aggregate). Checking a box always means "add this id to the set"; the
/// caller's `build_selection` decides whether that set means excluded or
/// included.
fn object_checklist(
    ui: &mut egui::Ui,
    probe: &MassAggregateProbe,
    world: &WorldSnapshot,
    members: &BTreeSet<ObjectId>,
    build_selection: impl Fn(BTreeSet<ObjectId>) -> MassSelection,
    output: &mut UiFrameOutput,
) {
    let candidates: Vec<_> = world
        .objects()
        .values()
        .filter(|object| !object.derived)
        .collect();
    if candidates.is_empty() {
        ui.weak("No objects in the scene yet.");
        return;
    }
    let mut updated = members.clone();
    let mut changed = false;
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .show(ui, |ui| {
            for object in candidates {
                let mut checked = updated.contains(&object.id);
                if ui.checkbox(&mut checked, &object.name).changed() {
                    changed = true;
                    if checked {
                        updated.insert(object.id);
                    } else {
                        updated.remove(&object.id);
                    }
                }
            }
        });
    if changed {
        output.edit(vec![WorldCommand::SetMassAggregateProbeSelection {
            probe: probe.id,
            selection: build_selection(updated),
        }]);
    }
}

fn live_values_panel(ui: &mut egui::Ui, probe: &MassAggregateProbe, compute: &ComputeView) {
    let Some(sample) = compute.mass_aggregates.get(&probe.id) else {
        ui.weak("No mass-bearing member yet.");
        return;
    };
    ui.weak("ⓘ").on_hover_text(
        "For an isolated system, these totals should hold steady over time — Noether's theorem \
         ties conservation of momentum and angular momentum to space's translation and rotation \
         symmetry, and conservation of energy to time symmetry. Plot them below (History) to \
         watch whether they actually do.",
    );
    egui::Grid::new(("mass_aggregate_probe_values", probe.id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Position");
            ui.label(super::object_inspector::format_vector(
                sample.center_of_mass,
                "m",
            ));
            ui.end_row();

            ui.label("Velocity")
                .on_hover_text("Σ mᵢvᵢ / Σ mᵢ — the mass-weighted centroid's own velocity.");
            ui.label(super::object_inspector::format_vector(
                sample.velocity,
                "m/s",
            ));
            ui.end_row();

            ui.label("Total momentum")
                .on_hover_text("Σ γmv over every member.");
            ui.label(super::object_inspector::format_vector(
                sample.total_momentum,
                "kg·m/s",
            ));
            ui.end_row();

            ui.label("Angular momentum")
                .on_hover_text("Σ (r−R_cm)×γmv about the centroid.");
            ui.label(super::object_inspector::format_vector(
                sample.angular_momentum,
                "kg·m²/s",
            ));
            ui.end_row();

            ui.label("Total kinetic energy")
                .on_hover_text("Σ (γ−1)mc² over every member.");
            ui.label(format!(
                "{} J",
                crate::ui::compute::format_engineering(sample.total_kinetic_energy_j)
            ));
            ui.end_row();

            ui.label("Total mass");
            ui.label(format!(
                "{} kg",
                crate::ui::compute::format_engineering(sample.total_mass_kg)
            ));
            ui.end_row();

            ui.label("Members");
            ui.label(sample.member_count.to_string());
            ui.end_row();
        });
}
