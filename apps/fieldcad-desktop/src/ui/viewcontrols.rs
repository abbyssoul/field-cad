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
}

fn display_toggles(ui: &mut egui::Ui, view: &mut ViewOptions) {
    ui.strong("Show");
    for (label, hover, field) in ViewOptions::ENTRIES {
        ui.checkbox(field(view), label).on_hover_text(hover);
    }
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
            });
        });
    }
}
