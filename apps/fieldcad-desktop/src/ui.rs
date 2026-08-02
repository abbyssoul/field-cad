//! egui panels.
//!
//! Every panel reads a [`ComputeView`] rather than a `&dyn FieldDataSource`. That
//! keeps the widgets testable without standing up a runtime, and stops the UI
//! from reaching into snapshot internals.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, DiagnosticSeverity, FieldValue, ObjectId, ObjectShape, ObjectSpec, PlaneId, ProbeId,
    ProbePosition, ProbeSpec, SampleValidity, SimulationMode, SlicePlane, SlicePlaneSpec,
    SnapshotFreshness, TimeStep, Transform, UndefinedReason, WorldCommand, WorldObject,
    WorldRevision, WorldSnapshot,
};
use fieldcad_electrostatics::{
    charge_component_id, charge_properties, charge_property_id, electric_field_channel_id,
    electric_potential_channel_id,
};
use fieldcad_simulation::{
    CommandPayload, DataSourceStatus, FieldDataSource, PlaybackSpeed, ProbeHistory,
};
use glam::{DVec2, DVec3};

use crate::{
    camera::AxisView,
    scene::{FieldLayerSettings, PlaneLayerSettings, PlaneVectorMode, SceneSelection},
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
    pub field_layers: FieldLayerSettings,
    pub plane_layers: BTreeMap<PlaneId, PlaneLayerSettings>,
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
            field_layers: FieldLayerSettings::default(),
            plane_layers: BTreeMap::new(),
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

/// A per-frame, read-only summary of what the data source is reporting.
///
/// Built once per frame so that panels take a plain value. Nothing here depends
/// on whether compute is local or remote.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputeView {
    pub description: String,
    pub status: DataSourceStatus,
    pub mode: SimulationMode,
    pub tick: u64,
    pub time_seconds: f64,
    pub time_step_seconds: f64,
    pub playback_speed: f64,
    pub pending_commands: usize,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: Option<u64>,
    pub freshness: Option<SnapshotFreshness>,
    pub total_samples: usize,
    pub domain_summary: String,
    pub probe_readings: Vec<ProbeRow>,
    pub channel_names: BTreeMap<ChannelId, String>,
    pub diagnostics: Vec<String>,
    pub has_errors: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchState {
    Connecting,
    Solving,
    Running,
    Paused,
    Disconnected,
    Invalid,
}

impl WorkbenchState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Solving => "Solving",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Disconnected => "Disconnected",
            Self::Invalid => "Invalid",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Running => egui::Color32::from_rgb(90, 205, 125),
            Self::Paused => egui::Color32::from_rgb(120, 175, 235),
            Self::Solving | Self::Connecting => egui::Color32::from_rgb(235, 190, 75),
            Self::Disconnected | Self::Invalid => egui::Color32::from_rgb(235, 105, 90),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeRow {
    pub probe_name: String,
    pub channel_name: String,
    pub value: String,
    pub validity: SampleValidity,
}

impl ComputeView {
    pub fn build(source: &dyn FieldDataSource, world: &WorldSnapshot) -> Self {
        let simulation = source.simulation_status();
        let snapshot = source.latest_snapshot();

        let mut probe_readings = Vec::new();
        let mut diagnostics = Vec::new();
        let mut has_errors = false;
        let mut channel_names = BTreeMap::new();
        let mut total_samples = 0;
        let mut domain_summary = "No data".to_owned();

        if let Some(snapshot) = &snapshot {
            total_samples = snapshot.total_samples();
            let cells = snapshot.domain.resolution().cells();
            domain_summary = format!(
                "{}×{}×{} = {} cells, {}",
                cells.x,
                cells.y,
                cells.z,
                snapshot.domain.resolution().cell_count(),
                snapshot.domain.precision().label(),
            );
            diagnostics = snapshot
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    has_errors |= diagnostic.severity == DiagnosticSeverity::Error;
                    format!("[{:?}] {}", diagnostic.severity, diagnostic.message)
                })
                .collect();

            for (channel_id, channel) in &snapshot.channels {
                channel_names.insert(channel_id.clone(), channel.schema.display_name.clone());
                for probe in world.probes().values() {
                    if !probe.channels.contains(channel_id) {
                        continue;
                    }
                    let Some(sample) = channel.probe_sample(probe.id) else {
                        continue;
                    };
                    probe_readings.push(ProbeRow {
                        probe_name: probe.name.clone(),
                        channel_name: channel.schema.display_name.clone(),
                        value: format_value(sample.value),
                        validity: sample.validity,
                    });
                }
            }
        }

        Self {
            description: source.description().to_owned(),
            status: source.status(),
            mode: simulation.mode(),
            tick: simulation.tick(),
            time_seconds: simulation.time_seconds(),
            time_step_seconds: simulation.time_step().seconds(),
            playback_speed: source.playback_speed().multiplier(),
            pending_commands: source.pending_command_count(),
            world_revision: simulation.world_revision,
            snapshot_sequence: snapshot.as_ref().map(|snapshot| snapshot.identity.sequence),
            freshness: snapshot
                .as_ref()
                .map(|snapshot| snapshot.freshness_against(simulation.world_revision)),
            total_samples,
            domain_summary,
            probe_readings,
            channel_names,
            diagnostics,
            has_errors,
        }
    }

    /// Transport controls are only meaningful against a connected source.
    pub fn accepts_commands(&self) -> bool {
        self.status == DataSourceStatus::Ready
    }

    pub fn workbench_state(&self) -> WorkbenchState {
        if self.has_errors
            || matches!(self.status, DataSourceStatus::Failed(_))
            || self.freshness == Some(SnapshotFreshness::Future)
        {
            return WorkbenchState::Invalid;
        }
        match self.status {
            DataSourceStatus::Connecting => WorkbenchState::Connecting,
            DataSourceStatus::Disconnected => WorkbenchState::Disconnected,
            DataSourceStatus::Failed(_) => WorkbenchState::Invalid,
            DataSourceStatus::Ready
                if self.snapshot_sequence.is_none()
                    || self.freshness == Some(SnapshotFreshness::Stale) =>
            {
                WorkbenchState::Solving
            }
            DataSourceStatus::Ready => match self.mode {
                SimulationMode::Running => WorkbenchState::Running,
                SimulationMode::Paused => WorkbenchState::Paused,
            },
        }
    }
}

