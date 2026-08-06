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
mod help;
mod panels;
mod plot;
mod viewcontrols;

pub use compute::ComputeView;

use help::help_window;
use panels::{
    diagnostics_window, field_brush_dialog, inspector, mcp_window, menu_bar, queue_window,
    scene_tree,
};
use plot::floating_probe_plots;
use viewcontrols::view_controls;

use std::collections::{BTreeMap, BTreeSet};

use fieldcad_core::{
    BoundaryConditions, BoxId, ChannelId, Domain, DomainBounds, ObjectId, PlaneId, Precision,
    ProbeId, Resolution, SphereId, WorldCommand, WorldSnapshot,
};
use fieldcad_simulation::{CommandPayload, ProbeHistory};
use glam::{DVec3, UVec3};

use crate::{
    camera::{AxisView, Projection},
    mcp::{McpAction, McpSession},
    scene::{
        BoxLayerSettings, FieldLayerSettings, GizmoDisplay, PlaneLayerSettings, SceneSelection,
        SphereLayerSettings, VectorDisplay,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraAction {
    Axis(AxisView),
    FocusSelection,
    Reset,
    SetProjection(Projection),
}

/// What a primary-button gesture in the viewport means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewportTool {
    /// Pick an inspector subject without ever starting a transform gesture.
    Select,
    /// Pick and manipulate the selected scene item. This preserves the
    /// workbench's original viewport behaviour.
    #[default]
    Transform,
    /// Reserved for an authoritative numerical-field disturbance command.
    FieldBrush,
}

impl ViewportTool {
    pub const ALL: [Self; 3] = [Self::Select, Self::Transform, Self::FieldBrush];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Transform => "Transform",
            Self::FieldBrush => "Field brush",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Select => "Select objects without moving or rotating them",
            Self::Transform => "Move or rotate selected objects in the viewport",
            Self::FieldBrush => "Configure a numerical-field disturbance brush",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldBrushDraft {
    pub radius_metres: f64,
    pub strength: f64,
    pub channel: Option<ChannelId>,
}

impl Default for FieldBrushDraft {
    fn default() -> Self {
        Self {
            radius_metres: 0.5,
            strength: 1.0,
            channel: None,
        }
    }
}

/// What the 3D view draws, as distinct from what the world contains.
///
/// These are presentation filters and nothing else: hiding probes does not stop
/// them recording, and hiding objects does not stop them sourcing a field. They
/// live with the view controls in the viewport because that is where their
/// effect is visible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewOptions {
    pub grid: bool,
    pub axes: bool,
    pub objects: bool,
    /// Master visibility for scene helpers that do not participate in the
    /// simulation: probes and field-sampling regions.
    pub auxiliary_objects: bool,
    /// The region the active solver discretizes. This is not an authored scene
    /// object, so it remains independent of auxiliary-object visibility.
    pub compute_bounds: bool,
    pub gizmo_display: GizmoDisplay,
    pub probes: bool,
    pub planes: bool,
    pub boxes: bool,
    pub spheres: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            grid: true,
            axes: true,
            objects: true,
            auxiliary_objects: true,
            compute_bounds: false,
            gizmo_display: GizmoDisplay::default(),
            probes: true,
            planes: true,
            boxes: true,
            spheres: true,
        }
    }
}

/// One display toggle: its label, its hover text, and how to reach the flag.
///
/// Declared as data so the view panel iterates rather than repeating five nearly
/// identical checkbox calls, and so a test can assert every toggle is offered
/// without naming them a second time.
pub type ViewToggle = (
    &'static str,
    &'static str,
    fn(&mut ViewOptions) -> &mut bool,
);

impl ViewOptions {
    pub const PRIMARY_ENTRIES: [ViewToggle; 4] = [
        ("Grid", "Construction grid on the XY plane", |view| {
            &mut view.grid
        }),
        ("Origin axes", "World X, Y, and Z at the origin", |view| {
            &mut view.axes
        }),
        (
            "Objects",
            "Simulated bodies. Hiding them does not remove them from the simulation.",
            |view| &mut view.objects,
        ),
        (
            "Auxiliary objects",
            "Probes and field-sampling regions. Hiding them does not affect simulation or recording.",
            |view| &mut view.auxiliary_objects,
        ),
    ];

