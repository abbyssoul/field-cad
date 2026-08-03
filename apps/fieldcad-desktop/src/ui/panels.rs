//! The individual panels, property editors, and authoring commands.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, ObjectId, ObjectShape, ObjectSpec, ProbeId, ProbePosition, ProbeSpec,
    SimulationMode, SlicePlane, SlicePlaneSpec, SnapshotFreshness, TimeStep, Transform,
    WorldCommand, WorldObject, WorldSnapshot,
};
use fieldcad_electrostatics::{
    charge_component_id, charge_properties, charge_property_id, electric_field_channel_id,
    electric_potential_channel_id,
};
use fieldcad_simulation::{CommandPayload, PlaybackSpeed, ProbeHistory};
use glam::{DVec2, DVec3};

use super::compute::{
    ComputeView, WorkbenchState, format_simulation_time, format_time_step, parse_playback_speed,
    time_step_drag_speed, validity_note,
};
use super::plot::probe_history_plots;
use super::{CameraAction, ChannelLayerSettings, FrameContext, UiFrameOutput, UiModel};
use crate::{
    camera::AxisView,
    scene::{PlaneVectorMode, SceneSelection},
};

pub(super) fn menu_bar(
    root: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let paused = frame.compute.mode == SimulationMode::Paused;
    let live = frame.compute.accepts_commands();

    egui::Panel::top("menu_bar").show(root, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.strong("Field CAD");
            ui.separator();
            ui.checkbox(&mut model.grid_visible, "Grid");
            ui.checkbox(&mut model.axes_visible, "XYZ axes");
            ui.checkbox(&mut model.diagnostics_visible, "Diagnostics");
            ui.separator();

            if ui
                .add_enabled(live && paused, egui::Button::new("▶ Play"))
                .clicked()
            {
                output.submit(CommandPayload::Play);
            }
            if ui
                .add_enabled(live && !paused, egui::Button::new("⏸ Pause"))
                .clicked()
            {
                output.submit(CommandPayload::Pause);
            }
            if ui
                .add_enabled(live && paused, egui::Button::new("Step"))
                .clicked()
            {
                output.submit(CommandPayload::Step);
            }

            ui.separator();
            ui.label("dt");
            let mut seconds = frame.compute.time_step_seconds;
            let drag_speed = time_step_drag_speed(seconds);
            let response = ui
                .add_enabled(
                    live,
                    egui::DragValue::new(&mut seconds)
                        .speed(drag_speed)
                        .range(f64::from_bits(1)..=f64::MAX)
                        .custom_formatter(|seconds, _| format_time_step(seconds))
                        .custom_parser(|text| {
                            text.parse::<TimeStep>().ok().map(TimeStep::seconds)
                        })
                        .update_while_editing(false),
                )
                .on_hover_text(
                    "Drag to adjust, or click to enter text. Unitless values are seconds; examples: 4.43e-3, 1.23ns, 7.3213e-4ms",
                );
            if response.changed()
                && let Ok(time_step) = TimeStep::from_seconds(seconds)
                && time_step.seconds() != frame.compute.time_step_seconds
            {
                output.submit(CommandPayload::SetTimeStep(time_step));
            }

            ui.separator();
            ui.label("speed");
            let mut speed = frame.compute.playback_speed;
            let drag_speed = (speed * 0.01).max(f64::from_bits(1));
            let response = ui
                .add_enabled(
                    live,
                    egui::DragValue::new(&mut speed)
                        .speed(drag_speed)
                        .range(f64::from_bits(1)..=f64::MAX)
                        .custom_formatter(|speed, _| format!("{speed:.4}×"))
                        .custom_parser(parse_playback_speed)
                        .update_while_editing(false),
                )
                .on_hover_text("Wall-clock playback rate. This never changes numerical dt.");
            if response.changed()
                && let Ok(speed) = PlaybackSpeed::from_multiplier(speed)
                && speed.multiplier() != frame.compute.playback_speed
            {
                output.submit(CommandPayload::SetPlaybackSpeed(speed));
            }

            ui.separator();
            state_badge(ui, frame.compute.workbench_state());
            if frame.compute.freshness == Some(SnapshotFreshness::Stale) {
                ui.colored_label(egui::Color32::from_rgb(235, 170, 70), "stale view");
            }
            if frame.compute.pending_commands > 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(235, 190, 75),
                    format!("{} queued", frame.compute.pending_commands),
                );
            }
            if let Some(error) = &model.command_error {
                ui.colored_label(egui::Color32::from_rgb(240, 105, 95), "command rejected")
                    .on_hover_text(error);
            }
            ui.separator();
            ui.monospace(format!("t = {}", format_simulation_time(frame.compute.time_seconds)));
        });
    });
}