fn format_value(value: FieldValue) -> String {
    match value {
        FieldValue::Scalar(value) => format!("{:.6} {}", value.si_value(), value.dimension()),
        FieldValue::Vector(value) => {
            let vector = value.si_value();
            format!(
                "({:.4}, {:.4}, {:.4}) {}",
                vector.x,
                vector.y,
                vector.z,
                value.dimension()
            )
        }
    }
}

/// A sample that is not defined must never be shown as though it were measured.
fn validity_note(validity: SampleValidity) -> Option<&'static str> {
    match validity {
        SampleValidity::Exact => None,
        SampleValidity::Interpolated(_) => Some("interpolated"),
        SampleValidity::Undefined(UndefinedReason::InsideSourceRadius) => {
            Some("undefined — inside source radius")
        }
        SampleValidity::Undefined(UndefinedReason::OutsideDomain) => {
            Some("undefined — outside domain")
        }
        SampleValidity::Undefined(UndefinedReason::NotConverged) => {
            Some("undefined — not converged")
        }
        SampleValidity::Undefined(UndefinedReason::NumericalOverflow) => {
            Some("undefined — numerical overflow")
        }
    }
}

#[derive(Debug)]
pub struct UiFrameOutput {
    pub viewport: egui::Rect,
    pub viewport_gesture: ViewportGesture,
    pub camera_action: Option<CameraAction>,
    pub command: Option<CommandPayload>,
}

