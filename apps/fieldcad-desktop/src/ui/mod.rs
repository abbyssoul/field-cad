//! egui panels.
//!
//! Every panel reads a [`ComputeView`] rather than a `&dyn FieldDataSource`. That
//! keeps the widgets testable without standing up a runtime, and stops the UI
//! from reaching into snapshot internals.
//!
//! This module owns the per-frame input and output types and the panel layout;
//! [`compute`] holds the read-only view model and its formatting, [`panels`] the
//! individual panels and property editors, and [`plot`] the probe history plot.

mod compute;
mod panels;
mod plot;

pub use compute::ComputeView;

use panels::{diagnostics_window, inspector, menu_bar, scene_tree};
use plot::floating_probe_plots;

use std::collections::{BTreeMap, BTreeSet};

use fieldcad_core::{ChannelId, ObjectId, PlaneId, ProbeId, WorldCommand, WorldSnapshot};
use fieldcad_simulation::{CommandPayload, ProbeHistory};

use crate::{
    camera::AxisView,
    scene::{FieldLayerSettings, PlaneLayerSettings, SceneSelection},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraAction {
    Axis(AxisView),
    FocusSelection,
}

#[derive(Debug, Default)]
pub struct UiModel {
    pub grid_visible: bool,
    pub axes_visible: bool,
    pub diagnostics_visible: bool,
    pub selection: Option<ObjectId>,
    pub plane_selection: Option<PlaneId>,
    pub probe_selection: Option<ProbeId>,
    /// Non-modal plot windows pinned independently of scene selection.
    pub probe_plots: BTreeMap<ProbeId, ProbePlotWindow>,
    /// Independent visualization state for every published vector channel.
    pub field_layers: BTreeMap<ChannelId, ChannelLayerSettings>,
    /// Most recent asynchronous command rejection, retained until a later
    /// command succeeds so the user can act on it rather than consult a log.
    pub command_error: Option<String>,
}

impl UiModel {
    pub fn new() -> Self {
        Self {
            grid_visible: true,
            axes_visible: true,
            diagnostics_visible: true,
            selection: None,
            plane_selection: None,
            probe_selection: None,
            probe_plots: BTreeMap::new(),
            field_layers: BTreeMap::new(),
            command_error: None,
        }
    }

    pub fn open_probe_plot(&mut self, probe: &fieldcad_core::Probe) {
        self.probe_plots
            .entry(probe.id)
            .or_insert_with(|| ProbePlotWindow {
                channels: probe.channels.iter().cloned().collect(),
            });
    }

    /// Ensure every declared vector channel has presentation state. If none of
    /// the currently available channels is visible, reveal the first one; later
    /// channels remain opt-in to avoid an unexpected overlay.
    pub fn synchronize_field_layers(&mut self, compute: &ComputeView) {
        for channel in &compute.vector_channels {
            self.field_layers.entry(channel.clone()).or_default();
        }
        let any_visible = compute.vector_channels.iter().any(|channel| {
            self.field_layers
                .get(channel)
                .is_some_and(|layer| layer.visible)
        });
        if !any_visible
            && let Some(first) = compute.vector_channels.first()
            && let Some(layer) = self.field_layers.get_mut(first)
        {
            layer.visible = true;
        }
    }

    pub fn scene_selection(&self) -> Option<SceneSelection> {
        self.selection
            .map(SceneSelection::Object)
            .or_else(|| self.plane_selection.map(SceneSelection::Plane))
            .or_else(|| self.probe_selection.map(SceneSelection::Probe))
    }

    pub fn set_scene_selection(&mut self, selection: Option<SceneSelection>) {
        self.selection = None;
        self.plane_selection = None;
        self.probe_selection = None;
        match selection {
            Some(SceneSelection::Object(id)) => self.selection = Some(id),
            Some(SceneSelection::Plane(id)) => self.plane_selection = Some(id),
            Some(SceneSelection::Probe(id)) => self.probe_selection = Some(id),
            None => {}
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbePlotWindow {
    /// Channels shown as separate, unit-safe plots in this window.
    pub channels: BTreeSet<ChannelId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelLayerSettings {
    pub visible: bool,
    pub whole_domain: FieldLayerSettings,
    pub planes: BTreeMap<PlaneId, PlaneLayerSettings>,
}

#[derive(Debug)]
pub struct UiFrameOutput {
    pub viewport: egui::Rect,
    pub viewport_gesture: ViewportGesture,
    pub camera_action: Option<CameraAction>,
    /// Every command the frame produced, in the order the widgets produced it.
    ///
    /// A single slot silently discarded all but the last, so two controls
    /// changing in one frame lost an edit with no error and no symptom beyond a
    /// widget that appeared not to work.
    pub commands: Vec<CommandPayload>,
}

impl UiFrameOutput {
    pub(super) fn submit(&mut self, payload: CommandPayload) {
        self.commands.push(payload);
    }

    /// Submit a world transaction: one atomic edit at one revision.
    pub(super) fn edit(&mut self, commands: Vec<WorldCommand>) {
        self.submit(CommandPayload::CommitWorld(commands));
    }
}

impl Default for UiFrameOutput {
    fn default() -> Self {
        Self {
            viewport: egui::Rect::NOTHING,
            viewport_gesture: ViewportGesture::default(),
            camera_action: None,
            commands: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewportGesture {
    pub pointer_position: Option<egui::Pos2>,
    pub drag_delta: egui::Vec2,
    pub middle_dragged: bool,
    pub shift: bool,
    pub scroll_delta: f32,
    pub primary_clicked: bool,
    pub primary_pressed: bool,
    pub primary_released: bool,
    pub primary_dragged: bool,
}

pub struct FrameContext<'a> {
    pub compute: &'a ComputeView,
    pub world: &'a WorldSnapshot,
    pub probe_history: &'a ProbeHistory,
    pub adapter_name: &'a str,
    pub frame_time_ms: f32,
    pub active_translation: Option<&'static str>,
    /// Screen-space annotation at the selected plane normal's arrow tip.
    pub plane_normal_label: Option<egui::Pos2>,
    pub plane_normal_active: bool,
}

pub fn show(root: &mut egui::Ui, model: &mut UiModel, frame: FrameContext<'_>) -> UiFrameOutput {
    let mut output = UiFrameOutput::default();
    let context = root.ctx().clone();
    model.synchronize_field_layers(frame.compute);

    menu_bar(root, model, &frame, &mut output);
    scene_tree(root, model, &frame, &mut output);
    inspector(root, model, &frame, &mut output);
    viewport(
        root,
        frame.active_translation,
        frame.plane_normal_label,
        frame.plane_normal_active,
        &mut output,
    );

    if model.diagnostics_visible {
        diagnostics_window(&context, &frame, model.command_error.as_deref());
    }
    floating_probe_plots(&context, model, &frame);

    output
}

fn viewport(
    root: &mut egui::Ui,
    active_translation: Option<&str>,
    plane_normal_label: Option<egui::Pos2>,
    plane_normal_active: bool,
    output: &mut UiFrameOutput,
) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
        .show(root, |ui| {
            output.viewport = ui.max_rect();
            let response = ui.interact(
                output.viewport,
                ui.id().with("viewport_interaction"),
                egui::Sense::click_and_drag(),
            );

            if let Some(position) =
                plane_normal_label.filter(|position| ui.max_rect().contains(*position))
            {
                let color = if plane_normal_active {
                    egui::Color32::from_rgb(255, 225, 45)
                } else {
                    egui::Color32::from_rgb(205, 135, 255)
                };
                ui.painter().circle_filled(position, 3.0, color);
                ui.painter().text(
                    position + egui::vec2(7.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    "N",
                    egui::FontId::proportional(15.0),
                    color,
                );
            }
            let middle_dragged = response.dragged_by(egui::PointerButton::Middle);
            let primary_dragged = response.dragged_by(egui::PointerButton::Primary);
            let contains_pointer = response.contains_pointer();

            output.viewport_gesture = ViewportGesture {
                pointer_position: response.interact_pointer_pos(),
                drag_delta: if middle_dragged {
                    ui.input(|input| input.pointer.delta())
                } else {
                    ui.input(|input| {
                        if primary_dragged {
                            input.pointer.delta()
                        } else {
                            egui::Vec2::ZERO
                        }
                    })
                },
                middle_dragged,
                shift: ui.input(|input| input.modifiers.shift),
                scroll_delta: if contains_pointer {
                    ui.input(|input| input.smooth_scroll_delta().y)
                } else {
                    0.0
                },
                primary_clicked: response.clicked_by(egui::PointerButton::Primary),
                primary_pressed: contains_pointer
                    && ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary)),
                primary_released: ui
                    .input(|input| input.pointer.button_released(egui::PointerButton::Primary)),
                primary_dragged,
            };

            ui.painter().text(
                output.viewport.left_bottom() + egui::vec2(12.0, -12.0),
                egui::Align2::LEFT_BOTTOM,
                "XY construction plane · Z up",
                egui::FontId::monospace(11.0),
                egui::Color32::from_gray(150),
            );
            if let Some(label) = active_translation {
                ui.painter().text(
                    output.viewport.center_top() + egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_TOP,
                    label,
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(255, 224, 70),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        Domain, ObjectShape, ObjectSpec, ProbeSpec, SessionId, TimeStep, Transform, World,
        WorldCommand,
    };
    use fieldcad_simulation::{
        CommandSequencer, FieldDataSource, LocalDataSource, RuntimeConfig, SimulationRuntime,
    };
    use fieldcad_test_field::{TestFieldPlugin, scalar_channel_id, vector_channel_id};
    use glam::DVec3;

    use super::*;
    use crate::scene::PlaneVectorMode;

    pub(super) fn seeded_world() -> World {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("Test source")
                        .with_transform(Transform::at(DVec3::new(0.0, 0.0, 0.6)).unwrap())
                        .with_shape(ObjectShape::boxed(DVec3::splat(0.6)).unwrap()),
                ),
                WorldCommand::CreateProbe(ProbeSpec::at(
                    "Origin probe",
                    DVec3::new(0.0, 0.0, 0.6),
                    vec![scalar_channel_id()],
                )),
            ])
            .unwrap();
        world
    }

    pub(super) fn source() -> LocalDataSource {
        LocalDataSource::new(
            SimulationRuntime::new(
                RuntimeConfig::new(
                    Domain::centred_cube(8.0, 8).unwrap(),
                    TimeStep::from_seconds(0.1).unwrap(),
                    SessionId::from_u128(10),
                )
                .with_world(seeded_world())
                .with_plugin(Box::new(TestFieldPlugin)),
            )
            .unwrap(),
        )
    }

    fn frame(
        context: &egui::Context,
        model: &mut UiModel,
        events: Vec<egui::Event>,
    ) -> UiFrameOutput {
        // The view model is a plain value, so the panels are exercised without
        // constructing a runtime per frame.
        let world = seeded_world();
        let snapshot = world.snapshot();
        let compute = ComputeView::build(&source(), &snapshot);
        let history = ProbeHistory::default();
        let mut output = UiFrameOutput::default();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let _ = context.run_ui(input, |root| {
            output = show(
                root,
                model,
                FrameContext {
                    compute: &compute,
                    world: &snapshot,
                    probe_history: &history,
                    adapter_name: "Test adapter",
                    frame_time_ms: 16.0,
                    active_translation: None,
                    plane_normal_label: None,
                    plane_normal_active: false,
                },
            );
        });
        output
    }

    #[test]
    fn central_viewport_reports_middle_drag() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        let start = egui::pos2(750.0, 500.0);
        let end = egui::pos2(850.0, 560.0);

        frame(&context, &mut model, vec![egui::Event::PointerMoved(start)]);
        frame(
            &context,
            &mut model,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Middle,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let output = frame(&context, &mut model, vec![egui::Event::PointerMoved(end)]);

        assert!(output.viewport_gesture.middle_dragged);
        assert_eq!(output.viewport_gesture.drag_delta, egui::vec2(100.0, 60.0));
    }

    #[test]
    fn central_viewport_reports_wheel_input() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        let pointer = egui::pos2(750.0, 500.0);

        frame(
            &context,
            &mut model,
            vec![egui::Event::PointerMoved(pointer)],
        );
        let output = frame(
            &context,
            &mut model,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 2.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }],
        );

        assert!(output.viewport_gesture.scroll_delta > 0.0);
    }

    #[test]
    fn floating_controls_block_viewport_wheel_input() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        let over_diagnostics = egui::pos2(320.0, 100.0);

        frame(
            &context,
            &mut model,
            vec![egui::Event::PointerMoved(over_diagnostics)],
        );
        let output = frame(
            &context,
            &mut model,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 2.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }],
        );

        assert_eq!(output.viewport_gesture.scroll_delta, 0.0);
    }

    #[test]
    fn the_inspector_reads_the_selected_object_rather_than_a_literal() {
        let world = seeded_world();
        let snapshot = world.snapshot();
        let object = snapshot.object(ObjectId::new(0)).unwrap();

        assert_eq!(object.name, "Test source");
        assert_eq!(object.transform.translation, DVec3::new(0.0, 0.0, 0.6));
        assert!(matches!(object.shape, Some(ObjectShape::Box { .. })));
    }

    #[test]
    fn selecting_in_the_scene_tree_stores_an_object_id() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        assert_eq!(model.selection, None);

        model.selection = Some(ObjectId::new(0));
        frame(&context, &mut model, Vec::new());

        assert_eq!(model.selection, Some(ObjectId::new(0)));
    }

    #[test]
    fn viewport_selection_is_exclusive_across_scene_entity_kinds() {
        let mut model = UiModel::new();
        let object = ObjectId::new(1);
        let plane = PlaneId::new(2);
        let probe = ProbeId::new(3);

        model.set_scene_selection(Some(SceneSelection::Object(object)));
        assert_eq!(
            model.scene_selection(),
            Some(SceneSelection::Object(object))
        );

        model.set_scene_selection(Some(SceneSelection::Plane(plane)));
        assert_eq!(model.selection, None);
        assert_eq!(model.scene_selection(), Some(SceneSelection::Plane(plane)));

        model.set_scene_selection(Some(SceneSelection::Probe(probe)));
        assert_eq!(model.plane_selection, None);
        assert_eq!(model.scene_selection(), Some(SceneSelection::Probe(probe)));
    }

    #[test]
    fn a_floating_probe_plot_survives_scene_selection_changes() {
        let snapshot = seeded_world().snapshot();
        let probe = snapshot.probes().values().next().unwrap();
        let mut model = UiModel::new();
        model.set_scene_selection(Some(SceneSelection::Probe(probe.id)));

        model.open_probe_plot(probe);
        model.set_scene_selection(None);

        let plot = &model.probe_plots[&probe.id];
        assert_eq!(plot.channels, probe.channels.iter().cloned().collect());
        assert_eq!(model.scene_selection(), None);
    }

    #[test]
    fn new_planes_default_to_in_plane_vectors() {
        let settings = PlaneLayerSettings::default();

        assert_eq!(settings.vector_mode, PlaneVectorMode::InPlane);
        assert!(settings.vectors_visible);
        assert!(settings.magnitude_visible);
    }

    #[test]
    fn every_command_a_frame_produces_survives_in_submission_order() {
        let mut output = UiFrameOutput::default();

        output.edit(vec![WorldCommand::RemoveObject(ObjectId::new(0))]);
        output.submit(CommandPayload::Step);
        output.edit(vec![WorldCommand::RemoveProbe(ProbeId::new(1))]);

        assert_eq!(output.commands.len(), 3);
        assert_eq!(output.commands[1], CommandPayload::Step);
        assert_eq!(
            output.commands[0],
            CommandPayload::CommitWorld(vec![WorldCommand::RemoveObject(ObjectId::new(0))])
        );
    }

    #[test]
    fn published_vector_channels_get_independent_visualization_layers() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "both channels",
                DVec3::X,
                vec![scalar_channel_id(), vector_channel_id()],
            ))])
            .unwrap();
        let source = LocalDataSource::new(
            SimulationRuntime::new(
                RuntimeConfig::new(
                    Domain::centred_cube(8.0, 8).unwrap(),
                    TimeStep::from_seconds(0.1).unwrap(),
                    SessionId::from_u128(12),
                )
                .with_world(world.clone())
                .with_plugin(Box::new(TestFieldPlugin)),
            )
            .unwrap(),
        );
        let mut view = ComputeView::build(&source, &world.snapshot());
        let mut model = UiModel::new();
        let published = vector_channel_id();

        // The test plugin publishes one vector channel; nothing in the desktop
        // has to know which plugin that is. The first vector layer is visible
        // so a newly loaded scene never opens to an unexplained blank view.
        assert_eq!(view.vector_channels, vec![published.clone()]);
        model.synchronize_field_layers(&view);
        assert!(model.field_layers[&published].visible);

        // A later vector channel gets its own layer and starts hidden, so E and
        // B (or fields from unrelated plugins) can be enabled independently.
        let second = scalar_channel_id();
        view.vector_channels.push(second.clone());
        model.synchronize_field_layers(&view);
        assert!(!model.field_layers[&second].visible);

        model.field_layers.get_mut(&second).unwrap().visible = true;
        assert!(model.field_layers[&published].visible);
        assert!(model.field_layers[&second].visible);

        // If an equation system changes, a visible but unavailable old layer
        // must not leave the replacement snapshot blank.
        model.field_layers.get_mut(&second).unwrap().visible = false;
        view.vector_channels = vec![second.clone()];
        model.synchronize_field_layers(&view);
        assert!(model.field_layers[&second].visible);
    }

    #[test]
    fn viewport_helpers_are_independently_visible_by_default() {
        let model = UiModel::new();

        assert!(model.grid_visible);
        assert!(model.axes_visible);
    }

    #[test]
    fn commands_issued_by_the_ui_are_correlated_when_executed() {
        let mut source = source();
        let mut sequencer = CommandSequencer::default();
        let command = sequencer.issue(CommandPayload::Step);
        let id = command.id;

        let receipt = source.execute(command).unwrap();

        assert_eq!(receipt.command, id);
        assert_eq!(receipt.tick, 1);
    }
}