fn state_badge(ui: &mut egui::Ui, state: WorkbenchState) {
    ui.colored_label(state.color(), format!("● {}", state.label()));
}

pub(super) fn scene_tree(
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
            ui.heading("Scene");
            ui.separator();

            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("+Q")
                    .on_hover_text("Add +1 nC point charge")
                    .clicked()
                {
                    output.submit(new_charge_command(
                        frame.world,
                        1.0e-9,
                        ChargeObjectKind::Point,
                    ));
                }
                if ui
                    .button("−Q")
                    .on_hover_text("Add −1 nC point charge")
                    .clicked()
                {
                    output.submit(new_charge_command(
                        frame.world,
                        -1.0e-9,
                        ChargeObjectKind::Point,
                    ));
                }
                if ui
                    .button("+ Sphere")
                    .on_hover_text("Add a uniformly charged +1 nC sphere")
                    .clicked()
                {
                    output.submit(new_charge_command(
                        frame.world,
                        1.0e-9,
                        ChargeObjectKind::Sphere,
                    ));
                }
                if ui.button("+ Probe").clicked() {
                    output.edit(vec![WorldCommand::CreateProbe(ProbeSpec::at(
                        format!("Probe {}", frame.world.probes().len() + 1),
                        DVec3::new(1.0, 0.0, 0.6),
                        vec![electric_field_channel_id(), electric_potential_channel_id()],
                    ))]);
                }
                if ui.button("+ Plane").clicked() {
                    output.edit(vec![WorldCommand::CreatePlane(
                        SlicePlaneSpec::new(
                            format!("XY plane {}", frame.world.planes().len() + 1),
                            DVec3::ZERO,
                            DVec3::Z,
                        )
                        .and_then(|plane| plane.with_half_extent(DVec2::splat(4.0)))
                        .expect("static plane parameters are valid"),
                    )]);
                }
            });
            ui.add_space(6.0);

            if frame.world.objects().is_empty() {
                ui.weak("No objects.");
            }
            for object in frame.world.objects().values() {
                ui.horizontal(|ui| {
                    if visibility_button(ui, object.visible).clicked() {
                        output.edit(vec![WorldCommand::SetObjectVisible {
                            object: object.id,
                            visible: !object.visible,
                        }]);
                    }
                    if ui
                        .selectable_label(
                            model.selection == Some(object.id),
                            format!("▣  {}", object.name),
                        )
                        .clicked()
                    {
                        model.set_scene_selection(Some(SceneSelection::Object(object.id)));
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Delete object")
                        .clicked()
                    {
                        output.edit(vec![WorldCommand::RemoveObject(object.id)]);
                    }
                });
            }

            if !frame.world.probes().is_empty() {
                ui.add_space(8.0);
                ui.label("Probes");
                for probe in frame.world.probes().values() {
                    ui.horizontal(|ui| {
                        if visibility_button(ui, probe.visible).clicked() {
                            output.edit(vec![WorldCommand::SetProbeVisible {
                                probe: probe.id,
                                visible: !probe.visible,
                            }]);
                        }
                        if ui
                            .selectable_label(
                                model.probe_selection == Some(probe.id),
                                format!("◉  {}", probe.name),
                            )
                            .clicked()
                        {
                            model.set_scene_selection(Some(SceneSelection::Probe(probe.id)));
                        }
                        if ui.small_button("×").on_hover_text("Delete probe").clicked() {
                            output.edit(vec![WorldCommand::RemoveProbe(probe.id)]);
                        }
                    });
                }
            }

            if !frame.world.planes().is_empty() {
                ui.add_space(8.0);
                ui.label("Slice planes");
                for plane in frame.world.planes().values() {
                    ui.horizontal(|ui| {
                        if visibility_button(ui, plane.visible).clicked() {
                            output.edit(vec![WorldCommand::SetPlaneVisible {
                                plane: plane.id,
                                visible: !plane.visible,
                            }]);
                        }
                        if ui
                            .selectable_label(
                                model.plane_selection == Some(plane.id),
                                format!("▦  {}", plane.name),
                            )
                            .clicked()
                        {
                            model.set_scene_selection(Some(SceneSelection::Plane(plane.id)));
                        }
                        if ui.small_button("×").on_hover_text("Delete plane").clicked() {
                            output.edit(vec![WorldCommand::RemovePlane(plane.id)]);
                        }
                    });
                }
            }
        });
}