impl Default for UiFrameOutput {
    fn default() -> Self {
        Self {
            viewport: egui::Rect::NOTHING,
            viewport_gesture: ViewportGesture::default(),
            camera_action: None,
            command: None,
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
}

pub fn show(root: &mut egui::Ui, model: &mut UiModel, frame: FrameContext<'_>) -> UiFrameOutput {
    let mut output = UiFrameOutput::default();
    let context = root.ctx().clone();

    menu_bar(root, model, &frame, &mut output);
    scene_tree(root, model, &frame, &mut output);
    inspector(root, model, &frame, &mut output);
    viewport(root, frame.active_translation, &mut output);

    if model.diagnostics_visible {
        diagnostics_window(&context, &frame);
    }

    output
}

fn menu_bar(
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
                output.command = Some(CommandPayload::Play);
            }
            if ui
                .add_enabled(live && !paused, egui::Button::new("⏸ Pause"))
                .clicked()
            {
                output.command = Some(CommandPayload::Pause);
            }
            if ui
                .add_enabled(live && paused, egui::Button::new("Step"))
                .clicked()
            {
                output.command = Some(CommandPayload::Step);
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
                output.command = Some(CommandPayload::SetTimeStep(time_step));
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
                output.command = Some(CommandPayload::SetPlaybackSpeed(speed));
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
            ui.separator();
            ui.monospace(format!("t = {}", format_simulation_time(frame.compute.time_seconds)));
        });
    });
}

fn parse_playback_speed(text: &str) -> Option<f64> {
    text.trim().trim_end_matches(['x', '×']).trim().parse().ok()
}

fn state_badge(ui: &mut egui::Ui, state: WorkbenchState) {
    ui.colored_label(state.color(), format!("● {}", state.label()));
}

fn format_simulation_time(seconds: f64) -> String {
    if seconds == 0.0 {
        "0 s".to_owned()
    } else {
        format_time_step(seconds)
    }
}

fn time_step_drag_speed(seconds: f64) -> f64 {
    (seconds.abs() * 0.01).max(f64::from_bits(1))
}

fn format_time_step(seconds: f64) -> String {
    let (factor, suffix) = if seconds >= 1.0 {
        (1.0, "s")
    } else if seconds >= 1.0e-3 {
        (1.0e-3, "ms")
    } else if seconds >= 1.0e-6 {
        (1.0e-6, "µs")
    } else if seconds >= 1.0e-9 {
        (1.0e-9, "ns")
    } else if seconds >= 1.0e-12 {
        (1.0e-12, "ps")
    } else {
        (1.0e-15, "fs")
    };
    format!("{} {suffix}", seconds / factor)
}

