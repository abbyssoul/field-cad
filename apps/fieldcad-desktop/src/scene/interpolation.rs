//! Trilinear/bilinear resampling of a published lattice at display density,
//! and the magnitude-to-colour ramp shared by every glyph layer.
//!
//! Shared between [`super::field`] (discrete arrow glyphs) and the flow-line
//! tracer: both need to evaluate the same published snapshot at points that
//! do not fall on a published lattice cell, and must agree on what a value
//! there looks like.

use fieldcad_core::{BoxLattice, GridLattice, PlaneLattice, SampleValidity, SceneScale};
use glam::{DVec3, Vec3};

/// Trilinear interpolation of the published domain lattice, mirroring
/// [`PlaneInterpolation`] one dimension up.
#[derive(Clone, Copy, Debug)]
pub(super) struct GridInterpolation {
    pub(super) position: Vec3,
    indices: [usize; 8],
    weights: [f64; 8],
}

impl GridInterpolation {
    pub(super) fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    pub(super) fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    pub(super) fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

pub(super) fn grid_interpolation(
    lattice: GridLattice,
    x: f64,
    y: f64,
    z: f64,
    scene_scale: SceneScale,
) -> Option<GridInterpolation> {
    let counts = lattice.counts();
    let axis = |value: f64, count: u32| {
        let value = value.clamp(0.0, f64::from(count.saturating_sub(1)));
        let low = value.floor();
        (low as usize, value.ceil() as usize, value - low)
    };
    let (x0, x1, fx) = axis(x, counts.x);
    let (y0, y1, fy) = axis(y, counts.y);
    let (z0, z1, fz) = axis(z, counts.z);
    let width = counts.x as usize;
    let height = counts.y as usize;
    let at = |x: usize, y: usize, z: usize| x + width * (y + height * z);
    let indices = [
        at(x0, y0, z0),
        at(x1, y0, z0),
        at(x0, y1, z0),
        at(x1, y1, z0),
        at(x0, y0, z1),
        at(x1, y0, z1),
        at(x0, y1, z1),
        at(x1, y1, z1),
    ];
    let weights = [
        (1.0 - fx) * (1.0 - fy) * (1.0 - fz),
        fx * (1.0 - fy) * (1.0 - fz),
        (1.0 - fx) * fy * (1.0 - fz),
        fx * fy * (1.0 - fz),
        (1.0 - fx) * (1.0 - fy) * fz,
        fx * (1.0 - fy) * fz,
        (1.0 - fx) * fy * fz,
        fx * fy * fz,
    ];
    if indices.iter().any(|&index| index >= lattice.len()) {
        return None;
    }
    Some(GridInterpolation {
        position: scene_scale.to_render_vec3(grid_point(lattice, x, y, z)?),
        indices,
        weights,
    })
}

/// The lattice is axis-aligned with a uniform step, so a fractional coordinate
/// resolves without interpolating positions.
pub(super) fn grid_point(lattice: GridLattice, x: f64, y: f64, z: f64) -> Option<DVec3> {
    Some(lattice.position(0)? + lattice.step() * DVec3::new(x, y, z))
}

pub(super) fn uniform_domain_spacing(
    lattice: GridLattice,
    xs: &[f64],
    ys: &[f64],
    zs: &[f64],
    scene_scale: SceneScale,
) -> f32 {
    let step = lattice.step();
    let spacing = |axis: &[f64], step: f64| {
        (axis.len() > 1).then(|| scene_scale.to_render(((axis[1] - axis[0]) * step).abs()))
    };
    [
        spacing(xs, step.x),
        spacing(ys, step.y),
        spacing(zs, step.z),
    ]
    .into_iter()
    .flatten()
    .filter(|spacing| *spacing > f32::EPSILON)
    .reduce(f32::min)
    .unwrap_or_else(|| scene_scale.to_render(0.25))
}

/// Trilinear interpolation of an oriented [`BoxLattice`], mirroring
/// [`GridInterpolation`] with a position built from weighted corner samples —
/// the same technique [`PlaneInterpolation`] uses — since the lattice's u/v/w
/// steps need not be axis-aligned.
#[derive(Clone, Copy, Debug)]
pub(super) struct BoxInterpolation {
    pub(super) position: Vec3,
    indices: [usize; 8],
    weights: [f64; 8],
}

impl BoxInterpolation {
    pub(super) fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    pub(super) fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    pub(super) fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

pub(super) fn box_interpolation(
    lattice: BoxLattice,
    u: f64,
    v: f64,
    w: f64,
    scene_scale: SceneScale,
) -> Option<BoxInterpolation> {
    let counts = lattice.counts();
    let axis = |value: f64, count: u32| {
        let value = value.clamp(0.0, f64::from(count.saturating_sub(1)));
        let low = value.floor();
        (low as usize, value.ceil() as usize, value - low)
    };
    let (u0, u1, fu) = axis(u, counts.x);
    let (v0, v1, fv) = axis(v, counts.y);
    let (w0, w1, fw) = axis(w, counts.z);
    let width = counts.x as usize;
    let height = counts.y as usize;
    let at = |u: usize, v: usize, w: usize| u + width * (v + height * w);
    let indices = [
        at(u0, v0, w0),
        at(u1, v0, w0),
        at(u0, v1, w0),
        at(u1, v1, w0),
        at(u0, v0, w1),
        at(u1, v0, w1),
        at(u0, v1, w1),
        at(u1, v1, w1),
    ];
    let weights = [
        (1.0 - fu) * (1.0 - fv) * (1.0 - fw),
        fu * (1.0 - fv) * (1.0 - fw),
        (1.0 - fu) * fv * (1.0 - fw),
        fu * fv * (1.0 - fw),
        (1.0 - fu) * (1.0 - fv) * fw,
        fu * (1.0 - fv) * fw,
        (1.0 - fu) * fv * fw,
        fu * fv * fw,
    ];
    if indices.iter().any(|&index| index >= lattice.len()) {
        return None;
    }
    let position = indices
        .into_iter()
        .zip(weights)
        .map(|(index, weight)| lattice.position(index).map(|point| point * weight))
        .sum::<Option<DVec3>>()?;
    Some(BoxInterpolation {
        position: scene_scale.to_render_vec3(position),
        indices,
        weights,
    })
}

/// The physical distance one full lattice-index step covers along each axis,
/// scaled by the display axes' fractional increment — the oriented analogue
/// of [`uniform_domain_spacing`], which can read the step directly off an
/// axis-aligned [`GridLattice`] where this instead reads it off two adjacent
/// lattice points.
pub(super) fn uniform_box_spacing(
    lattice: BoxLattice,
    xs: &[f64],
    ys: &[f64],
    zs: &[f64],
    scene_scale: SceneScale,
) -> f32 {
    let counts = lattice.counts();
    let width = counts.x as usize;
    let height = counts.y as usize;
    let Some(origin) = lattice.position(0) else {
        return scene_scale.to_render(0.25);
    };
    let physical_step = |index: usize| lattice.position(index).map(|point| point.distance(origin));
    let spacing = |axis: &[f64], step_index: usize| {
        if axis.len() <= 1 {
            return None;
        }
        physical_step(step_index)
            .map(|step| scene_scale.to_render(((axis[1] - axis[0]) * step).abs()))
    };
    [
        spacing(xs, 1),
        spacing(ys, width),
        spacing(zs, width * height),
    ]
    .into_iter()
    .flatten()
    .filter(|spacing| *spacing > f32::EPSILON)
    .reduce(f32::min)
    .unwrap_or_else(|| scene_scale.to_render(0.25))
}

/// Coordinates in snapshot-lattice space, distributed uniformly across its
/// complete extent. Fractional coordinates deliberately support a display
/// density above or between the published sample counts without clustering on
/// integer sample indices.
pub(super) fn uniform_axis(count: u32, target: u32) -> Vec<f64> {
    if target == 0 {
        return Vec::new();
    }
    let extent = f64::from(count.saturating_sub(1));
    if target == 1 {
        return vec![extent * 0.5];
    }
    (0..target)
        .map(|index| f64::from(index) * extent / f64::from(target - 1))
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PlaneInterpolation {
    pub(super) position: Vec3,
    indices: [usize; 4],
    weights: [f64; 4],
}

impl PlaneInterpolation {
    pub(super) fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    pub(super) fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    pub(super) fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

pub(super) fn plane_interpolation(
    lattice: PlaneLattice,
    u: f64,
    v: f64,
    scene_scale: SceneScale,
) -> Option<PlaneInterpolation> {
    let counts = lattice.counts();
    let u = u.clamp(0.0, f64::from(counts.x.saturating_sub(1)));
    let v = v.clamp(0.0, f64::from(counts.y.saturating_sub(1)));
    let left = u.floor() as usize;
    let right = u.ceil() as usize;
    let lower = v.floor() as usize;
    let upper = v.ceil() as usize;
    let u_fraction = u - left as f64;
    let v_fraction = v - lower as f64;
    let width = counts.x as usize;
    let indices = [
        lower * width + left,
        lower * width + right,
        upper * width + right,
        upper * width + left,
    ];
    let weights = [
        (1.0 - u_fraction) * (1.0 - v_fraction),
        u_fraction * (1.0 - v_fraction),
        u_fraction * v_fraction,
        (1.0 - u_fraction) * v_fraction,
    ];
    let position = indices
        .into_iter()
        .zip(weights)
        .map(|(index, weight)| lattice.position(index).map(|point| point * weight))
        .sum::<Option<DVec3>>()?;
    Some(PlaneInterpolation {
        position: scene_scale.to_render_vec3(position),
        indices,
        weights,
    })
}

pub(super) fn uniform_glyph_spacing(
    lattice: PlaneLattice,
    xs: &[f64],
    ys: &[f64],
    scene_scale: SceneScale,
) -> f32 {
    let mut spacings = Vec::with_capacity(2);
    if xs.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0], scene_scale),
            plane_interpolation(lattice, xs[1], ys[0], scene_scale),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    if ys.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0], scene_scale),
            plane_interpolation(lattice, xs[0], ys[1], scene_scale),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    spacings
        .into_iter()
        .filter(|spacing| *spacing > f32::EPSILON)
        .reduce(f32::min)
        .unwrap_or_else(|| scene_scale.to_render(0.25))
}