fn visibility_button(ui: &mut egui::Ui, visible: bool) -> egui::Response {
    ui.small_button(if visible { "◉" } else { "○" })
        .on_hover_text(if visible {
            "Hide in viewport"
        } else {
            "Show in viewport"
        })
}

pub(super) fn inspector(
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
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Inspector");
                ui.separator();

                if let Some(object) = model.selection.and_then(|id| frame.world.object(id)) {
                    object_properties(ui, object, output);
                } else if let Some(plane) = model
                    .plane_selection
                    .and_then(|id| frame.world.planes().get(&id))
                {
                    plane_properties(ui, plane, &mut model.field_layers, frame.compute, output);
                } else if let Some(probe) =
                    model.probe_selection.and_then(|id| frame.world.probe(id))
                {
                    probe_properties(
                        ui,
                        model,
                        probe,
                        frame.world,
                        frame.compute,
                        frame.probe_history,
                        output,
                    );
                } else {
                    ui.weak("Select an object, probe, or plane in the viewport or scene tree.");
                }

                ui.add_space(16.0);
                ui.heading("View");
                ui.horizontal(|ui| {
                    for (label, view) in [
                        ("+X", AxisView::PositiveX),
                        ("+Y", AxisView::PositiveY),
                        ("+Z", AxisView::PositiveZ),
                    ] {
                        if ui.button(label).clicked() {
                            output.camera_action = Some(CameraAction::Axis(view));
                        }
                    }
                });
                field_layer_controls(ui, model, frame.compute);

                ui.add_space(16.0);
                transport_sampling(ui, frame.compute, output);

                ui.add_space(16.0);
                compute_panel(ui, frame.compute);
            });
        });
}

fn object_properties(ui: &mut egui::Ui, object: &WorldObject, output: &mut UiFrameOutput) {
    ui.label(&object.name);
    let mut position = object.transform.translation;
    let mut position_changed = false;
    egui::Grid::new("object_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Position");
            ui.horizontal(|ui| {
                position_changed |= coordinate_editor(ui, "x", &mut position.x, " m");
                position_changed |= coordinate_editor(ui, "y", &mut position.y, " m");
                position_changed |= coordinate_editor(ui, "z", &mut position.z, " m");
            });
            ui.end_row();

            ui.label("Shape");
            match object.shape {
                Some(ObjectShape::Point { mut radius }) => {
                    ui.horizontal(|ui| {
                        ui.label("Point");
                        if radius_editor(ui, &mut radius, 0.0)
                            && let Ok(shape) = ObjectShape::point(radius)
                        {
                            output.edit(vec![WorldCommand::SetShape {
                                object: object.id,
                                shape: Some(shape),
                            }]);
                        }
                    });
                }
                Some(ObjectShape::Sphere { mut radius }) => {
                    ui.horizontal(|ui| {
                        ui.label("Uniform sphere");
                        if radius_editor(ui, &mut radius, 1.0e-4)
                            && let Ok(shape) = ObjectShape::sphere(radius)
                        {
                            output.edit(vec![WorldCommand::SetShape {
                                object: object.id,
                                shape: Some(shape),
                            }]);
                        }
                    });
                }
                Some(ObjectShape::Box { half_extent }) => {
                    ui.label(format!(
                        "Box, {:.2} × {:.2} × {:.2} m",
                        half_extent.x * 2.0,
                        half_extent.y * 2.0,
                        half_extent.z * 2.0
                    ));
                }
                None => {
                    ui.label("None");
                }
            }
            ui.end_row();

            let velocity = object.velocity.linear;
            if velocity.length_squared() > 0.0 {
                ui.label("Velocity");
                ui.monospace(format!(
                    "{:.3}, {:.3}, {:.3} m s^-1",
                    velocity.x, velocity.y, velocity.z
                ));
                ui.end_row();
            }
        });

    if position_changed && let Ok(transform) = Transform::new(position, object.transform.rotation) {
        output.edit(vec![WorldCommand::SetTransform {
            object: object.id,
            transform,
        }]);
    }

    if let Some(properties) = object.components.get(&charge_component_id()) {
        ui.add_space(8.0);
        ui.label("Electrostatics");
        if let Some(charge) = properties.scalar(&charge_property_id()) {
            let mut nanocoulombs = charge * 1.0e9;
            ui.horizontal(|ui| {
                ui.label("Charge");
                if ui
                    .add(
                        egui::DragValue::new(&mut nanocoulombs)
                            .speed(0.05)
                            .suffix(" nC"),
                    )
                    .changed()
                    && let Ok(properties) = charge_properties(nanocoulombs * 1.0e-9)
                {
                    output.edit(vec![WorldCommand::AttachComponent {
                        object: object.id,
                        component: charge_component_id(),
                        properties,
                    }]);
                }
            });
        }
    }

    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove object").clicked() {
        output.edit(vec![WorldCommand::RemoveObject(object.id)]);
    }
}

