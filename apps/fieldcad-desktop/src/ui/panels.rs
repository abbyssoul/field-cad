//! The individual panels, property editors, and authoring commands.

use std::collections::BTreeMap;

use fieldcad_core::{
    BoundaryCondition, ChannelId, Dimension, FieldBox, FieldBoxSpec, FieldSphere, FieldSphereSpec,
    FieldValueKind, ObjectId, ObjectShape, ObjectSpec, Precision, ProbeId, ProbePosition,
    ProbeSpec, PropertyBag, PropertyKind, PropertySchema, PropertyValue, Quantity, SimulationMode,
    SlicePlane, SlicePlaneSpec, SnapshotFreshness, TimeStep, Transform, VectorQuantity, Velocity,
    WorldCommand, WorldObject, WorldSnapshot, relativistic_kinetic_energy, relativistic_momentum,
};
use fieldcad_particles::{ParticleTemplate, template_particle_spec};
use fieldcad_simulation::{
    CommandLifecycle, CommandPayload, CommandRecord, PlaybackSpeed, ProbeHistory,
};
use fieldcad_sources::{inertial_mass_component_id, mass_property_id};
use glam::{DQuat, DVec2, DVec3};

use super::compute::{
    ComputeView, WorkbenchState, format_simulation_time, format_time_step, parse_playback_speed,
    time_step_drag_speed, validity_note,
};
use super::plot::{history_plot, probe_history_plots};
use super::{
    CameraAction, ChannelLayerSettings, FrameContext, UiFrameOutput, UiModel, ViewportTool,
};
use crate::{
    mcp::{self, McpAction, McpSession},
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
            // The top bar is the simulation's transport. View controls moved
            // into the 3D view, which is both where their effect is and what
            // freed the room for these to be labelled and spaced.
            ui.strong("Field CAD");
            ui.separator();

            ui.label("Tool");
            for tool in ViewportTool::ALL {
                let response = ui
                    .selectable_label(model.viewport_tool == tool, tool.label())
                    .on_hover_text(tool.description());
                if response.clicked() {
                    model.viewport_tool = tool;
                    if tool == ViewportTool::FieldBrush {
                        model.field_brush_dialog_open = true;
                    }
                }
            }
            ui.separator();

            if ui
                .add_enabled(live && paused, egui::Button::new("▶  Play"))
                .on_hover_text("Advance continuously at the playback rate")
                .clicked()
            {
                output.submit(CommandPayload::Play);
            }
            if ui
                .add_enabled(live && !paused, egui::Button::new("⏸  Pause"))
                .on_hover_text("Stop at the current tick boundary")
                .clicked()
            {
                output.submit(CommandPayload::Pause);
            }
            if ui
                .add_enabled(live && paused, egui::Button::new("⏭  Step"))
                .on_hover_text("Advance exactly one fixed time step")
                .clicked()
            {
                output.submit(CommandPayload::Step);
            }

            ui.separator();
            history_controls(ui, frame, output);

            ui.separator();
            ui.label("dt").on_hover_text("Numerical time step");
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
            ui.label("speed")
                .on_hover_text("Wall-clock playback rate; never changes dt");
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
            // A run that stops on its own reads as a fault unless something says
            // otherwise. This is the only place that can: the pause is
            // authoritative, so nothing downstream can tell it apart from one
            // the user asked for.
            if frame.paused_for_edit {
                ui.colored_label(egui::Color32::from_rgb(235, 190, 75), "⏸ paused for edit")
                    .on_hover_text(
                        "Editing the scene is not a physical process, so the simulation is held \
                         at its last tick. It resumes when you finish the edit.",
                    );
            }
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
            ui.monospace(format!(
                "t = {}",
                format_simulation_time(frame.compute.time_seconds)
            ))
            .on_hover_text("Simulation time, reconstructed from the tick count");

            // Window toggles sit at the far end, away from the transport
            // controls they are not part of.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("?  Help")
                    .on_hover_text("How to build a scene, and where everything lives")
                    .clicked()
                {
                    model.help_visible = !model.help_visible;
                }
                ui.checkbox(&mut model.diagnostics_visible, "Diagnostics")
                    .on_hover_text("Solver diagnostics and session status window");
                ui.checkbox(&mut model.mcp_panel_open, "MCP")
                    .on_hover_text(
                        "Let an external agent drive this session over MCP, with a bearer token",
                    );
                ui.checkbox(&mut model.queue_panel_open, "Queue").on_hover_text(
                    "Inspect pending mutations, pause or resume the queue, and cancel a \
                     still-queued command",
                );
            });
        });
    });
}

/// Configuration kept ready for the numerical field-brush command. Field
/// snapshots are currently read-only outputs, so this intentionally cannot
/// submit a faux world edit that an analytic solver would immediately replace.
pub(super) fn field_brush_dialog(
    context: &egui::Context,
    model: &mut UiModel,
    compute: &ComputeView,
) {
    if !model.field_brush_dialog_open {
        return;
    }
    egui::Window::new("Field brush")
        .collapsible(false)
        .resizable(false)
        .open(&mut model.field_brush_dialog_open)
        .show(context, |ui| {
            ui.label("Paint a disturbance into a selected slice plane.");
            ui.small("The plane defines the brush orientation.");
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Radius");
                ui.add(
                    egui::DragValue::new(&mut model.field_brush.radius_metres)
                        .range(f64::from_bits(1)..=f64::MAX)
                        .suffix(" m"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Strength");
                ui.add(egui::DragValue::new(&mut model.field_brush.strength));
            });
            ui.horizontal(|ui| {
                ui.label("Field");
                egui::ComboBox::from_id_salt("field_brush_channel")
                    .selected_text(
                        model
                            .field_brush
                            .channel
                            .as_ref()
                            .map(|channel| {
                                compute
                                    .channel_names
                                    .get(channel)
                                    .cloned()
                                    .unwrap_or_else(|| channel.to_string())
                            })
                            .unwrap_or_else(|| "Choose a field".to_owned()),
                    )
                    .show_ui(ui, |ui| {
                        for channel in &compute.vector_channels {
                            let label = compute
                                .channel_names
                                .get(channel)
                                .map_or_else(|| channel.to_string(), Clone::clone);
                            let writable = compute.mutable_vector_channels.contains(channel);
                            let response = ui.add_enabled(writable, egui::Button::selectable(
                                model.field_brush.channel.as_ref() == Some(channel), label,
                            ));
                            if response.clicked() { model.field_brush.channel = Some(channel.clone()); }
                            if !writable { response.on_hover_text("Read-only: the active solver is analytical or does not support painting."); }
                        }
                    });
            });
            ui.separator();
            ui.separator();
            if compute.mode != SimulationMode::Paused {
                ui.colored_label(egui::Color32::from_rgb(235, 190, 75), "Pause simulation to paint.");
            } else {
                ui.small("Select a slice plane, then drag in the viewport. Positive strength follows the plane normal; negative strength reverses it.");
            }
            ui.small("Analytical fields remain authoritative. A read-only field will explain why it cannot be painted.");
        });
}

fn state_badge(ui: &mut egui::Ui, state: WorkbenchState) {
    ui.colored_label(state.color(), format!("● {}", state.label()));
}

const UNDO_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
/// Both spellings, because muscle memory differs by platform and by decade.
const REDO_SHORTCUT: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::Z,
);
const REDO_ALT_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);