fn scene_tree(
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
                    output.command = Some(new_charge_command(
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
                    output.command = Some(new_charge_command(
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
                    output.command = Some(new_charge_command(
                        frame.world,
                        1.0e-9,
                        ChargeObjectKind::Sphere,
                    ));
                }
                if ui.button("+ Probe").clicked() {
                    output.command = Some(CommandPayload::CommitWorld(vec![
                        WorldCommand::CreateProbe(ProbeSpec::at(
                            format!("Probe {}", frame.world.probes().len() + 1),
                            DVec3::new(1.0, 0.0, 0.6),
                            vec![electric_field_channel_id(), electric_potential_channel_id()],
                        )),
                    ]));
                }
                if ui.button("+ Plane").clicked() {
                    output.command = Some(CommandPayload::CommitWorld(vec![
                        WorldCommand::CreatePlane(
                            SlicePlaneSpec::new(
                                format!("XY plane {}", frame.world.planes().len() + 1),
                                DVec3::ZERO,
                                DVec3::Z,
                            )
                            .and_then(|plane| plane.with_half_extent(DVec2::splat(4.0)))
                            .expect("static plane parameters are valid"),
                        ),
                    ]));
                }
            });
            ui.add_space(6.0);

            if frame.world.objects().is_empty() {
                ui.weak("No objects.");
            }
            for object in frame.world.objects().values() {
                ui.horizontal(|ui| {
                    if visibility_button(ui, object.visible).clicked() {
                        output.command = Some(CommandPayload::CommitWorld(vec![
                            WorldCommand::SetObjectVisible {
                                object: object.id,
                                visible: !object.visible,
                            },
                        ]));
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
                        output.command = Some(CommandPayload::CommitWorld(vec![
                            WorldCommand::RemoveObject(object.id),
                        ]));
                    }
                });
            }

            if !frame.world.probes().is_empty() {
                ui.add_space(8.0);
                ui.label("Probes");
                for probe in frame.world.probes().values() {
                    ui.horizontal(|ui| {
                        if visibility_button(ui, probe.visible).clicked() {
                            output.command = Some(CommandPayload::CommitWorld(vec![
                                WorldCommand::SetProbeVisible {
                                    probe: probe.id,
                                    visible: !probe.visible,
                                },
                            ]));
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
                            output.command = Some(CommandPayload::CommitWorld(vec![
                                WorldCommand::RemoveProbe(probe.id),
                            ]));
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
                            output.command = Some(CommandPayload::CommitWorld(vec![
                                WorldCommand::SetPlaneVisible {
                                    plane: plane.id,
                                    visible: !plane.visible,
                                },
                            ]));
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
                            output.command = Some(CommandPayload::CommitWorld(vec![
                                WorldCommand::RemovePlane(plane.id),
                            ]));
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

fn inspector(
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
                    let settings = model.plane_layers.entry(plane.id).or_default();
                    plane_properties(ui, plane, settings, output);
                } else if let Some(probe) =
                    model.probe_selection.and_then(|id| frame.world.probe(id))
                {
                    probe_properties(
                        ui,
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
                ui.checkbox(
                    &mut model.field_layers.domain_vectors,
                    "Sparse 3D vector glyphs",
                );

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
                            output.command =
                                Some(CommandPayload::CommitWorld(vec![WorldCommand::SetShape {
                                    object: object.id,
                                    shape: Some(shape),
                                }]));
                        }
                    });
                }
                Some(ObjectShape::Sphere { mut radius }) => {
                    ui.horizontal(|ui| {
                        ui.label("Uniform sphere");
                        if radius_editor(ui, &mut radius, 1.0e-4)
                            && let Ok(shape) = ObjectShape::sphere(radius)
                        {
                            output.command =
                                Some(CommandPayload::CommitWorld(vec![WorldCommand::SetShape {
                                    object: object.id,
                                    shape: Some(shape),
                                }]));
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
        output.command = Some(CommandPayload::CommitWorld(vec![
            WorldCommand::SetTransform {
                object: object.id,
                transform,
            },
        ]));
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
                    output.command = Some(CommandPayload::CommitWorld(vec![
                        WorldCommand::AttachComponent {
                            object: object.id,
                            component: charge_component_id(),
                            properties,
                        },
                    ]));
                }
            });
        }
    }

    if ui.button("Focus selection  [F]").clicked() {
        output.camera_action = Some(CameraAction::FocusSelection);
    }
    if ui.button("Remove object").clicked() {
        output.command = Some(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveObject(object.id),
        ]));
    }
}

fn plane_properties(
    ui: &mut egui::Ui,
    plane: &SlicePlane,
    settings: &mut PlaneLayerSettings,
    output: &mut UiFrameOutput,
) {
    ui.label(&plane.name);
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
        output.command = Some(CommandPayload::CommitWorld(vec![WorldCommand::SetPlane {
            plane: plane.id,
            spec,
        }]));
    }

    ui.add_space(8.0);
    ui.label("Field display");
    ui.checkbox(&mut settings.magnitude_visible, "Magnitude colour");
    density_editor(
        ui,
        "Magnitude density",
        &mut settings.magnitude_density,
        settings.magnitude_visible,
    );
    ui.checkbox(&mut settings.vectors_visible, "Vector arrows");
    density_editor(
        ui,
        "Arrow density",
        &mut settings.vector_density,
        settings.vectors_visible,
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
                output.command = Some(CommandPayload::CommitWorld(vec![WorldCommand::SetPlane {
                    plane: plane.id,
                    spec: spec.with_visibility(plane.visible),
                }]));
            }
        }
    });
    if ui.button("Duplicate plane").clicked() {
        output.command = Some(CommandPayload::CommitWorld(vec![
            WorldCommand::CreatePlane(
                SlicePlaneSpec::from_plane(plane).with_name(format!("{} copy", plane.name)),
            ),
        ]));
    }
    if ui.button("Remove plane").clicked() {
        output.command = Some(CommandPayload::CommitWorld(vec![
            WorldCommand::RemovePlane(plane.id),
        ]));
    }
}