fn plane_properties(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    ui.label(&plane.name);
    ui.small("Drag the dashed purple N arrow to reorient the plane; RGB arrows and squares move its origin.");
    let mut origin = plane.origin;
    let mut normal = plane.normal;
    let mut half_extent = plane.half_extent;
    let mut changed = false;

    egui::Grid::new("plane_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                changed |= coordinate_editor(ui, "x", &mut origin.x, " m");
                changed |= coordinate_editor(ui, "y", &mut origin.y, " m");
                changed |= coordinate_editor(ui, "z", &mut origin.z, " m");
            });
            ui.end_row();

            ui.label("Normal");
            ui.horizontal(|ui| {
                changed |= coordinate_editor(ui, "nx", &mut normal.x, "");
                changed |= coordinate_editor(ui, "ny", &mut normal.y, "");
                changed |= coordinate_editor(ui, "nz", &mut normal.z, "");
            });
            ui.end_row();

            ui.label("Half extent");
            ui.horizontal(|ui| {
                changed |= coordinate_editor(ui, "u", &mut half_extent.x, " m");
                changed |= coordinate_editor(ui, "v", &mut half_extent.y, " m");
            });
            ui.end_row();
        });

    if changed && let Ok(spec) = plane_spec(plane, origin, normal, half_extent) {
        output.edit(vec![WorldCommand::SetPlane {
            plane: plane.id,
            spec,
        }]);
    }

    ui.add_space(8.0);
    ui.label("Field display");
    for channel in &compute.vector_channels {
        let name = channel_label(channel, &compute.channel_names);
        let layer = field_layers.entry(channel.clone()).or_default();
        ui.collapsing(name, |ui| {
            ui.checkbox(&mut layer.visible, "Show layer");
            let settings = layer.planes.entry(plane.id).or_default();
            ui.checkbox(&mut settings.magnitude_visible, "Magnitude colour");
            density_editor(
                ui,
                "Magnitude density",
                &mut settings.magnitude_density,
                layer.visible && settings.magnitude_visible,
            );
            ui.checkbox(&mut settings.vectors_visible, "Vector arrows");
            density_editor(
                ui,
                "Arrow density",
                &mut settings.vector_density,
                layer.visible && settings.vectors_visible,
            );
            ui.horizontal(|ui| {
                ui.label("Vector component");
                ui.selectable_value(
                    &mut settings.vector_mode,
                    PlaneVectorMode::InPlane,
                    "In plane",
                )
                .on_hover_text("Project vectors into this plane (default)");
                ui.selectable_value(
                    &mut settings.vector_mode,
                    PlaneVectorMode::Full3d,
                    "Full 3D",
                )
                .on_hover_text("Show the component normal to the plane too");
            });
        });
    }

    ui.horizontal(|ui| {
        for (label, normal, u_axis) in [
            ("XY", DVec3::Z, DVec3::X),
            ("XZ", DVec3::Y, DVec3::X),
            ("YZ", DVec3::X, DVec3::Y),
        ] {
            if ui.button(label).clicked()
                && let Ok(spec) = SlicePlaneSpec::new(&plane.name, plane.origin, normal)
                    .and_then(|spec| spec.with_u_axis(u_axis))
                    .and_then(|spec| spec.with_half_extent(plane.half_extent))
            {
                output.edit(vec![WorldCommand::SetPlane {
                    plane: plane.id,
                    spec: spec.with_visibility(plane.visible),
                }]);
            }
        }
    });
    if ui.button("Duplicate plane").clicked() {
        output.edit(vec![WorldCommand::CreatePlane(
            SlicePlaneSpec::from_plane(plane).with_name(format!("{} copy", plane.name)),
        )]);
    }
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove plane").clicked() {
        output.edit(vec![WorldCommand::RemovePlane(plane.id)]);
    }
}

