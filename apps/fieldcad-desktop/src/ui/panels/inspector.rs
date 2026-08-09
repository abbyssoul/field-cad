//! Inspector panel (right side): dispatches to the right property editor
//! based on what is selected in the scene tree.

use crate::ui::{FrameContext, UiFrameOutput, UiModel};

pub fn inspector(
    root: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    egui::Panel::right("inspector_panel")
        .resizable(true)
        .default_size(280.0)
        .size_range(240.0..=460.0)
        .show(root, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if model.world_selected {
                        ui.heading("Simulation");
                        ui.separator();
                        super::world_inspector::world_properties(
                            ui,
                            model,
                            frame.compute,
                            frame.edit_in_progress,
                            output,
                        );
                    } else if let Some(object) =
                        model.selection.and_then(|id| frame.world.object(id))
                    {
                        ui.heading("Object");
                        ui.separator();
                        super::object_inspector::object_properties(
                            ui,
                            frame.world,
                            frame.compute,
                            object,
                            model.following,
                            output,
                        );
                    } else if let Some(plane) = model
                        .plane_selection
                        .and_then(|id| frame.world.planes().get(&id))
                    {
                        ui.heading("Slice plane");
                        ui.separator();
                        super::shape_inspector::plane_properties(
                            ui,
                            frame.world,
                            plane,
                            &mut model.field_layers,
                            frame.compute,
                            output,
                        );
                    } else if let Some(field_box) = model
                        .box_selection
                        .and_then(|id| frame.world.boxes().get(&id))
                    {
                        ui.heading("Field box");
                        ui.separator();
                        super::shape_inspector::box_properties(
                            ui,
                            frame.world,
                            field_box,
                            &mut model.field_layers,
                            frame.compute,
                            output,
                        );
                    } else if let Some(sphere) = model
                        .sphere_selection
                        .and_then(|id| frame.world.spheres().get(&id))
                    {
                        ui.heading("Field sphere");
                        ui.separator();
                        super::shape_inspector::sphere_properties(
                            ui,
                            frame.world,
                            sphere,
                            &mut model.field_layers,
                            frame.compute,
                            output,
                        );
                    } else if let Some(probe) =
                        model.probe_selection.and_then(|id| frame.world.probe(id))
                    {
                        ui.heading("Probe");
                        ui.separator();
                        super::probe_inspector::probe_properties(
                            ui,
                            model,
                            probe,
                            frame.world,
                            frame.compute,
                            frame.probe_history,
                            output,
                        );
                    } else if let Some(probe) = model
                        .distance_probe_selection
                        .and_then(|id| frame.world.distance_probe(id))
                    {
                        ui.heading("Distance probe");
                        ui.separator();
                        super::distance_probe_inspector::distance_probe_properties(
                            ui,
                            model,
                            probe,
                            frame.world,
                            frame.distance_history,
                            output,
                        );
                    } else {
                        ui.heading("Inspector");
                        ui.separator();
                        empty_inspector(ui, model);
                    }
                });
        });
}

fn empty_inspector(ui: &mut egui::Ui, model: &mut UiModel) {
    ui.weak("Nothing selected.");
    ui.add_space(6.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Select an object, probe, or slice plane — in the scene list or the 3D view — to \
                 edit it here.",
            )
            .small(),
        )
        .wrap(),
    );
    ui.add_space(10.0);
    if ui
        .button("Show simulation settings")
        .on_hover_text("Domain, field systems, sampling, and compute status")
        .clicked()
    {
        model.select_world();
    }
}