    pub const AUXILIARY_ENTRIES: [ViewToggle; 4] = [
        (
            "Probes",
            "Point recorders. Hidden probes keep recording.",
            |view| &mut view.probes,
        ),
        (
            "Slice planes",
            "Field sampling planes and their drawn values",
            |view| &mut view.planes,
        ),
        (
            "Field boxes",
            "Oriented volumes sampling and drawing the field as arrows",
            |view| &mut view.boxes,
        ),
        (
            "Field spheres",
            "Spherical volumes sampling and drawing the field as arrows",
            |view| &mut view.spheres,
        ),
    ];
}

#[derive(Debug, Default)]
pub struct UiModel {
    pub view: ViewOptions,
    pub viewport_tool: ViewportTool,
    pub field_brush_dialog_open: bool,
    pub field_brush: FieldBrushDraft,
    pub diagnostics_visible: bool,
    /// The getting-started window. Open on a first run, because the composition
    /// model is the part of this application a user cannot guess.
    pub help_visible: bool,
    /// Whether the scene's world/simulation node is the inspector's subject.
    ///
    /// The domain, active field systems, and sampling settings are properties of
    /// the scene itself, so they are reached by selecting a node in the scene
    /// list like anything else rather than by deselecting everything.
    pub world_selected: bool,
    pub selection: Option<ObjectId>,
    pub plane_selection: Option<PlaneId>,
    pub box_selection: Option<BoxId>,
    pub sphere_selection: Option<SphereId>,
    pub probe_selection: Option<ProbeId>,
    /// Non-modal plot windows pinned independently of scene selection.
    pub probe_plots: BTreeMap<ProbeId, ProbePlotWindow>,
    /// Independent visualization state for every published vector channel.
    pub field_layers: BTreeMap<ChannelId, ChannelLayerSettings>,
    /// Most recent asynchronous command rejection, retained until a later
    /// command succeeds so the user can act on it rather than consult a log.
    pub command_error: Option<String>,
    /// Staged numerical-domain values. They are intentionally independent of
    /// the authoritative source until the user applies the whole candidate.
    pub domain_draft: Option<DomainDraft>,
    /// Whether the MCP panel is shown. Independent of whether the embedded
    /// server is actually running (`crate::mcp::McpSession`, read-only from
    /// here) — this only controls the panel's visibility, the way
    /// `diagnostics_visible` does for its window. Defaults closed, unlike
    /// diagnostics: enabling remote control is a deliberate, security-
    /// relevant opt-in, not something to surface unasked.
    pub mcp_panel_open: bool,
    /// Whether the Queue panel is shown. Defaults closed, matching
    /// `mcp_panel_open`: an empty queue is the common case and shouldn't
    /// demand screen space by default.
    pub queue_panel_open: bool,
}

impl UiModel {
    pub fn new() -> Self {
        Self {
            view: ViewOptions::default(),
            viewport_tool: ViewportTool::default(),
            field_brush_dialog_open: false,
            field_brush: FieldBrushDraft::default(),
            diagnostics_visible: true,
            help_visible: true,
            // A new session opens on the world node, so the inspector explains
            // the scene's domain and field systems before anything is selected
            // rather than showing an empty panel.
            world_selected: true,
            selection: None,
            plane_selection: None,
            box_selection: None,
            sphere_selection: None,
            probe_selection: None,
            probe_plots: BTreeMap::new(),
            field_layers: BTreeMap::new(),
            command_error: None,
            domain_draft: None,
            mcp_panel_open: false,
            queue_panel_open: false,
        }
    }

    /// Make the world/simulation node the inspector's subject.
    pub fn select_world(&mut self) {
        self.set_scene_selection(None);
        self.world_selected = true;
    }

    pub fn open_probe_plot(&mut self, probe: &fieldcad_core::Probe) {
        self.probe_plots
            .entry(probe.id)
            .or_insert_with(|| ProbePlotWindow {
                channels: probe.channels.iter().cloned().collect(),
            });
    }

    /// Ensure every declared vector channel has presentation state, and reveal
    /// the first field a session ever sees.
    ///
    /// The reveal happens once, not whenever nothing is visible. Re-deciding it
    /// every frame cannot tell "this scene has never shown a field" from "the
    /// user just hid the last one", and answers both by switching a layer on —
    /// which makes the only vector channel in a scene impossible to hide,
    /// because clearing the checkbox is undone before the next frame is drawn.
    /// A channel that already has presentation state belongs to the user, and
    /// one arriving later stays opt-in rather than overlaying itself on a view
    /// they deliberately cleared.
    pub fn synchronize_field_layers(&mut self, compute: &ComputeView) {
        let first_field_of_the_session = self.field_layers.is_empty();
        for channel in &compute.vector_channels {
            self.field_layers.entry(channel.clone()).or_default();
        }
        if first_field_of_the_session
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
            .or_else(|| self.box_selection.map(SceneSelection::Box))
            .or_else(|| self.sphere_selection.map(SceneSelection::Sphere))
            .or_else(|| self.probe_selection.map(SceneSelection::Probe))
    }