/// Undo and redo, next to the transport they share a bar with.
///
/// They belong here because they are the same kind of thing: a control over what
/// the session is doing as a whole rather than over one selected entity. Each
/// names the edit it would reverse, so a step back is something a user chooses
/// rather than gambles on.
fn history_controls(ui: &mut egui::Ui, frame: &FrameContext<'_>, output: &mut UiFrameOutput) {
    let history = &frame.compute.edit_history;
    // An unfinished gesture has no meaningful step to take back: the edit it
    // would undo is still being made.
    let live = frame.compute.accepts_history_commands() && !frame.edit_in_progress;

    let reason = if frame.edit_in_progress {
        "Finish the edit in progress first."
    } else if !frame.compute.accepts_commands() {
        "The compute source is not accepting commands."
    } else {
        "Pause the simulation to step through the edit history."
    };

    for (glyph, shortcut, entry, payload, verb) in [
        (
            "↶",
            UNDO_SHORTCUT,
            history.undo.as_deref(),
            CommandPayload::Undo,
            "Undo",
        ),
        (
            "↷",
            REDO_SHORTCUT,
            history.redo.as_deref(),
            CommandPayload::Redo,
            "Redo",
        ),
    ] {
        let enabled = live && entry.is_some();
        let keys = ui.ctx().format_shortcut(&shortcut);
        let response = ui.add_enabled(enabled, egui::Button::new(glyph));
        // A disabled control that says only what it would have done is a dead
        // end; each of these says why it cannot, because every reason is
        // something the user can act on.
        let response = match (entry, enabled) {
            (Some(label), true) => response.on_hover_text(format!("{verb} · {label}   {keys}")),
            (Some(label), false) => {
                response.on_disabled_hover_text(format!("{verb} · {label}\n{reason}"))
            }
            (None, _) => {
                response.on_disabled_hover_text(format!("Nothing to {}", verb.to_lowercase()))
            }
        };
        if response.clicked() {
            output.submit(payload);
        }
    }

    // Deliberately only consumed when it would do something. The menu bar is
    // laid out before the panels, so consuming unconditionally would take Ctrl+Z
    // away from a text field the user is typing in and give it to nothing.
    let pressed =
        |ui: &mut egui::Ui, shortcut| ui.input_mut(|input| input.consume_shortcut(shortcut));
    if live && history.can_undo() && pressed(ui, &UNDO_SHORTCUT) {
        output.submit(CommandPayload::Undo);
    }
    if live
        && history.can_redo()
        && (pressed(ui, &REDO_SHORTCUT) || pressed(ui, &REDO_ALT_SHORTCUT))
    {
        output.submit(CommandPayload::Redo);
    }
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
            // Object names come from the world and are as long as the user made
            // them. Without a scroll area that refuses to shrink, the longest
            // name sets the panel's width; past `size_range` egui then reports a
            // clamped rect while painting the wider one, and the separator and
            // the 3D view are both placed from a rectangle that is not what is
            // on screen. Truncating keeps the common case free of a scrollbar.
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

/// The scene-level node, and what the simulation is composed of.
///
/// The header is the node itself rather than a label above it: there is one
/// thing here, and a folding header wrapped around a single selectable row
/// would read as a mistake. The arrow folds; the name selects.
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
            // What the simulation consists of, which is this panel's job. Read
            // only: activating a system is a physical decision and belongs with
            // the rest of the scene settings, one click away in the inspector.
            if frame.compute.field_systems.is_empty() {
                ui.weak("No field systems available.");
            }
            for system in &frame.compute.field_systems {
                let mark = if system.enabled { "◈" } else { "◇" };
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
    // The count goes in the header so a folded section still says how much is
    // behind it. Otherwise folding one loses the only clue that it has contents.
    let title = format!("Objects ({})", frame.world.objects().len());
    super::section(ui, "scene_objects_section", title, true, |ui| {
        // A bare object is the default — what it *does* is decided by the
        // components added to it in the inspector, not by which button created
        // it — but a particle-catalog preset is one click away in the dropdown
        // for the common case of "just give me an electron".
        let choices: Vec<(&str, ObjectPreset)> = std::iter::once(("Empty", ObjectPreset::Empty))
            .chain(
                ParticleTemplate::all()
                    .filter(|template| *template != ParticleTemplate::Custom)
                    .map(|template| (template.label(), ObjectPreset::Particle(template))),
            )
            .collect();
        if let Some(preset) = super::split_add_button(
            ui,
            "Add object",
            "Add an object at the origin.\n\
             Give it charge or mass in the inspector to couple it to a field.",
            ObjectPreset::Empty,
            &choices,
        ) {
            output.submit(match preset {
                ObjectPreset::Empty => new_object_command(frame.world),
                ObjectPreset::Particle(template) => template_object_command(frame.world, template),
            });
        }
        ui.add_space(4.0);

        if frame.world.objects().is_empty() {
            ui.weak("No objects yet.");
        }
        for object in frame.world.objects().values() {
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

/// Probes and planes are instruments, not physics.
///
/// They are one section because no equation system can see either: adding one
/// asks a question about the simulation without changing what is simulated.
fn measurement_section(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let instruments = frame.world.probes().len()
        + frame.world.planes().len()
        + frame.world.boxes().len()
        + frame.world.spheres().len();
    let title = format!("Measurement ({instruments})");
    super::section(ui, "scene_measurement_section", title, true, |ui| {
        ui.weak("Not simulated").on_hover_text(
            "Probes and slice planes sample the field for you.\n\
             They carry no charge or mass and never alter the result.",
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("+ Probe")
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
                output.edit(vec![WorldCommand::CreateProbe(ProbeSpec::at(
                    format!("Probe {}", frame.world.probes().len() + 1),
                    DVec3::new(1.0, 0.0, 0.6),
                    channels,
                ))]);
            }
            if let Some(preset) = super::split_add_button(
                ui,
                "Plane",
                "Draw the field across a slice",
                MeasurementPreset::Plane,
                &[
                    ("Plane", MeasurementPreset::Plane),
                    ("Box", MeasurementPreset::Box),
                    ("Sphere", MeasurementPreset::Sphere),
                ],
            ) {
                output.edit(vec![measurement_command(frame.world, preset)]);
            }
        });

        if !frame.world.probes().is_empty() {
            ui.add_space(8.0);
            ui.label("Probes");
            for probe in frame.world.probes().values() {
                match entity_row(
                    ui,
                    "◉",
                    &probe.name,
                    probe.visible,
                    model.probe_selection == Some(probe.id),
                    "Delete probe",
                ) {
                    Some(EntityRowAction::ToggleVisibility) => {
                        output.edit(vec![WorldCommand::SetProbeVisible {
                            probe: probe.id,
                            visible: !probe.visible,
                        }]);
                    }
                    Some(EntityRowAction::Select) => {
                        model.set_scene_selection(Some(SceneSelection::Probe(probe.id)));
                    }
                    Some(EntityRowAction::Delete) => {
                        output.edit(vec![WorldCommand::RemoveProbe(probe.id)]);
                    }
                    None => {}
                }
            }
        }

        if !frame.world.planes().is_empty() {
            ui.add_space(8.0);
            ui.label("Slice planes");
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
                        output.edit(vec![WorldCommand::SetPlaneVisible {
                            plane: plane.id,
                            visible: !plane.visible,
                        }]);
                    }
                    Some(EntityRowAction::Select) => {
                        model.set_scene_selection(Some(SceneSelection::Plane(plane.id)));
                    }
                    Some(EntityRowAction::Delete) => {
                        output.edit(vec![WorldCommand::RemovePlane(plane.id)]);
                    }
                    None => {}
                }
            }
        }

        if !frame.world.boxes().is_empty() {
            ui.add_space(8.0);
            ui.label("Field boxes");
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
                        output.edit(vec![WorldCommand::SetBoxVisible {
                            region: field_box.id,
                            visible: !field_box.visible,
                        }]);
                    }
                    Some(EntityRowAction::Select) => {
                        model.set_scene_selection(Some(SceneSelection::Box(field_box.id)));
                    }
                    Some(EntityRowAction::Delete) => {
                        output.edit(vec![WorldCommand::RemoveBox(field_box.id)]);
                    }
                    None => {}
                }
            }
        }

        if !frame.world.spheres().is_empty() {
            ui.add_space(8.0);
            ui.label("Field spheres");
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
                        output.edit(vec![WorldCommand::SetSphereVisible {
                            sphere: sphere.id,
                            visible: !sphere.visible,
                        }]);
                    }
                    Some(EntityRowAction::Select) => {
                        model.set_scene_selection(Some(SceneSelection::Sphere(sphere.id)));
                    }
                    Some(EntityRowAction::Delete) => {
                        output.edit(vec![WorldCommand::RemoveSphere(sphere.id)]);
                    }
                    None => {}
                }
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

/// The "visibility toggle, select, delete" row shape every scene entity
/// list (objects, probes, planes, boxes, spheres) shares. Drawing is the
/// only thing shared — each entity kind's own `WorldCommand` variants and
/// `SceneSelection` case differ enough that building the actual command
/// stays with the caller, which already has the id in scope from its own
/// loop.
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

/// The trailing "duplicate, focus, remove" actions every shape inspector
/// (plane, box, sphere) ends with — the shared shape is real, but each
/// shape's own `WorldCommand` variant and spec type differ enough (and the
/// `Focus selection` button needs nothing from the caller at all) that
/// building the two commands stays with the caller.
fn entity_actions(
    ui: &mut egui::Ui,
    output: &mut UiFrameOutput,
    kind: &str,
    duplicate: impl FnOnce() -> WorldCommand,
    remove: impl FnOnce() -> WorldCommand,
) {
    ui.add_space(10.0);
    if ui.button(format!("Duplicate {kind}")).clicked() {
        output.edit(vec![duplicate()]);
    }
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button(format!("Remove {kind}")).clicked() {
        output.edit(vec![remove()]);
    }
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
            // `auto_shrink` off on the horizontal axis is what keeps this panel
            // the width it asked for. Left on, the scroll area reports its
            // content's width, the panel grows to match, and egui then clamps
            // the *reported* rect back to `size_range` while the frame and its
            // contents stay painted at the wider size. Everything downstream —
            // the resize separator, the region left for the 3D view — is
            // positioned from the clamped rect, so it lands inside the panel.
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // The inspector shows exactly one subject: whatever is
                    // selected in the scene. Nothing is appended below it, so
                    // the panel's contents always answer "what am I looking at",
                    // and the same rule holds for the simulation node as for an
                    // object.
                    if model.world_selected {
                        ui.heading("Simulation");
                        ui.separator();
                        world_properties(ui, model, frame.compute, frame.edit_in_progress, output);
                    } else if let Some(object) =
                        model.selection.and_then(|id| frame.world.object(id))
                    {
                        ui.heading("Object");
                        ui.separator();
                        object_properties(ui, frame.world, frame.compute, object, output);
                    } else if let Some(plane) = model
                        .plane_selection
                        .and_then(|id| frame.world.planes().get(&id))
                    {
                        ui.heading("Slice plane");
                        ui.separator();
                        plane_properties(ui, plane, &mut model.field_layers, frame.compute, output);
                    } else if let Some(field_box) = model
                        .box_selection
                        .and_then(|id| frame.world.boxes().get(&id))
                    {
                        ui.heading("Field box");
                        ui.separator();
                        box_properties(
                            ui,
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
                        sphere_properties(
                            ui,
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
                        ui.heading("Inspector");
                        ui.separator();
                        empty_inspector(ui, model);
                    }
                });
        });
}

/// What the inspector says when the scene has nothing selected.
///
/// An empty panel is a dead end, so this points at the one node that is always
/// there. The hint is a button rather than prose because the fastest way to
/// explain where the simulation settings went is to take the user to them.
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

/// Everything that belongs to the scene rather than to one thing in it.
///
/// Ordered by how often it is touched: which physics is active, then how much of
/// it is transported for viewing, then read-only status. The domain summary sits
/// with the status because it is fixed for a session. Each is foldable, because
/// a user tuning sampling has no use for the status grid underneath it.
fn world_properties(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    super::section(
        ui,
        "inspector_numerical_domain",
        "Numerical domain",
        true,
        |ui| {
            numerical_domain_editor(ui, model, compute, edit_in_progress, output);
        },
    );
    super::section(ui, "inspector_fields", "Fields", true, |ui| {
        field_controls(ui, compute, output);
    });
    super::section(ui, "inspector_field_systems", "Field systems", true, |ui| {
        field_system_controls(ui, compute, output);
    });
    super::section(
        ui,
        "inspector_transport_sampling",
        "Transport sampling",
        true,
        |ui| transport_sampling(ui, compute, output),
    );
    super::section(ui, "inspector_compute", "Compute", true, |ui| {
        compute_panel(ui, compute);
    });
}

/// Edit the complete numerical lattice as one staged candidate. The individual
/// widgets deliberately do not submit commands: changing a domain rebuilds
/// solver state, so that only happens on the explicit apply action below.
fn numerical_domain_editor(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    compute: &ComputeView,
    edit_in_progress: bool,
    output: &mut UiFrameOutput,
) {
    let draft = model.domain_draft_for(compute.domain);

    ui.small(
        "Changing this lattice resets the local simulation to t = 0 and leaves it paused. \
         Transport sampling below does not change the solver grid.",
    );
    egui::Grid::new("numerical_domain_editor")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Bounds min");
            ui.horizontal(|ui| {
                domain_coordinate(ui, "x", &mut draft.min.x);
                domain_coordinate(ui, "y", &mut draft.min.y);
                domain_coordinate(ui, "z", &mut draft.min.z);
            });
            ui.end_row();

            ui.label("Bounds max");
            ui.horizontal(|ui| {
                domain_coordinate(ui, "x", &mut draft.max.x);
                domain_coordinate(ui, "y", &mut draft.max.y);
                domain_coordinate(ui, "z", &mut draft.max.z);
            });
            ui.end_row();

            ui.label("Cells");
            ui.horizontal(|ui| {
                domain_cells(ui, "x", &mut draft.cells.x);
                domain_cells(ui, "y", &mut draft.cells.y);
                domain_cells(ui, "z", &mut draft.cells.z);
            });
            ui.end_row();

            ui.label("Boundaries");
            ui.horizontal(|ui| {
                boundary_picker(ui, "x", &mut draft.boundaries.x);
                boundary_picker(ui, "y", &mut draft.boundaries.y);
                boundary_picker(ui, "z", &mut draft.boundaries.z);
            });
            ui.end_row();

            ui.label("Precision");
            egui::ComboBox::from_id_salt("domain_precision")
                .selected_text(match draft.precision {
                    Precision::F32 => "f32",
                    Precision::F64 => "f64",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut draft.precision, Precision::F32, "f32");
                    ui.selectable_value(&mut draft.precision, Precision::F64, "f64");
                });
            ui.end_row();
        });

    let candidate = draft.build();
    match candidate {
        Ok(domain) => {
            let spacing = domain.cell_size();
            let cells = domain.resolution().cell_count();
            let scalar_bytes = match domain.precision() {
                Precision::F32 => 4_u64,
                Precision::F64 => 8_u64,
            };
            let minimum_field_bytes = cells.saturating_mul(6).saturating_mul(scalar_bytes);
            ui.small(format!(
                "cell size {:.4} × {:.4} × {:.4} m · {cells} cells · Maxwell E/B minimum {}",
                spacing.x,
                spacing.y,
                spacing.z,
                format_bytes(minimum_field_bytes),
            ));
            let changed = domain != compute.domain;
            let response = ui
                .add_enabled(
                    compute.accepts_commands() && changed && !edit_in_progress,
                    egui::Button::new("Apply domain and reset"),
                )
                .on_hover_text(
                    "Validate the whole candidate, rebuild active solvers from the current authored \
                     world, clear run history, and pause at t = 0. If the current dt is unstable, \
                     the source selects 80% of the strictest reported limit.",
                );
            if response.clicked() {
                output.submit(CommandPayload::ReconfigureDomain(domain));
            }
        }
        Err(error) => {
            ui.colored_label(
                egui::Color32::from_rgb(240, 105, 95),
                format!("Invalid domain: {error}"),
            );
        }
    }
    if edit_in_progress {
        ui.small("Finish the scene edit in progress before applying a domain.");
    }
}