pub(super) fn lattice_normal(lattice: PlaneLattice) -> Vec3 {
    let origin = lattice.position(0).unwrap_or_default();
    let u = lattice.position(1).unwrap_or(origin + DVec3::X) - origin;
    let v_index = lattice.counts().x as usize;
    let v = lattice.position(v_index).unwrap_or(origin + DVec3::Y) - origin;
    u.cross(v).normalize_or_zero().as_vec3()
}

/// The logarithmic magnitude normalization for one batch.
///
/// Computed once per batch rather than once per glyph. The previous per-glyph
/// scan was O(glyphs × samples), and both factors are user-editable densities on
/// the same axis, so the cost of raising a density slider grew quadratically.
///
/// Colour and glyph length share this value deliberately: normalizing them
/// against different maxima would let a plugin that reports a large finite
/// number alongside `Undefined` validity make an arrow's length disagree with
/// its own colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MagnitudeScale {
    pub(super) maximum: f64,
}

impl MagnitudeScale {
    /// Over usable samples only. An undefined sample's placeholder is not a
    /// measurement and must not set the scale for the ones that are.
    pub(super) fn over(values: &[DVec3], validity: &[SampleValidity]) -> Self {
        Self {
            maximum: values
                .iter()
                .zip(validity)
                .filter(|(_, validity)| validity.is_usable())
                .map(|(value, _)| value.length())
                .fold(0.0_f64, f64::max),
        }
    }