fn probe_properties(
    ui: &mut egui::Ui,
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
                output.command = Some(CommandPayload::CommitWorld(vec![
                    WorldCommand::SetProbePosition {
                        probe: probe.id,
                        position: ProbePosition::World(position),
                    },
                ]));
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
                output.command = Some(CommandPayload::CommitWorld(vec![
                    WorldCommand::SetProbePosition {
                        probe: probe.id,
                        position: ProbePosition::Attached { object, offset },
                    },
                ]));
            }

            if ui.button("Detach at current position").clicked()
                && let Ok(position) = world.resolve_probe_position(probe)
            {
                output.command = Some(CommandPayload::CommitWorld(vec![
                    WorldCommand::SetProbePosition {
                        probe: probe.id,
                        position: ProbePosition::World(position),
                    },
                ]));
            }
            if let Ok(position) = world.resolve_probe_position(probe) {
                probe_attachment_picker(ui, probe.id, position, Some(object), world, output);
            }
        }
    }

    ui.add_space(10.0);
    probe_history_plots(ui, probe, compute, history);

    if ui.button("Remove probe").clicked() {
        output.command = Some(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveProbe(probe.id),
        ]));
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
                        output.command = Some(CommandPayload::CommitWorld(vec![
                            WorldCommand::SetProbePosition {
                                probe,
                                position: ProbePosition::Attached {
                                    object: object.id,
                                    offset,
                                },
                            },
                        ]));
                    }
                }
            });
    });
}

fn attachment_offset(world_position: DVec3, object: &WorldObject) -> DVec3 {
    object.transform.rotation.inverse() * (world_position - object.transform.translation)
}

fn probe_history_plots(
    ui: &mut egui::Ui,
    probe: &fieldcad_core::Probe,
    compute: &ComputeView,
    history: &ProbeHistory,
) {
    ui.collapsing("History", |ui| {
        ui.small(format!(
            "Bounded to {} samples per channel",
            history.capacity()
        ));
        for channel in &probe.channels {
            let title = compute
                .channel_names
                .get(channel)
                .map_or_else(|| channel.to_string(), Clone::clone);
            ui.label(title);
            let readings: Vec<_> = history.readings(probe.id, channel).copied().collect();
            if readings.is_empty() {
                ui.weak("No samples yet");
            } else {
                paint_probe_plot(ui, &readings);
            }
        }
    });
}

#[derive(Clone, Copy)]
struct ProbeTrace {
    label: &'static str,
    color: egui::Color32,
    component: fn(FieldValue) -> Option<f64>,
}

