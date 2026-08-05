use std::{collections::BTreeMap, sync::Arc};

use glam::{DQuat, DVec2, DVec3, UVec2, UVec3};
use serde::{Deserialize, Serialize};

use crate::{
    BoxId, BoxLattice, ChannelId, ComponentSchema, ComponentTypeId, ObjectId, PlaneId,
    PlaneLattice, ProbeId, PropertyBag, SchemaError, SphereId, SphereLattice, WorldRevision,
};

/// A finite, non-degenerate direction. Used for plane normals and in-plane axes.
fn is_usable_direction(direction: DVec3) -> bool {
    direction.is_finite() && direction.length_squared() > 0.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: DVec3,
    pub rotation: DQuat,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: DVec3::ZERO,
            rotation: DQuat::IDENTITY,
        }
    }
}

impl Transform {
    pub fn new(translation: DVec3, rotation: DQuat) -> Result<Self, WorldError> {
        let candidate = Self {
            translation,
            rotation,
        };
        candidate.validate()?;
        Ok(Self {
            translation,
            rotation: rotation.normalize(),
        })
    }

    pub fn at(translation: DVec3) -> Result<Self, WorldError> {
        Self::new(translation, DQuat::IDENTITY)
    }

    /// Transform a point from object space into world space.
    pub fn apply(self, point: DVec3) -> DVec3 {
        self.translation + self.rotation * point
    }

    /// The same transform with a unit rotation.
    ///
    /// The fields are public and `Transform` is `Deserialize`, so a value can
    /// reach the world without passing through [`Transform::new`]. A quaternion
    /// of length `k` scales every rotated vector by `k²`, which would move an
    /// attached probe's sample point while the reading still claimed `Exact`
    /// validity — so the command boundary normalizes rather than trusting its
    /// input.
    pub(crate) fn normalized(self) -> Self {
        Self {
            translation: self.translation,
            rotation: self.rotation.normalize(),
        }
    }

