use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use glam::{DQuat, DVec2, DVec3, UVec2, UVec3};
use serde::{Deserialize, Serialize};

use crate::{
    BoxId, BoxLattice, ChannelId, ComponentSchema, ComponentTypeId, DistanceProbeId,
    MassAggregateProbeId, ObjectId, PlaneId, PlaneLattice, ProbeId, PropertyBag, SchemaError,
    SphereId, SphereLattice, WorldRevision,
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
    /// Whether this is a runtime-owned object rather than an authored one.
    ///
    /// See [`WorldObject::derived`]. Not exposed through any builder a normal
    /// caller would reach for — only the simulation runtime itself sets this,
    /// via [`Self::derived`].
    pub derived: bool,
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
            derived: false,
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

    /// Mark this object as runtime-owned — see [`WorldObject::derived`].
    pub fn derived(mut self) -> Self {
        self.derived = true;
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
    /// Whether the simulation runtime owns this object rather than a user.
    ///
    /// A derived object (the universe's centre of mass, say) is computed
    /// every publish, not authored — it can still be a valid [`attached_to`]
    /// target for a plane/box/sphere/distance probe like any other object,
    /// but the desktop UI never lists, selects, or offers to move or delete
    /// it, and [`WorldCommand::RemoveObject`] rejects it outright. Its
    /// transform is still set through the ordinary [`WorldCommand::SetTransform`]
    /// path — there is no caller-identity concept to distinguish the runtime
    /// from an external client, so that one command is deliberately left
    /// unguarded; a stray external write is simply overwritten on the next
    /// publish.
    ///
    /// [`attached_to`]: crate::SlicePlane::attached_to
    pub derived: bool,
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
    attached_to: Option<ObjectId>,
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
            attached_to: None,
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

    /// Attach to `object`: `origin`/`normal`/`u_axis` are then interpreted as
    /// local to that object's frame rather than world space.
    pub fn with_attached_to(mut self, object: ObjectId) -> Self {
        self.attached_to = Some(object);
        self
    }

    /// Clear any attachment; `origin`/`normal`/`u_axis` return to being
    /// world-space.
    pub fn detached(mut self) -> Self {
        self.attached_to = None;
        self
    }

    /// Read back what a builder chain (or a deserialized command) settled
    /// on — needed by a consumer that only has the `WorldCommand` this spec
    /// travelled in, not the entity it will replace (a not-yet-applied
    /// preview, for instance).
    pub fn origin(&self) -> DVec3 {
        self.origin
    }

    pub fn normal(&self) -> DVec3 {
        self.normal
    }

    pub fn u_axis(&self) -> DVec3 {
        self.u_axis
    }

    pub fn half_extent(&self) -> DVec2 {
        self.half_extent
    }

    pub fn attached_to(&self) -> Option<ObjectId> {
        self.attached_to
    }

    pub fn from_plane(plane: &SlicePlane) -> Self {
        Self {
            name: plane.name.clone(),
            origin: plane.origin,
            normal: plane.normal,
            u_axis: plane.u_axis,
            half_extent: plane.half_extent,
            visible: plane.visible,
            attached_to: plane.attached_to,
        }
    }

    /// Every field's own constructor invariant, re-checked. Needed because
    /// this type's fields are private but it derives `Deserialize` — a
    /// value can reach `apply_command` by deserializing caller-supplied
    /// JSON directly (`commit_world`), bypassing every constructor above.
    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        if !self.origin.is_finite()
            || !is_usable_direction(self.normal)
            || !self.half_extent.is_finite()
            || self.half_extent.min_element() <= 0.0
        {
            return Err(WorldError::InvalidPlane);
        }
        let normal = self.normal.normalize();
        let projected_u = self.u_axis - normal * self.u_axis.dot(normal);
        if !is_usable_direction(projected_u) {
            return Err(WorldError::InvalidPlane);
        }
        Ok(())
    }

    /// `normal` to unit length, `u_axis` to the same orthonormal-to-`normal`
    /// basis every safe constructor already guarantees. Only meaningful
    /// once [`Self::validate`] has passed — same split as
    /// [`Transform::validate`]/[`Transform::normalized`].
    pub(crate) fn normalized(self) -> Self {
        let normal = self.normal.normalize();
        let u_axis = (self.u_axis - normal * self.u_axis.dot(normal)).normalize();
        Self {
            normal,
            u_axis,
            ..self
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
    /// When set, `origin`/`normal`/`u_axis` are local to this object's frame
    /// rather than world space — see [`WorldSnapshot::resolve_plane_frame`].
    pub attached_to: Option<ObjectId>,
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
    attached_to: Option<ObjectId>,
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
            attached_to: None,
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

    /// Attach to `object`: `origin`/`rotation` are then interpreted as local
    /// to that object's frame rather than world space.
    pub fn with_attached_to(mut self, object: ObjectId) -> Self {
        self.attached_to = Some(object);
        self
    }

    /// Clear any attachment; `origin`/`rotation` return to being world-space.
    pub fn detached(mut self) -> Self {
        self.attached_to = None;
        self
    }

    /// Read back what a builder chain (or a deserialized command) settled
    /// on — see [`SlicePlaneSpec::origin`].
    pub fn origin(&self) -> DVec3 {
        self.origin
    }

    pub fn rotation(&self) -> DQuat {
        self.rotation
    }

    pub fn half_extent(&self) -> DVec3 {
        self.half_extent
    }

    pub fn attached_to(&self) -> Option<ObjectId> {
        self.attached_to
    }

    pub fn from_box(field_box: &FieldBox) -> Self {
        Self {
            name: field_box.name.clone(),
            origin: field_box.origin,
            rotation: field_box.rotation,
            half_extent: field_box.half_extent,
            visible: field_box.visible,
            attached_to: field_box.attached_to,
        }
    }

    /// Every field's own constructor invariant, re-checked — see
    /// [`SlicePlaneSpec::validate`] for why this is needed despite private
    /// fields.
    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        if !self.origin.is_finite()
            || !self.half_extent.is_finite()
            || self.half_extent.min_element() <= 0.0
            || !self.rotation.is_finite()
            || self.rotation.length_squared() == 0.0
        {
            return Err(WorldError::InvalidBox);
        }
        Ok(())
    }

    /// `rotation` to unit length. Only meaningful once [`Self::validate`]
    /// has passed.
    pub(crate) fn normalized(self) -> Self {
        Self {
            rotation: self.rotation.normalize(),
            ..self
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
    /// When set, `origin`/`rotation` are local to this object's frame rather
    /// than world space — see [`WorldSnapshot::resolve_box_frame`].
    pub attached_to: Option<ObjectId>,
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
    attached_to: Option<ObjectId>,
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
            attached_to: None,
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

    /// Attach to `object`: `origin` is then interpreted as local to that
    /// object's frame rather than world space.
    pub fn with_attached_to(mut self, object: ObjectId) -> Self {
        self.attached_to = Some(object);
        self
    }

    /// Clear any attachment; `origin` returns to being world-space.
    pub fn detached(mut self) -> Self {
        self.attached_to = None;
        self
    }

    /// Read back what a builder chain (or a deserialized command) settled
    /// on — see [`SlicePlaneSpec::origin`].
    pub fn origin(&self) -> DVec3 {
        self.origin
    }

    pub fn radius(&self) -> f64 {
        self.radius
    }

    pub fn attached_to(&self) -> Option<ObjectId> {
        self.attached_to
    }

    pub fn from_sphere(sphere: &FieldSphere) -> Self {
        Self {
            name: sphere.name.clone(),
            origin: sphere.origin,
            radius: sphere.radius,
            visible: sphere.visible,
            attached_to: sphere.attached_to,
        }
    }

    /// Every field's own constructor invariant, re-checked — see
    /// [`SlicePlaneSpec::validate`] for why this is needed despite private
    /// fields.
    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        if !self.origin.is_finite() || !self.radius.is_finite() || self.radius <= 0.0 {
            return Err(WorldError::InvalidSphere);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldSphere {
    pub id: SphereId,
    pub name: String,
    pub origin: DVec3,
    pub radius: f64,
    pub visible: bool,
    /// When set, `origin` is local to this object's frame rather than world
    /// space — see [`WorldSnapshot::resolve_sphere_origin`].
    pub attached_to: Option<ObjectId>,
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

/// Default bounded per-object kinematics history (see `fieldcad_simulation::
/// BodyHistory`). Same reasoning as `DEFAULT_PROBE_HISTORY`: bounded so a long
/// run doesn't grow a buffer without limit.
pub const DEFAULT_BODY_HISTORY: usize = 2_048;

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

/// A pure geometric measurement — the distance between two objects' live
/// positions. Kept separate from [`Probe`], which always means "sample field
/// channels at a point": a distance has no [`ChannelId`] and no plugin
/// behind it, so it doesn't fit that shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceProbeSpec {
    name: String,
    object_a: ObjectId,
    object_b: ObjectId,
    visible: bool,
    #[serde(default = "default_true")]
    show_line: bool,
}

impl DistanceProbeSpec {
    pub fn new(name: impl Into<String>, object_a: ObjectId, object_b: ObjectId) -> Self {
        Self {
            name: name.into(),
            object_a,
            object_b,
            visible: true,
            show_line: true,
        }
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn object_a(&self) -> ObjectId {
        self.object_a
    }

    pub fn object_b(&self) -> ObjectId {
        self.object_b
    }

    pub fn from_distance_probe(probe: &DistanceProbe) -> Self {
        Self {
            name: probe.name.clone(),
            object_a: probe.object_a,
            object_b: probe.object_b,
            visible: probe.visible,
            show_line: probe.show_line,
        }
    }

    /// The two objects must be distinct, or this always reads zero.
    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        if self.object_a == self.object_b {
            return Err(WorldError::InvalidDistanceProbe);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceProbe {
    pub id: DistanceProbeId,
    pub name: String,
    pub object_a: ObjectId,
    pub object_b: ObjectId,
    pub visible: bool,
    /// Whether a dashed line between `object_a` and `object_b` is drawn
    /// whenever this probe is visible. Purely a display preference — never
    /// affects the measured distance itself.
    #[serde(default = "default_true")]
    pub show_line: bool,
}

/// Which mass-bearing objects a [`MassAggregateProbe`] sums over.
///
/// `Universe` follows every mass-bearing object except the ones named in
/// `excluded`; `Selection` follows only the objects named in `included`. An
/// id that stops carrying mass, or is deleted, simply drops out of the
/// computed aggregate — membership here is a *reference*, not a claim that
/// the object currently exists or is dynamic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MassSelection {
    Universe { excluded: BTreeSet<ObjectId> },
    Selection { included: BTreeSet<ObjectId> },
}

impl MassSelection {
    /// Whether `id` counts toward this selection's aggregate. Used by
    /// `fieldcad_dynamics::mass_aggregate`'s per-body filter, so the
    /// computation crate never has to duplicate this match.
    pub fn includes(&self, id: ObjectId) -> bool {
        match self {
            Self::Universe { excluded } => !excluded.contains(&id),
            Self::Selection { included } => included.contains(&id),
        }
    }
}

/// A live centre-of-mass/momentum/energy measurement over a set of
/// mass-bearing objects — the generalised, user-added counterpart to the
/// old hidden, whole-universe-only "Center of mass" object. Kept separate
/// from [`Probe`] for the same reason [`DistanceProbe`] is: this has no
/// [`ChannelId`] or plugin behind it, it's a pure computed quantity (see
/// `fieldcad_dynamics::mass_aggregate`).
/// `#[serde(default = "default_true")]` for a display-toggle field added
/// after its owning probe type already shipped: a document saved before the
/// field existed has no key for it in its JSON at all, and defaulting to
/// "on" keeps the visual behaving the way it did implicitly before the field
/// existed to turn it off.
fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MassAggregateProbeSpec {
    pub name: String,
    pub selection: MassSelection,
    pub visible: bool,
    #[serde(default = "default_true")]
    pub show_member_lines: bool,
}

impl MassAggregateProbeSpec {
    pub fn new(name: impl Into<String>, selection: MassSelection) -> Self {
        Self {
            name: name.into(),
            selection,
            visible: true,
            show_member_lines: true,
        }
    }

    pub fn with_visibility(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MassAggregateProbe {
    pub id: MassAggregateProbeId,
    pub name: String,
    pub selection: MassSelection,
    pub visible: bool,
    /// Whether a dashed line from the centroid to each current member is
    /// drawn while this probe is the active selection. Purely a display
    /// preference — never affects `mass_aggregate`'s computed totals.
    #[serde(default = "default_true")]
    pub show_member_lines: bool,
    /// The derived, pinned object this probe's live centroid drives, so a
    /// plane/box/sphere/probe can attach to it like any other object. Created
    /// and removed together with the probe — see
    /// [`WorldCommand::CreateMassAggregateProbe`]/
    /// [`WorldCommand::RemoveMassAggregateProbe`].
    pub anchor: ObjectId,
}

/// Each map is `Arc`-wrapped so `WorldState::clone()` — taken on every
/// [`World::commit`] and [`World::restore`] — is a handful of refcount bumps
/// rather than a deep copy of the whole scene. A command only ever writes one
/// map (see `apply_command`'s `*_mut` helpers, which go through
/// [`Arc::make_mut`]), so a commit that touches, say, only `probes` clones
/// just that one map and shares the rest with the revision it started from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    revision: WorldRevision,
    objects: Arc<BTreeMap<ObjectId, WorldObject>>,
    planes: Arc<BTreeMap<PlaneId, SlicePlane>>,
    boxes: Arc<BTreeMap<BoxId, FieldBox>>,
    spheres: Arc<BTreeMap<SphereId, FieldSphere>>,
    probes: Arc<BTreeMap<ProbeId, Probe>>,
    distance_probes: Arc<BTreeMap<DistanceProbeId, DistanceProbe>>,
    /// `#[serde(default)]`: a document saved before mass-aggregate probes
    /// existed has no `mass_aggregate_probes` key in its JSON at all —
    /// without this, `World::from_document` would fail to deserialize every
    /// scene ever saved before today, not just decline to load one.
    #[serde(default)]
    mass_aggregate_probes: Arc<BTreeMap<MassAggregateProbeId, MassAggregateProbe>>,
    component_schemas: Arc<BTreeMap<ComponentTypeId, ComponentSchema>>,
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

    pub fn distance_probes(&self) -> &BTreeMap<DistanceProbeId, DistanceProbe> {
        &self.0.distance_probes
    }

    pub fn distance_probe(&self, id: DistanceProbeId) -> Option<&DistanceProbe> {
        self.0.distance_probes.get(&id)
    }

    pub fn mass_aggregate_probes(&self) -> &BTreeMap<MassAggregateProbeId, MassAggregateProbe> {
        &self.0.mass_aggregate_probes
    }

    pub fn mass_aggregate_probe(&self, id: MassAggregateProbeId) -> Option<&MassAggregateProbe> {
        self.0.mass_aggregate_probes.get(&id)
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

    /// The live distance between a [`DistanceProbe`]'s two objects.
    pub fn resolve_distance(&self, probe: &DistanceProbe) -> Result<f64, WorldError> {
        let a = self
            .0
            .objects
            .get(&probe.object_a)
            .ok_or(WorldError::ObjectNotFound { id: probe.object_a })?;
        let b = self
            .0
            .objects
            .get(&probe.object_b)
            .ok_or(WorldError::ObjectNotFound { id: probe.object_b })?;
        Ok((a.transform.translation - b.transform.translation).length())
    }

    /// A plane's world-space `(origin, normal, u_axis)`. Identity when
    /// unattached; otherwise composed with the live transform of the object
    /// it's attached to, so a moving object carries the plane along with it.
    pub fn resolve_plane_frame(
        &self,
        plane: &SlicePlane,
    ) -> Result<(DVec3, DVec3, DVec3), WorldError> {
        match plane.attached_to {
            None => Ok((plane.origin, plane.normal, plane.u_axis)),
            Some(object) => {
                let object = self
                    .0
                    .objects
                    .get(&object)
                    .ok_or(WorldError::ObjectNotFound { id: object })?;
                let transform = object.transform;
                Ok((
                    transform.apply(plane.origin),
                    transform.rotation * plane.normal,
                    transform.rotation * plane.u_axis,
                ))
            }
        }
    }

    /// A box's world-space `(origin, rotation)`. See [`Self::resolve_plane_frame`].
    pub fn resolve_box_frame(&self, region: &FieldBox) -> Result<(DVec3, DQuat), WorldError> {
        match region.attached_to {
            None => Ok((region.origin, region.rotation)),
            Some(object) => {
                let object = self
                    .0
                    .objects
                    .get(&object)
                    .ok_or(WorldError::ObjectNotFound { id: object })?;
                let transform = object.transform;
                Ok((
                    transform.apply(region.origin),
                    transform.rotation * region.rotation,
                ))
            }
        }
    }

    /// A sphere's world-space origin. See [`Self::resolve_plane_frame`].
    pub fn resolve_sphere_origin(&self, sphere: &FieldSphere) -> Result<DVec3, WorldError> {
        match sphere.attached_to {
            None => Ok(sphere.origin),
            Some(object) => {
                let object = self
                    .0
                    .objects
                    .get(&object)
                    .ok_or(WorldError::ObjectNotFound { id: object })?;
                Ok(object.transform.apply(sphere.origin))
            }
        }
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

/// A `World`'s full contents — state and identifier counters — captured for
/// durable storage.
///
/// Sibling of [`WorldCheckpoint`], but serde-round-trippable
/// (`WorldCheckpoint` only ever comes from the live undo stack, and its
/// counters are always the enclosing `World`'s current ones) and consumable
/// without an existing `World` to restore onto. Opaque outside this crate on
/// purpose, the same way `WorldCheckpoint` is: a thing to hand to
/// [`World::from_document`], not a second way to read a world.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldDocument {
    state: WorldState,
    counters: Counters,
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
                objects: Arc::new(BTreeMap::new()),
                planes: Arc::new(BTreeMap::new()),
                boxes: Arc::new(BTreeMap::new()),
                spheres: Arc::new(BTreeMap::new()),
                probes: Arc::new(BTreeMap::new()),
                distance_probes: Arc::new(BTreeMap::new()),
                mass_aggregate_probes: Arc::new(BTreeMap::new()),
                component_schemas: Arc::new(BTreeMap::new()),
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

    /// Capture full contents — state and identifier counters — for durable
    /// storage. Distinct from [`Self::checkpoint`], which is for the
    /// in-memory undo stack and does not need counters, since a checkpoint is
    /// always restored onto the same `World` whose counters never rewind.
    pub fn to_document(&self) -> WorldDocument {
        WorldDocument {
            state: (*self.state).clone(),
            counters: self.counters,
        }
    }

    /// Adopt a previously captured document as a fresh `World`.
    ///
    /// Same whole-state-swap shape as [`Self::restore`], but the document's
    /// own revision and counters are kept as-is rather than rebased forward:
    /// a loaded document is the start of a session, not an edit within one
    /// that already had its own revision/counter history.
    ///
    /// No validation happens here — the document model owns durable state,
    /// but adopting it against an active set of solvers is the simulation
    /// runtime's job, not this crate's.
    pub fn from_document(document: WorldDocument) -> Self {
        Self {
            state: Arc::new(document.state),
            counters: document.counters,
        }
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

    /// One-time migration for a document saved before mass-aggregate probes
    /// existed. Adopts a legacy, unowned `derived` object (the old hidden,
    /// whole-universe-only "Center of mass" singleton) as a fresh
    /// [`MassAggregateProbe`]'s anchor, instead of leaving it stuck forever:
    /// `RemoveObject` rejects any `derived` object still owned by a probe,
    /// and there is deliberately no command that lets a user delete an
    /// *unowned* one either, so a document this old must not be left with an
    /// orphan that is neither exposed nor removable.
    ///
    /// Not a [`WorldCommand`]: reusing an already-existing object as a new
    /// probe's anchor is a one-time reinterpretation of already-durable
    /// state at load time, not an edit a user or plugin ever authors — every
    /// other path always mints a fresh anchor
    /// ([`WorldCommand::CreateMassAggregateProbe`]). A no-op if no such
    /// orphan exists.
    pub fn adopt_legacy_center_of_mass(&mut self) -> Option<MassAggregateProbeId> {
        let owned: BTreeSet<ObjectId> = self
            .state
            .mass_aggregate_probes
            .values()
            .map(|probe| probe.anchor)
            .collect();
        let (anchor, name, visible) = self
            .state
            .objects
            .values()
            .find(|object| object.derived && !owned.contains(&object.id))
            .map(|object| (object.id, object.name.clone(), object.visible))?;

        let mut candidate = (*self.state).clone();
        let mut counters = self.counters;
        let id = counters.next_mass_aggregate_probe();
        mass_aggregate_probes_mut(&mut candidate).insert(
            id,
            MassAggregateProbe {
                id,
                name,
                selection: MassSelection::Universe {
                    excluded: BTreeSet::new(),
                },
                visible,
                show_member_lines: true,
                anchor,
            },
        );
        candidate.revision = candidate.revision.next();
        self.state = Arc::new(candidate);
        self.counters = counters;
        Some(id)
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
    CreateDistanceProbe(DistanceProbeSpec),
    SetDistanceProbeName {
        probe: DistanceProbeId,
        name: String,
    },
    SetDistanceProbeObjects {
        probe: DistanceProbeId,
        object_a: ObjectId,
        object_b: ObjectId,
    },
    SetDistanceProbeVisible {
        probe: DistanceProbeId,
        visible: bool,
    },
    SetDistanceProbeShowLine {
        probe: DistanceProbeId,
        show_line: bool,
    },
    RemoveDistanceProbe(DistanceProbeId),
    CreateMassAggregateProbe(MassAggregateProbeSpec),
    SetMassAggregateProbeName {
        probe: MassAggregateProbeId,
        name: String,
    },
    SetMassAggregateProbeSelection {
        probe: MassAggregateProbeId,
        selection: MassSelection,
    },
    SetMassAggregateProbeVisible {
        probe: MassAggregateProbeId,
        visible: bool,
    },
    SetMassAggregateProbeShowMemberLines {
        probe: MassAggregateProbeId,
        show_member_lines: bool,
    },
    RemoveMassAggregateProbe(MassAggregateProbeId),
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
            Self::CreateDistanceProbe(_) => "Add distance probe",
            Self::SetDistanceProbeName { .. } => "Rename distance probe",
            Self::SetDistanceProbeObjects { .. } => "Change distance probe objects",
            Self::SetDistanceProbeVisible { .. } => "Show or hide distance probe",
            Self::SetDistanceProbeShowLine { .. } => "Show or hide distance probe line",
            Self::RemoveDistanceProbe(_) => "Remove distance probe",
            Self::CreateMassAggregateProbe(_) => "Add center of mass",
            Self::SetMassAggregateProbeName { .. } => "Rename center of mass",
            Self::SetMassAggregateProbeSelection { .. } => "Change center of mass membership",
            Self::SetMassAggregateProbeVisible { .. } => "Show or hide center of mass",
            Self::SetMassAggregateProbeShowMemberLines { .. } => {
                "Show or hide center-of-mass member lines"
            }
            Self::RemoveMassAggregateProbe(_) => "Remove center of mass",
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReport {
    pub revision: WorldRevision,
    pub created_objects: Vec<ObjectId>,
    pub created_planes: Vec<PlaneId>,
    pub created_boxes: Vec<BoxId>,
    pub created_spheres: Vec<SphereId>,
    pub created_probes: Vec<ProbeId>,
    pub created_distance_probes: Vec<DistanceProbeId>,
    /// Deliberately excludes the anchor object `CreateMassAggregateProbe`
    /// also creates: that anchor is `derived`, so it must never win
    /// `first_created`'s "what should the editor auto-select" role over the
    /// probe itself.
    pub created_mass_aggregate_probes: Vec<MassAggregateProbeId>,
}

/// One entity a [`CommitReport`] says was created, tagged with its kind.
///
/// Exists so a caller that wants "the thing this commit just created" (an
/// editor auto-selecting a new entity, say) can match one type instead of
/// checking every `created_*` field itself. Add a variant here in the same
/// change that adds a new `created_*` field to `CommitReport` — the two are
/// meant to be extended together, and [`CommitReport::first_created`] is the
/// only place that needs to know about every `created_*` field at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatedEntity {
    Object(ObjectId),
    Plane(PlaneId),
    Box(BoxId),
    Sphere(SphereId),
    Probe(ProbeId),
    DistanceProbe(DistanceProbeId),
    MassAggregateProbe(MassAggregateProbeId),
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
            created_distance_probes: Vec::new(),
            created_mass_aggregate_probes: Vec::new(),
        }
    }

    /// No creations to report, at a given revision.
    ///
    /// Public counterpart of [`Self::unchanged`] for callers outside this
    /// module that must attach a report to a receipt for a command that
    /// created nothing (every non-`CommitWorld` command) or has not yet
    /// applied (a `CommitWorld` queued behind a tick boundary).
    pub fn empty(revision: WorldRevision) -> Self {
        Self::unchanged(revision)
    }

    /// The first entity this commit created, if any, in `created_*` field
    /// declaration order. A single `CommitWorld` transaction creates at most
    /// one kind of entity in every caller this ships with, so "first" is
    /// really "the one" in practice; the ordering only matters as a
    /// tie-break should that ever change.
    pub fn first_created(&self) -> Option<CreatedEntity> {
        self.created_objects
            .first()
            .copied()
            .map(CreatedEntity::Object)
            .or_else(|| {
                self.created_planes
                    .first()
                    .copied()
                    .map(CreatedEntity::Plane)
            })
            .or_else(|| self.created_boxes.first().copied().map(CreatedEntity::Box))
            .or_else(|| {
                self.created_spheres
                    .first()
                    .copied()
                    .map(CreatedEntity::Sphere)
            })
            .or_else(|| {
                self.created_probes
                    .first()
                    .copied()
                    .map(CreatedEntity::Probe)
            })
            .or_else(|| {
                self.created_distance_probes
                    .first()
                    .copied()
                    .map(CreatedEntity::DistanceProbe)
            })
            .or_else(|| {
                self.created_mass_aggregate_probes
                    .first()
                    .copied()
                    .map(CreatedEntity::MassAggregateProbe)
            })
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Counters {
    object: u64,
    plane: u64,
    field_box: u64,
    sphere: u64,
    probe: u64,
    distance_probe: u64,
    /// `#[serde(default)]` for the same reason as `WorldState::mass_aggregate_probes`
    /// — absent in any document saved before mass-aggregate probes existed.
    #[serde(default)]
    mass_aggregate_probe: u64,
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

    fn next_distance_probe(&mut self) -> DistanceProbeId {
        let id = DistanceProbeId::new(self.distance_probe);
        self.distance_probe += 1;
        id
    }

    fn next_mass_aggregate_probe(&mut self) -> MassAggregateProbeId {
        let id = MassAggregateProbeId::new(self.mass_aggregate_probe);
        self.mass_aggregate_probe += 1;
        id
    }
}

/// `Arc::make_mut`, one per map — see the note on [`WorldState`]. Every
/// write in `apply_command` goes through one of these instead of touching
/// `state.<map>` directly, so it clones only the one map it changes.
fn objects_mut(state: &mut WorldState) -> &mut BTreeMap<ObjectId, WorldObject> {
    Arc::make_mut(&mut state.objects)
}

fn planes_mut(state: &mut WorldState) -> &mut BTreeMap<PlaneId, SlicePlane> {
    Arc::make_mut(&mut state.planes)
}

fn boxes_mut(state: &mut WorldState) -> &mut BTreeMap<BoxId, FieldBox> {
    Arc::make_mut(&mut state.boxes)
}

fn spheres_mut(state: &mut WorldState) -> &mut BTreeMap<SphereId, FieldSphere> {
    Arc::make_mut(&mut state.spheres)
}

fn probes_mut(state: &mut WorldState) -> &mut BTreeMap<ProbeId, Probe> {
    Arc::make_mut(&mut state.probes)
}

fn distance_probes_mut(state: &mut WorldState) -> &mut BTreeMap<DistanceProbeId, DistanceProbe> {
    Arc::make_mut(&mut state.distance_probes)
}

fn mass_aggregate_probes_mut(
    state: &mut WorldState,
) -> &mut BTreeMap<MassAggregateProbeId, MassAggregateProbe> {
    Arc::make_mut(&mut state.mass_aggregate_probes)
}

fn component_schemas_mut(
    state: &mut WorldState,
) -> &mut BTreeMap<ComponentTypeId, ComponentSchema> {
    Arc::make_mut(&mut state.component_schemas)
}

fn object_mut(state: &mut WorldState, id: ObjectId) -> Result<&mut WorldObject, WorldError> {
    objects_mut(state)
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
            component_schemas_mut(state).insert(schema.id.clone(), schema);
        }
        WorldCommand::CreateObject(spec) => {
            spec.validate()?;
            validate_object_components(state, &spec.components)?;
            let id = counters.next_object();
            objects_mut(state).insert(
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
                    derived: spec.derived,
                },
            );
            report.created_objects.push(id);
        }
        WorldCommand::RemoveObject(id) => {
            let Some(object) = state.objects.get(&id) else {
                return Err(WorldError::ObjectNotFound { id });
            };
            if object.derived {
                return Err(WorldError::DerivedObjectCannotBeRemoved { id });
            }
            if state.probes.values().any(|probe| {
                matches!(probe.position, ProbePosition::Attached { object, .. } if object == id)
            }) {
                return Err(WorldError::ObjectHasAttachedProbe { id });
            }
            if state
                .planes
                .values()
                .any(|plane| plane.attached_to == Some(id))
            {
                return Err(WorldError::ObjectHasAttachedPlane { id });
            }
            if state
                .boxes
                .values()
                .any(|region| region.attached_to == Some(id))
            {
                return Err(WorldError::ObjectHasAttachedBox { id });
            }
            if state
                .spheres
                .values()
                .any(|sphere| sphere.attached_to == Some(id))
            {
                return Err(WorldError::ObjectHasAttachedSphere { id });
            }
            if state
                .distance_probes
                .values()
                .any(|probe| probe.object_a == id || probe.object_b == id)
            {
                return Err(WorldError::ObjectHasAttachedDistanceProbe { id });
            }
            // Deliberately no guard for `mass_aggregate_probes`: unlike every
            // check above, a probe's included/excluded set is N-of-M
            // membership, not a required 1:1/2:2 target. Losing a member
            // gracefully shrinks the aggregate (or leaves an inert stale id
            // in `excluded`, which never resolves to anything once the
            // object is gone) rather than breaking a resolution the probe
            // can't function without.
            objects_mut(state).remove(&id);
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
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let spec = spec.normalized();
            let id = counters.next_plane();
            planes_mut(state).insert(
                id,
                SlicePlane {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    normal: spec.normal,
                    u_axis: spec.u_axis,
                    half_extent: spec.half_extent,
                    visible: spec.visible,
                    attached_to: spec.attached_to,
                },
            );
            report.created_planes.push(id);
        }
        WorldCommand::SetPlaneName { plane, name } => {
            planes_mut(state)
                .get_mut(&plane)
                .ok_or(WorldError::PlaneNotFound { id: plane })?
                .name = name;
        }
        WorldCommand::SetPlaneVisible { plane, visible } => {
            planes_mut(state)
                .get_mut(&plane)
                .ok_or(WorldError::PlaneNotFound { id: plane })?
                .visible = visible;
        }
        WorldCommand::SetPlane { plane, spec } => {
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let spec = spec.normalized();
            let current = planes_mut(state)
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
                attached_to: spec.attached_to,
            };
        }
        WorldCommand::RemovePlane(id) => {
            if planes_mut(state).remove(&id).is_none() {
                return Err(WorldError::PlaneNotFound { id });
            }
        }
        WorldCommand::CreateBox(spec) => {
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let spec = spec.normalized();
            let id = counters.next_box();
            boxes_mut(state).insert(
                id,
                FieldBox {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    rotation: spec.rotation,
                    half_extent: spec.half_extent,
                    visible: spec.visible,
                    attached_to: spec.attached_to,
                },
            );
            report.created_boxes.push(id);
        }
        WorldCommand::SetBoxName { region, name } => {
            boxes_mut(state)
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?
                .name = name;
        }
        WorldCommand::SetBoxVisible { region, visible } => {
            boxes_mut(state)
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?
                .visible = visible;
        }
        WorldCommand::SetBox { region, spec } => {
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let spec = spec.normalized();
            let current = boxes_mut(state)
                .get_mut(&region)
                .ok_or(WorldError::BoxNotFound { id: region })?;
            *current = FieldBox {
                id: region,
                name: spec.name,
                origin: spec.origin,
                rotation: spec.rotation,
                half_extent: spec.half_extent,
                visible: spec.visible,
                attached_to: spec.attached_to,
            };
        }
        WorldCommand::RemoveBox(id) => {
            if boxes_mut(state).remove(&id).is_none() {
                return Err(WorldError::BoxNotFound { id });
            }
        }
        WorldCommand::CreateSphere(spec) => {
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let id = counters.next_sphere();
            spheres_mut(state).insert(
                id,
                FieldSphere {
                    id,
                    name: spec.name,
                    origin: spec.origin,
                    radius: spec.radius,
                    visible: spec.visible,
                    attached_to: spec.attached_to,
                },
            );
            report.created_spheres.push(id);
        }
        WorldCommand::SetSphereName { sphere, name } => {
            spheres_mut(state)
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?
                .name = name;
        }
        WorldCommand::SetSphereVisible { sphere, visible } => {
            spheres_mut(state)
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?
                .visible = visible;
        }
        WorldCommand::SetSphere { sphere, spec } => {
            spec.validate()?;
            validate_attachment(state, spec.attached_to)?;
            let current = spheres_mut(state)
                .get_mut(&sphere)
                .ok_or(WorldError::SphereNotFound { id: sphere })?;
            *current = FieldSphere {
                id: sphere,
                name: spec.name,
                origin: spec.origin,
                radius: spec.radius,
                visible: spec.visible,
                attached_to: spec.attached_to,
            };
        }
        WorldCommand::RemoveSphere(id) => {
            if spheres_mut(state).remove(&id).is_none() {
                return Err(WorldError::SphereNotFound { id });
            }
        }
        WorldCommand::CreateProbe(spec) => {
            validate_probe(state, &spec)?;
            let id = counters.next_probe();
            probes_mut(state).insert(
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
            probes_mut(state)
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
            probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?
                .position = position;
        }
        WorldCommand::SetProbeChannels { probe, channels } => {
            let probe = probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?;
            probe.channels = channels;
        }
        WorldCommand::SetProbeVisible { probe, visible } => {
            probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::ProbeNotFound { id: probe })?
                .visible = visible;
        }
        WorldCommand::RemoveProbe(id) => {
            if probes_mut(state).remove(&id).is_none() {
                return Err(WorldError::ProbeNotFound { id });
            }
        }
        WorldCommand::CreateDistanceProbe(spec) => {
            spec.validate()?;
            validate_distance_probe_objects(state, spec.object_a, spec.object_b)?;
            let id = counters.next_distance_probe();
            distance_probes_mut(state).insert(
                id,
                DistanceProbe {
                    id,
                    name: spec.name,
                    object_a: spec.object_a,
                    object_b: spec.object_b,
                    visible: spec.visible,
                    show_line: spec.show_line,
                },
            );
            report.created_distance_probes.push(id);
        }
        WorldCommand::SetDistanceProbeName { probe, name } => {
            distance_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::DistanceProbeNotFound { id: probe })?
                .name = name;
        }
        WorldCommand::SetDistanceProbeObjects {
            probe,
            object_a,
            object_b,
        } => {
            if object_a == object_b {
                return Err(WorldError::InvalidDistanceProbe);
            }
            validate_distance_probe_objects(state, object_a, object_b)?;
            let current = distance_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::DistanceProbeNotFound { id: probe })?;
            current.object_a = object_a;
            current.object_b = object_b;
        }
        WorldCommand::SetDistanceProbeVisible { probe, visible } => {
            distance_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::DistanceProbeNotFound { id: probe })?
                .visible = visible;
        }
        WorldCommand::SetDistanceProbeShowLine { probe, show_line } => {
            distance_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::DistanceProbeNotFound { id: probe })?
                .show_line = show_line;
        }
        WorldCommand::RemoveDistanceProbe(id) => {
            if distance_probes_mut(state).remove(&id).is_none() {
                return Err(WorldError::DistanceProbeNotFound { id });
            }
        }
        WorldCommand::CreateMassAggregateProbe(spec) => {
            validate_mass_aggregate_probe(state, &spec.selection)?;
            let id = counters.next_mass_aggregate_probe();
            let anchor = counters.next_object();
            objects_mut(state).insert(
                anchor,
                WorldObject {
                    id: anchor,
                    name: spec.name.clone(),
                    transform: Transform::default(),
                    velocity: Velocity::default(),
                    shape: None,
                    visible: spec.visible,
                    pinned: true,
                    components: BTreeMap::new(),
                    derived: true,
                },
            );
            mass_aggregate_probes_mut(state).insert(
                id,
                MassAggregateProbe {
                    id,
                    name: spec.name,
                    selection: spec.selection,
                    visible: spec.visible,
                    show_member_lines: spec.show_member_lines,
                    anchor,
                },
            );
            report.created_mass_aggregate_probes.push(id);
        }
        WorldCommand::SetMassAggregateProbeName { probe, name } => {
            mass_aggregate_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::MassAggregateProbeNotFound { id: probe })?
                .name = name;
        }
        WorldCommand::SetMassAggregateProbeSelection { probe, selection } => {
            validate_mass_aggregate_probe(state, &selection)?;
            mass_aggregate_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::MassAggregateProbeNotFound { id: probe })?
                .selection = selection;
        }
        WorldCommand::SetMassAggregateProbeVisible { probe, visible } => {
            let anchor = {
                let current = mass_aggregate_probes_mut(state)
                    .get_mut(&probe)
                    .ok_or(WorldError::MassAggregateProbeNotFound { id: probe })?;
                current.visible = visible;
                current.anchor
            };
            if let Some(object) = objects_mut(state).get_mut(&anchor) {
                object.visible = visible;
            }
        }
        WorldCommand::SetMassAggregateProbeShowMemberLines {
            probe,
            show_member_lines,
        } => {
            mass_aggregate_probes_mut(state)
                .get_mut(&probe)
                .ok_or(WorldError::MassAggregateProbeNotFound { id: probe })?
                .show_member_lines = show_member_lines;
        }
        WorldCommand::RemoveMassAggregateProbe(id) => {
            let probe = mass_aggregate_probes_mut(state)
                .remove(&id)
                .ok_or(WorldError::MassAggregateProbeNotFound { id })?;
            // The one legitimate path that deletes a `derived` object:
            // `RemoveObject` itself keeps rejecting them so a user can never
            // orphan an anchor by targeting it directly.
            objects_mut(state).remove(&probe.anchor);
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

/// An AUX object's attachment target, if any, must exist. Shared by
/// planes/boxes/spheres — see `validate_probe` for the probe equivalent.
fn validate_attachment(
    state: &WorldState,
    attached_to: Option<ObjectId>,
) -> Result<(), WorldError> {
    if let Some(object) = attached_to
        && !state.objects.contains_key(&object)
    {
        return Err(WorldError::ObjectNotFound { id: object });
    }
    Ok(())
}

fn validate_distance_probe_objects(
    state: &WorldState,
    object_a: ObjectId,
    object_b: ObjectId,
) -> Result<(), WorldError> {
    if !state.objects.contains_key(&object_a) {
        return Err(WorldError::ObjectNotFound { id: object_a });
    }
    if !state.objects.contains_key(&object_b) {
        return Err(WorldError::ObjectNotFound { id: object_b });
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

fn validate_mass_aggregate_probe(
    state: &WorldState,
    selection: &MassSelection,
) -> Result<(), WorldError> {
    let ids = match selection {
        MassSelection::Universe { excluded } => excluded,
        MassSelection::Selection { included } => included,
    };
    for &id in ids {
        if !state.objects.contains_key(&id) {
            return Err(WorldError::ObjectNotFound { id });
        }
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
    #[error("object {id} still has an attached slice plane")]
    ObjectHasAttachedPlane { id: ObjectId },
    #[error("object {id} still has an attached field box")]
    ObjectHasAttachedBox { id: ObjectId },
    #[error("object {id} still has an attached field sphere")]
    ObjectHasAttachedSphere { id: ObjectId },
    #[error("object {id} still has an attached distance probe")]
    ObjectHasAttachedDistanceProbe { id: ObjectId },
    #[error("object {id} is runtime-owned and cannot be removed")]
    DerivedObjectCannotBeRemoved { id: ObjectId },
    #[error("a distance probe requires two distinct objects")]
    InvalidDistanceProbe,
    #[error("distance probe {id} does not exist")]
    DistanceProbeNotFound { id: DistanceProbeId },
    #[error("plane {id} does not exist")]
    PlaneNotFound { id: PlaneId },
    #[error("field box {id} does not exist")]
    BoxNotFound { id: BoxId },
    #[error("field sphere {id} does not exist")]
    SphereNotFound { id: SphereId },
    #[error("probe {id} does not exist")]
    ProbeNotFound { id: ProbeId },
    #[error("center-of-mass probe {id} does not exist")]
    MassAggregateProbeNotFound { id: MassAggregateProbeId },
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
                description: None,
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
    fn first_created_reports_nothing_for_an_edit_that_creates_nothing() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("a"))])
            .unwrap();
        let report = world
            .commit([WorldCommand::SetObjectName {
                object: ObjectId::new(0),
                name: "renamed".to_owned(),
            }])
            .unwrap();

        assert_eq!(report.first_created(), None);
    }

    #[test]
    fn first_created_reports_a_newly_created_distance_probe() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("a")),
                WorldCommand::CreateObject(ObjectSpec::new("b")),
            ])
            .unwrap();
        let report = world
            .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                "gap",
                ObjectId::new(0),
                ObjectId::new(1),
            ))])
            .unwrap();

        assert_eq!(
            report.first_created(),
            Some(CreatedEntity::DistanceProbe(DistanceProbeId::new(0)))
        );
    }

    #[test]
    fn first_created_prefers_an_object_over_a_probe_in_the_same_commit() {
        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("source")),
                WorldCommand::CreateProbe(ProbeSpec::at("probe", DVec3::X, Vec::new())),
            ])
            .unwrap();

        assert_eq!(
            report.first_created(),
            Some(CreatedEntity::Object(ObjectId::new(0)))
        );
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

    /// PH-7 regression: `SlicePlaneSpec` has private fields, but derives
    /// `Deserialize`, and `WorldCommand` is `Deserialize` end-to-end — a
    /// caller-supplied JSON `commit_world` command (the exact path
    /// `crates/fieldcad-mcp` uses) can carry a value no safe constructor
    /// would ever produce. `apply_command` must reject it rather than trust
    /// a "cannot be constructed unvalidated" invariant that was never true
    /// for this type.
    #[test]
    fn commit_world_rejects_a_deserialized_plane_that_bypassed_its_constructor() {
        let valid = SlicePlaneSpec::new("plane", DVec3::ZERO, DVec3::Z)
            .unwrap()
            .with_half_extent(DVec2::splat(2.0))
            .unwrap();
        let mut json = serde_json::to_value(&valid).unwrap();
        json["normal"] = serde_json::json!([0.0, 0.0, 0.0]);
        json["half_extent"] = serde_json::json!([-1.0, 1.0]);
        let tampered: SlicePlaneSpec = serde_json::from_value(json).unwrap();

        let mut world = World::new();
        assert!(world.commit([WorldCommand::CreatePlane(tampered)]).is_err());
    }

    /// Same hazard, `FieldBoxSpec`.
    #[test]
    fn commit_world_rejects_a_deserialized_box_that_bypassed_its_constructor() {
        let valid = FieldBoxSpec::new("box", DVec3::ZERO, DVec3::splat(1.0)).unwrap();
        let mut json = serde_json::to_value(&valid).unwrap();
        json["rotation"] = serde_json::to_value(DQuat::from_xyzw(0.0, 0.0, 0.0, 0.0)).unwrap();
        json["half_extent"] = serde_json::json!([-1.0, 1.0, 1.0]);
        let tampered: FieldBoxSpec = serde_json::from_value(json).unwrap();

        let mut world = World::new();
        assert!(world.commit([WorldCommand::CreateBox(tampered)]).is_err());
    }

    /// Same hazard, `FieldSphereSpec`.
    #[test]
    fn commit_world_rejects_a_deserialized_sphere_that_bypassed_its_constructor() {
        let valid = FieldSphereSpec::new("sphere", DVec3::ZERO, 1.0).unwrap();
        let mut json = serde_json::to_value(&valid).unwrap();
        json["radius"] = serde_json::json!(-1.0);
        let tampered: FieldSphereSpec = serde_json::from_value(json).unwrap();

        let mut world = World::new();
        assert!(
            world
                .commit([WorldCommand::CreateSphere(tampered)])
                .is_err()
        );
    }

    /// The fix validates *and* normalizes, the same split `Transform` already
    /// uses — a finite, non-degenerate but non-unit normal is not an error,
    /// it is silently corrected, the same as every safe constructor already
    /// does.
    #[test]
    fn commit_world_normalizes_a_deserialized_planes_non_unit_normal() {
        let valid = SlicePlaneSpec::new("plane", DVec3::ZERO, DVec3::Z).unwrap();
        let mut json = serde_json::to_value(&valid).unwrap();
        json["normal"] = serde_json::json!([0.0, 0.0, 3.0]);
        let tampered: SlicePlaneSpec = serde_json::from_value(json).unwrap();

        let mut world = World::new();
        world.commit([WorldCommand::CreatePlane(tampered)]).unwrap();
        let plane = world.snapshot().planes().values().next().unwrap().clone();
        assert!((plane.normal.length() - 1.0).abs() < 1.0e-12);
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
            attached_to: None,
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
    fn attaching_an_aux_object_to_a_nonexistent_object_is_rejected() {
        let mut world = World::new();
        assert!(
            world
                .commit([WorldCommand::CreatePlane(
                    SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                        .unwrap()
                        .with_attached_to(ObjectId::new(0)),
                )])
                .is_err()
        );
        assert!(
            world
                .commit([WorldCommand::CreateBox(
                    FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(1.0))
                        .unwrap()
                        .with_attached_to(ObjectId::new(0)),
                )])
                .is_err()
        );
        assert!(
            world
                .commit([WorldCommand::CreateSphere(
                    FieldSphereSpec::new("ball", DVec3::ZERO, 1.0)
                        .unwrap()
                        .with_attached_to(ObjectId::new(0)),
                )])
                .is_err()
        );
    }

    #[test]
    fn re_attaching_an_existing_plane_to_a_nonexistent_object_is_rejected() {
        let mut world = World::new();
        let created = world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let plane = created.created_planes[0];
        assert!(
            world
                .commit([WorldCommand::SetPlane {
                    plane,
                    spec: SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                        .unwrap()
                        .with_attached_to(ObjectId::new(99)),
                }])
                .is_err()
        );
    }

    #[test]
    fn removing_an_object_with_an_attached_plane_leaves_it_in_place() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z)
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 1);
    }

    #[test]
    fn removing_an_object_with_an_attached_box_leaves_it_in_place() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("cube", DVec3::ZERO, DVec3::splat(1.0))
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 1);
    }

    #[test]
    fn removing_an_object_with_an_attached_sphere_leaves_it_in_place() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        world
            .commit([WorldCommand::CreateSphere(
                FieldSphereSpec::new("ball", DVec3::ZERO, 1.0)
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 1);
    }

    #[test]
    fn attached_plane_follows_object_translation_and_rotation() {
        let mut world = World::new();
        let rotation = DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2);
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::new(DVec3::new(1.0, 0.0, 0.0), rotation).unwrap()),
            )])
            .unwrap();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::X, DVec3::Z)
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        let snapshot = world.snapshot();
        let plane = snapshot.planes().values().next().unwrap();
        let (origin, normal, u_axis) = snapshot.resolve_plane_frame(plane).unwrap();

        // Local origin (1,0,0) rotated 90° about Z and translated by (1,0,0).
        assert!((origin - DVec3::new(1.0, 1.0, 0.0)).length() < 1.0e-12);
        // A Z-axis rotation leaves the Z normal unchanged.
        assert!((normal - DVec3::Z).length() < 1.0e-12);
        assert!(u_axis.dot(normal).abs() < 1.0e-12);
        assert!((u_axis.length() - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn attached_box_follows_object_transform() {
        let mut world = World::new();
        let rotation = DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2);
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::new(DVec3::new(1.0, 0.0, 0.0), rotation).unwrap()),
            )])
            .unwrap();
        world
            .commit([WorldCommand::CreateBox(
                FieldBoxSpec::new("cube", DVec3::X, DVec3::splat(1.0))
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        let snapshot = world.snapshot();
        let region = snapshot.boxes().values().next().unwrap();
        let (origin, box_rotation) = snapshot.resolve_box_frame(region).unwrap();

        assert!((origin - DVec3::new(1.0, 1.0, 0.0)).length() < 1.0e-12);
        assert!((box_rotation * DVec3::X - DVec3::Y).length() < 1.0e-12);
    }

    #[test]
    fn attached_sphere_follows_object_translation() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::at(DVec3::new(2.0, 0.0, 0.0)).unwrap()),
            )])
            .unwrap();
        world
            .commit([WorldCommand::CreateSphere(
                FieldSphereSpec::new("ball", DVec3::X, 1.0)
                    .unwrap()
                    .with_attached_to(ObjectId::new(0)),
            )])
            .unwrap();

        let snapshot = world.snapshot();
        let sphere = snapshot.spheres().values().next().unwrap();
        let origin = snapshot.resolve_sphere_origin(sphere).unwrap();

        assert!((origin - DVec3::new(3.0, 0.0, 0.0)).length() < 1.0e-12);
    }

    #[test]
    fn distance_probe_reports_the_live_gap_between_two_objects() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(
                    ObjectSpec::new("a").with_transform(Transform::at(DVec3::ZERO).unwrap()),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("b")
                        .with_transform(Transform::at(DVec3::new(3.0, 4.0, 0.0)).unwrap()),
                ),
            ])
            .unwrap();
        let created = world
            .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                "gap",
                ObjectId::new(0),
                ObjectId::new(1),
            ))])
            .unwrap();
        let probe_id = created.created_distance_probes[0];

        let snapshot = world.snapshot();
        let probe = snapshot.distance_probe(probe_id).unwrap();
        assert!((snapshot.resolve_distance(probe).unwrap() - 5.0).abs() < 1.0e-12);

        world
            .commit([WorldCommand::SetTransform {
                object: ObjectId::new(1),
                transform: Transform::at(DVec3::new(3.0, 0.0, 0.0)).unwrap(),
            }])
            .unwrap();
        let snapshot = world.snapshot();
        let probe = snapshot.distance_probe(probe_id).unwrap();
        assert!((snapshot.resolve_distance(probe).unwrap() - 3.0).abs() < 1.0e-12);
    }

    #[test]
    fn setting_a_distance_probes_show_line_toggles_it() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("a")),
                WorldCommand::CreateObject(ObjectSpec::new("b")),
            ])
            .unwrap();
        let created = world
            .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                "gap",
                ObjectId::new(0),
                ObjectId::new(1),
            ))])
            .unwrap();
        let probe_id = created.created_distance_probes[0];
        assert!(
            world
                .snapshot()
                .distance_probe(probe_id)
                .unwrap()
                .show_line
        );

        world
            .commit([WorldCommand::SetDistanceProbeShowLine {
                probe: probe_id,
                show_line: false,
            }])
            .unwrap();

        assert!(
            !world
                .snapshot()
                .distance_probe(probe_id)
                .unwrap()
                .show_line
        );

        let missing = DistanceProbeId::new(9999);
        assert!(
            world
                .commit([WorldCommand::SetDistanceProbeShowLine {
                    probe: missing,
                    show_line: true,
                }])
                .is_err()
        );
    }

    #[test]
    fn a_distance_probe_saved_before_show_line_existed_still_loads_shown() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("a")),
                WorldCommand::CreateObject(ObjectSpec::new("b")),
            ])
            .unwrap();
        world
            .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                "gap",
                ObjectId::new(0),
                ObjectId::new(1),
            ))])
            .unwrap();

        let mut value = serde_json::to_value(world.to_document()).unwrap();
        for probe in value["state"]["distance_probes"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            probe.as_object_mut().unwrap().remove("show_line");
        }

        let document: WorldDocument = serde_json::from_value(value).unwrap();
        let restored = World::from_document(document);
        let snapshot = restored.snapshot();
        let probe = snapshot.distance_probes().values().next().unwrap();
        assert!(probe.show_line);
    }

    #[test]
    fn a_distance_probe_needs_two_distinct_objects() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("a"))])
            .unwrap();
        assert!(
            world
                .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                    "gap",
                    ObjectId::new(0),
                    ObjectId::new(0),
                ))])
                .is_err()
        );
    }

    #[test]
    fn creating_a_distance_probe_on_a_nonexistent_object_is_rejected() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("a"))])
            .unwrap();
        assert!(
            world
                .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                    "gap",
                    ObjectId::new(0),
                    ObjectId::new(99),
                ))])
                .is_err()
        );
    }

    #[test]
    fn removing_an_object_with_a_distance_probe_leaves_it_in_place() {
        let mut world = World::new();
        world
            .commit([
                WorldCommand::CreateObject(ObjectSpec::new("a")),
                WorldCommand::CreateObject(ObjectSpec::new("b")),
            ])
            .unwrap();
        world
            .commit([WorldCommand::CreateDistanceProbe(DistanceProbeSpec::new(
                "gap",
                ObjectId::new(0),
                ObjectId::new(1),
            ))])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 2);
    }

    #[test]
    fn a_derived_object_cannot_be_removed() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("center of mass").derived(),
            )])
            .unwrap();

        assert!(
            world
                .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
                .is_err()
        );
        assert_eq!(world.snapshot().objects().len(), 1);
    }

    #[test]
    fn a_derived_object_can_still_have_its_transform_set() {
        // `derived` only guards `RemoveObject` — the runtime that owns a
        // derived object still needs to reposition it every publish through
        // the ordinary `SetTransform` path.
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("center of mass").derived(),
            )])
            .unwrap();

        world
            .commit([WorldCommand::SetTransform {
                object: ObjectId::new(0),
                transform: Transform::at(DVec3::new(1.0, 2.0, 3.0)).unwrap(),
            }])
            .unwrap();
        assert_eq!(
            world
                .snapshot()
                .object(ObjectId::new(0))
                .unwrap()
                .transform
                .translation,
            DVec3::new(1.0, 2.0, 3.0)
        );
    }

    #[test]
    fn adopt_legacy_center_of_mass_turns_an_orphaned_derived_object_into_a_probe() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("Center of mass").derived(),
            )])
            .unwrap();
        let anchor = ObjectId::new(0);
        let before = world.revision();

        let probe_id = world.adopt_legacy_center_of_mass().unwrap();

        assert!(world.revision() > before);
        let snapshot = world.snapshot();
        let probe = snapshot.mass_aggregate_probe(probe_id).unwrap();
        assert_eq!(probe.anchor, anchor);
        assert_eq!(
            probe.selection,
            MassSelection::Universe {
                excluded: BTreeSet::new()
            }
        );
        // Calling this again must not mint a second probe for the same
        // now-owned anchor.
        assert_eq!(world.adopt_legacy_center_of_mass(), None);
        assert_eq!(snapshot.mass_aggregate_probes().len(), 1);
    }

    #[test]
    fn adopt_legacy_center_of_mass_is_a_no_op_without_an_orphan() {
        let mut world = World::new();
        let before = world.revision();

        assert_eq!(world.adopt_legacy_center_of_mass(), None);
        assert_eq!(world.revision(), before);
    }

    #[test]
    fn adopt_legacy_center_of_mass_ignores_an_anchor_a_probe_already_owns() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();

        assert_eq!(world.adopt_legacy_center_of_mass(), None);
    }

    #[test]
    fn a_world_document_saved_before_mass_aggregate_probes_existed_still_loads() {
        // Regression test: a document saved before mass-aggregate probes
        // existed has neither `WorldState::mass_aggregate_probes` nor
        // `Counters::mass_aggregate_probe` in its JSON at all —
        // `#[serde(default)]` (or its absence) on *each* is the only thing
        // standing between "old scenes keep loading" and "every scene saved
        // before today fails to open" (a real regression this test would
        // have caught: `Counters::mass_aggregate_probe` originally shipped
        // without it). Simulates both gaps by round-tripping through JSON
        // and deleting the keys an old file would never have had, rather
        // than asserting on a document built by today's own code (which
        // always writes both keys, just possibly empty/zero, and so would
        // never catch this).
        let world = World::new();
        let mut value = serde_json::to_value(world.to_document()).unwrap();
        value["state"]
            .as_object_mut()
            .unwrap()
            .remove("mass_aggregate_probes");
        value["counters"]
            .as_object_mut()
            .unwrap()
            .remove("mass_aggregate_probe");

        let document: WorldDocument = serde_json::from_value(value).unwrap();
        let mut restored = World::from_document(document);

        assert!(restored.snapshot().mass_aggregate_probes().is_empty());
        // The counter must still mint id 0 for the first probe created after
        // loading, not panic or silently start from a poisoned default.
        let report = restored
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();
        assert_eq!(
            report.created_mass_aggregate_probes[0],
            MassAggregateProbeId::new(0)
        );
    }

    #[test]
    fn creating_a_mass_aggregate_probe_mints_a_derived_anchor() {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();

        let probe_id = report.created_mass_aggregate_probes[0];
        let snapshot = world.snapshot();
        let probe = snapshot.mass_aggregate_probe(probe_id).unwrap();
        let anchor = snapshot.object(probe.anchor).unwrap();
        assert!(anchor.derived);
        assert!(anchor.pinned);
        assert_eq!(
            report.first_created(),
            Some(CreatedEntity::MassAggregateProbe(probe_id))
        );
    }

    #[test]
    fn setting_a_mass_aggregate_probes_show_member_lines_toggles_it() {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();
        let probe_id = report.created_mass_aggregate_probes[0];
        assert!(
            world
                .snapshot()
                .mass_aggregate_probe(probe_id)
                .unwrap()
                .show_member_lines
        );

        world
            .commit([WorldCommand::SetMassAggregateProbeShowMemberLines {
                probe: probe_id,
                show_member_lines: false,
            }])
            .unwrap();

        assert!(
            !world
                .snapshot()
                .mass_aggregate_probe(probe_id)
                .unwrap()
                .show_member_lines
        );

        let missing = MassAggregateProbeId::new(9999);
        assert!(
            world
                .commit([WorldCommand::SetMassAggregateProbeShowMemberLines {
                    probe: missing,
                    show_member_lines: true,
                }])
                .is_err()
        );
    }

    #[test]
    fn a_mass_aggregate_probe_saved_before_show_member_lines_existed_still_loads_shown() {
        // Same reasoning as the whole-map regression above, one field
        // narrower: a probe saved before `show_member_lines` existed has no
        // key for it in its JSON at all, and must default to `true` (the
        // behaviour this display had implicitly before the field existed to
        // turn it off) rather than fail to deserialize.
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();

        let mut value = serde_json::to_value(world.to_document()).unwrap();
        for probe in value["state"]["mass_aggregate_probes"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            probe.as_object_mut().unwrap().remove("show_member_lines");
        }

        let document: WorldDocument = serde_json::from_value(value).unwrap();
        let restored = World::from_document(document);
        let snapshot = restored.snapshot();
        let probe = snapshot.mass_aggregate_probes().values().next().unwrap();
        assert!(probe.show_member_lines);
    }

    #[test]
    fn creating_a_mass_aggregate_probe_rejects_an_unknown_excluded_object() {
        let mut world = World::new();
        assert!(
            world
                .commit([WorldCommand::CreateMassAggregateProbe(
                    MassAggregateProbeSpec::new(
                        "System",
                        MassSelection::Universe {
                            excluded: BTreeSet::from([ObjectId::new(99)]),
                        },
                    ),
                )])
                .is_err()
        );
    }

    #[test]
    fn creating_a_mass_aggregate_probe_rejects_an_unknown_included_object() {
        let mut world = World::new();
        assert!(
            world
                .commit([WorldCommand::CreateMassAggregateProbe(
                    MassAggregateProbeSpec::new(
                        "Planets",
                        MassSelection::Selection {
                            included: BTreeSet::from([ObjectId::new(99)]),
                        },
                    ),
                )])
                .is_err()
        );
    }

    #[test]
    fn removing_a_mass_aggregate_probe_also_removes_its_anchor() {
        let mut world = World::new();
        let report = world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "System",
                    MassSelection::Universe {
                        excluded: BTreeSet::new(),
                    },
                ),
            )])
            .unwrap();
        let probe_id = report.created_mass_aggregate_probes[0];
        let anchor = world
            .snapshot()
            .mass_aggregate_probe(probe_id)
            .unwrap()
            .anchor;

        world
            .commit([WorldCommand::RemoveMassAggregateProbe(probe_id)])
            .unwrap();

        let snapshot = world.snapshot();
        assert!(snapshot.mass_aggregate_probe(probe_id).is_none());
        assert!(snapshot.object(anchor).is_none());
    }

    #[test]
    fn removing_an_object_named_by_a_mass_aggregate_probes_selection_still_succeeds() {
        // Unlike a distance probe's two required endpoints, a mass-aggregate
        // probe's included/excluded set is N-of-M membership: losing one
        // member gracefully shrinks the aggregate rather than breaking a
        // resolution the probe can't function without, so `RemoveObject`
        // must not reject this the way it rejects a distance-probe endpoint.
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateObject(ObjectSpec::new("star"))])
            .unwrap();
        world
            .commit([WorldCommand::CreateMassAggregateProbe(
                MassAggregateProbeSpec::new(
                    "Planets",
                    MassSelection::Selection {
                        included: BTreeSet::from([ObjectId::new(0)]),
                    },
                ),
            )])
            .unwrap();

        world
            .commit([WorldCommand::RemoveObject(ObjectId::new(0))])
            .unwrap();
        assert!(world.snapshot().object(ObjectId::new(0)).is_none());
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
