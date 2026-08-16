//! Transport bar, history controls, and field brush dialog.

use fieldcad_core::{SimulationMode, SnapshotFreshness, TimeStep};
use fieldcad_simulation::{CommandPayload, PlaybackSpeed};

use crate::ui::compute::{
    ComputeView, WorkbenchState, format_simulation_time, format_time_step, parse_playback_speed,
    time_step_drag_speed,
};
use crate::ui::{AppAction, FrameContext, UiFrameOutput, UiModel, ViewportTool};

pub fn menu_bar(
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
            // A background save (see `WindowState::save_scene`) is in
            // flight while `save_in_progress`: New/Open would discard the
            // session it's writing out from under it, and a second Save
            // would race the first, so the whole group is disabled rather
            // than only the specific action that would collide.
            let file_actions_enabled = !model.save_in_progress;
            ui.menu_button("File", |ui| {
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("New (Empty)"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::NewScene { template: false });
                    ui.close();
                }
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("New (Demo Scene)"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::NewScene { template: true });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("Save"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::SaveScene);
                    ui.close();
                }
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("Save As…"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::SaveSceneAs);
                    ui.close();
                }
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("Open…"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::OpenScene);
                    ui.close();
                }
                ui.separator();
                if frame.is_recording {
                    if ui.button("Stop Recording…").clicked() {
                        output.app_action = Some(AppAction::StopRecording);
                        ui.close();
                    }
                } else if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("Start Recording"))
                    .on_hover_text(
                        "Record every command this session executes and every wall-clock \
                         poll it's given, so it can be replayed later.",
                    )
                    .clicked()
                {
                    output.app_action = Some(AppAction::StartRecording);
                    ui.close();
                }
                if ui
                    .add_enabled(file_actions_enabled, egui::Button::new("Replay Recording…"))
                    .clicked()
                {
                    output.app_action = Some(AppAction::ReplayRecording);
                    ui.close();
                }
                ui.separator();
                if ui.button("Reload Catalog").clicked() {
                    output.app_action = Some(AppAction::ReloadCatalog);
                    ui.close();
                }
                if ui.button("Catalog…").clicked() {
                    model.catalog_visible = true;
                    ui.close();
                }
                if ui.checkbox(&mut model.settings_visible, "Settings…").clicked() {
                    ui.close();
                }
            });
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
            if frame.is_recording {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), "● Recording")
                    .on_hover_text("Every command and wall-clock poll is being recorded.");
            }
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

pub fn field_brush_dialog(context: &egui::Context, model: &mut UiModel, compute: &ComputeView) {
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
    // •, not ●: the latter is missing from egui's bundled fonts and
    // renders as a tofu box (this is the "Pause" badge that looked broken).
    ui.colored_label(state.color(), format!("• {}", state.label()));
}

const UNDO_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
const REDO_SHORTCUT: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::Z,
);
const REDO_ALT_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);

/// Undo and redo, next to the transport they share a bar with.
pub(super) fn history_controls(
    ui: &mut egui::Ui,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let history = &frame.compute.edit_history;
    let live = frame.compute.accepts_history_commands() && !frame.edit_in_progress;

    let reason = if frame.edit_in_progress {
        "Finish the edit in progress first."
    } else if !frame.compute.accepts_commands() {
        "The compute source is not accepting commands."
    } else {
        "Pause the simulation to step through the edit history."
    };

    // ↺/↻ (not ↶/↷) because egui's bundled fonts lack glyphs for the latter
    // pair and render them as tofu boxes.
    for (glyph, shortcut, entry, payload, verb) in [
        (
            "↺",
            UNDO_SHORTCUT,
            history.undo.as_deref(),
            CommandPayload::Undo,
            "Undo",
        ),
        (
            "↻",
            REDO_SHORTCUT,
            history.redo.as_deref(),
            CommandPayload::Redo,
            "Redo",
        ),
    ] {
        let enabled = live && entry.is_some();
        let keys = ui.ctx().format_shortcut(&shortcut);
        let response = ui.add_enabled(enabled, egui::Button::new(glyph));
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