fn domain_coordinate(ui: &mut egui::Ui, axis: &str, value: &mut f64) {
    ui.add(
        egui::DragValue::new(value)
            .speed(0.02)
            .prefix(format!("{axis}: "))
            .suffix(" m"),
    );
}

fn domain_cells(ui: &mut egui::Ui, axis: &str, value: &mut u32) {
    ui.add(
        egui::DragValue::new(value)
            .speed(1.0)
            .prefix(format!("{axis}: "))
            .range(0..=u32::MAX),
    );
}

fn boundary_picker(ui: &mut egui::Ui, axis: &str, value: &mut BoundaryCondition) {
    let label = |boundary| match boundary {
        BoundaryCondition::Periodic => "Periodic",
        BoundaryCondition::Dirichlet => "Dirichlet",
        BoundaryCondition::Neumann => "Neumann",
        BoundaryCondition::Absorbing => "Absorbing",
        BoundaryCondition::Open => "Open",
    };
    egui::ComboBox::from_id_salt(("domain_boundary", axis))
        .selected_text(format!("{axis}: {}", label(*value)))
        .show_ui(ui, |ui| {
            for boundary in [
                BoundaryCondition::Periodic,
                BoundaryCondition::Dirichlet,
                BoundaryCondition::Neumann,
                BoundaryCondition::Absorbing,
                BoundaryCondition::Open,
            ] {
                ui.selectable_value(value, boundary, label(boundary));
            }
        });
}

fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// The fields this scene can have, and which model computes each.
///
/// A scene has one electric field. Whether it is solved analytically from static
/// charges or advanced in time by Maxwell's equations is a choice of *model*,
/// not a second field: two of them would publish contradictory values under one
/// name and each push a charge with its own version of the same force. So this
/// reads as one row per field with a model chosen for it, and the systems below
/// are what those models are made of.
fn field_controls(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    if compute.fields.is_empty() {
        ui.weak("No fields are available. Compose a field system into the scene.");
        return;
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "A field is computed by one model at a time. Choosing another replaces it, \
                 and brings whatever else that model computes with it.",
            )
            .small(),
        )
        .wrap(),
    );
    ui.add_space(4.0);

    let name_of = |plugin: &fieldcad_core::PluginId| {
        compute
            .field_systems
            .iter()
            .find(|system| &system.plugin.id == plugin)
            .map_or_else(
                || plugin.to_string(),
                |system| system.plugin.display_name.clone(),
            )
    };

    for field in &compute.fields {
        ui.push_id(&field.channel, |ui| {
            ui.horizontal(|ui| {
                ui.label(&field.display_name).on_hover_text(format!(
                    "{}\n{}",
                    field.channel,
                    field.kind_label()
                ));

                let selected = match &field.provider {
                    Some(provider) => name_of(provider),
                    None => NOT_COMPUTED.to_owned(),
                };
                let mut chosen: Option<Option<fieldcad_core::PluginId>> = None;
                ui.add_enabled_ui(compute.accepts_commands(), |ui| {
                    egui::ComboBox::from_id_salt(("field_model", &field.channel))
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(field.provider.is_none(), NOT_COMPUTED)
                                .clicked()
                            {
                                chosen = Some(None);
                            }
                            for candidate in &field.candidates {
                                let active = field.provider.as_ref() == Some(candidate);
                                if ui.selectable_label(active, name_of(candidate)).clicked() {
                                    chosen = Some(Some(candidate.clone()));
                                }
                            }
                        })
                        .response
                        .on_hover_text(if field.has_alternatives() {
                            "Which equation system computes this field"
                        } else {
                            "The only model of this field composed into the scene"
                        });
                });
                if let Some(provider) = chosen
                    && provider != field.provider
                {
                    output.submit(CommandPayload::SetFieldModel {
                        channel: field.channel.clone(),
                        provider,
                    });
                }
            });
        });
    }
}

const NOT_COMPUTED: &str = "Not computed";

/// The first field this system computes that another system already does, named
/// along with the system that holds it.
fn taken_field(
    system: &fieldcad_simulation::FieldSystemStatus,
    compute: &ComputeView,
) -> Option<(String, String)> {
    if system.enabled {
        return None;
    }
    system.channels.iter().find_map(|channel| {
        let field = compute
            .fields
            .iter()
            .find(|field| field.channel == channel.id)?;
        let provider = field.provider.as_ref()?;
        let name = compute
            .field_systems
            .iter()
            .find(|other| &other.plugin.id == provider)
            .map_or_else(
                || provider.to_string(),
                |other| other.plugin.display_name.clone(),
            );
        Some((field.display_name.clone(), name))
    })
}

/// Scene-level equation-system composition. A system is the activation unit,
/// rather than an individual channel, because channels such as Maxwell E and B
/// may be coupled by one solver.
fn field_system_controls(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Inactive systems do not simulate or publish fields. Their object properties remain available in the scene.",
            )
            .small(),
        )
        .wrap(),
    );

    if compute.field_systems.is_empty() {
        ui.weak("No field systems are available.");
        return;
    }

    for system in &compute.field_systems {
        ui.push_id(&system.plugin.id, |ui| {
            let mut enabled = system.enabled;
            // A system whose fields another system is already computing cannot
            // simply be switched on: which model computes a field is a choice,
            // and it is made above. Pointing at that control is more use than
            // letting the click through and reporting a rejection.
            let taken = taken_field(system, compute);
            let response = ui
                .add_enabled(
                    compute.accepts_commands() && (system.enabled || taken.is_none()),
                    egui::Checkbox::new(&mut enabled, &system.plugin.display_name),
                )
                .on_hover_text(format!(
                    "{}\n{} · version {}",
                    system.plugin.description, system.plugin.id, system.plugin.version
                ));
            let response = match taken {
                Some((field, provider)) => response.on_disabled_hover_text(format!(
                    "{field} is computed by {provider}.\n\
                     Choose this system as its model under Fields instead."
                )),
                None => response,
            };
            if response.changed() && enabled != system.enabled {
                output.submit(CommandPayload::SetFieldSystemEnabled {
                    plugin: system.plugin.id.clone(),
                    enabled,
                });
            }

            realtime_control(ui, system, compute, output);

            egui::CollapsingHeader::new("Fields and settings")
                .default_open(false)
                .show(ui, |ui| {
                    for channel in &system.channels {
                        let kind = match channel.value_kind {
                            FieldValueKind::Scalar(_) => "scalar",
                            FieldValueKind::Vector(_) => "vector",
                        };
                        ui.add_enabled_ui(system.enabled, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{} · {} · {}",
                                        channel.display_name,
                                        kind,
                                        channel.dimension()
                                    ))
                                    .small(),
                                )
                                .wrap(),
                            );
                        });
                    }

                    if !system.configuration_schema.properties.is_empty() {
                        ui.add_space(3.0);
                        ui.strong("Settings");
                        for property in &system.configuration_schema.properties {
                            let value = system
                                .configuration
                                .get(&property.id)
                                .map(format_configuration_value)
                                .unwrap_or_else(|| "not set".to_owned());
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{}: {value}",
                                        property.display_name
                                    ))
                                    .small(),
                                )
                                .wrap(),
                            );
                        }
                    }
                });
        });
    }
}

/// How closely one field system follows an edit that is still being made.
///
/// Dragging a body through a scene is not a physical process, so nothing is lost
/// by a system computing only the pose the user settles on. What is gained is a
/// viewport that stays responsive when an evaluator is expensive: without this,
/// the cost of a whole solve lands between one mouse position and the next, and
/// a scene becomes undraggable long before it becomes unsolvable.
fn realtime_control(
    ui: &mut egui::Ui,
    system: &fieldcad_simulation::FieldSystemStatus,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    ui.indent(("realtime", &system.plugin.id), |ui| {
        let mut realtime = system.realtime;
        let response = ui
            .add_enabled(
                compute.accepts_commands() && system.enabled,
                egui::Checkbox::new(&mut realtime, "Update while editing"),
            )
            .on_hover_text(
                "On: recompute this system for every intermediate value while you drag a body \
                 or type a property.\n\
                 Off: keep the last result until you let go, then recompute once from the \
                 values you committed.\n\
                 Either way the committed scene produces the same field.",
            );
        if response.changed() && realtime != system.realtime {
            output.submit(CommandPayload::SetFieldSystemRealtime {
                plugin: system.plugin.id.clone(),
                realtime,
            });
        }
    });
}

fn format_configuration_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Scalar(value) => format!(
            "{} {}",
            format_configuration_number(value.si_value()),
            value.dimension()
        ),
        PropertyValue::Vector(value) => {
            let vector = value.si_value();
            format!(
                "({}, {}, {}) {}",
                format_configuration_number(vector.x),
                format_configuration_number(vector.y),
                format_configuration_number(vector.z),
                value.dimension()
            )
        }
        PropertyValue::Boolean(value) => value.to_string(),
        PropertyValue::Text(value) | PropertyValue::Choice(value) => value.clone(),
    }
}

fn format_configuration_number(value: f64) -> String {
    if value != 0.0 && !(1.0e-3..1.0e6).contains(&value.abs()) {
        format!("{value:.6e}")
    } else {
        let formatted = format!("{value:.6}");
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn object_properties(
    ui: &mut egui::Ui,
    world: &WorldSnapshot,
    compute: &ComputeView,
    object: &WorldObject,
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

    // Outside the sections: these are things to do to the subject rather than a
    // group of its properties, and folding them away would hide the only way to
    // delete an object from the inspector.
    ui.add_space(10.0);
    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove object").clicked() {
        output.edit(vec![WorldCommand::RemoveObject(object.id)]);
    }
}

/// Where the object is, how big it is, and who decides where it goes next.
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
                position_changed |= coordinate_editor(ui, "x", &mut position.x, " m", editing);
                position_changed |= coordinate_editor(ui, "y", &mut position.y, " m", editing);
                position_changed |= coordinate_editor(ui, "z", &mut position.z, " m", editing);
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
                velocity_changed |= coordinate_editor(ui, "vx", &mut velocity.x, " m/s", editing);
                velocity_changed |= coordinate_editor(ui, "vy", &mut velocity.y, " m/s", editing);
                velocity_changed |= coordinate_editor(ui, "vz", &mut velocity.z, " m/s", editing);
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

/// Choose whether an object is a point or occupies a volume.
///
/// An object with no shape is a bare marker: it still has a position, and any
/// component attached to it still works, but it draws as a small proxy and a
/// field solver treats it as a point.
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
            // Carry the current radius across a kind change so switching from
            // point to sphere does not silently resize the object.
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

/// The default radius for an object whose shape was just chosen.
const DEFAULT_AUTHORING_RADIUS: f64 = 0.15;

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
            Self::Box => Some(ObjectShape::boxed(DVec3::splat(radius.max(1.0e-4)))?),
        };
        Ok(shape)
    }
}