    pub fn set_scene_selection(&mut self, selection: Option<SceneSelection>) {
        self.selection = None;
        self.plane_selection = None;
        self.box_selection = None;
        self.sphere_selection = None;
        self.probe_selection = None;
        // Selecting anything in the scene — including nothing — takes the
        // inspector off the world node, so the two can never both look selected.
        self.world_selected = false;
        match selection {
            Some(SceneSelection::Object(id)) => self.selection = Some(id),
            Some(SceneSelection::Plane(id)) => self.plane_selection = Some(id),
            Some(SceneSelection::Box(id)) => self.box_selection = Some(id),
            Some(SceneSelection::Sphere(id)) => self.sphere_selection = Some(id),
            Some(SceneSelection::Probe(id)) => self.probe_selection = Some(id),
            None => {}
        }
    }
}

/// Editable representation of [`Domain`] which can temporarily contain values
/// that do not make a valid domain (for example while a user is typing max x).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DomainDraft {
    pub min: DVec3,
    pub max: DVec3,
    pub cells: UVec3,
    pub boundaries: BoundaryConditions,
    pub precision: Precision,
}

impl DomainDraft {
    pub fn from_domain(domain: Domain) -> Self {
        Self {
            min: domain.bounds().min(),
            max: domain.bounds().max(),
            cells: domain.resolution().cells(),
            boundaries: domain.boundaries(),
            precision: domain.precision(),
        }
    }

