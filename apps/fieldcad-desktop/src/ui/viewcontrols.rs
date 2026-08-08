//! Camera and display controls, floating over the 3D view.
//!
//! These belong to the viewport rather than to a side panel. Framing the camera
//! and choosing what is drawn are things a user does *while looking at the
//! result*, so putting them at the edge of the window meant a round trip across
//! the screen for every adjustment — and it spent side-panel space that the
//! scene and the inspector need for their own jobs.
//!
//! Nothing here edits the world. Every control changes only how the scene is
//! presented, which is why hiding a probe cannot alter a recording and choosing
//! a viewpoint cannot alter a field.

use fieldcad_core::SceneScale;
use fieldcad_simulation::CommandPayload;

use crate::camera::{AxisView, Projection};

use super::compute::ComputeView;
use super::{CameraAction, FrameContext, UiFrameOutput, UiModel, ViewOptions};

/// Inset from the viewport's top-left corner.
///
/// Far enough in that the window does not sit on the panel resize handles, which
/// are hit-tested along the same edge.
const MARGIN: egui::Vec2 = egui::vec2(12.0, 12.0);

/// Stable window identity, so its position and collapsed state survive a
/// retitle and so tests can find where it landed.
pub(super) const WINDOW_ID: &str = "view_controls_window";

pub(super) fn view_controls(
    context: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    // Anchored to the viewport rather than the window, so it stays in the 3D
    // view when the side panels are resized.
    let anchor = output.viewport.min + MARGIN;

    // Open on first run. These are the controls a new user needs first — how do
    // I look at this, and what am I looking at — and a collapsed title bar
    // teaches neither. It is small, in a corner, and collapsible once learned.
    egui::Window::new("View")
        .id(egui::Id::new(WINDOW_ID))
        .default_open(true)
        .resizable(false)
        .collapsible(true)
        .current_pos(anchor)
        .constrain_to(output.viewport)
        .show(context, |ui| {
            camera_controls(ui, model, frame, output);
            ui.add_space(6.0);
            ui.separator();
            display_toggles(ui, &mut model.view);

            if !frame.compute.vector_channels.is_empty() {
                ui.add_space(6.0);
                ui.separator();
                field_layers(ui, model, frame.compute);
            }
        });
}

fn camera_controls(
    ui: &mut egui::Ui,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    ui.strong("Camera");
    // Above the viewpoints, because it changes what every one of them shows.
    ui.horizontal(|ui| {
        for projection in Projection::ALL {
            if ui
                .selectable_label(frame.projection == projection, projection.label())
                .on_hover_text(projection.description())
                .clicked()
                && frame.projection != projection
            {
                output.camera_action = Some(CameraAction::SetProjection(projection));
            }
        }
    });
    ui.add_space(4.0);

    scene_scale_controls(ui, frame.compute, output);
    ui.add_space(4.0);

    // Two rows of three keeps each axis's pair adjacent, so +X and −X read as
    // opposites rather than as six unrelated buttons.
    egui::Grid::new("view_axis_buttons")
        .num_columns(3)
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (index, view) in AxisView::ALL.into_iter().enumerate() {
                if ui
                    .add(egui::Button::new(view.label()).min_size(egui::vec2(34.0, 0.0)))
                    .on_hover_text(view.description())
                    .clicked()
                {
                    output.camera_action = Some(CameraAction::Axis(view));
                }
                if index % 3 == 2 {
                    ui.end_row();
                }
            }
        });

    ui.add_space(4.0);
    let has_selection = model.scene_selection().is_some();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_selection, egui::Button::new("Focus  [F]"))
            .on_hover_text("Frame the selected item")
            .on_disabled_hover_text("Select something in the scene or the 3D view first")
            .clicked()
        {
            output.camera_action = Some(CameraAction::FocusSelection);
        }
        if ui
            .button("Reset")
            .on_hover_text("Return to the default viewpoint")
            .clicked()
        {
            output.camera_action = Some(CameraAction::Reset);
        }
    });

    if model.following.is_some() {
        ui.add_space(4.0);
        follow_controls(ui, model, frame, output);
    }
}

