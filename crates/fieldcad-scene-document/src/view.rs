//! Saved presentation state: camera framing, what the camera follows, the
//! global view toggles, and each channel's per-region display settings.
//!
//! These are plain mirrors of the desktop host's own live types (`OrbitCamera`,
//! `ui::ViewOptions`, `scene::{VectorDisplay, FlowLineDisplay, ...}`) rather
//! than the types themselves: this crate is also used by `fieldcad-mcp`,
//! which has no camera or UI concept, and cannot depend on `fieldcad-desktop`
//! (the wrong dependency direction — an app crate, not a library one). The
//! desktop host owns the conversion both ways.

use std::collections::BTreeMap;

use fieldcad_core::{BoxId, ChannelId, ObjectId, PlaneId, SphereId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    pub target: [f32; 3],
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub projection: CameraProjection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraProjection {
    Perspective,
    Orthographic,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorDisplayState {
    pub visible: bool,
    pub density: u32,
    pub scale: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowLineDisplayState {
    pub visible: bool,
    pub density: u32,
    pub thickness_px: f32,
    pub animated: bool,
    pub speed: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldLayerViewState {
    pub vectors: VectorDisplayState,
    pub flow_lines: FlowLineDisplayState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PlaneVectorModeState {
    #[default]
    InPlane,
    Full3d,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaneViewState {
    pub visible: bool,
    pub magnitude_visible: bool,
    pub magnitude_density: u32,
    pub vectors: VectorDisplayState,
    pub vector_mode: PlaneVectorModeState,
    pub flow_lines: FlowLineDisplayState,
}

/// Shared shape for a field box's and a field sphere's per-channel display —
/// see `BoxLayerSettings`/`SphereLayerSettings` on the desktop side, which
/// are likewise identical to each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RegionViewState {
    pub visible: bool,
    pub vectors: VectorDisplayState,
    pub flow_lines: FlowLineDisplayState,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelViewState {
    pub visible: bool,
    pub whole_domain: FieldLayerViewState,
    pub planes: BTreeMap<PlaneId, PlaneViewState>,
    pub boxes: BTreeMap<BoxId, RegionViewState>,
    pub spheres: BTreeMap<SphereId, RegionViewState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GizmoDisplayState {
    pub axis_length_px: f32,
    pub axis_thickness_px: f32,
    pub rotation_diameter_px: f32,
    pub rotation_thickness_px: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewOptionsState {
    pub grid: bool,
    pub axes: bool,
    pub objects: bool,
    pub auxiliary_objects: bool,
    pub compute_bounds: bool,
    pub gizmo_display: GizmoDisplayState,
    pub probes: bool,
    pub planes: bool,
    pub boxes: bool,
    pub spheres: bool,
}

/// Everything about how a session was being viewed, captured at save time.
/// `#[serde(default)]` on `SceneDocument::view` means a document saved before
/// this existed simply loads with `SceneViewState::default()` — an absent
/// camera, no follow target, default view toggles, no per-channel overrides.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneViewState {
    #[serde(default)]
    pub camera: Option<CameraState>,
    #[serde(default)]
    pub following: Option<ObjectId>,
    #[serde(default)]
    pub view_options: Option<ViewOptionsState>,
    #[serde(default)]
    pub channels: BTreeMap<ChannelId, ChannelViewState>,
}