    pub fn build(self) -> Result<Domain, fieldcad_core::DomainError> {
        Ok(Domain::new(
            DomainBounds::new(self.min, self.max)?,
            Resolution::new(self.cells.x, self.cells.y, self.cells.z)?,
            self.boundaries,
            self.precision,
        ))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbePlotWindow {
    /// Channels shown as separate, unit-safe plots in this window.
    pub channels: BTreeSet<ChannelId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelLayerSettings {
    pub visible: bool,
    pub whole_domain: FieldLayerSettings,
    pub planes: BTreeMap<PlaneId, PlaneLayerSettings>,
    pub boxes: BTreeMap<BoxId, BoxLayerSettings>,
    pub spheres: BTreeMap<SphereId, SphereLayerSettings>,
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
    /// Whether a control that edits the world is being held this frame: a drag
    /// in progress, or a value being typed but not yet committed.
    ///
    /// Only the held part of an edit counts. A checkbox or a menu choice is
    /// already atomic — it produces one command and is over — so it has no
    /// duration for the simulation to be held across.
    pub scene_edit_in_progress: bool,
    /// A one-shot request to start or stop the embedded MCP server —
    /// app-level infrastructure, not a simulation command, so it travels
    /// the same way `camera_action` does rather than through `commands`.
    pub mcp_action: Option<McpAction>,
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
            scene_edit_in_progress: false,
            mcp_action: None,
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
    /// The simulation was advancing and an interactive edit has suspended it.
    /// Said plainly in the transport bar, because a run that stops on its own is
    /// otherwise indistinguishable from one that broke.
    pub paused_for_edit: bool,
    /// An interactive edit is open, whether or not it suspended a run. Controls
    /// that act on a *completed* edit — undo and redo — stand down while one is
    /// still being made.
    pub edit_in_progress: bool,
    /// How the camera is currently mapping the scene to the screen, so the
    /// control that changes it can show which is in force.
    pub projection: Projection,
    /// The embedded MCP server's current state, read-only from here — the
    /// panel renders it and emits an `McpAction` to change it, the way any
    /// other control emits a command rather than mutating state directly.
    pub mcp: &'a McpSession,
}

/// A default-action button with an attached "▾" dropdown listing every named
/// choice, the default included.
///
/// Used wherever "add X" has one obvious default — an empty object, a slice
/// plane — but also a small catalog of named variants a user reaches for less
/// often. Clicking the button itself performs the default so the common case
/// stays one click; opening the dropdown is how a less common choice gets
/// made without growing the panel into a row of buttons per choice.
fn split_add_button<T: Clone>(
    ui: &mut egui::Ui,
    default_label: &str,
    default_hover: &str,
    default: T,
    choices: &[(&str, T)],
) -> Option<T> {
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        if ui
            .button(format!("+  {default_label}"))
            .on_hover_text(default_hover)
            .clicked()
        {
            chosen = Some(default.clone());
        }
        ui.menu_button("▾", |ui| {
            for (label, value) in choices {
                if ui.button(*label).clicked() {
                    chosen = Some(value.clone());
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("More choices");
    });
    chosen
}

/// The controls for drawing a vector field over any region.
///
/// One widget wherever vectors are drawn — a slice plane, the whole domain —
/// because the questions are the same each time. Only the plane adds one of its
/// own, projection, and it adds it beside this rather than inside it.
///
/// Density resamples the published lattice by interpolation, so raising it draws
/// more arrows without claiming more accuracy; the hover says where real detail
/// comes from instead.
fn vector_display_controls(
    ui: &mut egui::Ui,
    display: &mut VectorDisplay,
    label: &str,
    hover: &str,
) {
    ui.checkbox(&mut display.visible, label)
        .on_hover_text(hover);
    ui.add_enabled_ui(display.visible, |ui| {
        ui.horizontal(|ui| {
            ui.label("Arrows");
            ui.add(
                egui::DragValue::new(&mut display.density)
                    .speed(0.25)
                    .range(0..=256),
            )
            .on_hover_text(
                "Arrows along the longest axis, interpolated from the published samples.\n\
                 This is how densely the field is drawn, not how densely it was solved — \
                 for that, raise the Simulation node's transport sampling.",
            );

            ui.label("Scale");
            ui.add(
                egui::DragValue::new(&mut display.scale)
                    .speed(0.01)
                    .range(0.05..=20.0)
                    .custom_formatter(|scale, _| format!("{scale:.2}×")),
            )
            .on_hover_text(
                "Multiplies the arrow length. Arrows are sized to their spacing by default; \
                 shorten them to read a dense field, lengthen them to read a sparse one.",
            );
        });
    });
}

/// One foldable group, in the single style both side panels use.
///
/// A scene grows without bound and an inspected subject can carry more than
/// fits on screen, so anything a user is not looking at right now has to be
/// possible to put away. Routing every group through one helper is what keeps a
/// section of the scene tree and a section of the inspector recognisably the
/// same idea rather than two conventions that drifted apart.
///
/// Fold state lives in egui's memory, keyed by `id`, so it survives selection
/// changes and re-layout for as long as the session does.
fn section<R>(
    ui: &mut egui::Ui,
    id: impl egui::AsIdSalt,
    title: impl Into<String>,
    default_open: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong())
        .id_salt(id)
        .default_open(default_open)
        .show(ui, body)
        .body_returned
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

    // After `viewport`, which is what establishes the rect these anchor to.
    view_controls(&context, model, &frame, &mut output);
    help_window(&context, model);
    if model.diagnostics_visible {
        diagnostics_window(&context, &frame, model.command_error.as_deref());
    }
    if model.mcp_panel_open {
        output.mcp_action = mcp_window(&context, frame.mcp);
    }
    if model.queue_panel_open {
        queue_window(&context, &frame, &mut output);
    }
    floating_probe_plots(&context, model, &frame);
    field_brush_dialog(&context, model, frame.compute);

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
            // The scene fills the whole central region, but it must not claim
            // the pointer along the edges: the side panels put their resize
            // handles there, and this interaction is registered after theirs,
            // so it wins the hit test and the splitters become undraggable.
            // Keeping clear of the grab radius leaves the handles reachable and
            // costs only a gutter the camera never needed.
            let grab = ui.style().interaction.resize_grab_radius_side;
            let response = ui.interact(
                output.viewport.shrink(grab),
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
        frame_sized(context, model, events, egui::vec2(1_280.0, 800.0))
    }

    /// Everything a frame painted, as text. Used to assert which panel a control
    /// ended up in, which is the substance of the layout contract.
    fn frame_text(context: &egui::Context, model: &mut UiModel) -> String {
        frame_text_editing(context, model, false)
    }

    fn frame_text_editing(
        context: &egui::Context,
        model: &mut UiModel,
        paused_for_edit: bool,
    ) -> String {
        let world = seeded_world();
        let snapshot = world.snapshot();
        let compute = ComputeView::build(&source(), &snapshot, None);
        let history = ProbeHistory::default();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_280.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = context.run_ui(input, |root| {
            show(
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
                    paused_for_edit,
                    edit_in_progress: false,
                    projection: Projection::default(),
                    mcp: &McpSession::Disabled,
                },
            );
        });

        fn collect(shape: &egui::epaint::Shape, out: &mut String) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    out.push_str(&text.galley.job.text);
                    out.push('\n');
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for clipped in &full_output.shapes {
            collect(&clipped.shape, &mut text);
        }
        text
    }

    fn frame_sized(
        context: &egui::Context,
        model: &mut UiModel,
        events: Vec<egui::Event>,
        screen: egui::Vec2,
    ) -> UiFrameOutput {
        // The view model is a plain value, so the panels are exercised without
        // constructing a runtime per frame.
        let world = seeded_world();
        let snapshot = world.snapshot();
        let compute = ComputeView::build(&source(), &snapshot, None);
        let history = ProbeHistory::default();
        let mut output = UiFrameOutput::default();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
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
                    paused_for_edit: false,
                    edit_in_progress: false,
                    projection: Projection::default(),
                    mcp: &McpSession::Disabled,
                },
            );
        });
        output
    }

    /// The inspector's left edge lands mid-pixel — egui sizes panels in logical
    /// points, and with this layout the boundary is fractional even at scale
    /// 1.0. The scene is scissored to the central region and the panels are
    /// painted over it, so a scissor rounded outward puts scene pixels under
    /// the inspector's feathered border and they read as a seam between the two.
    #[test]
    fn the_scene_is_never_scissored_into_the_side_panels() {
        for pixels_per_point in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let context = egui::Context::default();
            context.set_pixels_per_point(pixels_per_point);
            let mut model = UiModel::new();
            // The first frame lays the panels out; the second reports settled
            // rectangles.
            frame(&context, &mut model, vec![]);
            let output = frame(&context, &mut model, vec![]);

            let surface = (
                (1_280.0 * pixels_per_point) as u32,
                (800.0 * pixels_per_point) as u32,
            );
            let viewport = crate::camera::Viewport::from_logical(
                glam::Vec2::new(output.viewport.min.x, output.viewport.min.y),
                glam::Vec2::new(output.viewport.width(), output.viewport.height()),
                pixels_per_point,
                surface,
            );

            let inspector_edge = output.viewport.max.x * pixels_per_point;
            let scene_edge = (viewport.x + viewport.width) as f32;
            assert!(
                scene_edge <= inspector_edge,
                "at {pixels_per_point}x the scene is drawn to {scene_edge} but the \
                 inspector starts at {inspector_edge}",
            );

            let scene_tree_edge = output.viewport.min.x * pixels_per_point;
            assert!(
                viewport.x as f32 >= scene_tree_edge,
                "at {pixels_per_point}x the scene starts at {} but the scene tree \
                 panel ends at {scene_tree_edge}",
                viewport.x,
            );
        }
    }

    /// A panel's own width must win over its content's. egui reports a rect
    /// clamped to `size_range` but paints the frame at the size the content
    /// demanded, so once content overflows, the resize separator and the region
    /// left for the 3D view are both placed from a rectangle that is not what is
    /// on screen: the separator lands inside the panel and the scene is drawn
    /// underneath it. A narrow window is the cheapest way to force the overflow,
    /// because egui caps a panel's maximum at what the window can spare.
    #[test]
    fn the_panels_and_the_3d_view_tile_the_window_without_overlapping() {
        for width in [1_280.0f32, 900.0, 700.0, 560.0] {
            let context = egui::Context::default();
            let mut model = UiModel::new();
            let screen = egui::vec2(width, 800.0);
            frame_sized(&context, &mut model, vec![], screen);
            let output = frame_sized(&context, &mut model, vec![], screen);

            let panel = |name: &str| {
                egui::containers::panel::PanelState::load(&context, egui::Id::new(name))
                    .expect("panel should have laid out")
                    .outer_rect
            };
            let scene = panel("scene_panel");
            let inspector = panel("inspector_panel");

            assert_eq!(
                scene.min.x, 0.0,
                "at {width}px the scene panel starts off-screen",
            );
            assert_eq!(
                inspector.max.x, width,
                "at {width}px the inspector runs past the window edge",
            );
            assert_eq!(
                scene.max.x, output.viewport.min.x,
                "at {width}px the scene panel and the 3D view do not meet",
            );
            assert_eq!(
                output.viewport.max.x, inspector.min.x,
                "at {width}px the 3D view and the inspector do not meet",
            );
        }
    }

    /// The scene fills the central region and senses drags there for the camera,
    /// but it is registered after the panels, so a full-width interaction wins
    /// the hit test over their resize handles and the splitters stop working —
    /// the panel appears to resize and the 3D view never takes the space.
    #[test]
    fn dragging_a_panel_edge_resizes_the_3d_view() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        frame(&context, &mut model, vec![]);
        let settled = frame(&context, &mut model, vec![]);
        let edge = settled.viewport.min.x;

        let handle = egui::pos2(edge, 400.0);
        frame(
            &context,
            &mut model,
            vec![egui::Event::PointerMoved(handle)],
        );
        frame(
            &context,
            &mut model,
            vec![egui::Event::PointerButton {
                pos: handle,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let dragged = frame(
            &context,
            &mut model,
            vec![egui::Event::PointerMoved(egui::pos2(edge - 30.0, 400.0))],
        );

        assert_eq!(
            dragged.viewport.min.x,
            edge - 30.0,
            "the 3D view did not follow the scene panel's edge",
        );
    }

    #[test]
    fn central_viewport_reports_middle_drag() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        // Low in the viewport, clear of the floating View, Help, and
        // Diagnostics windows, which legitimately capture the pointer.
        let start = egui::pos2(880.0, 700.0);
        let end = egui::pos2(980.0, 760.0);

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
        let pointer = egui::pos2(880.0, 700.0);

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
        assert!(settings.vectors.visible);
        assert!(settings.magnitude_visible);
    }

    /// Both regions configure their arrows with the same value, so a control
    /// added for one is a control the other has too. What differs is where the
    /// arrows go and how many are legible there, not what can be set.
    #[test]
    fn a_plane_and_the_whole_domain_configure_their_arrows_identically() {
        let plane = PlaneLayerSettings::default().vectors;
        let domain = FieldLayerSettings::default().vectors;

        assert_eq!(plane.scale, 1.0);
        assert_eq!(domain.scale, plane.scale);
        // Volume glyphs occlude, so they start off and sparser than a plane's.
        assert!(plane.visible);
        assert!(!domain.visible);
        assert!(domain.density < plane.density);
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
        let mut view = ComputeView::build(&source, &world.snapshot(), None);
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
    }

    /// Turning the last visible layer off has to stay off.
    ///
    /// Revealing a layer is for a scene that has never shown one, so that a
    /// first run is not an unexplained blank view. Re-deciding that every frame
    /// meant the only vector channel in a scene could never be switched off: the
    /// checkbox cleared and the next frame set it again. With one active model
    /// publishing one vector field — which is now the default scene — that made
    /// the control useless rather than merely surprising.
    #[test]
    fn hiding_the_only_field_layer_is_not_undone_on_the_next_frame() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot(), None);
        view.vector_channels = vec![vector_channel_id()];
        let mut model = UiModel::new();

        model.synchronize_field_layers(&view);
        assert!(
            model.field_layers[&vector_channel_id()].visible,
            "a scene that has never shown a field opens showing one"
        );

        // Exactly what clearing the checkbox does.
        model
            .field_layers
            .get_mut(&vector_channel_id())
            .unwrap()
            .visible = false;
        for _ in 0..3 {
            model.synchronize_field_layers(&view);
            assert!(
                !model.field_layers[&vector_channel_id()].visible,
                "the layer was hidden by the user and must stay hidden"
            );
        }
    }

    /// A field that arrives later is still opt-in, and does not switch itself on
    /// because the user has everything else hidden.
    #[test]
    fn a_new_channel_does_not_reveal_itself_over_a_hidden_scene() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot(), None);
        view.vector_channels = vec![vector_channel_id()];
        let mut model = UiModel::new();
        model.synchronize_field_layers(&view);
        model
            .field_layers
            .get_mut(&vector_channel_id())
            .unwrap()
            .visible = false;

        // Choosing a model that computes a second field, with everything hidden.
        let arrived = scalar_channel_id();
        view.vector_channels.push(arrived.clone());
        model.synchronize_field_layers(&view);

        assert!(model.field_layers.contains_key(&arrived));
        assert!(!model.field_layers[&arrived].visible);
        assert!(!model.field_layers[&vector_channel_id()].visible);
    }

    /// The inspector's job is to describe one selected thing. Simulation
    /// settings used to be appended to it unconditionally, which meant the panel
    /// answered two questions at once and the domain had no home in the scene.
    #[test]
    fn the_inspector_shows_simulation_settings_only_for_the_world_node() {
        let context = egui::Context::default();
        let mut model = UiModel::new();

        model.select_world();
        frame_text(&context, &mut model);
        let world_node = frame_text(&context, &mut model);
        assert!(
            world_node.contains("Field systems"),
            "the world node must own the field-system controls: {world_node}"
        );

        model.set_scene_selection(Some(SceneSelection::Object(ObjectId::new(0))));
        frame_text(&context, &mut model);
        let object = frame_text(&context, &mut model);
        assert!(
            !object.contains("Field systems"),
            "selecting an object must not also show scene settings: {object}"
        );
        assert!(
            !object.contains("Transport sampling"),
            "selecting an object must not also show transport sampling: {object}"
        );
    }

    /// Camera and display controls belong to the 3D view, not to a side panel.
    #[test]
    fn camera_and_display_controls_live_over_the_3d_view() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        model.select_world();

        frame_text(&context, &mut model);
        let text = frame_text(&context, &mut model);

        // Every axis button and every display toggle is reachable there.
        for label in AxisView::ALL.map(AxisView::label) {
            assert!(
                text.contains(label),
                "{label} view button is missing: {text}"
            );
        }
        for (label, _, _) in ViewOptions::PRIMARY_ENTRIES {
            assert!(text.contains(label), "{label} toggle is missing: {text}");
        }
        assert!(text.contains("Auxiliary object types"));
        assert!(text.contains("Compute"));
        assert!(text.contains("Reset"), "camera reset is missing: {text}");

        // And it is positioned inside the 3D view rather than over a panel.
        let output = frame_sized(&context, &mut model, vec![], egui::vec2(1_280.0, 800.0));
        let window = egui::AreaState::load(&context, egui::Id::new(viewcontrols::WINDOW_ID))
            .map(|state| state.rect())
            .expect("the view window should have laid out");
        assert!(
            output.viewport.contains_rect(window),
            "view controls at {window:?} escape the 3D view {:?}",
            output.viewport,
        );
    }

    /// Projection belongs with the camera controls it changes the meaning of,
    /// and reaches the shell as an action rather than by the panel reaching into
    /// the camera.
    #[test]
    fn the_view_window_offers_both_projections_and_reports_the_active_one() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        model.select_world();

        frame_text(&context, &mut model);
        let text = frame_text(&context, &mut model);
        for projection in Projection::ALL {
            assert!(
                text.contains(projection.label()),
                "{} is not offered: {text}",
                projection.label()
            );
        }
    }

    /// A getting-started window that opens on a first run has to teach without
    /// hiding the thing it is teaching about.
    #[test]
    fn the_help_window_opens_without_burying_the_scene() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        assert!(model.help_visible, "a first run should offer guidance");

        frame(&context, &mut model, vec![]);
        let output = frame(&context, &mut model, vec![]);
        let help = egui::AreaState::load(&context, egui::Id::new(help::WINDOW_ID))
            .map(|state| state.rect())
            .expect("the help window should have laid out");

        let covered =
            (help.width() * help.height()) / (output.viewport.width() * output.viewport.height());
        assert!(
            covered < 0.5,
            "help covers {:.0}% of the 3D view",
            covered * 100.0
        );

        // And it is dismissible, leaving the view entirely clear. egui fades a
        // closing window out against the wall clock, which this harness never
        // advances, so the fade is switched off rather than waited on.
        context.all_styles_mut(|style| style.animation_time = 0.0);
        model.help_visible = false;
        frame_text(&context, &mut model);
        let text = frame_text(&context, &mut model);
        assert!(
            !text.contains("Build a scene"),
            "help did not close: {text}"
        );
    }

    /// The whole point of a section: putting away what you are not looking at.
    /// A header that folds but leaves its contents painted would be decoration.
    #[test]
    fn folding_a_section_puts_its_contents_away() {
        let context = egui::Context::default();
        // egui fades a body out against the wall clock, which this harness never
        // advances, so the fade is switched off rather than waited on.
        context.all_styles_mut(|style| style.animation_time = 0.0);

        let run = |events: Vec<egui::Event>| {
            let mut rect = egui::Rect::NOTHING;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let full_output = context.run_ui(input, |ui| {
                section(ui, "folding_test_section", "Group", true, |ui| {
                    ui.label("contents");
                });
                rect = ui.min_rect();
            });
            let mut text = String::new();
            for clipped in &full_output.shapes {
                if let egui::epaint::Shape::Text(shape) = &clipped.shape {
                    text.push_str(&shape.galley.job.text);
                    text.push('\n');
                }
            }
            (text, rect)
        };

        let (text, rect) = run(Vec::new());
        assert!(text.contains("Group"), "the header is missing: {text}");
        assert!(text.contains("contents"), "a section opens open: {text}");

        // The header row sits at the top of the section, the toggle at its left.
        let header = rect.left_top() + egui::vec2(6.0, 8.0);
        run(vec![egui::Event::PointerMoved(header)]);
        run(vec![egui::Event::PointerButton {
            pos: header,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let (text, _) = run(vec![egui::Event::PointerButton {
            pos: header,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert!(
            text.contains("Group"),
            "a folded section still has to say what it is: {text}"
        );
        assert!(!text.contains("contents"), "folding hid nothing: {text}");
    }

    /// Both panels are lists that grow without bound, so both are divided into
    /// named groups rather than one scroll of everything.
    #[test]
    fn the_scene_panel_groups_its_contents_into_named_sections() {
        let context = egui::Context::default();
        let mut model = UiModel::new();

        frame_text(&context, &mut model);
        let text = frame_text(&context, &mut model);

        for heading in ["Simulation", "Objects", "Measurement"] {
            assert!(
                text.contains(heading),
                "the scene panel has no {heading} section: {text}"
            );
        }
        // Counts belong in the header, so a folded section still says how much
        // is behind it. The seeded world has one object and one probe.
        assert!(text.contains("Objects (1)"), "{text}");
        assert!(text.contains("Measurement (1)"), "{text}");
    }

    #[test]
    fn every_inspector_subject_groups_its_properties_into_named_sections() {
        let context = egui::Context::default();
        let mut model = UiModel::new();
        let world = seeded_world().snapshot();

        let subjects: [(SceneSelection, &[&str]); 2] = [
            (
                SceneSelection::Object(ObjectId::new(0)),
                &["Placement", "Components"],
            ),
            (
                SceneSelection::Probe(*world.probes().keys().next().unwrap()),
                &["Position", "Recorded channels", "History"],
            ),
        ];
        for (selection, sections) in subjects {
            model.set_scene_selection(Some(selection));
            frame_text(&context, &mut model);
            let text = frame_text(&context, &mut model);
            for heading in sections {
                assert!(
                    text.contains(heading),
                    "{selection:?} is missing its {heading} section: {text}"
                );
            }
        }

        model.select_world();
        frame_text(&context, &mut model);
        let text = frame_text(&context, &mut model);
        for heading in ["Field systems", "Transport sampling", "Compute"] {
            assert!(
                text.contains(heading),
                "the simulation node is missing its {heading} section: {text}"
            );
        }
    }

    /// A run that stops by itself is indistinguishable from one that broke, so
    /// the transport bar has to say which it is — and stop saying it the moment
    /// the run is handed back.
    #[test]
    fn the_transport_bar_says_when_an_edit_is_holding_the_simulation() {
        let context = egui::Context::default();
        let mut model = UiModel::new();

        frame_text_editing(&context, &mut model, true);
        let editing = frame_text_editing(&context, &mut model, true);
        assert!(
            editing.contains("paused for edit"),
            "a suspended run must say why: {editing}"
        );

        let resumed = frame_text_editing(&context, &mut model, false);
        assert!(!resumed.contains("paused for edit"));
    }

    #[test]
    fn a_session_opens_on_the_world_node_rather_than_an_empty_inspector() {
        let model = UiModel::new();

        assert!(model.world_selected);
        assert_eq!(model.scene_selection(), None);
    }

    #[test]
    fn the_world_node_and_a_scene_selection_are_mutually_exclusive() {
        let mut model = UiModel::new();
        let object = ObjectId::new(4);

        model.set_scene_selection(Some(SceneSelection::Object(object)));
        assert!(!model.world_selected);
        assert_eq!(
            model.scene_selection(),
            Some(SceneSelection::Object(object))
        );

        model.select_world();
        assert!(model.world_selected);
        assert_eq!(model.scene_selection(), None);

        // Clearing the scene selection is not the same as selecting the world:
        // deselecting must leave the inspector genuinely empty.
        model.set_scene_selection(None);
        assert!(!model.world_selected);
    }

    #[test]
    fn viewport_helpers_are_independently_visible_by_default() {
        let model = UiModel::new();

        assert!(model.view.grid);
        assert!(model.view.axes);
        assert!(model.view.objects);
        assert!(model.view.auxiliary_objects);
        assert!(!model.view.compute_bounds);
        assert!(model.view.probes);
        assert!(model.view.planes);
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