/// Status, stop control, and precise distance/angle fields for the camera's
/// follow lock (see `CameraAction::ToggleFollow`, started from the object
/// inspector's "Follow" button). Only shown while following: with nothing
/// locked, distance and angle are just the ordinary dolly/orbit state, and
/// showing these fields unconditionally would suggest they mean something
/// they do not.
///
/// The fields duplicate what dolly and orbit already adjust with the mouse —
/// they exist for a value a user wants set exactly (matching a previous
/// shot, or a round number) rather than by eye, and stay in sync with mouse
/// input either way since both write the same camera state.
fn follow_controls(
    ui: &mut egui::Ui,
    model: &UiModel,
    frame: &FrameContext<'_>,
    output: &mut UiFrameOutput,
) {
    let Some(id) = model.following else {
        return;
    };
    let name = frame
        .world
        .object(id)
        .map_or("(deleted)", |object| object.name.as_str());

    ui.horizontal(|ui| {
        ui.label(format!("Following: {name}"))
            .on_hover_text("The camera is locked onto this object; it appears motionless while the rest of the scene moves around it.");
        if ui.small_button("Stop").clicked() {
            output.camera_action = Some(CameraAction::ToggleFollow(id));
        }
    });

    egui::Grid::new("follow_distance_angle")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Distance");
            let mut distance = frame.camera_distance;
            let distance_speed = (distance * 0.02).max(0.001);
            if ui
                .add(
                    egui::DragValue::new(&mut distance)
                        .speed(distance_speed)
                        .range(0.001..=f32::MAX),
                )
                .changed()
            {
                output.camera_action = Some(CameraAction::SetDistance(distance));
            }
            ui.end_row();

            ui.label("Yaw");
            let mut yaw_degrees = frame.camera_yaw.to_degrees();
            if ui
                .add(
                    egui::DragValue::new(&mut yaw_degrees)
                        .speed(1.0)
                        .suffix("°"),
                )
                .changed()
            {
                output.camera_action = Some(CameraAction::SetYaw(yaw_degrees.to_radians()));
            }
            ui.end_row();

            ui.label("Pitch");
            let mut pitch_degrees = frame.camera_pitch.to_degrees();
            if ui
                .add(
                    egui::DragValue::new(&mut pitch_degrees)
                        .speed(1.0)
                        .range(-89.0..=89.0)
                        .suffix("°"),
                )
                .changed()
            {
                output.camera_action = Some(CameraAction::SetPitch(pitch_degrees.to_radians()));
            }
            ui.end_row();
        });
}

/// A named preset offered by the scale picker: its label, and the constructor
/// it selects.
type ScenePresetEntry = (&'static str, fn() -> SceneScale);

/// Named presets offered by the scale picker, in order from smallest to
/// largest. A value that does not match any of these (typed directly into
/// the metres field) shows as "Custom".
const SCENE_SCALE_PRESETS: &[ScenePresetEntry] = &[
    ("Nanometre", SceneScale::nanometre),
    ("Micrometre", SceneScale::micrometre),
    ("Millimetre", SceneScale::millimetre),
    ("Metre (default)", SceneScale::metre),
    ("Kilometre", SceneScale::kilometre),
    ("Astronomical unit", SceneScale::astronomical_unit),
    ("Light-year", SceneScale::light_year),
];

fn scene_scale_label(scale: SceneScale) -> &'static str {
    SCENE_SCALE_PRESETS
        .iter()
        .find(|(_, preset)| preset() == scale)
        .map_or("Custom", |(label, _)| label)
}