fn probe_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    probe: &fieldcad_core::Probe,
    world: &WorldSnapshot,
    compute: &ComputeView,
    history: &ProbeHistory,
    output: &mut UiFrameOutput,
) {
    ui.label(&probe.name);
    match probe.position {
        ProbePosition::World(mut position) => {
            let mut changed = false;
            ui.horizontal(|ui| {
                changed |= coordinate_editor(ui, "x", &mut position.x, " m");
                changed |= coordinate_editor(ui, "y", &mut position.y, " m");
                changed |= coordinate_editor(ui, "z", &mut position.z, " m");
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
                changed |= coordinate_editor(ui, "x", &mut offset.x, " m");
                changed |= coordinate_editor(ui, "y", &mut offset.y, " m");
                changed |= coordinate_editor(ui, "z", &mut offset.z, " m");
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

    ui.add_space(10.0);
    ui.collapsing("Recorded channels", |ui| {
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
    });
    if ui.button("Open floating plot").clicked() {
        model.open_probe_plot(probe);
    }
    probe_history_plots(ui, probe, compute, history);

    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove probe").clicked() {
        output.edit(vec![WorldCommand::RemoveProbe(probe.id)]);
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

fn attachment_offset(world_position: DVec3, object: &WorldObject) -> DVec3 {
    object.transform.rotation.inverse() * (world_position - object.transform.translation)
}

fn coordinate_editor(ui: &mut egui::Ui, label: &str, value: &mut f64, suffix: &str) -> bool {
    ui.add(
        egui::DragValue::new(value)
            .speed(0.02)
            .prefix(format!("{label}: "))
            .suffix(suffix),
    )
    .changed()
}

fn radius_editor(ui: &mut egui::Ui, radius: &mut f64, minimum: f64) -> bool {
    ui.add(
        egui::DragValue::new(radius)
            .speed(0.01)
            .range(minimum..=f64::INFINITY)
            .prefix("r: ")
            .suffix(" m"),
    )
    .changed()
}

fn density_editor(ui: &mut egui::Ui, label: &str, density: &mut u32, enabled: bool) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(density).speed(1.0).range(0..=u32::MAX),
        )
        .on_hover_text("Enter any non-negative whole number");
    });
}

fn plane_spec(
    plane: &SlicePlane,
    origin: DVec3,
    normal: DVec3,
    half_extent: DVec2,
) -> Result<SlicePlaneSpec, fieldcad_core::WorldError> {
    let spec = if normal == plane.normal {
        SlicePlaneSpec::from_plane(plane).with_origin(origin)?
    } else {
        SlicePlaneSpec::new(&plane.name, origin, normal)?.with_visibility(plane.visible)
    };
    spec.with_half_extent(half_extent)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChargeObjectKind {
    Point,
    Sphere,
}

fn new_charge_command(
    world: &WorldSnapshot,
    charge_coulombs: f64,
    kind: ChargeObjectKind,
) -> CommandPayload {
    let index = world.objects().len() + 1;
    let x = (index.saturating_sub(1) as f64) * 0.6;
    let (kind_name, shape) = match kind {
        ChargeObjectKind::Point => (
            "point charge",
            ObjectShape::point(0.15).expect("static radius is valid"),
        ),
        ChargeObjectKind::Sphere => (
            "charged sphere",
            ObjectShape::sphere(0.35).expect("static radius is valid"),
        ),
    };
    CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(
        ObjectSpec::new(format!(
            "{} {kind_name} {index}",
            if charge_coulombs >= 0.0 {
                "Positive"
            } else {
                "Negative"
            }
        ))
        .with_transform(Transform::at(DVec3::new(x, 0.0, 0.6)).expect("static position is finite"))
        .with_shape(shape)
        .with_component(
            charge_component_id(),
            charge_properties(charge_coulombs).expect("static charge is finite"),
        ),
    )])
}