/// Who decides how this object moves.
///
/// Motion is not a capability an object opts into — everything in the space has
/// a position, and a position that changes is velocity. The only question is
/// whether a solver integrates it or the user authors it.
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

/// Explain, in the object's own terms, what will actually happen to it.
///
/// This is the one place the composition model becomes visible: an object that
/// has not been given mass will not move no matter how the flag is set, and
/// saying so is cheaper than letting a user discover it by running.
fn motion_summary(object: &WorldObject) -> &'static str {
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

/// The object's inertial mass, if it has a valid one attached. Reading
/// straight off the component rather than through
/// `fieldcad_sources::inertial_mass_of` because the inspector wants
/// this one object's value, not every massive body in the scene.
fn inertial_mass_kg(object: &WorldObject) -> Option<f64> {
    object
        .components
        .get(&inertial_mass_component_id())
        .and_then(|properties| properties.scalar(&mass_property_id()))
        .filter(|mass| mass.is_finite() && *mass > 0.0)
}

/// Kinetic energy, momentum, and the force this body feels right now —
/// read-only context for watching a simulation, not something anything
/// downstream reads back. Shown only once the object has mass, since none of
/// the three means anything without it.
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

/// The same parenthesized `(x, y, z) unit` shape [`ComputeView`]'s
/// `format_value` uses for a published field sample, with engineering
/// notation per component instead of a fixed four decimals — these values can
/// span far more orders of magnitude than a normalized field reading.
fn format_vector(vector: DVec3, unit: &str) -> String {
    format!(
        "({}, {}, {}) {unit}",
        format_engineering(vector.x),
        format_engineering(vector.y),
        format_engineering(vector.z),
    )
}

/// Every component attached to this object, plus the ones it could still gain.
///
/// Rendered entirely from the registered [`ComponentSchema`]s. The inspector
/// knows nothing about charge or mass specifically, so a plugin that declares a
/// new component becomes editable here without a line changing in this file.
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
            // A component whose plugin is no longer loaded. Its values are
            // preserved in the world, so say so rather than dropping them
            // silently or pretending the component is not there.
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

/// Offer every registered component the object does not already carry.
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
                // A component is only offered if it can be attached with valid
                // defaults. Presenting an entry that fails on click would put
                // the burden of an unrepresentable schema on the user.
                let Ok(properties) = schema.default_properties() else {
                    ui.add_enabled(false, egui::Button::new(&schema.display_name))
                        .on_disabled_hover_text(
                            "This component has no default value and cannot be added here.",
                        );
                    continue;
                };
                if ui.button(&schema.display_name).clicked() {
                    // Attach with schema defaults so the object is immediately
                    // valid; the user then edits the value in place rather than
                    // filling in a dialog before anything exists.
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

/// One property row, chosen by its declared kind.
///
/// Returns whether the value changed. Physical scalars use engineering notation
/// because the values that matter here span an electron's mass to a coulomb.
fn property_editor(
    ui: &mut egui::Ui,
    object: ObjectId,
    schema: &PropertySchema,
    values: &mut PropertyBag,
    editing: &mut bool,
) -> bool {
    // Relevance is read from the bag being edited, not from the committed one,
    // so clearing a governing checkbox enables its dependent field in the same
    // frame rather than a frame later. The schema declares the controlling
    // property first, which is what puts it earlier in this loop.
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

/// The editor for one property's kind, without the relevance wrapper.
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
                let suffix = format!(" {}", dimension.unit_symbol());
                let mut vector_changed = false;
                vector_changed |= coordinate_editor(ui, "x", &mut vector.x, &suffix, editing);
                vector_changed |= coordinate_editor(ui, "y", &mut vector.y, &suffix, editing);
                vector_changed |= coordinate_editor(ui, "z", &mut vector.z, &suffix, editing);
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

/// A dimensioned scalar spanning many orders of magnitude.
///
/// A fixed step would be useless across this range — 0.01 kg/s is absurd for an
/// electron and glacial for a planet — so the drag speed follows the value's own
/// magnitude, and typing an exponent is always available as an escape.
fn scalar_editor(
    ui: &mut egui::Ui,
    value: &mut f64,
    dimension: Dimension,
    editing: &mut bool,
) -> bool {
    let speed = (value.abs() * 0.01).max(f64::MIN_POSITIVE);
    let suffix = format!(" {}", dimension.unit_symbol());
    let response = ui.add(
        egui::DragValue::new(value)
            .speed(speed)
            .custom_formatter(|value, _| format_engineering(value))
            .custom_parser(|text| text.trim().parse().ok())
            .update_while_editing(false)
            .suffix(suffix),
    );
    note_held_edit(&response, editing)
}

/// Readable across the range physical constants actually occupy.
fn format_engineering(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let magnitude = value.abs();
    if (1.0e-3..1.0e6).contains(&magnitude) {
        format!("{value:.4}")
    } else {
        format!("{value:.6e}")
    }
}

/// An inspector name edit is staged locally until the user explicitly accepts
/// it. That keeps Escape a genuine cancel even though the inspector is rebuilt
/// from an immutable world snapshot every frame.
fn name_editor(
    ui: &mut egui::Ui,
    source: impl std::hash::Hash + std::fmt::Debug,
    name: &str,
) -> Option<String> {
    let id = ui.make_persistent_id(source);
    let mut draft = ui.data_mut(|data| {
        data.get_temp::<String>(id)
            .unwrap_or_else(|| name.to_owned())
    });
    let response = ui
        .horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut draft)
                    .id(id)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner;
    let cancel = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let accept = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

    if cancel {
        ui.data_mut(|data| data.remove::<String>(id));
        None
    } else if accept {
        ui.data_mut(|data| data.remove::<String>(id));
        (draft != name).then_some(draft)
    } else if response.has_focus() {
        // Only an active text edit owns a local draft. Caching an idle value
        // would let the one old snapshot observed while an async command is in
        // flight override the new authoritative name forever.
        ui.data_mut(|data| data.insert_temp(id, draft));
        None
    } else {
        ui.data_mut(|data| data.remove::<String>(id));
        None
    }
}

fn plane_properties(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("plane_name", plane.id), &plane.name) {
        output.edit(vec![WorldCommand::SetPlaneName {
            plane: plane.id,
            name,
        }]);
    }
    ui.small("Drag the dashed purple N arrow to reorient the plane; RGB arrows and squares move its origin.");
    super::section(ui, "inspector_plane_geometry", "Geometry", true, |ui| {
        plane_geometry_editors(ui, plane, output);
    });
    super::section(ui, "inspector_plane_display", "Field display", true, |ui| {
        plane_field_layers(ui, plane, field_layers, compute);
    });

    entity_actions(
        ui,
        output,
        "plane",
        || {
            WorldCommand::CreatePlane(
                SlicePlaneSpec::from_plane(plane).with_name(format!("{} copy", plane.name)),
            )
        },
        || WorldCommand::RemovePlane(plane.id),
    );
}

/// Where the plane sits and how far it reaches, including the three standard
/// orientations — those are geometry, not display, so they belong here.
fn plane_geometry_editors(ui: &mut egui::Ui, plane: &SlicePlane, output: &mut UiFrameOutput) {
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
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, " m", editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, " m", editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, " m", editing);
            });
            ui.end_row();

            ui.label("Normal");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "nx", &mut normal.x, "", editing);
                changed |= coordinate_editor(ui, "ny", &mut normal.y, "", editing);
                changed |= coordinate_editor(ui, "nz", &mut normal.z, "", editing);
            });
            ui.end_row();

            ui.label("Half extent");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "u", &mut half_extent.x, " m", editing);
                changed |= coordinate_editor(ui, "v", &mut half_extent.y, " m", editing);
            });
            ui.end_row();
        });

    if changed && let Ok(spec) = plane_spec(plane, origin, normal, half_extent) {
        output.edit(vec![WorldCommand::SetPlane {
            plane: plane.id,
            spec,
        }]);
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
}

/// Warns that a channel's own visibility layer is off, so no per-entity
/// setting here will actually draw anything until it is turned on under
/// Fields in the View window. Byte-identical across every field-layers
/// panel (plane, box, sphere) before this was shared.
fn hidden_everywhere_warning(ui: &mut egui::Ui) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "This field is hidden everywhere. Turn its layer on under Fields \
                 in the View window.",
            )
            .small()
            .color(egui::Color32::from_rgb(230, 180, 80)),
        )
        .wrap(),
    );
}

/// The fields every per-entity field-layer settings type has in common —
/// `BoxLayerSettings` and `SphereLayerSettings` are otherwise identical
/// structs by different names; `PlaneLayerSettings` has these plus its own
/// extra magnitude/vector-mode controls, which is why only box and sphere
/// share [`volume_field_layers`] below.
trait VectorLayerSettings {
    fn visible_mut(&mut self) -> &mut bool;
    fn vectors_mut(&mut self) -> &mut crate::scene::VectorDisplay;
}

impl VectorLayerSettings for crate::scene::BoxLayerSettings {
    fn visible_mut(&mut self) -> &mut bool {
        &mut self.visible
    }
    fn vectors_mut(&mut self) -> &mut crate::scene::VectorDisplay {
        &mut self.vectors
    }
}

impl VectorLayerSettings for crate::scene::SphereLayerSettings {
    fn visible_mut(&mut self) -> &mut bool {
        &mut self.visible
    }
    fn vectors_mut(&mut self) -> &mut crate::scene::VectorDisplay {
        &mut self.vectors
    }
}

/// The display text a [`volume_field_layers`] caller supplies, since it's
/// the only thing that actually differs between a box and a sphere.
struct VolumeFieldLayerText<'a> {
    checkbox_label: &'a str,
    checkbox_hover: &'a str,
    arrow_hover: &'a str,
}