    pub(crate) fn validate(self) -> Result<(), WorldError> {
        if !self.translation.is_finite()
            || !self.rotation.is_finite()
            || self.rotation.length_squared() == 0.0
        {
            return Err(WorldError::InvalidTransform);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Velocity {
    pub linear: DVec3,
    pub angular: DVec3,
}

impl Velocity {
    pub fn new(linear: DVec3, angular: DVec3) -> Result<Self, WorldError> {
        let candidate = Self { linear, angular };
        candidate.validate()?;
        Ok(candidate)
    }

    pub(crate) fn validate(self) -> Result<(), WorldError> {
        if !self.linear.is_finite() || !self.angular.is_finite() {
            return Err(WorldError::InvalidVelocity);
        }
        Ok(())
    }
}

/// The optional geometry an object occupies.
///
/// `CONTEXT.md` defers solid modelling; this is deliberately the smallest set
/// that supports point/sphere sources and a selectable authoring proxy.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ObjectShape {
    /// A point source with a declared radius inside which the analytic field is
    /// undefined rather than merely large.
    Point {
        radius: f64,
    },
    Sphere {
        radius: f64,
    },
    Box {
        half_extent: DVec3,
    },
}

impl ObjectShape {
    pub fn point(radius: f64) -> Result<Self, WorldError> {
        Self::Point { radius }.validated()
    }

    pub fn sphere(radius: f64) -> Result<Self, WorldError> {
        Self::Sphere { radius }.validated()
    }

    pub fn boxed(half_extent: DVec3) -> Result<Self, WorldError> {
        Self::Box { half_extent }.validated()
    }

    fn validated(self) -> Result<Self, WorldError> {
        self.validate()?;
        Ok(self)
    }

    pub(crate) fn validate(self) -> Result<(), WorldError> {
        let valid = match self {
            Self::Point { radius } | Self::Sphere { radius } => radius.is_finite() && radius >= 0.0,
            Self::Box { half_extent } => half_extent.is_finite() && half_extent.min_element() > 0.0,
        };
        if valid {
            Ok(())
        } else {
            Err(WorldError::InvalidShape)
        }
    }

    /// Object-space half-extent of an axis-aligned box enclosing the shape.
    pub fn half_extent(self) -> DVec3 {
        match self {
            Self::Point { radius } | Self::Sphere { radius } => DVec3::splat(radius.max(1.0e-3)),
            Self::Box { half_extent } => half_extent,
        }
    }

    /// Radius of a sphere enclosing the shape, for camera framing.
    pub fn bounding_radius(self) -> f64 {
        self.half_extent().length()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectSpec {
    pub name: String,
    pub transform: Transform,
    pub velocity: Velocity,
    pub shape: Option<ObjectShape>,
    /// Presentation visibility. Hidden objects still participate in equation
    /// systems; hiding a charge must never alter the physical result.
    pub visible: bool,
    /// Whether this object's motion is authored rather than solver-integrated.
    ///
    /// See [`WorldObject::pinned`]. Objects default to unpinned, so attaching
    /// mass is by itself enough to make a body respond to fields.
    pub pinned: bool,
    pub components: BTreeMap<ComponentTypeId, PropertyBag>,
}

impl ObjectSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transform: Transform::default(),
            velocity: Velocity::default(),
            shape: None,
            visible: true,
            pinned: false,
            components: BTreeMap::new(),
        }
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }

    pub fn with_velocity(mut self, velocity: Velocity) -> Self {
        self.velocity = velocity;
        self
    }

    pub fn with_shape(mut self, shape: ObjectShape) -> Self {
        self.shape = Some(shape);
        self
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn with_pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    pub fn with_component(mut self, component: ComponentTypeId, properties: PropertyBag) -> Self {
        self.components.insert(component, properties);
        self
    }

    fn validate(&self) -> Result<(), WorldError> {
        self.transform.validate()?;
        self.velocity.validate()?;
        if let Some(shape) = self.shape {
            shape.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldObject {
    pub id: ObjectId,
    pub name: String,
    pub transform: Transform,
    pub velocity: Velocity,
    pub shape: Option<ObjectShape>,
    pub visible: bool,
    /// Whether this object's motion is authored rather than solver-integrated.
    ///
    /// Every object in the space has a pose, and a pose that changes over time
    /// is velocity — so movement is not a property an object opts into. What a
    /// user chooses is *who decides* the motion. An unpinned body with mass is
    /// advanced by whichever equation system claims it; a pinned one follows the
    /// authored transform and velocity exactly, which is how a static charge
    /// configuration is held in place.
    pub pinned: bool,
    pub components: BTreeMap<ComponentTypeId, PropertyBag>,
}

impl WorldObject {
    /// World-space centre and radius of a sphere enclosing this object, for
    /// camera framing and coarse picking.
    pub fn bounding_sphere(&self) -> (DVec3, f64) {
        let radius = self
            .shape
            .map_or(DEFAULT_PROXY_RADIUS, ObjectShape::bounding_radius);
        (self.transform.translation, radius)
    }
}

/// Half-size of the authoring proxy drawn for an object with no declared shape.
pub const DEFAULT_PROXY_RADIUS: f64 = 0.25;

/// A bounded, orientable plane on which a field is sampled and drawn.
///
/// Origin and normal alone describe an infinite plane, which cannot be sampled
/// into an image. The in-plane `u_axis` fixes the image's orientation so that a
/// re-sampled plane does not silently rotate between snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlicePlaneSpec {
    name: String,
    origin: DVec3,
    normal: DVec3,
    u_axis: DVec3,
    half_extent: DVec2,
    visible: bool,
}

impl SlicePlaneSpec {
    pub fn new(name: impl Into<String>, origin: DVec3, normal: DVec3) -> Result<Self, WorldError> {
        if !origin.is_finite() || !is_usable_direction(normal) {
            return Err(WorldError::InvalidPlane);
        }
        let normal = normal.normalize();
        Ok(Self {
            name: name.into(),
            origin,
            normal,
            u_axis: default_u_axis(normal),
            half_extent: DVec2::splat(1.0),
            visible: true,
        })
    }

    pub fn with_half_extent(mut self, half_extent: DVec2) -> Result<Self, WorldError> {
        if !half_extent.is_finite() || half_extent.min_element() <= 0.0 {
            return Err(WorldError::InvalidPlane);
        }
        self.half_extent = half_extent;
        Ok(self)
    }

    /// Pin the in-plane horizontal axis. The component along the normal is
    /// removed; the result must still be non-degenerate.
    pub fn with_u_axis(mut self, u_axis: DVec3) -> Result<Self, WorldError> {
        let projected = u_axis - self.normal * u_axis.dot(self.normal);
        if !is_usable_direction(projected) {
            return Err(WorldError::InvalidPlane);
        }
        self.u_axis = projected.normalize();
        Ok(self)
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn with_origin(mut self, origin: DVec3) -> Result<Self, WorldError> {
        if !origin.is_finite() {
            return Err(WorldError::InvalidPlane);
        }
        self.origin = origin;
        Ok(self)
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn from_plane(plane: &SlicePlane) -> Self {
        Self {
            name: plane.name.clone(),
            origin: plane.origin,
            normal: plane.normal,
            u_axis: plane.u_axis,
            half_extent: plane.half_extent,
            visible: plane.visible,
        }
    }
}

/// Choose a stable in-plane axis for a normal, so that two planes with the same
/// normal always produce the same image orientation.
fn default_u_axis(normal: DVec3) -> DVec3 {
    let reference = if normal.z.abs() < 0.9 {
        DVec3::Z
    } else {
        DVec3::X
    };
    reference.cross(normal).normalize()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlicePlane {
    pub id: PlaneId,
    pub name: String,
    pub origin: DVec3,
    pub normal: DVec3,
    pub u_axis: DVec3,
    pub half_extent: DVec2,
    pub visible: bool,
}

impl SlicePlane {
    /// The orthonormal in-plane basis, right-handed with respect to the normal.
    pub fn basis(&self) -> (DVec3, DVec3) {
        (self.u_axis, self.normal.cross(self.u_axis))
    }

    /// A sampling lattice covering the plane's extent at the requested counts.
    ///
    /// Sampling density is a visualization setting; it is deliberately an
    /// argument here rather than stored on the plane, so that changing it cannot
    /// change the physical result.
    pub fn lattice(&self, counts: UVec2) -> PlaneLattice {
        let counts = counts.max(UVec2::ONE);
        let (u, v) = self.basis();
        let span = self.half_extent * 2.0;
        let divisor = (counts.as_dvec2() - DVec2::ONE).max(DVec2::ONE);
        let u_step = u * (span.x / divisor.x);
        let v_step = v * (span.y / divisor.y);
        let origin = self.origin - u * self.half_extent.x - v * self.half_extent.y;
        PlaneLattice::new(origin, u_step, v_step, counts)
    }
}

/// A bounded, orientable box in which a field is sampled and drawn as arrows.
///
/// Unlike [`SlicePlane`], a box has no colour surface: a 2D magnitude map has
/// a natural home on a slice, but a volume's interior cannot be flattened onto
/// one without hiding most of it, so a box only ever draws vectors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldBoxSpec {
    name: String,
    origin: DVec3,
    rotation: DQuat,
    half_extent: DVec3,
    visible: bool,
}

impl FieldBoxSpec {
    pub fn new(
        name: impl Into<String>,
        origin: DVec3,
        half_extent: DVec3,
    ) -> Result<Self, WorldError> {
        if !origin.is_finite() || !half_extent.is_finite() || half_extent.min_element() <= 0.0 {
            return Err(WorldError::InvalidBox);
        }
        Ok(Self {
            name: name.into(),
            origin,
            rotation: DQuat::IDENTITY,
            half_extent,
            visible: true,
        })
    }

    pub fn with_half_extent(mut self, half_extent: DVec3) -> Result<Self, WorldError> {
        if !half_extent.is_finite() || half_extent.min_element() <= 0.0 {
            return Err(WorldError::InvalidBox);
        }
        self.half_extent = half_extent;
        Ok(self)
    }

    pub fn with_rotation(mut self, rotation: DQuat) -> Result<Self, WorldError> {
        if !rotation.is_finite() || rotation.length_squared() == 0.0 {
            return Err(WorldError::InvalidBox);
        }
        self.rotation = rotation.normalize();
        Ok(self)
    }

    pub fn with_origin(mut self, origin: DVec3) -> Result<Self, WorldError> {
        if !origin.is_finite() {
            return Err(WorldError::InvalidBox);
        }
        self.origin = origin;
        Ok(self)
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn from_box(field_box: &FieldBox) -> Self {
        Self {
            name: field_box.name.clone(),
            origin: field_box.origin,
            rotation: field_box.rotation,
            half_extent: field_box.half_extent,
            visible: field_box.visible,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldBox {
    pub id: BoxId,
    pub name: String,
    pub origin: DVec3,
    pub rotation: DQuat,
    pub half_extent: DVec3,
    pub visible: bool,
}

impl FieldBox {
    /// A sampling lattice covering the box's oriented extent at the requested
    /// counts. Sampling density is a visualization setting, kept out of the
    /// stored box for the same reason as [`SlicePlane::lattice`].
    pub fn lattice(&self, counts: UVec3) -> BoxLattice {
        let counts = counts.max(UVec3::ONE);
        let u = self.rotation * DVec3::X;
        let v = self.rotation * DVec3::Y;
        let w = self.rotation * DVec3::Z;
        let span = self.half_extent * 2.0;
        let divisor = (counts.as_dvec3() - DVec3::ONE).max(DVec3::ONE);
        let u_step = u * (span.x / divisor.x);
        let v_step = v * (span.y / divisor.y);
        let w_step = w * (span.z / divisor.z);
        let origin =
            self.origin - u * self.half_extent.x - v * self.half_extent.y - w * self.half_extent.z;
        BoxLattice::new(origin, u_step, v_step, w_step, counts)
    }
}

/// A bounded sphere in which a field is sampled and drawn as arrows.
///
/// A "crystal ball": position and radius only, no orientation to author.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldSphereSpec {
    name: String,
    origin: DVec3,
    radius: f64,
    visible: bool,
}

impl FieldSphereSpec {
    pub fn new(name: impl Into<String>, origin: DVec3, radius: f64) -> Result<Self, WorldError> {
        if !origin.is_finite() || !radius.is_finite() || radius <= 0.0 {
            return Err(WorldError::InvalidSphere);
        }
        Ok(Self {
            name: name.into(),
            origin,
            radius,
            visible: true,
        })
    }

    pub fn with_radius(mut self, radius: f64) -> Result<Self, WorldError> {
        if !radius.is_finite() || radius <= 0.0 {
            return Err(WorldError::InvalidSphere);
        }
        self.radius = radius;
        Ok(self)
    }

    pub fn with_origin(mut self, origin: DVec3) -> Result<Self, WorldError> {
        if !origin.is_finite() {
            return Err(WorldError::InvalidSphere);
        }
        self.origin = origin;
        Ok(self)
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn from_sphere(sphere: &FieldSphere) -> Self {
        Self {
            name: sphere.name.clone(),
            origin: sphere.origin,
            radius: sphere.radius,
            visible: sphere.visible,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldSphere {
    pub id: SphereId,
    pub name: String,
    pub origin: DVec3,
    pub radius: f64,
    pub visible: bool,
}

impl FieldSphere {
    /// A sampling lattice over the sphere's bounding cube at the requested
    /// per-axis count. Points outside the sphere are still evaluated —
    /// display culls them, per [`SphereLattice`] — which keeps this the same
    /// simple axis-aligned construction [`GridLattice`](crate::GridLattice)
    /// already uses rather than a second, variable-length geometry shape.
    pub fn lattice(&self, counts_per_axis: u32) -> SphereLattice {
        let counts_per_axis = counts_per_axis.max(1);
        let origin = self.origin - DVec3::splat(self.radius);
        let divisor = f64::from(counts_per_axis.saturating_sub(1)).max(1.0);
        let step = DVec3::splat(2.0 * self.radius / divisor);
        SphereLattice::new(
            origin,
            step,
            UVec3::splat(counts_per_axis),
            self.origin,
            self.radius,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProbePosition {
    World(DVec3),
    Attached { object: ObjectId, offset: DVec3 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeSpec {
    pub name: String,
    pub position: ProbePosition,
    pub channels: Vec<ChannelId>,
    /// Presentation visibility. Sampling/history remain active when hidden.
    pub visible: bool,
    /// How many samples of history to retain per channel.
    pub history_capacity: usize,
}

/// Default bounded probe history. Bounded because `CONTEXT.md` requires probe
/// buffers not to grow without limit during a long run.
pub const DEFAULT_PROBE_HISTORY: usize = 2_048;

impl ProbeSpec {
    pub fn at(name: impl Into<String>, position: DVec3, channels: Vec<ChannelId>) -> Self {
        Self {
            name: name.into(),
            position: ProbePosition::World(position),
            channels,
            visible: true,
            history_capacity: DEFAULT_PROBE_HISTORY,
        }
    }

    pub fn attached(
        name: impl Into<String>,
        object: ObjectId,
        offset: DVec3,
        channels: Vec<ChannelId>,
    ) -> Self {
        Self {
            name: name.into(),
            position: ProbePosition::Attached { object, offset },
            channels,
            visible: true,
            history_capacity: DEFAULT_PROBE_HISTORY,
        }
    }

    pub fn with_history_capacity(mut self, capacity: usize) -> Self {
        self.history_capacity = capacity.max(1);
        self
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub id: ProbeId,
    pub name: String,
    pub position: ProbePosition,
    pub channels: Vec<ChannelId>,
    pub visible: bool,
    pub history_capacity: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    revision: WorldRevision,
    objects: BTreeMap<ObjectId, WorldObject>,
    planes: BTreeMap<PlaneId, SlicePlane>,
    boxes: BTreeMap<BoxId, FieldBox>,
    spheres: BTreeMap<SphereId, FieldSphere>,
    probes: BTreeMap<ProbeId, Probe>,
    component_schemas: BTreeMap<ComponentTypeId, ComponentSchema>,
}

/// An immutable view of the world at one revision.
///
/// Every field a solver can read lives behind this type, so a solver cannot
/// observe half of an edit, and two views that report the same revision always
/// describe identical state — including registered schemas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot(Arc<WorldState>);

impl WorldSnapshot {
    pub fn revision(&self) -> WorldRevision {
        self.0.revision
    }

    pub fn objects(&self) -> &BTreeMap<ObjectId, WorldObject> {
        &self.0.objects
    }

    pub fn object(&self, id: ObjectId) -> Option<&WorldObject> {
        self.0.objects.get(&id)
    }

    pub fn planes(&self) -> &BTreeMap<PlaneId, SlicePlane> {
        &self.0.planes
    }

    pub fn boxes(&self) -> &BTreeMap<BoxId, FieldBox> {
        &self.0.boxes
    }

    pub fn field_box(&self, id: BoxId) -> Option<&FieldBox> {
        self.0.boxes.get(&id)
    }

    pub fn spheres(&self) -> &BTreeMap<SphereId, FieldSphere> {
        &self.0.spheres
    }

    pub fn sphere(&self, id: SphereId) -> Option<&FieldSphere> {
        self.0.spheres.get(&id)
    }

    pub fn probes(&self) -> &BTreeMap<ProbeId, Probe> {
        &self.0.probes
    }

    pub fn probe(&self, id: ProbeId) -> Option<&Probe> {
        self.0.probes.get(&id)
    }

    pub fn component_schemas(&self) -> &BTreeMap<ComponentTypeId, ComponentSchema> {
        &self.0.component_schemas
    }

    /// Objects carrying a given plugin component, with that component's values.
    pub fn objects_with(
        &self,
        component: &ComponentTypeId,
    ) -> impl Iterator<Item = (&WorldObject, &PropertyBag)> {
        self.0
            .objects
            .values()
            .filter_map(move |object| Some((object, object.components.get(component)?)))
    }

    pub fn resolve_probe_position(&self, probe: &Probe) -> Result<DVec3, WorldError> {
        let position = match probe.position {
            ProbePosition::World(position) => position,
            ProbePosition::Attached { object, offset } => {
                let object = self
                    .0
                    .objects
                    .get(&object)
                    .ok_or(WorldError::ObjectNotFound { id: object })?;
                object.transform.apply(offset)
            }
        };
        Ok(position)
    }
}

#[derive(Clone, Debug)]
pub struct World {
    state: Arc<WorldState>,
    counters: Counters,
}

/// World contents captured at one revision, for later restoration.
///
/// Opaque on purpose: it is a thing to hand back to [`World::restore`], not a
/// second way to read a world. Reading goes through [`WorldSnapshot`], which
/// carries the revision that identifies what is being read.
#[derive(Clone, Debug)]
pub struct WorldCheckpoint(Arc<WorldState>);

impl WorldCheckpoint {
    /// The revision these contents were captured at.
    ///
    /// Provenance, not identity: restoring them produces a later revision.
    pub fn captured_at(&self) -> WorldRevision {
        self.0.revision
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            state: Arc::new(WorldState {
                revision: WorldRevision::INITIAL,
                objects: BTreeMap::new(),
                planes: BTreeMap::new(),
                boxes: BTreeMap::new(),
                spheres: BTreeMap::new(),
                probes: BTreeMap::new(),
                component_schemas: BTreeMap::new(),
            }),
            counters: Counters::default(),
        }
    }

    pub fn snapshot(&self) -> WorldSnapshot {
        WorldSnapshot(Arc::clone(&self.state))
    }

    pub fn revision(&self) -> WorldRevision {
        self.state.revision
    }

    /// Capture the current contents so they can be restored later.
    ///
    /// Cheap, and deliberately so: the state is already reference-counted, so a
    /// checkpoint is a pointer rather than a copy of the scene. That is what
    /// makes it affordable to take one before every edit.
    pub fn checkpoint(&self) -> WorldCheckpoint {
        WorldCheckpoint(Arc::clone(&self.state))
    }

    /// Restore captured contents as a **new** revision.
    ///
    /// A revision is a point in this world's history, not a place to return to.
    /// Restoring contents that once existed still produces a revision that never
    /// existed before, so a value remains attributable to exactly one revision
    /// and a consumer that cached one is not handed different data under the
    /// same number.
    ///
    /// Identifier counters are not rewound either. Undoing the creation of an
    /// object frees nothing: the next object is a different object and gets a
    /// different identifier, so no consumer keyed by identifier — a probe
    /// attachment, a recorded history — can silently inherit a predecessor's
    /// past.
    ///
    /// Returns the revision now in force, which is unchanged if the checkpoint
    /// is already the current state.
    pub fn restore(&mut self, checkpoint: &WorldCheckpoint) -> WorldRevision {
        if Arc::ptr_eq(&self.state, &checkpoint.0) {
            return self.state.revision;
        }
        let mut restored = (*checkpoint.0).clone();
        restored.revision = self.state.revision.next();
        let revision = restored.revision;
        self.state = Arc::new(restored);
        revision
    }

    /// Apply a batch of commands as one atomic edit.
    ///
    /// Either every command succeeds and the world moves to exactly one new
    /// revision, or the world and its identifier counters are left untouched.
    pub fn commit(
        &mut self,
        commands: impl IntoIterator<Item = WorldCommand>,
    ) -> Result<CommitReport, WorldError> {
        let commands: Vec<_> = commands.into_iter().collect();
        if commands.is_empty() {
            return Ok(CommitReport::unchanged(self.state.revision));
        }

        let mut candidate = (*self.state).clone();
        let mut counters = self.counters;
        let mut report = CommitReport::unchanged(candidate.revision.next());
        for command in commands {
            apply_command(&mut candidate, &mut counters, &mut report, command)?;
        }
        candidate.revision = report.revision;
        self.state = Arc::new(candidate);
        self.counters = counters;
        Ok(report)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorldCommand {
    /// Declare a plugin's object-component schema.
    ///
    /// This is a command rather than a setter so that schema registration lands
    /// on a revision like any other edit. Two world views reporting the same
    /// revision must describe the same schemas, or a value's provenance is not
    /// attributable to that revision.
    RegisterComponentSchema(ComponentSchema),
    CreateObject(ObjectSpec),
    RemoveObject(ObjectId),
    SetObjectName {
        object: ObjectId,
        name: String,
    },
    SetTransform {
        object: ObjectId,
        transform: Transform,
    },
    SetVelocity {
        object: ObjectId,
        velocity: Velocity,
    },
    SetShape {
        object: ObjectId,
        shape: Option<ObjectShape>,
    },
    SetObjectVisible {
        object: ObjectId,
        visible: bool,
    },
    /// Hand authority over an object's motion to the user, or back to solvers.
    SetObjectPinned {
        object: ObjectId,
        pinned: bool,
    },
    AttachComponent {
        object: ObjectId,
        component: ComponentTypeId,
        properties: PropertyBag,
    },
    DetachComponent {
        object: ObjectId,
        component: ComponentTypeId,
    },
    CreatePlane(SlicePlaneSpec),
    SetPlaneName {
        plane: PlaneId,
        name: String,
    },
    SetPlane {
        plane: PlaneId,
        spec: SlicePlaneSpec,
    },
    SetPlaneVisible {
        plane: PlaneId,
        visible: bool,
    },
    RemovePlane(PlaneId),
    CreateBox(FieldBoxSpec),
    SetBoxName {
        region: BoxId,
        name: String,
    },
    SetBox {
        region: BoxId,
        spec: FieldBoxSpec,
    },
    SetBoxVisible {
        region: BoxId,
        visible: bool,
    },
    RemoveBox(BoxId),
    CreateSphere(FieldSphereSpec),
    SetSphereName {
        sphere: SphereId,
        name: String,
    },
    SetSphere {
        sphere: SphereId,
        spec: FieldSphereSpec,
    },
    SetSphereVisible {
        sphere: SphereId,
        visible: bool,
    },
    RemoveSphere(SphereId),
    CreateProbe(ProbeSpec),
    SetProbeName {
        probe: ProbeId,
        name: String,
    },
    SetProbePosition {
        probe: ProbeId,
        position: ProbePosition,
    },
    SetProbeChannels {
        probe: ProbeId,
        channels: Vec<ChannelId>,
    },
    SetProbeVisible {
        probe: ProbeId,
        visible: bool,
    },
    RemoveProbe(ProbeId),
}

impl WorldCommand {
    /// A short name for this edit, in the user's terms.
    ///
    /// Lives with the command rather than in the desktop because an undo entry,
    /// a history log, and a remote client's transaction list all need the same
    /// words for the same edit, and a command a plugin adds later should not be
    /// nameable in three places that can disagree.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::RegisterComponentSchema(_) => "Register component schema",
            Self::CreateObject(_) => "Add object",
            Self::RemoveObject(_) => "Remove object",
            Self::SetObjectName { .. } => "Rename object",
            Self::SetTransform { .. } => "Move object",
            Self::SetVelocity { .. } => "Set velocity",
            Self::SetShape { .. } => "Change shape",
            Self::SetObjectVisible { .. } => "Show or hide object",
            Self::SetObjectPinned { .. } => "Change motion authority",
            Self::AttachComponent { .. } => "Edit component",
            Self::DetachComponent { .. } => "Remove component",
            Self::CreatePlane(_) => "Add slice plane",
            Self::SetPlaneName { .. } => "Rename slice plane",
            Self::SetPlane { .. } => "Move slice plane",
            Self::SetPlaneVisible { .. } => "Show or hide slice plane",
            Self::RemovePlane(_) => "Remove slice plane",
            Self::CreateBox(_) => "Add field box",
            Self::SetBoxName { .. } => "Rename field box",
            Self::SetBox { .. } => "Move field box",
            Self::SetBoxVisible { .. } => "Show or hide field box",
            Self::RemoveBox(_) => "Remove field box",
            Self::CreateSphere(_) => "Add field sphere",
            Self::SetSphereName { .. } => "Rename field sphere",
            Self::SetSphere { .. } => "Move field sphere",
            Self::SetSphereVisible { .. } => "Show or hide field sphere",
            Self::RemoveSphere(_) => "Remove field sphere",
            Self::CreateProbe(_) => "Add probe",
            Self::SetProbeName { .. } => "Rename probe",
            Self::SetProbePosition { .. } => "Move probe",
            Self::SetProbeChannels { .. } => "Change recorded channels",
            Self::SetProbeVisible { .. } => "Show or hide probe",
            Self::RemoveProbe(_) => "Remove probe",
        }
    }

    /// Name a batch committed as one atomic edit.
    ///
    /// A transaction is one step to a user however many commands it took, so it
    /// is named after the first and counts the rest rather than listing them.
    pub fn batch_label(commands: &[Self]) -> String {
        match commands {
            [] => "Edit scene".to_owned(),
            [only] => only.label().to_owned(),
            [first, rest @ ..] => format!("{} and {} more", first.label(), rest.len()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReport {
    pub revision: WorldRevision,
    pub created_objects: Vec<ObjectId>,
    pub created_planes: Vec<PlaneId>,
    pub created_boxes: Vec<BoxId>,
    pub created_spheres: Vec<SphereId>,
    pub created_probes: Vec<ProbeId>,
}

impl CommitReport {
    fn unchanged(revision: WorldRevision) -> Self {
        Self {
            revision,
            created_objects: Vec::new(),
            created_planes: Vec::new(),
            created_boxes: Vec::new(),
            created_spheres: Vec::new(),
            created_probes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    object: u64,
    plane: u64,
    field_box: u64,
    sphere: u64,
    probe: u64,
}

impl Counters {
    fn next_object(&mut self) -> ObjectId {
        let id = ObjectId::new(self.object);
        self.object += 1;
        id
    }

    fn next_plane(&mut self) -> PlaneId {
        let id = PlaneId::new(self.plane);
        self.plane += 1;
        id
    }

    fn next_box(&mut self) -> BoxId {
        let id = BoxId::new(self.field_box);
        self.field_box += 1;
        id
    }

    fn next_sphere(&mut self) -> SphereId {
        let id = SphereId::new(self.sphere);
        self.sphere += 1;
        id
    }

    fn next_probe(&mut self) -> ProbeId {
        let id = ProbeId::new(self.probe);
        self.probe += 1;
        id
    }
}

fn object_mut(state: &mut WorldState, id: ObjectId) -> Result<&mut WorldObject, WorldError> {
    state
        .objects
        .get_mut(&id)
        .ok_or(WorldError::ObjectNotFound { id })
}

fn apply_command(
    state: &mut WorldState,
    counters: &mut Counters,
    report: &mut CommitReport,
    command: WorldCommand,
) -> Result<(), WorldError> {
    match command {
        WorldCommand::RegisterComponentSchema(schema) => {
            if state.component_schemas.contains_key(&schema.id) {
                return Err(WorldError::DuplicateComponentSchema { id: schema.id });
            }
            state.component_schemas.insert(schema.id.clone(), schema);
        }
        WorldCommand::CreateObject(spec) => {
            spec.validate()?;
            validate_object_components(state, &spec.components)?;
            let id = counters.next_object();
            state.objects.insert(
                id,
                WorldObject {
                    id,
                    name: spec.name,
                    transform: spec.transform.normalized(),
                    velocity: spec.velocity,
                    shape: spec.shape,
                    visible: spec.visible,
                    pinned: spec.pinned,
                    components: spec.components,
                },
            );
            report.created_objects.push(id);
        }
        WorldCommand::RemoveObject(id) => {
            if !state.objects.contains_key(&id) {
                return Err(WorldError::ObjectNotFound { id });
            }
            if state.probes.values().any(|probe| {
                matches!(probe.position, ProbePosition::Attached { object, .. } if object == id)
            }) {
                return Err(WorldError::ObjectHasAttachedProbe { id });
            }
            state.objects.remove(&id);
        }
        WorldCommand::SetObjectName { object, name } => {
            object_mut(state, object)?.name = name;
        }
        WorldCommand::SetTransform { object, transform } => {
            transform.validate()?;
            object_mut(state, object)?.transform = transform.normalized();
        }
        WorldCommand::SetVelocity { object, velocity } => {
            velocity.validate()?;
            object_mut(state, object)?.velocity = velocity;
        }
        WorldCommand::SetShape { object, shape } => {
            if let Some(shape) = shape {
                shape.validate()?;
            }
            object_mut(state, object)?.shape = shape;
        }
        WorldCommand::SetObjectVisible { object, visible } => {
            object_mut(state, object)?.visible = visible;
        }
        WorldCommand::SetObjectPinned { object, pinned } => {
            object_mut(state, object)?.pinned = pinned;
        }
        WorldCommand::AttachComponent {
            object,
            component,
            properties,
        } => {
            let schema = state.component_schemas.get(&component).ok_or_else(|| {
                WorldError::ComponentSchemaNotFound {
                    id: component.clone(),
                }
            })?;
            schema.validate(&properties)?;
            object_mut(state, object)?
                .components
                .insert(component, properties);
        }
        WorldCommand::DetachComponent { object, component } => {
            let object = object_mut(state, object)?;
            if object.components.remove(&component).is_none() {
                return Err(WorldError::ComponentNotAttached { id: component });
            }
        }
        WorldCommand::CreatePlane(spec) => {
            // `SlicePlaneSpec` cannot be constructed unvalidated, so there is no
            // second copy of the plane predicate here.
            let id = counters.next_plane();
            state.planes.insert(
                id,
                SlicePlane {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    normal: spec.normal,
                    u_axis: spec.u_axis,
                    half_extent: spec.half_extent,
                    visible: spec.visible,
                },
            );
            report.created_planes.push(id);
        }
        WorldCommand::SetPlaneName { plane, name } => {
            state
                .planes
                .get_mut(&plane)
                .ok_or(WorldError::PlaneNotFound { id: plane })?
                .name = name;
        }
        WorldCommand::SetPlaneVisible { plane, visible } => {
            state
                .planes
                .get_mut(&plane)
                .ok_or(WorldError::PlaneNotFound { id: plane })?
                .visible = visible;
        }
        WorldCommand::SetPlane { plane, spec } => {
            let current = state
                .planes
                .get_mut(&plane)
                .ok_or(WorldError::PlaneNotFound { id: plane })?;
            *current = SlicePlane {
                id: plane,
                name: spec.name,
                origin: spec.origin,
                normal: spec.normal,
                u_axis: spec.u_axis,
                half_extent: spec.half_extent,
                visible: spec.visible,
            };
        }
        WorldCommand::RemovePlane(id) => {
            if state.planes.remove(&id).is_none() {
                return Err(WorldError::PlaneNotFound { id });
            }
        }
        WorldCommand::CreateBox(spec) => {
            // `FieldBoxSpec` cannot be constructed unvalidated, so there is no
            // second copy of the box predicate here.
            let id = counters.next_box();
            state.boxes.insert(
                id,
                FieldBox {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    rotation: spec.rotation,
                    half_extent: spec.half_extent,
                    visible: spec.visible,
                },
            );
            report.created_boxes.push(id);
        }
        WorldCommand::SetBoxName { region, name } => {
            state
                .boxes
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?
                .name = name;
        }
        WorldCommand::SetBoxVisible { region, visible } => {
            state
                .boxes
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?
                .visible = visible;
        }
        WorldCommand::SetBox { region, spec } => {
            let current = state
                .boxes
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?;
            *current = FieldBox {
                id: region,
                name: spec.name,
                origin: spec.origin,
                rotation: spec.rotation,
                half_extent: spec.half_extent,
                visible: spec.visible,
            };
        }
        WorldCommand::RemoveBox(id) => {
            if state.boxes.remove(&id).is_none() {
                return Err(WorldError::BoxNotFound { id });
            }
        }
        WorldCommand::CreateSphere(spec) => {
            let id = counters.next_sphere();
            state.spheres.insert(
                id,
                FieldSphere {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    radius: spec.radius,
                    visible: spec.visible,
                },
            );
            report.created_spheres.push(id);
        }
        WorldCommand::SetSphereName { sphere, name } => {
            state
                .spheres
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?
                .name = name;
        }
        WorldCommand::SetSphereVisible { sphere, visible } => {
            state
                .spheres
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?
                .visible = visible;
        }
        WorldCommand::SetSphere { sphere, spec } => {
            let current = state
                .spheres
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?;
            *current = FieldSphere {
                id: sphere,
                name: spec.name,
                origin: spec.origin,
                radius: spec.radius,
                visible: spec.visible,
            };
        }
        WorldCommand::RemoveSphere(id) => {
            if state.spheres.remove(&id).is_none() {
                return Err(WorldError::SphereNotFound { id });
            }
        }
        WorldCommand::CreateProbe(spec) => {
            validate_probe(state, &spec)?;
            let id = counters.next_probe();
            state.probes.insert(
                id,
                Probe {
                    id,
                    name: spec.name,
                    position: spec.position,
                    channels: spec.channels,
                    visible: spec.visible,
                    history_capacity: spec.history_capacity.max(1),
                },
            );
            report.created_probes.push(id);
        }
        WorldCommand::SetProbeName { probe, name } => {
            state
                .probes
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?
                .name = name;
        }
        WorldCommand::SetProbePosition { probe, position } => {
            let probe_spec = ProbeSpec {
                name: String::new(),
                position: position.clone(),
                channels: Vec::new(),
                visible: true,
                history_capacity: 1,
            };
            validate_probe(state, &probe_spec)?;
            state
                .probes
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?
                .position = position;
        }
        WorldCommand::SetProbeChannels { probe, channels } => {
            let probe = state
                .probes
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?;
            probe.channels = channels;
        }
        WorldCommand::SetProbeVisible { probe, visible } => {
            state
                .probes
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?
                .visible = visible;
        }
        WorldCommand::RemoveProbe(id) => {
            if state.probes.remove(&id).is_none() {
                return Err(WorldError::ProbeNotFound { id });
            }
        }
    }
    Ok(())
}

fn validate_object_components(
    state: &WorldState,
    components: &BTreeMap<ComponentTypeId, PropertyBag>,
) -> Result<(), WorldError> {
    for (id, properties) in components {
        let schema = state
            .component_schemas
            .get(id)
            .ok_or_else(|| WorldError::ComponentSchemaNotFound { id: id.clone() })?;
        schema.validate(properties)?;
    }
    Ok(())
}

fn validate_probe(state: &WorldState, probe: &ProbeSpec) -> Result<(), WorldError> {
    let position = match probe.position {
        ProbePosition::World(position) => position,
        ProbePosition::Attached { object, offset } => {
            if !state.objects.contains_key(&object) {
                return Err(WorldError::ObjectNotFound { id: object });
            }
            offset
        }
    };
    if !position.is_finite() {
        return Err(WorldError::InvalidProbe);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum WorldError {
    #[error("transform contains non-finite values or an invalid rotation")]
    InvalidTransform,
    #[error("velocity contains non-finite values")]
    InvalidVelocity,
    #[error("object shape requires finite, non-negative dimensions")]
    InvalidShape,
    #[error("slice plane requires a finite origin, non-zero normal, and positive extent")]
    InvalidPlane,
    #[error("field box requires a finite origin, unit rotation, and positive half extent")]
    InvalidBox,
    #[error("field sphere requires a finite origin and a positive radius")]
    InvalidSphere,
    #[error("probe position must be finite")]
    InvalidProbe,
    #[error("object {id} does not exist")]
    ObjectNotFound { id: ObjectId },
    #[error("object {id} still has an attached probe")]
    ObjectHasAttachedProbe { id: ObjectId },
    #[error("plane {id} does not exist")]
    PlaneNotFound { id: PlaneId },
    #[error("field box {id} does not exist")]
    BoxNotFound { id: BoxId },
    #[error("field sphere {id} does not exist")]
    SphereNotFound { id: SphereId },
    #[error("probe {id} does not exist")]
    ProbeNotFound { id: ProbeId },
    #[error("component schema '{id}' is not registered")]
    ComponentSchemaNotFound { id: ComponentTypeId },
    #[error("component schema '{id}' is already registered")]
    DuplicateComponentSchema { id: ComponentTypeId },
    #[error("component '{id}' is not attached")]
    ComponentNotAttached { id: ComponentTypeId },
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Dimension, PluginId, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity,
    };

    fn charge_component() -> ComponentSchema {
        ComponentSchema {
            id: ComponentTypeId::new(PluginId::new("test").unwrap(), "charge").unwrap(),
            display_name: "Charge".to_owned(),
            properties: vec![PropertySchema {
                id: PropertyId::new("charge").unwrap(),
                display_name: "Charge".to_owned(),
                kind: PropertyKind::Scalar(Dimension::CHARGE),
                required: true,
                default_value: None,
                relevant_when: None,
            }],
        }
    }

    #[test]
    fn a_batch_commits_at_one_revision() {
        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("source")),
                WorldCommand::CreateProbe(ProbeSpec::at("probe", DVec3::X, Vec::new())),
            ])
            .unwrap();

        assert_eq!(report.revision.get(), 1);
        assert_eq!(world.snapshot().objects().len(), 1);
        assert_eq!(world.snapshot().probes().len(), 1);
    }

    #[test]
    fn renaming_an_object_preserves_its_identity() {
        let mut world = World::new();
        let id = world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("before"))])
            .unwrap()
            .created_objects[0];

        world
            .commit([WorldCommand::SetObjectName {
                object: id,
                name: "after".to_owned(),
            }])
            .unwrap();

        let snapshot = world.snapshot();
        let object = snapshot.object(id).unwrap();
        assert_eq!(object.id, id);
        assert_eq!(object.name, "after");
    }

    #[test]
    fn a_failed_batch_is_atomic_and_does_not_consume_ids() {
        let mut world = World::new();
        let missing = ObjectId::new(99);
        assert!(
            world
                .commit([
                    WorldCommand::CreateObject(ObjectSpec::new("not committed")),
                    WorldCommand::RemoveObject(missing),
                ])
                .is_err()
        );
        assert_eq!(world.snapshot().revision(), WorldRevision::INITIAL);
        assert!(world.snapshot().objects().is_empty());

        let report = world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("first"))])
            .unwrap();
        assert_eq!(report.created_objects, vec![ObjectId::new(0)]);
    }

    #[test]
    fn schema_registration_advances_the_revision() {
        let mut world = World::new();
        let before = world.snapshot();

        world
            .commit([WorldCommand::RegisterComponentSchema(charge_component())])
            .unwrap();
        let after = world.snapshot();

        // Two views must never report the same revision while describing
        // different schemas.
        assert_ne!(before.revision(), after.revision());
        assert!(before.component_schemas().is_empty());
        assert_eq!(after.component_schemas().len(), 1);
    }

    #[test]
    fn attaching_an_undimensioned_component_fails_without_changing_the_world() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component()),
                WorldCommand::CreateObject(ObjectSpec::new("source")),
            ])
            .unwrap();
        let revision = world.revision();

        let wrong_dimension: PropertyBag = [(
            PropertyId::new("charge").unwrap(),
            PropertyValue::Scalar(Quantity::new(2.0, Dimension::MASS).unwrap()),
        )]
        .into_iter()
        .collect();

        assert!(
            world
                .commit([WorldCommand::AttachComponent {
                    object: ObjectId::new(0),
                    component: charge_component().id,
                    properties: wrong_dimension,
                }])
                .is_err()
        );
        assert_eq!(world.revision(), revision);
    }

    #[test]
    fn removing_an_object_with_an_attached_probe_leaves_it_in_place() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::attached(
                "attached",
                ObjectId::new(0),
                DVec3::ZERO,
                Vec::new(),
            ))])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 1);
    }

    #[test]
    fn plane_lattice_covers_the_declared_extent() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_half_extent(DVec2::splat(2.0))
                    .unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let lattice = plane.lattice(UVec2::new(3, 3));

        let (u, v) = plane.basis();
        assert!((u.dot(plane.normal)).abs() < 1.0e-12);
        assert!((u.cross(v).dot(plane.normal) - 1.0).abs() < 1.0e-12);
        assert_eq!(lattice.len(), 9);
        // Corner to corner spans the full 4 m extent, and the centre sample sits
        // on the plane origin.
        assert!((lattice.position(4).unwrap() - plane.origin).length() < 1.0e-12);
        assert!(
            (lattice.position(8).unwrap() - lattice.position(0).unwrap()).length()
                - (4.0_f64 * 2.0_f64.sqrt())
                < 1.0e-9
        );
    }

    #[test]
    fn sampling_density_does_not_change_the_plane() {
        let plane = SlicePlane {
            id: PlaneId::new(0),
            name: "xy".to_owned(),
            origin: DVec3::ZERO,
            normal: DVec3::Z,
            u_axis: DVec3::X,
            half_extent: DVec2::splat(1.0),
            visible: true,
        };

        let coarse = plane.lattice(UVec2::new(2, 2));
        let fine = plane.lattice(UVec2::new(16, 16));

        assert_eq!(coarse.position(0).unwrap(), fine.position(0).unwrap());
        assert_eq!(coarse.len(), 4);
        assert_eq!(fine.len(), 256);
    }

    #[test]
    fn a_plane_edit_replaces_its_geometry_at_one_revision() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let plane = created.created_planes[0];
        let before = world.revision();
        let replacement = SlicePlaneSpec::new("tilted", DVec3::new(1.0, 2.0, 3.0), DVec3::Y)
            .unwrap()
            .with_half_extent(DVec2::new(4.0, 2.0))
            .unwrap()
            .hidden();

        let report = world
            .commit([WorldCommand::SetPlane {
                plane,
                spec: replacement,
            }])
            .unwrap();
        let edited = world.snapshot().planes().get(&plane).unwrap().clone();

        assert_eq!(report.revision, before.next());
        assert_eq!(edited.id, plane);
        assert_eq!(edited.name, "tilted");
        assert_eq!(edited.origin, DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(edited.normal, DVec3::Y);
        assert_eq!(edited.half_extent, DVec2::new(4.0, 2.0));
        assert!(!edited.visible);
    }

    #[test]
    fn an_invalid_probe_move_is_atomic() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "probe",
                DVec3::X,
                Vec::new(),
            ))])
            .unwrap();
        let probe = created.created_probes[0];
        let before = world.snapshot();

        let result = world.commit([WorldCommand::SetProbePosition {
            probe,
            position: ProbePosition::World(DVec3::new(f64::NAN, 0.0, 0.0)),
        }]);

        assert_eq!(result, Err(WorldError::InvalidProbe));
        assert_eq!(world.revision(), before.revision());
        assert_eq!(world.snapshot().probe(probe), before.probe(probe));
    }

    #[test]
    fn probe_recording_channels_change_at_one_world_revision() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "probe",
                DVec3::ZERO,
                Vec::new(),
            ))])
            .unwrap();
        let probe = created.created_probes[0];
        let channel = ChannelId::new(crate::PluginId::new("test").unwrap(), "field").unwrap();

        let report = world
            .commit([WorldCommand::SetProbeChannels {
                probe,
                channels: vec![channel.clone()],
            }])
            .unwrap();

        assert_eq!(world.revision(), report.revision);
        assert_eq!(
            world.snapshot().probe(probe).unwrap().channels,
            vec![channel]
        );
    }

    #[test]
    fn viewport_visibility_is_revisioned_without_removing_entities() {
        let mut world = World::new();
        let created = world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("source")),
                WorldCommand::CreateProbe(ProbeSpec::at("probe", DVec3::ZERO, Vec::new())),
            ])
            .unwrap();
        let object = created.created_objects[0];
        let probe = created.created_probes[0];
        let before = world.revision();

        world
            .commit([
                WorldCommand::SetObjectVisible {
                    object,
                    visible: false,
                },
                WorldCommand::SetProbeVisible {
                    probe,
                    visible: false,
                },
            ])
            .unwrap();
        let snapshot = world.snapshot();

        assert_eq!(snapshot.revision(), before.next());
        assert!(!snapshot.object(object).unwrap().visible);
        assert!(!snapshot.probe(probe).unwrap().visible);
        assert_eq!(snapshot.objects().len(), 1);
        assert_eq!(snapshot.probes().len(), 1);
    }

    #[test]
    fn a_denormalised_rotation_cannot_scale_an_attached_probe_offset() {
        let mut world = World::new();
        // Built literally rather than through `Transform::new`, which is the
        // only reason a non-unit quaternion can reach a command at all.
        let stretched = Transform {
            translation: DVec3::ZERO,
            rotation: DQuat::from_xyzw(0.0, 0.0, 0.0, 3.0),
        };
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source").with_transform(stretched),
            )])
            .unwrap();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::attached(
                "offset",
                ObjectId::new(0),
                DVec3::X,
                Vec::new(),
            ))])
            .unwrap();

        let snapshot = world.snapshot();
        let probe = snapshot.probes().values().next().unwrap();
        let position = snapshot.resolve_probe_position(probe).unwrap();

        // A length-3 quaternion would have placed the probe nine metres out.
        assert!((position - DVec3::X).length() < 1.0e-12);

        world
            .commit([WorldCommand::SetTransform {
                object: ObjectId::new(0),
                transform: stretched,
            }])
            .unwrap();
        let snapshot = world.snapshot();
        let probe = snapshot.probes().values().next().unwrap();
        assert!((snapshot.resolve_probe_position(probe).unwrap() - DVec3::X).length() < 1.0e-12);
    }

    #[test]
    fn attached_probes_follow_object_rotation() {
        let mut world = World::new();
        let rotation = DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2);
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::new(DVec3::new(1.0, 0.0, 0.0), rotation).unwrap()),
            )])
            .unwrap();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::attached(
                "offset",
                ObjectId::new(0),
                DVec3::X,
                Vec::new(),
            ))])
            .unwrap();

        let snapshot = world.snapshot();
        let probe = snapshot.probes().values().next().unwrap();
        let position = snapshot.resolve_probe_position(probe).unwrap();

        assert!((position - DVec3::new(1.0, 1.0, 0.0)).length() < 1.0e-12);
    }

    #[test]
    fn box_lattice_covers_the_declared_extent() {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(2.0)).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let field_box = snapshot.boxes().get(&report.created_boxes[0]).unwrap();
        let lattice = field_box.lattice(UVec3::new(3, 3, 3));

        assert_eq!(lattice.len(), 27);
        // The centre sample sits on the box origin, and the lattice spans
        // corner to corner across the declared extent.
        assert!((lattice.position(13).unwrap() - field_box.origin).length() < 1.0e-12);
        assert!(
            (lattice.position(0).unwrap() - DVec3::splat(-2.0)).length() < 1.0e-9,
            "unrotated box lattice starts at -half_extent"
        );
        assert!(
            (lattice.position(26).unwrap() - DVec3::splat(2.0)).length() < 1.0e-9,
            "unrotated box lattice ends at +half_extent"
        );
    }

    #[test]
    fn a_box_edit_replaces_its_geometry_at_one_revision() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(1.0)).unwrap(),
            )])
            .unwrap();
        let region = created.created_boxes[0];
        let before = world.revision();
        let rotation = DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2);
        let replacement = FieldBoxSpec::new(
            "tilted",
            DVec3::new(1.0, 2.0, 3.0),
            DVec3::new(4.0, 2.0, 1.0),
        )
        .unwrap()
        .with_rotation(rotation)
        .unwrap()
        .hidden();

        let report = world
            .commit([WorldCommand::SetBox {
                region,
                spec: replacement,
            }])
            .unwrap();
        let edited = world.snapshot().boxes().get(&region).unwrap().clone();

        assert_eq!(report.revision, before.next());
        assert_eq!(edited.id, region);
        assert_eq!(edited.name, "tilted");
        assert_eq!(edited.origin, DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(edited.rotation, rotation);
        assert_eq!(edited.half_extent, DVec3::new(4.0, 2.0, 1.0));
        assert!(!edited.visible);
    }

    #[test]
    fn a_failed_box_batch_is_atomic_and_does_not_consume_ids() {
        let mut world = World::new();
        let missing = BoxId::new(99);
        assert!(
            world
                .commit([
                    WorldCommand::CreateBox(
                        FieldBoxSpec::new("not committed", DVec3::ZERO, DVec3::ONE).unwrap()
                    ),
                    WorldCommand::RemoveBox(missing),
                ])
                .is_err()
        );
        assert_eq!(world.snapshot().revision(), WorldRevision::INITIAL);
        assert!(world.snapshot().boxes().is_empty());

        let report = world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("first", DVec3::ZERO, DVec3::ONE).unwrap(),
            )])
            .unwrap();
        assert_eq!(report.created_boxes, vec![BoxId::new(0)]);
    }

    #[test]
    fn sphere_lattice_covers_the_bounding_cube() {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateSphere(
                FieldSphereSpec::new("ball", DVec3::ZERO, 2.0).unwrap(),
            )])
            .unwrap();
        let snapshot = world.snapshot();
        let sphere = snapshot.spheres().get(&report.created_spheres[0]).unwrap();
        let lattice = sphere.lattice(3);

        assert_eq!(lattice.len(), 27);
        assert_eq!(lattice.radius(), 2.0);
        assert_eq!(lattice.centre(), DVec3::ZERO);
        assert!((lattice.position(13).unwrap() - sphere.origin).length() < 1.0e-12);
        assert!((lattice.position(0).unwrap() - DVec3::splat(-2.0)).length() < 1.0e-9);
        assert!((lattice.position(26).unwrap() - DVec3::splat(2.0)).length() < 1.0e-9);
    }

    #[test]
    fn a_sphere_edit_replaces_its_geometry_at_one_revision() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreateSphere(
                FieldSphereSpec::new("ball", DVec3::ZERO, 1.0).unwrap(),
            )])
            .unwrap();
        let sphere = created.created_spheres[0];
        let before = world.revision();
        let replacement = FieldSphereSpec::new("moved", DVec3::new(1.0, 2.0, 3.0), 4.0)
            .unwrap()
            .hidden();

        let report = world
            .commit([WorldCommand::SetSphere {
                sphere,
                spec: replacement,
            }])
            .unwrap();
        let edited = world.snapshot().spheres().get(&sphere).unwrap().clone();

        assert_eq!(report.revision, before.next());
        assert_eq!(edited.id, sphere);
        assert_eq!(edited.name, "moved");
        assert_eq!(edited.origin, DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(edited.radius, 4.0);
        assert!(!edited.visible);
    }

    #[test]
    fn a_failed_sphere_batch_is_atomic_and_does_not_consume_ids() {
        let mut world = World::new();
        let missing = SphereId::new(99);
        assert!(
            world
                .commit([
                    WorldCommand::CreateSphere(
                        FieldSphereSpec::new("not committed", DVec3::ZERO, 1.0).unwrap()
                    ),
                    WorldCommand::RemoveSphere(missing),
                ])
                .is_err()
        );
        assert_eq!(world.snapshot().revision(), WorldRevision::INITIAL);
        assert!(world.snapshot().spheres().is_empty());

        let report = world
            .commit([WorldCommand::CreateSphere(
                FieldSphereSpec::new("first", DVec3::ZERO, 1.0).unwrap(),
            )])
            .unwrap();
        assert_eq!(report.created_spheres, vec![SphereId::new(0)]);
    }
}