/// Choose which published vector channel the generic layers draw.
///
/// Hidden while only one channel exists, so the electrostatic slice gains no
/// ceremony; present as soon as a plugin publishes a second — `B` alongside `E`,
/// or gravitational acceleration alongside either.
fn field_layer_controls(ui: &mut egui::Ui, model: &mut UiModel, compute: &ComputeView) {
    ui.label("Vector layers");
    for channel in &compute.vector_channels {
        let layer = model.field_layers.entry(channel.clone()).or_default();
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut layer.visible,
                channel_label(channel, &compute.channel_names),
            );
            ui.add_enabled_ui(layer.visible, |ui| {
                ui.checkbox(&mut layer.whole_domain.domain_vectors, "3D glyphs");
            });
        });
    }
}

fn channel_label(id: &ChannelId, names: &BTreeMap<ChannelId, String>) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

/// How densely the *source* is asked to sample, as opposed to how densely the
/// visualizer draws what it receives.
///
/// These are separate on purpose. Presentation density interpolates the
/// published lattice and claims no extra accuracy; raising transport density
/// asks for samples that were actually evaluated. Neither changes the domain or
/// the physical result — only how much of it is observed.
fn transport_sampling(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    ui.heading("Transport sampling");
    let mut subscription = compute.subscription;
    let mut changed = false;

    ui.horizontal(|ui| {
        let mut planes = subscription.planes.map_or(0, |counts| counts.x);
        ui.label("Plane samples");
        changed |= ui
            .add_enabled(
                compute.accepts_commands(),
                egui::DragValue::new(&mut planes)
                    .speed(1.0)
                    .range(0..=1_024),
            )
            .on_hover_text("Samples per axis the source evaluates on each visible plane")
            .changed();
        if changed {
            subscription.planes = (planes > 0).then(|| glam::UVec2::splat(planes));
        }
    });

    ui.horizontal(|ui| {
        let mut stride = subscription.domain_stride.unwrap_or(0);
        ui.label("Domain stride");
        let response = ui
            .add_enabled(
                compute.accepts_commands(),
                egui::DragValue::new(&mut stride).speed(1.0).range(0..=256),
            )
            .on_hover_text("Whole-domain lattice decimation; 0 publishes no 3D grid");
        if response.changed() {
            subscription.domain_stride = (stride > 0).then_some(stride);
            changed = true;
        }
    });

    if changed && subscription != compute.subscription {
        output.submit(CommandPayload::SetSubscription(subscription));
    }
}