/// How each published vector channel is drawn inside a box or sphere —
/// arrows only, since neither has a natural surface to flatten a magnitude
/// map onto the way a plane does. `box_field_layers`/`sphere_field_layers`
/// are thin, differing only in which per-entity settings map they read and
/// their own display text.
fn volume_field_layers<Id: Ord + Copy, S: VectorLayerSettings + Default>(
    ui: &mut egui::Ui,
    id: Id,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    layer_map: impl Fn(&mut ChannelLayerSettings) -> &mut BTreeMap<Id, S>,
    text: VolumeFieldLayerText<'_>,
) {
    for channel in &compute.vector_channels {
        let name = channel_label(channel, &compute.channel_names);
        let layer = field_layers.entry(channel.clone()).or_default();
        let layer_visible = layer.visible;
        let settings = layer_map(layer).entry(id).or_default();
        ui.collapsing(name, |ui| {
            ui.checkbox(settings.visible_mut(), text.checkbox_label)
                .on_hover_text(text.checkbox_hover);
            if !layer_visible {
                hidden_everywhere_warning(ui);
            }
            ui.add_enabled_ui(*settings.visible_mut(), |ui| {
                super::vector_display_controls(
                    ui,
                    settings.vectors_mut(),
                    "Vector arrows",
                    text.arrow_hover,
                );
            });
        });
    }
}

/// How each published vector channel is drawn on this plane. Presentation only:
/// nothing here changes a computed value.
fn plane_field_layers(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    for channel in &compute.vector_channels {
        let name = channel_label(channel, &compute.channel_names);
        let layer = field_layers.entry(channel.clone()).or_default();
        // Read before the per-plane settings are borrowed. This panel
        // deliberately does not offer the layer's own visibility: that belongs
        // to the view, and a second control for it here is what made hiding a
        // field on one plane hide it everywhere.
        let layer_visible = layer.visible;
        let settings = layer.planes.entry(plane.id).or_default();
        ui.collapsing(name, |ui| {
            ui.checkbox(&mut settings.visible, "Show on this plane")
                .on_hover_text(
                    "Whether this plane draws this field. Other planes and the \
                     whole-domain arrows are unaffected.",
                );
            if !layer_visible {
                hidden_everywhere_warning(ui);
            }

            ui.add_enabled_ui(settings.visible, |ui| {
                ui.checkbox(&mut settings.magnitude_visible, "Magnitude colour");
                density_editor(
                    ui,
                    "Magnitude density",
                    &mut settings.magnitude_density,
                    settings.magnitude_visible,
                );
                super::vector_display_controls(
                    ui,
                    &mut settings.vectors,
                    "Vector arrows",
                    "Draw the field as arrows on this plane",
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
        });
    }
}

fn box_properties(
    ui: &mut egui::Ui,
    field_box: &FieldBox,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("box_name", field_box.id), &field_box.name) {
        output.edit(vec![WorldCommand::SetBoxName {
            region: field_box.id,
            name,
        }]);
    }
    ui.small("Drag the RGB rings to reorient the box; RGB arrows and squares move its origin.");
    super::section(ui, "inspector_box_geometry", "Geometry", true, |ui| {
        box_geometry_editors(ui, field_box, output);
    });
    super::section(ui, "inspector_box_display", "Field display", true, |ui| {
        box_field_layers(ui, field_box, field_layers, compute);
    });

    entity_actions(
        ui,
        output,
        "box",
        || {
            WorldCommand::CreateBox(
                FieldBoxSpec::from_box(field_box).with_name(format!("{} copy", field_box.name)),
            )
        },
        || WorldCommand::RemoveBox(field_box.id),
    );
}

/// Where the box sits and how big it is. Orientation is primarily set by
/// dragging the rotation rings in the viewport; this offers only a reset,
/// the way the plane's geometry section offers axis-snap buttons alongside
/// its draggable normal.
fn box_geometry_editors(ui: &mut egui::Ui, field_box: &FieldBox, output: &mut UiFrameOutput) {
    let mut origin = field_box.origin;
    let mut half_extent = field_box.half_extent;
    let mut changed = false;

    egui::Grid::new("box_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, " m", editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, " m", editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, " m", editing);
            });
            ui.end_row();

            ui.label("Half extent");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "w", &mut half_extent.x, " m", editing);
                changed |= coordinate_editor(ui, "h", &mut half_extent.y, " m", editing);
                changed |= coordinate_editor(ui, "d", &mut half_extent.z, " m", editing);
            });
            ui.end_row();
        });

    if changed
        && let Ok(spec) = FieldBoxSpec::from_box(field_box)
            .with_origin(origin)
            .and_then(|spec| spec.with_half_extent(half_extent))
    {
        output.edit(vec![WorldCommand::SetBox {
            region: field_box.id,
            spec,
        }]);
    }

    if ui
        .button("Reset orientation")
        .on_hover_text("Return this box to the world axes")
        .clicked()
        && let Ok(spec) = FieldBoxSpec::from_box(field_box).with_rotation(DQuat::IDENTITY)
    {
        output.edit(vec![WorldCommand::SetBox {
            region: field_box.id,
            spec,
        }]);
    }
}

/// How each published vector channel is drawn inside this box. Arrows only:
/// a volume's interior has no natural surface for a magnitude map, unlike a
/// plane. Presentation only: nothing here changes a computed value.
fn box_field_layers(
    ui: &mut egui::Ui,
    field_box: &FieldBox,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    volume_field_layers(
        ui,
        field_box.id,
        field_layers,
        compute,
        |layer| &mut layer.boxes,
        VolumeFieldLayerText {
            checkbox_label: "Show in this box",
            checkbox_hover: "Whether this box draws this field. Other boxes, spheres, and \
                              planes are unaffected.",
            arrow_hover: "Draw the field as arrows inside this box",
        },
    );
}

fn sphere_properties(
    ui: &mut egui::Ui,
    sphere: &FieldSphere,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
    output: &mut UiFrameOutput,
) {
    if let Some(name) = name_editor(ui, ("sphere_name", sphere.id), &sphere.name) {
        output.edit(vec![WorldCommand::SetSphereName {
            sphere: sphere.id,
            name,
        }]);
    }
    ui.small("RGB arrows and squares move its centre; drag the radius below to resize it.");
    super::section(ui, "inspector_sphere_geometry", "Geometry", true, |ui| {
        sphere_geometry_editors(ui, sphere, output);
    });
    super::section(
        ui,
        "inspector_sphere_display",
        "Field display",
        true,
        |ui| {
            sphere_field_layers(ui, sphere, field_layers, compute);
        },
    );

    entity_actions(
        ui,
        output,
        "sphere",
        || {
            WorldCommand::CreateSphere(
                FieldSphereSpec::from_sphere(sphere).with_name(format!("{} copy", sphere.name)),
            )
        },
        || WorldCommand::RemoveSphere(sphere.id),
    );
}

fn sphere_geometry_editors(ui: &mut egui::Ui, sphere: &FieldSphere, output: &mut UiFrameOutput) {
    let mut origin = sphere.origin;
    let mut radius = sphere.radius;
    let mut changed = false;

    egui::Grid::new("sphere_properties")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Origin");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "x", &mut origin.x, " m", editing);
                changed |= coordinate_editor(ui, "y", &mut origin.y, " m", editing);
                changed |= coordinate_editor(ui, "z", &mut origin.z, " m", editing);
            });
            ui.end_row();

            ui.label("Radius");
            ui.horizontal(|ui| {
                let editing = &mut output.scene_edit_in_progress;
                changed |= coordinate_editor(ui, "r", &mut radius, " m", editing);
            });
            ui.end_row();
        });

    if changed
        && let Ok(spec) = FieldSphereSpec::from_sphere(sphere)
            .with_origin(origin)
            .and_then(|spec| spec.with_radius(radius))
    {
        output.edit(vec![WorldCommand::SetSphere {
            sphere: sphere.id,
            spec,
        }]);
    }
}

/// How each published vector channel is drawn inside this sphere. See
/// [`box_field_layers`].
fn sphere_field_layers(
    ui: &mut egui::Ui,
    sphere: &FieldSphere,
    field_layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
    compute: &ComputeView,
) {
    volume_field_layers(
        ui,
        sphere.id,
        field_layers,
        compute,
        |layer| &mut layer.spheres,
        VolumeFieldLayerText {
            checkbox_label: "Show in this sphere",
            checkbox_hover: "Whether this sphere draws this field. Other spheres, boxes, and \
                              planes are unaffected.",
            arrow_hover: "Draw the field as arrows inside this sphere",
        },
    );
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

/// Where the probe samples: a world point, or an offset carried by an object.
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

/// Which channels this probe records. Every declared channel is offered,
/// including ones whose system is currently inactive, so a recorder survives a
/// system being switched off and on.
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

fn attachment_offset(world_position: DVec3, object: &WorldObject) -> DVec3 {
    object.transform.rotation.inverse() * (world_position - object.transform.translation)
}

/// Note that a control which edits the world is being held.
///
/// A drag or a half-typed value is one edit spread over many frames, and every
/// frame of it submits a command. Recording that here is what lets the shell
/// treat the whole gesture as a single edit: suspend the simulation for its
/// duration, and let a field system decide whether to follow the intermediate
/// values or wait for the one the user actually meant.
fn note_held_edit(response: &egui::Response, editing: &mut bool) -> bool {
    *editing |= response.dragged() || response.has_focus();
    response.changed()
}

fn coordinate_editor(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    suffix: &str,
    editing: &mut bool,
) -> bool {
    let response = ui.add(
        egui::DragValue::new(value)
            .speed(0.02)
            .prefix(format!("{label}: "))
            .suffix(suffix),
    );
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

/// What the "+ Add object" split button offers: a bare object, the default,
/// or a named particle-catalog preset from the dropdown.
#[derive(Clone, Copy, PartialEq)]
enum ObjectPreset {
    Empty,
    Particle(ParticleTemplate),
}

/// Fan successive objects out along x, so a second one is not created hidden
/// inside the first.
fn next_object_position(world: &WorldSnapshot) -> DVec3 {
    let index = world.objects().len();
    DVec3::new(index as f64 * 0.6, 0.0, 0.6)
}

/// Add a bare object: a named position in space and nothing else.
///
/// This is the default way the scene panel creates a modelled object. It
/// couples to no field until a component is attached, which is what makes the
/// inspector's component list the single place physics enters a scene.
fn new_object_command(world: &WorldSnapshot) -> CommandPayload {
    let index = world.objects().len() + 1;
    CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(
        ObjectSpec::new(format!("Object {index}"))
            .with_transform(
                Transform::at(next_object_position(world)).expect("static position is finite"),
            )
            .with_shape(
                ObjectShape::point(DEFAULT_AUTHORING_RADIUS).expect("static radius is valid"),
            ),
    )])
}

/// Add a named particle-catalog preset: mass, charge, and provenance composed
/// the same way [`fieldcad_particles::template_particle_spec`] does anywhere
/// else in this application. Dynamic and unpinned by default, so it responds
/// to whatever field is active rather than sitting still until the user opts
/// it in.
fn template_object_command(world: &WorldSnapshot, template: ParticleTemplate) -> CommandPayload {
    CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(
        template_particle_spec(
            template,
            false,
            next_object_position(world),
            DVec3::ZERO,
            DEFAULT_AUTHORING_RADIUS,
        )
        .expect("catalog template parameters are valid"),
    )])
}

/// What the "+ Plane" split button offers: a slice plane, the default, or a
/// field box/sphere from the dropdown.
#[derive(Clone, Copy, PartialEq)]
enum MeasurementPreset {
    Plane,
    Box,
    Sphere,
}