fn paint_probe_plot(ui: &mut egui::Ui, readings: &[fieldcad_simulation::ProbeReading]) {
    let vector = readings
        .iter()
        .map(|reading| match reading.value {
            FieldValue::Vector(_) => true,
            FieldValue::Scalar(_) => false,
        })
        .next()
        .unwrap_or(false);
    let traces: &[ProbeTrace] = if vector {
        &[
            ProbeTrace {
                label: "x",
                color: egui::Color32::from_rgb(235, 90, 90),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().x),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "y",
                color: egui::Color32::from_rgb(95, 210, 120),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().y),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "z",
                color: egui::Color32::from_rgb(100, 155, 245),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().z),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "|v|",
                color: egui::Color32::from_rgb(245, 205, 75),
                component: |value| Some(value.magnitude()),
            },
        ]
    } else {
        &[ProbeTrace {
            label: "value",
            color: egui::Color32::from_rgb(245, 205, 75),
            component: |value| match value {
                FieldValue::Scalar(value) => Some(value.si_value()),
                FieldValue::Vector(_) => None,
            },
        }]
    };

    let values: Vec<_> = traces
        .iter()
        .flat_map(|trace| {
            readings.iter().filter_map(move |reading| {
                is_plot_valid(reading.validity).then(|| (trace.component)(reading.value))?
            })
        })
        .collect();
    let Some((mut y_min, mut y_max)) = value_bounds(values.iter().copied()) else {
        ui.colored_label(
            egui::Color32::from_rgb(230, 150, 60),
            "Samples are currently undefined",
        );
        return;
    };
    if y_min == y_max {
        let padding = y_min.abs().max(1.0) * 0.05;
        y_min -= padding;
        y_max += padding;
    }
    let x_min = readings.first().map_or(0.0, |reading| reading.time_seconds);
    let x_max = readings
        .last()
        .map_or(x_min, |reading| reading.time_seconds);

    let desired = egui::vec2(ui.available_width().max(120.0), 150.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let plot = rect.shrink2(egui::vec2(8.0, 20.0));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(45));
    painter.rect_stroke(
        plot,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(75)),
        egui::StrokeKind::Inside,
    );

    if y_min <= 0.0 && y_max >= 0.0 {
        let y = remap(0.0, y_min, y_max, plot.bottom(), plot.top());
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(65)),
        );
    }

    for trace in traces {
        let points: Vec<_> = readings
            .iter()
            .filter(|reading| is_plot_valid(reading.validity))
            .filter_map(|reading| {
                let value = (trace.component)(reading.value)?;
                let x = if x_min == x_max {
                    plot.center().x
                } else {
                    remap(
                        reading.time_seconds,
                        x_min,
                        x_max,
                        plot.left(),
                        plot.right(),
                    )
                };
                let y = remap(value, y_min, y_max, plot.bottom(), plot.top());
                Some(egui::pos2(x, y))
            })
            .collect();
        if points.len() == 1 {
            painter.circle_filled(points[0], 2.0, trace.color);
        } else if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.4, trace.color),
            ));
        }
    }

    let mut legend_x = plot.left();
    for trace in traces {
        painter.text(
            egui::pos2(legend_x, rect.top() + 3.0),
            egui::Align2::LEFT_TOP,
            trace.label,
            egui::FontId::monospace(10.0),
            trace.color,
        );
        legend_x += 30.0;
    }
    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -3.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{x_min:.3e} s"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.right_bottom() + egui::vec2(-4.0, -3.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{x_max:.3e} s"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
}

fn is_plot_valid(validity: SampleValidity) -> bool {
    !matches!(validity, SampleValidity::Undefined(_))
}

fn value_bounds(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    values
        .filter(|value| value.is_finite())
        .fold(None, |bounds, value| {
            Some(match bounds {
                Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                None => (value, value),
            })
        })
}