fn compute_panel(ui: &mut egui::Ui, compute: &ComputeView) {
    ui.heading("Compute");
    egui::Grid::new("compute_status")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Source");
            ui.label(&compute.description);
            ui.end_row();

            ui.label("State");
            ui.label(compute.status.label());
            ui.end_row();

            ui.label("Mode");
            ui.colored_label(
                compute.workbench_state().color(),
                compute.workbench_state().label(),
            );
            ui.end_row();

            ui.label("Playback");
            ui.monospace(format!("{}×", compute.playback_speed));
            ui.end_row();

            ui.label("Queued edits");
            ui.monospace(compute.pending_commands.to_string());
            ui.end_row();

            ui.label("Tick / time");
            ui.monospace(format!("{} / {:.4} s", compute.tick, compute.time_seconds));
            ui.end_row();

            ui.label("World revision");
            ui.monospace(compute.world_revision.to_string());
            ui.end_row();

            ui.label("Snapshot");
            ui.monospace(
                compute
                    .snapshot_sequence
                    .map_or_else(|| "None".to_owned(), |sequence| format!("#{sequence}")),
            );
            ui.end_row();

            ui.label("Freshness");
            ui.label(
                compute
                    .freshness
                    .map_or("No data", SnapshotFreshness::label),
            );
            ui.end_row();

            ui.label("Domain");
            ui.monospace(&compute.domain_summary);
            ui.end_row();

            ui.label("Samples");
            ui.monospace(compute.total_samples.to_string());
            ui.end_row();
        });

    if !compute.probe_readings.is_empty() {
        ui.collapsing("Probe samples", |ui| {
            for reading in &compute.probe_readings {
                ui.label(format!("{} · {}", reading.probe_name, reading.channel_name));
                match validity_note(reading.validity) {
                    Some(note) => {
                        ui.colored_label(egui::Color32::from_rgb(230, 150, 60), note);
                    }
                    None => {
                        ui.monospace(&reading.value);
                    }
                }
            }
        });
    }
}

pub(super) fn diagnostics_window(
    context: &egui::Context,
    frame: &FrameContext<'_>,
    command_error: Option<&str>,
) {
    egui::Window::new("Diagnostics")
        .default_pos(egui::pos2(218.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .show(context, |ui| {
            egui::Grid::new("render_diagnostics")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Frame");
                    ui.monospace(format!("{:.2} ms", frame.frame_time_ms));
                    ui.end_row();
                    ui.label("GPU");
                    ui.monospace(frame.adapter_name);
                    ui.end_row();
                    ui.label("Compute");
                    ui.monospace(&frame.compute.description);
                    ui.end_row();
                    ui.label("Objects");
                    ui.monospace(frame.world.objects().len().to_string());
                    ui.end_row();
                });

            if !frame.compute.diagnostics.is_empty() {
                ui.separator();
                for line in &frame.compute.diagnostics {
                    ui.small(line);
                }
            }
            if let Some(error) = command_error {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(240, 105, 95),
                    format!("Last command rejected: {error}"),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{ObjectSpec, Transform, World, WorldCommand};

    use glam::{DQuat, DVec3};

    use super::*;

    #[test]
    fn attachment_offset_preserves_probe_world_position_under_object_rotation() {
        let transform = Transform::new(
            DVec3::new(2.0, -1.0, 0.5),
            DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2),
        )
        .unwrap();
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("rotated").with_transform(transform),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let object = snapshot.objects().values().next().unwrap();
        let local_position = DVec3::new(0.25, 0.5, -0.75);
        let world_position = transform.apply(local_position);

        let recovered = attachment_offset(world_position, object);

        assert!((recovered - local_position).length() < 1.0e-12);
    }

    #[test]
    fn charge_authoring_distinguishes_point_and_uniform_sphere_sources() {
        let world = World::new().snapshot();
        let point = new_charge_command(&world, -1.0e-9, ChargeObjectKind::Point);
        let sphere = new_charge_command(&world, 1.0e-9, ChargeObjectKind::Sphere);

        let CommandPayload::CommitWorld(point) = point else {
            panic!("charge authoring must issue a world transaction");
        };
        let CommandPayload::CommitWorld(sphere) = sphere else {
            panic!("charge authoring must issue a world transaction");
        };
        let WorldCommand::CreateObject(point) = &point[0] else {
            panic!("point transaction must create an object");
        };
        let WorldCommand::CreateObject(sphere) = &sphere[0] else {
            panic!("sphere transaction must create an object");
        };

        assert!(matches!(point.shape, Some(ObjectShape::Point { .. })));
        assert!(matches!(sphere.shape, Some(ObjectShape::Sphere { .. })));
        assert_eq!(
            point.components[&charge_component_id()].scalar(&charge_property_id()),
            Some(-1.0e-9)
        );
        assert_eq!(
            sphere.components[&charge_component_id()].scalar(&charge_property_id()),
            Some(1.0e-9)
        );
    }
}