fn measurement_command(world: &WorldSnapshot, preset: MeasurementPreset) -> WorldCommand {
    match preset {
        MeasurementPreset::Plane => WorldCommand::CreatePlane(
            SlicePlaneSpec::new(
                format!("XY plane {}", world.planes().len() + 1),
                DVec3::ZERO,
                DVec3::Z,
            )
            .and_then(|plane| plane.with_half_extent(DVec2::splat(4.0)))
            .expect("static plane parameters are valid"),
        ),
        MeasurementPreset::Box => WorldCommand::CreateBox(
            FieldBoxSpec::new(
                format!("Box {}", world.boxes().len() + 1),
                DVec3::ZERO,
                DVec3::splat(1.0),
            )
            .expect("static box parameters are valid"),
        ),
        MeasurementPreset::Sphere => WorldCommand::CreateSphere(
            FieldSphereSpec::new(
                format!("Sphere {}", world.spheres().len() + 1),
                DVec3::ZERO,
                0.75,
            )
            .expect("static sphere parameters are valid"),
        ),
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
    let mut subscription = compute.subscription;
    let mut changed = false;
    let enabled = compute.accepts_commands();

    if let Some(planes) = density_field(
        ui,
        "Plane samples",
        "Samples per axis the source evaluates on each visible plane",
        enabled,
        0..=1_024,
        subscription.planes.map_or(0, |counts| counts.x),
    ) {
        subscription.planes = (planes > 0).then(|| glam::UVec2::splat(planes));
        changed = true;
    }

    if let Some(stride) = density_field(
        ui,
        "Domain stride",
        "Whole-domain lattice decimation; 0 publishes no 3D grid",
        enabled,
        0..=256,
        subscription.domain_stride.unwrap_or(0),
    ) {
        subscription.domain_stride = (stride > 0).then_some(stride);
        changed = true;
    }

    if let Some(boxes) = density_field(
        ui,
        "Box samples",
        "Samples per axis the source evaluates in each visible field box",
        enabled,
        0..=1_024,
        subscription.boxes.map_or(0, |counts| counts.x),
    ) {
        subscription.boxes = (boxes > 0).then(|| glam::UVec3::splat(boxes));
        changed = true;
    }

    if let Some(spheres) = density_field(
        ui,
        "Sphere samples",
        "Samples per axis the source evaluates over each visible sphere's bounding cube",
        enabled,
        0..=1_024,
        subscription.spheres.unwrap_or(0),
    ) {
        subscription.spheres = (spheres > 0).then_some(spheres);
        changed = true;
    }

    if changed && subscription != compute.subscription {
        output.submit(CommandPayload::SetSubscription(subscription));
    }
}

/// One transport-density drag value, gated on *its own* widget response —
/// never on whether some other field on the form changed, which would let
/// this one rewrite itself from a stale read whenever it happened to run
/// after a sibling that did (the bug this shape exists to make impossible
/// to reintroduce: reordering these calls must never matter).
fn density_field(
    ui: &mut egui::Ui,
    label: &str,
    hover: &str,
    enabled: bool,
    range: std::ops::RangeInclusive<u32>,
    mut count: u32,
) -> Option<u32> {
    let mut result = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui
            .add_enabled(
                enabled,
                egui::DragValue::new(&mut count).speed(1.0).range(range),
            )
            .on_hover_text(hover);
        if response.changed() {
            result = Some(count);
        }
    });
    result
}

fn compute_panel(ui: &mut egui::Ui, compute: &ComputeView) {
    egui::Grid::new("compute_status")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            // A grid column is as wide as its widest cell, and these values are
            // read from the running session, so their length is not something
            // this panel controls. Truncating makes the panel's width the
            // authority instead: without it one long value — the domain summary
            // is usually the offender — widens the grid, and with it the whole
            // inspector, past the width the panel was given.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);

            ui.label("Source");
            ui.add(egui::Label::new(&compute.description).truncate())
                .on_hover_text(&compute.description);
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
            ui.monospace(&compute.domain_summary)
                .on_hover_text(&compute.domain_summary);
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
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    command_error: Option<&str>,
) {
    let config = &mut model.diagnostics_config;

    egui::Window::new("Diagnostics")
        .default_pos(egui::pos2(218.0, 48.0))
        .collapsible(true)
        .show(context, |ui| {
            // ── Settings collapsible ────────────────────────────────────────
            ui.collapsing("⚙ Settings", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Update every:");
                    ui.add(
                        egui::Slider::new(&mut config.update_interval_ms, 16..=5000)
                            .suffix("ms")
                            .logarithmic(true),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_frame_time, "Frame timing");
                    ui.checkbox(&mut config.show_memory, "Memory");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_cpu, "CPU");
                    ui.checkbox(&mut config.show_scene_info, "Scene info");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut config.show_solver_step, "Solver step time");
                    ui.checkbox(&mut config.show_solver_diagnostics, "Solver diagnostics");
                });
            });

            // ── Frame timing ─────────────────────────────────────────────────
            if config.show_frame_time {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Frame");
                    ui.monospace(format!("{:.2} ms", frame.frame_time_ms));
                    if !frame.frame_history.is_empty() && frame.frame_min_ms.is_finite() {
                        ui.label(format!(
                            "(min {:.1} / max {:.1})",
                            frame.frame_min_ms, frame.frame_max_ms
                        ));
                    }
                });
                metric_history_dropdown(ui, "frame_plot", frame.frame_history, FRAME_TRACE_COLOR);
            }

            // ── Memory ───────────────────────────────────────────────────────
            if config.show_memory {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Mem");
                    if frame.process_rss_kb > 0 {
                        ui.monospace(format!("{:.1} MiB", frame.process_rss_kb as f64 / 1024.0));
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(ui, "mem_plot", frame.mem_history, MEM_TRACE_COLOR);
            }

            // ── CPU ──────────────────────────────────────────────────────────
            if config.show_cpu {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("CPU");
                    if frame.process_cpu_ms > 0.0 {
                        ui.monospace(format!("{:.1}s total", frame.process_cpu_ms / 1000.0));
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(ui, "cpu_plot", frame.cpu_history, CPU_TRACE_COLOR);
            }

            // ── Solver step time ────────────────────────────────────────────
            if config.show_solver_step {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong("Step");
                    let compute_ms = frame.compute.step_compute_ms;
                    if compute_ms > 0.0 {
                        ui.monospace(format!("{compute_ms:.2} ms")).on_hover_text(
                            "Wall-clock time the compute thread took to finish the most \
                                 recent simulation tick: force collection, every time-stepped \
                                 solver's own advance, dynamics integration, and the snapshot \
                                 it publishes.",
                        );
                        let dt_ms = frame.compute.time_step_seconds * 1_000.0;
                        if dt_ms > 0.0 {
                            let factor = dt_ms / compute_ms as f64;
                            let color = if factor >= 1.0 {
                                egui::Color32::from_rgb(95, 210, 120)
                            } else {
                                egui::Color32::from_rgb(235, 105, 90)
                            };
                            ui.colored_label(color, format!("({factor:.2}× real-time)"))
                                .on_hover_text(
                                    "Simulated dt ÷ time to compute one step. Below 1× means \
                                     this machine cannot compute steps as fast as the \
                                     simulation clock advances at the current dt — running \
                                     will fall behind wall-clock time.",
                                );
                        }
                    } else {
                        ui.monospace("—");
                    }
                });
                metric_history_dropdown(
                    ui,
                    "step_plot",
                    frame.step_compute_history,
                    STEP_TRACE_COLOR,
                );
            }

            // ── Scene info ───────────────────────────────────────────────────
            // Not a time series — an adapter name and two counters, so these
            // stay a plain grid rather than getting the instant+plot
            // treatment the measured metrics above get.
            if config.show_scene_info {
                ui.separator();
                ui.strong("Scene");
                egui::Grid::new("scene_info")
                    .num_columns(2)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        ui.label("GPU");
                        ui.monospace(frame.adapter_name);
                        ui.end_row();
                        ui.label("Objects");
                        ui.monospace(frame.world.objects().len().to_string());
                        ui.end_row();
                        ui.label("Samples");
                        ui.monospace(format_count(frame.compute.total_samples));
                        ui.end_row();
                    });
                ui.label(format!("Compute: {}", frame.compute.description));
            }

            // ── Solver diagnostics ──────────────────────────────────────────
            if config.show_solver_diagnostics && !frame.compute.diagnostics.is_empty() {
                ui.separator();
                ui.strong("Solver diagnostics");
                for line in &frame.compute.diagnostics {
                    ui.small(line);
                }
            }

            // ── Command rejection ──────────────────────────────────────────
            if let Some(error) = command_error {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(240, 105, 95),
                    format!("Last command rejected: {error}"),
                );
            }
        });
}

const FRAME_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(100, 155, 245);
const MEM_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(95, 210, 120);
const CPU_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(245, 205, 75);
const STEP_TRACE_COLOR: egui::Color32 = egui::Color32::from_rgb(200, 120, 220);

/// The instant value above this is the metric; the plot is context you ask
/// for, not something a constantly-open window should redraw every frame
/// whether or not anyone is looking at it.
fn metric_history_dropdown(ui: &mut egui::Ui, id: &str, history: &[f32], color: egui::Color32) {
    if history.len() < 2 {
        return;
    }
    egui::CollapsingHeader::new("Plot")
        .id_salt(id)
        .default_open(false)
        .show(ui, |ui| {
            history_plot(ui, history, color);
        });
}

fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        // Use non-breaking space as thousands separator
        let s = n.to_string();
        let mut result = String::with_capacity(s.len() + 2);
        for (i, c) in s.chars().enumerate() {
            if (s.len() - i).is_multiple_of(3) && i > 0 {
                result.push('\u{202f}');
            }
            result.push(c);
        }
        result
    } else {
        n.to_string()
    }
}