fn remap(value: f64, from_min: f64, from_max: f64, to_min: f32, to_max: f32) -> f32 {
    let fraction = ((value - from_min) / (from_max - from_min)) as f32;
    to_min + fraction * (to_max - to_min)
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

fn viewport(root: &mut egui::Ui, active_translation: Option<&str>, output: &mut UiFrameOutput) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
        .show(root, |ui| {
            output.viewport = ui.max_rect();
            let response = ui.interact(
                output.viewport,
                ui.id().with("viewport_interaction"),
                egui::Sense::click_and_drag(),
            );
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

fn diagnostics_window(context: &egui::Context, frame: &FrameContext<'_>) {
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
        });
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        Domain, ObjectSpec, ProbeSpec, SessionId, TimeStep, Transform, World, WorldCommand,
    };
    use fieldcad_simulation::{
        CommandSequencer, LocalDataSource, RuntimeConfig, SimulationRuntime,
    };
    use fieldcad_test_field::{TestFieldPlugin, scalar_channel_id};
    use glam::{DQuat, DVec3};

    use super::*;

    fn seeded_world() -> World {
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

    fn source() -> LocalDataSource {
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
    fn the_compute_view_reports_provenance_from_the_source() {
        let world = seeded_world();
        let view = ComputeView::build(&source(), &world.snapshot());

        assert_eq!(view.mode, SimulationMode::Paused);
        assert_eq!(view.freshness, Some(SnapshotFreshness::Current));
        assert!(view.domain_summary.contains("512"));
        assert!(view.domain_summary.contains("f64"));
        assert_eq!(view.probe_readings.len(), 1);
        assert_eq!(view.probe_readings[0].probe_name, "Origin probe");
    }

    #[test]
    fn probe_values_are_shown_with_their_units() {
        let world = seeded_world();
        let view = ComputeView::build(&source(), &world.snapshot());

        // The probe sits at z = 0.6, so the linear scalar reads 3 * 0.6 m.
        assert!(view.probe_readings[0].value.starts_with("1.800000"));
        assert!(view.probe_readings[0].value.ends_with(" m"));
    }

    #[test]
    fn undefined_samples_are_labelled_rather_than_printed_as_numbers() {
        assert_eq!(validity_note(SampleValidity::Exact), None);
        assert_eq!(
            validity_note(SampleValidity::Undefined(
                UndefinedReason::InsideSourceRadius
            )),
            Some("undefined — inside source radius")
        );
    }

    #[test]
    fn a_disconnected_source_cannot_be_commanded_from_the_ui() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot());
        assert!(view.accepts_commands());

        view.status = DataSourceStatus::Disconnected;

        assert!(!view.accepts_commands());
        assert_eq!(view.status.label(), "Disconnected");
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
    fn new_planes_default_to_in_plane_vectors() {
        let settings = PlaneLayerSettings::default();

        assert_eq!(settings.vector_mode, PlaneVectorMode::InPlane);
        assert!(settings.vectors_visible);
        assert!(settings.magnitude_visible);
    }

    #[test]
    fn viewport_helpers_are_independently_visible_by_default() {
        let model = UiModel::new();

        assert!(model.grid_visible);
        assert!(model.axes_visible);
    }

    #[test]
    fn time_step_control_formats_values_at_a_readable_si_scale() {
        assert_eq!(format_time_step(432.0), "432 s");
        assert_eq!(format_time_step(1.23e-9), "1.23 ns");
        assert_eq!(format_time_step(4.43e-3), "4.43 ms");
        assert_eq!(format_time_step(7.3213e-7), "732.13 ns");
    }

    #[test]
    fn time_step_drag_speed_tracks_the_current_order_of_magnitude() {
        assert_eq!(time_step_drag_speed(1.0), 0.01);
        assert!((time_step_drag_speed(1.0e-9) - 1.0e-11).abs() < 1.0e-26);
        assert!(time_step_drag_speed(f64::from_bits(1)) > 0.0);
    }

    #[test]
    fn playback_speed_control_accepts_plain_and_multiplier_notation() {
        assert_eq!(parse_playback_speed("2"), Some(2.0));
        assert_eq!(parse_playback_speed("0.25×"), Some(0.25));
        assert_eq!(parse_playback_speed("1e2x"), Some(100.0));
        assert_eq!(parse_playback_speed("fast"), None);
    }

    #[test]
    fn workbench_state_distinguishes_paused_solving_stale_and_invalid() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot());
        assert_eq!(view.workbench_state(), WorkbenchState::Paused);

        view.freshness = Some(SnapshotFreshness::Stale);
        assert_eq!(view.workbench_state(), WorkbenchState::Solving);

        view.has_errors = true;
        assert_eq!(view.workbench_state(), WorkbenchState::Invalid);

        view.has_errors = false;
        view.status = DataSourceStatus::Disconnected;
        assert_eq!(view.workbench_state(), WorkbenchState::Disconnected);
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
    fn probe_plot_bounds_include_negative_components() {
        assert_eq!(
            value_bounds([-4.0, 2.0, 9.0, -1.0].into_iter()),
            Some((-4.0, 9.0))
        );
        assert_eq!(value_bounds([f64::NAN].into_iter()), None);
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