    /// Position of `magnitude` on a log ramp between the scale's noise floor and
    /// its maximum, in `0..=1`.
    pub(super) fn normalized(self, magnitude: f64) -> f32 {
        if magnitude <= 0.0 || self.maximum <= 0.0 {
            return 0.0;
        }
        let floor = (self.maximum * 1.0e-4).max(f64::MIN_POSITIVE);
        ((magnitude.max(floor).ln() - floor.ln()) / (self.maximum.ln() - floor.ln()).max(1.0e-12))
            .clamp(0.0, 1.0) as f32
    }

    /// The noise floor below which a sample is treated as effectively zero —
    /// used by the flow-line tracer to decide when a streamline has wandered
    /// somewhere the field no longer means anything.
    pub(super) fn noise_floor(self) -> f64 {
        (self.maximum * 1.0e-4).max(f64::MIN_POSITIVE)
    }

    pub(super) fn colors(self, values: &[DVec3], validity: &[SampleValidity]) -> Vec<Vec3> {
        values
            .iter()
            .zip(validity)
            .map(|(value, validity)| {
                if validity.is_usable() {
                    field_color(self.normalized(value.length()))
                } else {
                    Vec3::ZERO
                }
            })
            .collect()
    }

    /// Arrow length as a fraction of the glyph spacing. The floor keeps a weak
    /// but defined sample visible as a direction rather than a dot.
    pub(super) fn glyph_length(self, magnitude: f32) -> f32 {
        0.18 + self.normalized(f64::from(magnitude)) * 0.62
    }
}

pub(super) fn field_color(value: f32) -> Vec3 {
    let deep_blue = Vec3::new(0.02, 0.10, 0.42);
    let cyan = Vec3::new(0.02, 0.82, 0.88);
    let yellow = Vec3::new(1.0, 0.84, 0.12);
    let red = Vec3::new(0.95, 0.12, 0.04);
    if value < 0.4 {
        deep_blue.lerp(cyan, value / 0.4)
    } else if value < 0.75 {
        cyan.lerp(yellow, (value - 0.4) / 0.35)
    } else {
        yellow.lerp(red, (value - 0.75) / 0.25)
    }
}