pub(super) fn mcp_window(context: &egui::Context, mcp: &McpSession) -> Option<McpAction> {
    let mut action = None;
    egui::Window::new("MCP")
        .default_pos(egui::pos2(218.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .show(context, |ui| match mcp {
            McpSession::Disabled => {
                ui.label(
                    "Let an external agent (or another client) drive this exact session over MCP.",
                );
                if ui.button("Enable MCP").clicked() {
                    action = Some(McpAction::Enable);
                }
            }
            McpSession::Running(running) => {
                ui.horizontal(|ui| {
                    match mcp::connection_count(running) {
                        Some(0) => {
                            ui.colored_label(egui::Color32::GRAY, "●");
                            ui.label("No client connected");
                        }
                        Some(count) => {
                            ui.colored_label(egui::Color32::from_rgb(95, 200, 110), "●");
                            ui.label(format!(
                                "{count} client{} connected",
                                if count == 1 { "" } else { "s" }
                            ));
                        }
                        // Only while a request happens to be touching the
                        // session table this exact frame; resolves itself
                        // next frame.
                        None => {
                            ui.colored_label(egui::Color32::GRAY, "●");
                            ui.label("Checking…");
                        }
                    }
                })
                .response
                .on_hover_text(
                    "A session persists until a client explicitly disconnects, so this can lag \
                     behind a client that vanished uncleanly (e.g. was killed).",
                );
                ui.label("Pass this token and URL to your agent's MCP client config:");
                egui::Grid::new("mcp_running")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Token");
                        ui.horizontal(|ui| {
                            // A local, per-frame copy: `TextEdit` needs `&mut
                            // String`, but nothing here should let a user
                            // "edit" the actual token, so any change is
                            // simply discarded at the end of the frame.
                            let mut token = running.token.clone();
                            ui.add(
                                egui::TextEdit::singleline(&mut token)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                            if ui.button("Copy").clicked() {
                                context.copy_text(running.token.clone());
                            }
                        });
                        ui.end_row();
                        ui.label("URL");
                        ui.monospace(format!("http://{}/mcp", running.addr));
                        ui.end_row();
                    });
                ui.separator();
                if ui.button("Disable MCP").clicked() {
                    action = Some(McpAction::Disable);
                }
            }
            McpSession::Failed(error) => {
                ui.colored_label(
                    egui::Color32::from_rgb(240, 105, 95),
                    format!("MCP server failed: {error}"),
                );
                if ui.button("Enable MCP").clicked() {
                    action = Some(McpAction::Enable);
                }
            }
        });
    action
}

/// A short, human-facing label for one queue entry's lifecycle state — the
/// UI's own presentation, distinct from the wire-format `snake_case` used by
/// `CommandLifecycle`'s `Serialize` impl.
fn lifecycle_label(state: CommandLifecycle) -> &'static str {
    match state {
        CommandLifecycle::Submitted => "Submitted",
        CommandLifecycle::Queued => "Queued",
        CommandLifecycle::Applied => "Applied",
        CommandLifecycle::Rejected => "Rejected",
        CommandLifecycle::Cancelled => "Cancelled",
    }
}

/// One row in the pending or history list: the command's kind, id, and
/// lifecycle state, plus — for a still-queued record — a Cancel button.
fn queue_record_row(ui: &mut egui::Ui, record: &CommandRecord, output: &mut UiFrameOutput) {
    ui.horizontal(|ui| {
        ui.monospace(format!("#{}", record.command.get()));
        ui.label(record.kind.label());
        ui.label(lifecycle_label(record.state));
        if record.state == CommandLifecycle::Rejected
            && let Some(error) = &record.error
        {
            ui.colored_label(egui::Color32::from_rgb(240, 105, 95), "⚠")
                .on_hover_text(error);
        }
        if record.state == CommandLifecycle::Queued && ui.button("Cancel").clicked() {
            output.submit(CommandPayload::CancelQueuedCommand(record.command));
        }
    });
}