/// How many metres one render/camera unit represents — a camera setting, not
/// a simulation one: it never changes a stored object position, size, or
/// physical constant, only how the viewport's distance/near/far numbers map
/// onto real space. That is also why it lives here rather than in the
/// inspector's "Numerical domain" section. Unlike a domain change, this never
/// fails validation in a way worth staging and has no destructive effect on
/// solver state, so each change submits immediately rather than waiting on
/// an explicit apply.
fn scene_scale_controls(ui: &mut egui::Ui, compute: &ComputeView, output: &mut UiFrameOutput) {
    let live = compute.accepts_commands();
    let current = compute.scene_scale;

    ui.horizontal(|ui| {
        ui.label("Scale").on_hover_text(
            "How many metres one render/camera unit represents. Sets the \
             viewport's camera range and default object sizing — never \
             changes a stored object position, size, or physical constant.",
        );
        egui::ComboBox::from_id_salt("scene_scale_preset")
            .selected_text(scene_scale_label(current))
            .show_ui(ui, |ui| {
                for (label, preset) in SCENE_SCALE_PRESETS {
                    let preset = preset();
                    if ui.selectable_label(current == preset, *label).clicked() && current != preset
                    {
                        output.submit(CommandPayload::SetSceneScale(preset));
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        ui.label("metres / unit");
        let mut metres = current.metres();
        let drag_speed = (metres * 0.01).max(f64::from_bits(1));
        let response = ui
            .add_enabled(
                live,
                egui::DragValue::new(&mut metres)
                    .speed(drag_speed)
                    .range(f64::from_bits(1)..=f64::MAX)
                    .custom_formatter(|metres, _| {
                        fieldcad_core::format_si_value(metres, fieldcad_core::Dimension::LENGTH)
                            .unwrap()
                    })
                    .custom_parser(|text| {
                        fieldcad_core::parse_si_value(text, fieldcad_core::Dimension::LENGTH)
                            .or_else(|| text.trim().parse().ok())
                    })
                    .update_while_editing(false),
            )
            .on_hover_text(
                "Drag to adjust, or click to enter a value, e.g. 1nm for nanometre scale",
            );
        if response.changed()
            && let Ok(scale) = SceneScale::from_metres(metres)
            && scale != current
        {
            output.submit(CommandPayload::SetSceneScale(scale));
        }
    });
}

fn display_toggles(ui: &mut egui::Ui, view: &mut ViewOptions) {
    ui.strong("Show");
    for (label, hover, field) in ViewOptions::PRIMARY_ENTRIES {
        ui.checkbox(field(view), label).on_hover_text(hover);
    }
    egui::CollapsingHeader::new("Auxiliary object types")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_enabled_ui(view.auxiliary_objects, |ui| {
                for (label, hover, field) in ViewOptions::AUXILIARY_ENTRIES {
                    ui.checkbox(field(view), label).on_hover_text(hover);
                }
            });
        });
    egui::CollapsingHeader::new("Compute")
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut view.compute_bounds, "Bounds")
                .on_hover_text("The spatial extent of the active computation; this does not change the solver domain");
        });
    egui::CollapsingHeader::new("Transform gizmo")
        .default_open(false)
        .show(ui, |ui| {
            ui.small("Screen-space size; does not change the selected object.");
            egui::Grid::new("transform_gizmo_display")
                .num_columns(2)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Origin arrows");
                    ui.horizontal(|ui| {
                        ui.label("Length");
                        ui.add(
                            egui::DragValue::new(&mut view.gizmo_display.axis_length_px)
                                .range(12.0..=300.0)
                                .suffix(" px"),
                        );
                        ui.label("Thickness");
                        ui.add(
                            egui::DragValue::new(&mut view.gizmo_display.axis_thickness_px)
                                .range(0.5..=24.0)
                                .suffix(" px"),
                        );
                    });
                    ui.end_row();
                    ui.label("Rotation rings");
                    ui.horizontal(|ui| {
                        ui.label("Diameter");
                        ui.add(
                            egui::DragValue::new(&mut view.gizmo_display.rotation_diameter_px)
                                .range(24.0..=600.0)
                                .suffix(" px"),
                        );
                        ui.label("Thickness");
                        ui.add(
                            egui::DragValue::new(&mut view.gizmo_display.rotation_thickness_px)
                                .range(0.5..=24.0)
                                .suffix(" px"),
                        );
                    });
                    ui.end_row();
                });
        });
}

/// Which published field channels are drawn.
///
/// This is a display choice like any other toggle above it, so it lives here
/// rather than in the inspector: turning `B` off does not stop Maxwell solving
/// for it.
fn field_layers(ui: &mut egui::Ui, model: &mut UiModel, compute: &ComputeView) {
    ui.strong("Fields");
    for channel in &compute.vector_channels {
        let label = compute
            .channel_names
            .get(channel)
            .cloned()
            .unwrap_or_else(|| channel.to_string());
        let layer = model.field_layers.entry(channel.clone()).or_default();
        ui.checkbox(&mut layer.visible, label);
        ui.add_enabled_ui(layer.visible, |ui| {
            ui.indent(("domain_vectors", channel), |ui| {
                // The same control the slice plane uses. A volume needs no
                // extent of its own: what it draws is the domain the solver
                // published, and framing it is the camera's job.
                super::vector_display_controls(
                    ui,
                    &mut layer.whole_domain.vectors,
                    "Through the domain",
                    "Sparse glyphs through the whole domain, in 3D",
                );
                super::flow_line_display_controls(
                    ui,
                    &mut layer.whole_domain.flow_lines,
                    "Flow lines",
                    "Continuous flow lines through the whole domain, independent of the \
                     arrows above",
                );
            });
        });
    }
}