/// Inspect pending mutations, pause/resume the queue, and cancel a
/// still-queued command — the desktop follow-on to
/// `docs/tasks/session-events-and-queue-control.md`'s server-side queue
/// surface. Non-modal, like `diagnostics_window`/`mcp_window`: a user can
/// keep editing the scene while this stays open.
pub(super) fn queue_window(
    context: &egui::Context,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let queue = &frame.compute.queue;
    egui::Window::new("Queue")
        .default_pos(egui::pos2(218.0, 48.0))
        .resizable(false)
        .collapsible(true)
        .show(context, |ui| {
            ui.horizontal(|ui| {
                if queue.paused {
                    ui.colored_label(egui::Color32::from_rgb(235, 190, 75), "⏸ paused");
                    if ui
                        .button("Resume queue")
                        .on_hover_text(
                            "Held mutations apply at the next eligible tick boundary, in \
                             submission order",
                        )
                        .clicked()
                    {
                        output.submit(CommandPayload::ResumeQueue);
                    }
                } else {
                    ui.colored_label(egui::Color32::from_rgb(95, 200, 110), "● running");
                    if ui
                        .button("Pause queue")
                        .on_hover_text(
                            "Hold queued scene/domain mutations at their tick boundary; \
                             simulation ticks continue and new mutations are still accepted",
                        )
                        .clicked()
                    {
                        output.submit(CommandPayload::PauseQueue);
                    }
                }
            });

            ui.separator();
            if queue.pending.is_empty() {
                ui.label("Nothing pending.");
            } else {
                for record in &queue.pending {
                    queue_record_row(ui, record, output);
                }
            }

            if !queue.history.is_empty() {
                ui.separator();
                ui.collapsing("History", |ui| {
                    for record in queue.history.iter().rev() {
                        queue_record_row(ui, record, output);
                    }
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        ObjectSpec, Transform, World, WorldCommand,
        quantities::{MassKg, kilogram},
    };

    use glam::{DQuat, DVec3};

    use super::*;

    #[test]
    fn an_idle_name_editor_does_not_cache_the_authoritative_name() {
        let context = egui::Context::default();
        let source = ("name_editor_test", ObjectId::new(1));
        let mut id = None;
        let _ = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                id = Some(ui.make_persistent_id(source));
                assert_eq!(name_editor(ui, source, "old name"), None);
            });
        });

        assert!(
            context
                .data(|data| data.get_temp::<String>(id.unwrap()))
                .is_none(),
            "an untouched editor must follow a later authoritative world refresh"
        );
    }

    fn painted_text(shape: &egui::epaint::Shape, output: &mut String) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                output.push_str(&text.galley.job.text);
                output.push('\n');
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    painted_text(shape, output);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn field_system_details_are_collapsed_by_default_in_the_narrow_inspector() {
        let world = super::super::tests::seeded_world();
        let source = super::super::tests::source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(280.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            field_system_controls(ui, &compute, &mut UiFrameOutput::default());
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }

        assert!(text.contains("Analytic test field"));
        assert!(
            !text.contains("Linear scalar"),
            "expanded channel details overcrowd the inspector: {text}"
        );
    }

    /// UI-19 regression: `transport_sampling` used to gate its first field's
    /// write on the *shared* `changed` accumulator rather than that field's
    /// own widget response — correct only because that field happened to be
    /// first, so nothing had set the accumulator yet. `density_field` now
    /// gates every field on its own response with no shared accumulator to
    /// leak from, so rendering the panel with nothing dragged must never
    /// submit a subscription change, regardless of field order.
    #[test]
    fn transport_sampling_does_not_submit_when_nothing_was_dragged() {
        let world = super::super::tests::seeded_world();
        let source = super::super::tests::source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let context = egui::Context::default();
        let mut output = UiFrameOutput::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            transport_sampling(ui, &compute, &mut output);
        });

        assert!(
            output.commands.is_empty(),
            "rendering the transport-density fields without touching any of \
             them must not submit a subscription change: {:?}",
            output.commands
        );
    }

    /// Undo names the edit it would reverse, rather than offering an unlabelled
    /// arrow the user has to press to find out what it does.
    #[test]
    fn the_transport_bar_offers_undo_and_redo_for_what_the_source_recorded() {
        let mut compute = ComputeView::build(
            &super::super::tests::source(),
            &super::super::tests::seeded_world().snapshot(),
            None,
        );
        compute.edit_history = fieldcad_simulation::EditHistoryStatus {
            undo: Some("Move object".to_owned()),
            redo: None,
            undo_depth: 1,
            redo_depth: 0,
        };

        let (commands, enabled) = drive_history_controls(&compute, false);
        assert!(enabled, "a paused, connected source can step back");
        assert_eq!(commands, vec![CommandPayload::Undo]);

        // An unfinished gesture has no completed edit to reverse.
        let (commands, enabled) = drive_history_controls(&compute, true);
        assert!(!enabled);
        assert!(commands.is_empty());

        // Neither does a running simulation: the scene an undo names is being
        // replaced under it.
        compute.mode = SimulationMode::Running;
        let (commands, enabled) = drive_history_controls(&compute, false);
        assert!(!enabled);
        assert!(commands.is_empty());
        assert!(!compute.accepts_history_commands());
    }

    #[test]
    fn undo_is_offered_as_nothing_to_do_when_the_history_is_empty() {
        let compute = ComputeView::build(
            &super::super::tests::source(),
            &super::super::tests::seeded_world().snapshot(),
            None,
        );
        assert!(!compute.edit_history.can_undo());

        let (commands, enabled) = drive_history_controls(&compute, false);

        assert!(!enabled);
        assert!(commands.is_empty());
    }

    /// Render the transport bar's history controls and click the undo button.
    /// Returns the commands produced and whether the button accepted the click.
    fn drive_history_controls(
        compute: &ComputeView,
        edit_in_progress: bool,
    ) -> (Vec<CommandPayload>, bool) {
        let context = egui::Context::default();
        let world = super::super::tests::seeded_world().snapshot();
        let history = ProbeHistory::default();

        let run = |events: Vec<egui::Event>| {
            let mut output = UiFrameOutput::default();
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 120.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                // Laid out as the transport bar lays it out, so the undo button
                // is the leftmost widget rather than the topmost.
                ui.horizontal(|ui| {
                    history_controls(
                        ui,
                        &FrameContext {
                            compute,
                            world: &world,
                            probe_history: &history,
                            adapter_name: "Test adapter",
                            frame_time_ms: 16.0,
                            active_translation: None,
                            plane_normal_label: None,
                            plane_normal_active: false,
                            paused_for_edit: false,
                            edit_in_progress,
                            projection: crate::camera::Projection::default(),
                            mcp: &McpSession::Disabled,
                            frame_history: &[],
                            frame_min_ms: 0.0,
                            frame_max_ms: 0.0,
                            process_rss_kb: 0,
                            process_cpu_ms: 0.0,
                            mem_history: &[],
                            cpu_history: &[],
                            step_compute_history: &[],
                        },
                        &mut output,
                    );
                    rect = ui.min_rect();
                });
            });
            (output.commands, rect)
        };

        let (_, rect) = run(Vec::new());
        let centre = egui::pos2(rect.left() + 10.0, rect.center().y);
        run(vec![egui::Event::PointerMoved(centre)]);
        run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let (commands, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        let enabled = !commands.is_empty();
        (commands, enabled)
    }

    /// The control the user reached for. Clearing it in the plane inspector must
    /// scope to that plane; the layer's own visibility stays where it belongs,
    /// in the View window, and is not reachable from here.
    #[test]
    fn the_plane_inspector_hides_a_field_on_that_plane_and_not_everywhere() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let mut compute = ComputeView::build(&super::super::tests::source(), &snapshot, None);
        let channel = fieldcad_test_field::vector_channel_id();
        compute.vector_channels = vec![channel.clone()];

        let context = egui::Context::default();
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let mut layers: BTreeMap<ChannelId, ChannelLayerSettings> = BTreeMap::new();
        layers.insert(channel.clone(), ChannelLayerSettings::default());
        layers.get_mut(&channel).unwrap().visible = true;

        let run = |layers: &mut BTreeMap<ChannelId, ChannelLayerSettings>,
                   events: Vec<egui::Event>| {
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(360.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                plane_field_layers(ui, plane, layers, &compute);
                rect = ui.min_rect();
            });
            rect
        };

        // Open the channel's group, then clear its first checkbox.
        let rect = run(&mut layers, Vec::new());
        let header = rect.left_top() + egui::vec2(6.0, 8.0);
        for pressed in [true, false] {
            run(
                &mut layers,
                vec![
                    egui::Event::PointerMoved(header),
                    egui::Event::PointerButton {
                        pos: header,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let rect = run(&mut layers, Vec::new());
        assert!(
            rect.height() > 40.0,
            "the channel group did not open: {rect:?}"
        );

        let toggle = egui::pos2(rect.left() + 24.0, rect.top() + 28.0);
        for pressed in [true, false] {
            run(
                &mut layers,
                vec![
                    egui::Event::PointerMoved(toggle),
                    egui::Event::PointerButton {
                        pos: toggle,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }

        let layer = &layers[&channel];
        assert!(
            !layer.planes[&plane.id].visible,
            "clearing the plane's checkbox must hide the field on this plane"
        );
        assert!(
            layer.visible,
            "and must leave the layer itself visible everywhere else"
        );
    }

    /// The plane is the one inspector subject the shared world fixture has no
    /// instance of, so its grouping is exercised here rather than through a
    /// whole frame.
    #[test]
    fn the_plane_inspector_separates_geometry_from_how_it_is_drawn() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let compute = ComputeView::build(&super::super::tests::source(), &snapshot, None);
        let mut layers = BTreeMap::new();

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |ui| {
            plane_properties(
                ui,
                plane,
                &mut layers,
                &compute,
                &mut UiFrameOutput::default(),
            );
        });
        let mut text = String::new();
        for clipped in &full_output.shapes {
            painted_text(&clipped.shape, &mut text);
        }

        for heading in ["Geometry", "Field display"] {
            assert!(
                text.contains(heading),
                "the plane inspector is missing its {heading} section: {text}"
            );
        }
    }

    /// The signal the whole gesture rests on. A value editor that reported a
    /// change but not a *hold* would pause and resume the simulation between
    /// every pair of mouse positions instead of once around the drag.
    #[test]
    fn a_held_value_editor_reports_an_edit_in_progress_and_a_released_one_does_not() {
        let context = egui::Context::default();
        let mut value = 1.0;

        let mut run = |events: Vec<egui::Event>| {
            let mut editing = false;
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                coordinate_editor(ui, "x", &mut value, " m", &mut editing);
                rect = ui.min_rect();
            });
            (editing, rect.center())
        };

        let (editing, centre) = run(Vec::new());
        assert!(!editing, "an untouched editor is not an edit in progress");

        run(vec![egui::Event::PointerMoved(centre)]);
        let (editing, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!editing, "a press alone has not started a drag");

        let (editing, _) = run(vec![egui::Event::PointerMoved(
            centre + egui::vec2(24.0, 0.0),
        )]);
        assert!(editing, "a drag in progress is an edit in progress");

        let (editing, _) = run(vec![egui::Event::PointerButton {
            pos: centre + egui::vec2(24.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!editing, "releasing commits the edit");
    }

    /// Every field system carries the choice, and it reaches the source as a
    /// command rather than staying a local view preference — the deferral has to
    /// happen where the solving does.
    #[test]
    fn realtime_update_is_offered_per_field_system_and_submitted_as_a_command() {
        let world = super::super::tests::seeded_world();
        let source = super::super::tests::source();
        let compute = ComputeView::build(&source, &world.snapshot(), None);
        let system = compute.field_systems[0].clone();
        assert!(system.realtime, "a scene starts fully live");

        let context = egui::Context::default();
        let run = |events: Vec<egui::Event>| {
            let mut output = UiFrameOutput::default();
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(280.0, 800.0),
                )),
                events,
                ..Default::default()
            };
            let full_output = context.run_ui(input, |ui| {
                realtime_control(ui, &system, &compute, &mut output);
                rect = ui.min_rect();
            });
            let mut text = String::new();
            for clipped in &full_output.shapes {
                painted_text(&clipped.shape, &mut text);
            }
            (output.commands, text, rect.center())
        };

        let (_, text, centre) = run(Vec::new());
        assert!(
            text.contains("Update while editing"),
            "the control is missing: {text}"
        );

        run(vec![egui::Event::PointerMoved(centre)]);
        let (commands, _, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            commands.is_empty(),
            "a press alone must not commit a choice"
        );

        let (commands, _, _) = run(vec![egui::Event::PointerButton {
            pos: centre,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(
            commands,
            vec![CommandPayload::SetFieldSystemRealtime {
                plugin: system.plugin.id.clone(),
                realtime: false,
            }]
        );
    }

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
    fn the_scene_panel_creates_an_object_with_no_physics_attached() {
        // The whole point of the single add button: what comes out couples to
        // nothing until the user says otherwise.
        let world = World::new().snapshot();

        let CommandPayload::CommitWorld(commands) = new_object_command(&world) else {
            panic!("object authoring must issue a world transaction");
        };
        let WorldCommand::CreateObject(spec) = &commands[0] else {
            panic!("transaction must create an object");
        };

        assert!(spec.components.is_empty());
        assert!(!spec.pinned);
        assert!(matches!(spec.shape, Some(ObjectShape::Point { .. })));
    }

    #[test]
    fn an_object_becomes_movable_by_attaching_mass_alone() {
        // Walks the authoring path the inspector exposes: create bare, attach
        // one component, and check the object model agrees it can now move.
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(
                    fieldcad_sources::inertial_mass_component_schema(),
                ),
                WorldCommand::CreateObject(ObjectSpec::new("gizmo")),
            ])
            .unwrap();
        let object = ObjectId::new(0);

        let bare = world.snapshot();
        assert_eq!(
            motion_summary(bare.object(object).unwrap()),
            "no inertia — add Inertial mass to make it movable"
        );

        // Exactly what the "+ Add → Mass" menu item issues.
        let schema = fieldcad_sources::inertial_mass_component_schema();
        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: schema.id.clone(),
                properties: schema.default_properties().unwrap(),
            }])
            .unwrap();

        let massive = world.snapshot();
        assert_eq!(
            motion_summary(massive.object(object).unwrap()),
            "moved by the forces acting on it"
        );
    }

    #[test]
    fn pinning_hands_motion_back_to_the_user() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("held").with_pinned(true),
            )])
            .unwrap();
        let object = ObjectId::new(0);
        let snapshot = world.snapshot();

        assert_eq!(
            motion_summary(snapshot.object(object).unwrap()),
            "held in place"
        );

        world
            .commit([WorldCommand::SetVelocity {
                object,
                velocity: Velocity::new(DVec3::X, DVec3::ZERO).unwrap(),
            }])
            .unwrap();
        let snapshot = world.snapshot();

        assert_eq!(
            motion_summary(snapshot.object(object).unwrap()),
            "carried at the velocity you set"
        );
    }

    #[test]
    fn every_registered_component_schema_can_be_attached_from_the_generic_menu() {
        // The M7 promise: a plugin declaring a new component becomes editable
        // without this file changing. If a shipped schema cannot produce valid
        // defaults, the "+ Add" menu would offer a dead entry.
        for schema in [
            fieldcad_electromagnetic_sources::charge_component_schema(),
            fieldcad_sources::inertial_mass_component_schema(),
            fieldcad_particles::particle_component_schema(),
        ] {
            let properties = schema
                .default_properties()
                .unwrap_or_else(|error| panic!("{} has no defaults: {error}", schema.display_name));
            assert!(
                schema.validate(&properties).is_ok(),
                "{} defaults do not satisfy its own schema",
                schema.display_name
            );
        }
    }

    /// Drive the real editor and check that an inert property refuses input.
    ///
    /// Asserting the schema flag alone would not catch the editor ignoring it,
    /// which is exactly the defect this exists to prevent.
    #[test]
    fn a_linked_gravitational_mass_cannot_be_edited_through_the_inspector() {
        use fieldcad_sources::{
            gravitational_mass_component_schema, independent_gravitational_mass_properties,
            linked_gravitational_mass_properties, mass_property_id,
        };

        let schema = gravitational_mass_component_schema();
        let mass = schema
            .properties
            .iter()
            .find(|property| property.id == mass_property_id())
            .unwrap();

        // Render the row and ask egui whether it accepts interaction. This is
        // the assertion that catches the editor ignoring the schema flag: a
        // widget inside a disabled scope reports `enabled() == false`.
        let row_is_interactive = |mut values: PropertyBag| {
            let context = egui::Context::default();
            let mut interactive = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            let _ = context.run_ui(input, |ui| {
                ui.add_enabled_ui(mass.is_relevant(&values), |ui| {
                    interactive = Some(ui.is_enabled());
                });
                property_editor(ui, ObjectId::new(0), mass, &mut values, &mut false);
            });
            interactive.expect("the row should have been laid out")
        };

        assert!(
            !row_is_interactive(linked_gravitational_mass_properties()),
            "a linked gravitational mass must render as non-interactive"
        );
        assert!(
            row_is_interactive(
                independent_gravitational_mass_properties(MassKg::new::<kilogram>(2.0)).unwrap()
            ),
            "unlinking must restore interaction"
        );
    }

    /// Toggling the link must free the value in the same frame, not the next.
    /// The editor reads relevance from the bag it is editing, and the schema
    /// declares the switch first, which is what makes that true.
    #[test]
    fn clearing_the_link_enables_the_value_within_one_frame() {
        use fieldcad_sources::{
            follows_inertial_property_id, gravitational_mass_component_schema,
            linked_gravitational_mass_properties, mass_property_id,
        };

        let schema = gravitational_mass_component_schema();
        let mut values = linked_gravitational_mass_properties();
        let mass = schema
            .properties
            .iter()
            .find(|property| property.id == mass_property_id())
            .unwrap();

        assert!(!mass.is_relevant(&values));

        // Exactly what the checkbox row does when a user clears it.
        values.insert(
            follows_inertial_property_id(),
            PropertyValue::Boolean(false),
        );

        assert!(
            mass.is_relevant(&values),
            "the value must be editable as soon as the switch is cleared"
        );
    }

    #[test]
    fn scalar_properties_render_across_the_range_physics_actually_uses() {
        // An electron mass and a coulomb must both be legible in the same editor.
        assert_eq!(format_engineering(0.0), "0");
        assert_eq!(format_engineering(1.5), "1.5000");
        assert!(format_engineering(9.109e-31).contains("e-31"));
        assert!(format_engineering(6.02e23).contains("e23"));
    }

    #[test]
    fn inertial_mass_kg_reads_a_valid_attached_component_and_nothing_else() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(
                    fieldcad_sources::inertial_mass_component_schema(),
                ),
                WorldCommand::CreateObject(ObjectSpec::new("gizmo")),
            ])
            .unwrap();
        let object = ObjectId::new(0);

        let bare = world.snapshot();
        assert_eq!(inertial_mass_kg(bare.object(object).unwrap()), None);

        world
            .commit([WorldCommand::AttachComponent {
                object,
                component: fieldcad_sources::inertial_mass_component_id(),
                properties: fieldcad_sources::inertial_mass_properties(MassKg::new::<kilogram>(
                    3.5,
                ))
                .unwrap(),
            }])
            .unwrap();

        let massive = world.snapshot();
        assert_eq!(inertial_mass_kg(massive.object(object).unwrap()), Some(3.5));
    }

    #[test]
    fn format_vector_uses_engineering_notation_per_component() {
        let formatted = format_vector(DVec3::new(1.5, -2.0e8, 0.0), "N");
        assert!(formatted.starts_with("(1.5000, "));
        assert!(formatted.contains("e8"));
        assert!(formatted.ends_with(") N"));
    }
}
