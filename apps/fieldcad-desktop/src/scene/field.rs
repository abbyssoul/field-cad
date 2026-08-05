//! Snapshot batches to drawable colour and glyphs.
//!
//! Everything here works from declared channel schemas and per-sample validity,
//! never from a named equation system: a generic layer must be able to draw
//! whatever a plugin publishes.

use std::collections::BTreeMap;

use fieldcad_core::{
    BoxId, BoxLattice, ChannelId, FieldColumn, FieldSnapshot, GridLattice, PlaneId,
    SampleGeometry, SampleValidity, SphereId, SphereLattice,
};
use glam::{DVec3, Vec3};

use super::{
    BoxLayerSettings, ColoredVertex, FieldGeometry, FieldLayerSettings, PlaneLayerSettings,
    PlaneVectorMode, SphereLayerSettings, VectorDisplay, append_arrow,
};

/// Convert one vector channel's batches into generic coloured triangles and
/// line glyphs. Undefined samples are omitted rather than clamped into
/// something that looks measured.
///
/// The channel is a parameter rather than a constant: a snapshot declares the
/// shape and dimension of everything it publishes, which is all a generic glyph
/// layer needs, so this stays independent of which equation systems are loaded.
pub fn field_geometry(
    snapshot: &FieldSnapshot,
    channel: &ChannelId,
    settings: FieldLayerSettings,
    plane_layers: &BTreeMap<PlaneId, PlaneLayerSettings>,
    box_layers: &BTreeMap<BoxId, BoxLayerSettings>,
    sphere_layers: &BTreeMap<SphereId, SphereLayerSettings>,
) -> FieldGeometry {
    let Some(channel) = snapshot.channel(channel) else {
        return FieldGeometry::default();
    };
    let mut output = FieldGeometry::default();

    for batch in channel.batches.iter() {
        let FieldColumn::Vector(values) = batch.values() else {
            continue;
        };
        match batch.geometry() {
            SampleGeometry::Plane { plane, lattice } => {
                let plane_settings = plane_layers.get(plane).copied().unwrap_or_default();
                // Whether this plane draws this field is the plane's own
                // setting. The channel's visibility is checked by the caller,
                // which is what keeps the two independent: hiding a field here
                // leaves every other plane showing it.
                if !plane_settings.visible {
                    continue;
                }
                let normal = lattice_normal(*lattice);
                let displayed_values: Vec<_> = values
                    .iter()
                    .map(|value| displayed_plane_vector(*value, normal, plane_settings.vector_mode))
                    .collect();
                let scale = MagnitudeScale::over(&displayed_values, batch.validity());
                let colors = scale.colors(&displayed_values, batch.validity());
                let field = PlaneField {
                    lattice: *lattice,
                    values: &displayed_values,
                    validity: batch.validity(),
                    colors: &colors,
                    scale,
                };
                let offset = normal * 0.006;
                if plane_settings.magnitude_visible {
                    append_plane_surface(
                        &mut output.surface_triangles,
                        &field,
                        offset,
                        plane_settings.magnitude_density,
                    );
                }
                if plane_settings.vectors.visible {
                    // Lifted clear of the colour mesh so the arrows are not
                    // z-fighting with the surface they describe.
                    append_plane_vectors(
                        &mut output.vector_lines,
                        &field,
                        offset + normal * 0.008,
                        plane_settings.vectors,
                    );
                }
            }
            SampleGeometry::Grid(lattice) if settings.vectors.visible => {
                let scale = MagnitudeScale::over(values, batch.validity());
                let colors = scale.colors(values, batch.validity());
                append_domain_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    settings.vectors,
                );
            }
            SampleGeometry::Box { region, lattice } => {
                let box_settings = box_layers.get(region).copied().unwrap_or_default();
                if !box_settings.visible || !box_settings.vectors.visible {
                    continue;
                }
                let scale = MagnitudeScale::over(values, batch.validity());
                let colors = scale.colors(values, batch.validity());
                append_box_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    box_settings.vectors,
                );
            }
            SampleGeometry::Sphere { region, lattice } => {
                let sphere_settings = sphere_layers.get(region).copied().unwrap_or_default();
                if !sphere_settings.visible || !sphere_settings.vectors.visible {
                    continue;
                }
                let scale = MagnitudeScale::over(values, batch.validity());
                let colors = scale.colors(values, batch.validity());
                append_sphere_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    sphere_settings.vectors,
                );
            }
            _ => {}
        }
    }
    output
}

fn displayed_plane_vector(value: DVec3, normal: Vec3, mode: PlaneVectorMode) -> DVec3 {
    match mode {
        PlaneVectorMode::InPlane => value - normal.as_dvec3() * value.dot(normal.as_dvec3()),
        PlaneVectorMode::Full3d => value,
    }
}

/// One plane batch prepared for drawing: where the samples are, what they are
/// worth, and the shared normalization that colour and glyph length both use.
///
/// The surface and vector layers read the same fields, so they take the same
/// value rather than a growing list of parallel slices that must stay aligned.
struct PlaneField<'a> {
    lattice: fieldcad_core::PlaneLattice,
    values: &'a [DVec3],
    validity: &'a [SampleValidity],
    colors: &'a [Vec3],
    scale: MagnitudeScale,
}

fn append_plane_surface(
    triangles: &mut Vec<ColoredVertex>,
    field: &PlaneField<'_>,
    offset: Vec3,
    density: u32,
) {
    let PlaneField {
        lattice,
        validity,
        colors,
        ..
    } = *field;
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, density);
    let ys = uniform_axis(counts.y, density);
    for y_pair in ys.windows(2) {
        for x_pair in xs.windows(2) {
            let sample = |u, v| {
                let interpolation = plane_interpolation(lattice, u, v)?;
                interpolation.is_usable(validity).then_some(ColoredVertex {
                    position: interpolation.position + offset,
                    color: interpolation.vec3(colors).extend(0.78),
                })
            };
            let (Some(lower_left), Some(lower_right), Some(upper_right), Some(upper_left)) = (
                sample(x_pair[0], y_pair[0]),
                sample(x_pair[1], y_pair[0]),
                sample(x_pair[1], y_pair[1]),
                sample(x_pair[0], y_pair[1]),
            ) else {
                continue;
            };
            triangles.extend([
                lower_left,
                lower_right,
                upper_right,
                lower_left,
                upper_right,
                upper_left,
            ]);
        }
    }
}

fn append_plane_vectors(
    lines: &mut Vec<ColoredVertex>,
    field: &PlaneField<'_>,
    offset: Vec3,
    display: VectorDisplay,
) {
    let PlaneField {
        lattice,
        values,
        validity,
        colors,
        scale,
    } = *field;
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let step_length = uniform_glyph_spacing(lattice, &xs, &ys);
    for &y in &ys {
        for &x in &xs {
            let Some(interpolation) = plane_interpolation(lattice, x, y) else {
                continue;
            };
            if !interpolation.is_usable(validity) {
                continue;
            }
            let vector = interpolation.dvec3(values).as_vec3();
            append_arrow(
                lines,
                interpolation.position + offset,
                vector,
                step_length * scale.glyph_length(vector.length()) * display.scale,
                interpolation.vec3(colors).extend(1.0),
            );
        }
    }
}

/// Sparse glyphs distributed uniformly through the published domain lattice.
///
/// Resampled the same way a plane is, rather than drawing one arrow per
/// published point: the transport stride and the display density answer
/// different questions, and tying them together means a user who wants a
/// legible volume has to ask the solver for fewer samples to get it.
fn append_domain_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: GridLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_domain_spacing(lattice, &xs, &ys, &zs);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = grid_interpolation(lattice, x, y, z) else {
                    continue;
                };
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values).as_vec3();
                append_arrow(
                    lines,
                    interpolation.position,
                    vector,
                    step_length * scale.glyph_length(vector.length()) * display.scale,
                    interpolation.vec3(colors).extend(1.0),
                );
            }
        }
    }
}

/// Sparse glyphs distributed uniformly through an oriented field box, resampled
/// from the published lattice the same way a plane or the whole domain is.
fn append_box_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: BoxLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_box_spacing(lattice, &xs, &ys, &zs);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = box_interpolation(lattice, x, y, z) else {
                    continue;
                };
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values).as_vec3();
                append_arrow(
                    lines,
                    interpolation.position,
                    vector,
                    step_length * scale.glyph_length(vector.length()) * display.scale,
                    interpolation.vec3(colors).extend(1.0),
                );
            }
        }
    }
}

/// Sparse glyphs distributed uniformly through a field sphere, culled to the
/// inscribed sphere rather than its published bounding cube — the solver
/// evaluated the whole cube (see [`SphereLattice`]), but only the samples
/// actually inside the sphere are drawn, which is what makes the arrows fill
/// a ball rather than a box.
fn append_sphere_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: SphereLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
) {
    let grid = lattice.grid();
    let counts = grid.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_domain_spacing(grid, &xs, &ys, &zs);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = grid_interpolation(grid, x, y, z) else {
                    continue;
                };
                if !lattice.contains(interpolation.position.as_dvec3()) {
                    continue;
                }
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values).as_vec3();
                append_arrow(
                    lines,
                    interpolation.position,
                    vector,
                    step_length * scale.glyph_length(vector.length()) * display.scale,
                    interpolation.vec3(colors).extend(1.0),
                );
            }
        }
    }
}

/// Trilinear interpolation of the published domain lattice, mirroring
/// [`PlaneInterpolation`] one dimension up.
#[derive(Clone, Copy, Debug)]
struct GridInterpolation {
    position: Vec3,
    indices: [usize; 8],
    weights: [f64; 8],
}

impl GridInterpolation {
    fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

fn grid_interpolation(lattice: GridLattice, x: f64, y: f64, z: f64) -> Option<GridInterpolation> {
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
        position: grid_point(lattice, x, y, z)?.as_vec3(),
        indices,
        weights,
    })
}

/// The lattice is axis-aligned with a uniform step, so a fractional coordinate
/// resolves without interpolating positions.
fn grid_point(lattice: GridLattice, x: f64, y: f64, z: f64) -> Option<DVec3> {
    Some(lattice.position(0)? + lattice.step() * DVec3::new(x, y, z))
}

fn uniform_domain_spacing(lattice: GridLattice, xs: &[f64], ys: &[f64], zs: &[f64]) -> f32 {
    let step = lattice.step();
    let spacing = |axis: &[f64], step: f64| {
        (axis.len() > 1).then(|| ((axis[1] - axis[0]) * step).abs() as f32)
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
    .unwrap_or(0.25)
}

/// Trilinear interpolation of an oriented [`BoxLattice`], mirroring
/// [`GridInterpolation`] with a position built from weighted corner samples —
/// the same technique [`PlaneInterpolation`] uses — since the lattice's u/v/w
/// steps need not be axis-aligned.
#[derive(Clone, Copy, Debug)]
struct BoxInterpolation {
    position: Vec3,
    indices: [usize; 8],
    weights: [f64; 8],
}

impl BoxInterpolation {
    fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

fn box_interpolation(lattice: BoxLattice, u: f64, v: f64, w: f64) -> Option<BoxInterpolation> {
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
        .sum::<Option<DVec3>>()?
        .as_vec3();
    Some(BoxInterpolation {
        position,
        indices,
        weights,
    })
}

/// The physical distance one full lattice-index step covers along each axis,
/// scaled by the display axes' fractional increment — the oriented analogue
/// of [`uniform_domain_spacing`], which can read the step directly off an
/// axis-aligned [`GridLattice`] where this instead reads it off two adjacent
/// lattice points.
fn uniform_box_spacing(lattice: BoxLattice, xs: &[f64], ys: &[f64], zs: &[f64]) -> f32 {
    let counts = lattice.counts();
    let width = counts.x as usize;
    let height = counts.y as usize;
    let Some(origin) = lattice.position(0) else {
        return 0.25;
    };
    let physical_step = |index: usize| lattice.position(index).map(|point| point.distance(origin));
    let spacing = |axis: &[f64], step_index: usize| {
        if axis.len() <= 1 {
            return None;
        }
        physical_step(step_index).map(|step| ((axis[1] - axis[0]) * step).abs() as f32)
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
    .unwrap_or(0.25)
}

/// Coordinates in snapshot-lattice space, distributed uniformly across its
/// complete extent. Fractional coordinates deliberately support a display
/// density above or between the published sample counts without clustering on
/// integer sample indices.
fn uniform_axis(count: u32, target: u32) -> Vec<f64> {
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
struct PlaneInterpolation {
    position: Vec3,
    indices: [usize; 4],
    weights: [f64; 4],
}

impl PlaneInterpolation {
    fn is_usable(self, validity: &[SampleValidity]) -> bool {
        self.indices
            .iter()
            .all(|&index| validity[index].is_usable())
    }

    fn dvec3(self, values: &[DVec3]) -> DVec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight)
            .sum()
    }

    fn vec3(self, values: &[Vec3]) -> Vec3 {
        self.indices
            .into_iter()
            .zip(self.weights)
            .map(|(index, weight)| values[index] * weight as f32)
            .sum()
    }
}

fn plane_interpolation(
    lattice: fieldcad_core::PlaneLattice,
    u: f64,
    v: f64,
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
        .sum::<Option<DVec3>>()?
        .as_vec3();
    Some(PlaneInterpolation {
        position,
        indices,
        weights,
    })
}

fn uniform_glyph_spacing(lattice: fieldcad_core::PlaneLattice, xs: &[f64], ys: &[f64]) -> f32 {
    let mut spacings = Vec::with_capacity(2);
    if xs.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0]),
            plane_interpolation(lattice, xs[1], ys[0]),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    if ys.len() > 1
        && let (Some(first), Some(second)) = (
            plane_interpolation(lattice, xs[0], ys[0]),
            plane_interpolation(lattice, xs[0], ys[1]),
        )
    {
        spacings.push(first.position.distance(second.position));
    }
    spacings
        .into_iter()
        .filter(|spacing| *spacing > f32::EPSILON)
        .reduce(f32::min)
        .unwrap_or(0.25)
}

fn lattice_normal(lattice: fieldcad_core::PlaneLattice) -> Vec3 {
    let origin = lattice.position(0).unwrap_or_default();
    let u = lattice.position(1).unwrap_or(origin + glam::DVec3::X) - origin;
    let v_index = lattice.counts().x as usize;
    let v = lattice.position(v_index).unwrap_or(origin + glam::DVec3::Y) - origin;
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
struct MagnitudeScale {
    maximum: f64,
}

impl MagnitudeScale {
    /// Over usable samples only. An undefined sample's placeholder is not a
    /// measurement and must not set the scale for the ones that are.
    fn over(values: &[DVec3], validity: &[SampleValidity]) -> Self {
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
    fn normalized(self, magnitude: f64) -> f32 {
        if magnitude <= 0.0 || self.maximum <= 0.0 {
            return 0.0;
        }
        let floor = (self.maximum * 1.0e-4).max(f64::MIN_POSITIVE);
        ((magnitude.max(floor).ln() - floor.ln()) / (self.maximum.ln() - floor.ln()).max(1.0e-12))
            .clamp(0.0, 1.0) as f32
    }

    fn colors(self, values: &[DVec3], validity: &[SampleValidity]) -> Vec<Vec3> {
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
    fn glyph_length(self, magnitude: f32) -> f32 {
        0.18 + self.normalized(f64::from(magnitude)) * 0.62
    }
}

fn field_color(value: f32) -> Vec3 {
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

#[cfg(test)]
mod tests {
    use fieldcad_core::{PlaneLattice, SampleValidity, UndefinedReason};
    use glam::{DVec3, UVec2, UVec3};

    use super::*;

    /// A uniform batch of unit +X vectors, every sample exact.
    fn uniform_plane_field<'a>(
        lattice: PlaneLattice,
        values: &'a [DVec3],
        validity: &'a [SampleValidity],
        colors: &'a [Vec3],
    ) -> PlaneField<'a> {
        PlaneField {
            lattice,
            values,
            validity,
            colors,
            scale: MagnitudeScale::over(values, validity),
        }
    }

    #[test]
    fn magnitude_surface_omits_cells_with_undefined_corners() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2));
        let values = vec![DVec3::X; 4];
        let colors = vec![Vec3::ONE; 4];
        let validity = [
            SampleValidity::Exact,
            SampleValidity::Exact,
            SampleValidity::Undefined(UndefinedReason::InsideSourceRadius),
            SampleValidity::Exact,
        ];
        let mut triangles = Vec::new();

        append_plane_surface(
            &mut triangles,
            &uniform_plane_field(lattice, &values, &validity, &colors),
            Vec3::ZERO,
            2,
        );

        assert!(triangles.is_empty());
    }

    #[test]
    fn a_valid_plane_cell_becomes_two_coloured_triangles() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2));
        let values = vec![DVec3::X; 4];
        let colors = vec![Vec3::new(0.2, 0.4, 0.8); 4];
        let validity = [SampleValidity::Exact; 4];
        let mut triangles = Vec::new();

        append_plane_surface(
            &mut triangles,
            &uniform_plane_field(lattice, &values, &validity, &colors),
            Vec3::ZERO,
            2,
        );

        assert_eq!(triangles.len(), 6);
        assert!(triangles.iter().all(|vertex| vertex.position.is_finite()));
        assert!(triangles.iter().all(|vertex| vertex.color.is_finite()));
    }

    #[test]
    fn magnitude_density_reduces_only_the_draw_mesh() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(9));
        let values = vec![DVec3::X; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let mut triangles = Vec::new();

        append_plane_surface(
            &mut triangles,
            &uniform_plane_field(lattice, &values, &validity, &colors),
            Vec3::ZERO,
            3,
        );

        assert_eq!(triangles.len(), 4 * 6);
    }

    #[test]
    fn vector_density_places_glyphs_uniformly_when_it_does_not_divide_the_snapshot() {
        let lattice = PlaneLattice::new(
            DVec3::new(-4.0, -4.0, 0.0),
            DVec3::new(0.25, 0.0, 0.0),
            DVec3::new(0.0, 0.25, 0.0),
            UVec2::splat(33),
        );
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let mut lines = Vec::new();

        append_plane_vectors(
            &mut lines,
            &uniform_plane_field(lattice, &values, &validity, &colors),
            Vec3::ZERO,
            VectorDisplay::new(true, 25),
        );

        let origins: Vec<_> = lines
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();
        assert_eq!(origins.len(), 25 * 25);
        let expected_step = 8.0 / 24.0;
        for pair in origins[..25].windows(2) {
            assert!((pair[1].x - pair[0].x - expected_step).abs() < 1.0e-5);
        }
        assert!((origins[0].x + 4.0).abs() < 1.0e-5);
        assert!((origins[24].x - 4.0).abs() < 1.0e-5);
    }

    #[test]
    fn vector_density_can_be_zero_or_exceed_the_snapshot_density() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(5));
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let mut lines = Vec::new();
        let field = uniform_plane_field(lattice, &values, &validity, &colors);

        append_plane_vectors(&mut lines, &field, Vec3::ZERO, VectorDisplay::new(true, 0));
        assert!(lines.is_empty());

        append_plane_vectors(&mut lines, &field, Vec3::ZERO, VectorDisplay::new(true, 8));
        assert_eq!(lines.len() / 6, 8 * 8);
    }

    /// A snapshot publishing one vector channel on two slice planes.
    fn two_plane_snapshot() -> (FieldSnapshot, ChannelId, [PlaneId; 2]) {
        use fieldcad_core::{
            ChannelSchema, Dimension, Domain, FieldValueKind, PluginId, SessionId,
            SnapshotCompleteness, SnapshotIdentity, WorldRevision,
        };
        use std::sync::Arc;

        let plugin = PluginId::new("test").unwrap();
        let channel = ChannelId::new(plugin.clone(), "vector").unwrap();
        let planes = [PlaneId::new(0), PlaneId::new(1)];
        let batches: Vec<_> = planes
            .iter()
            .map(|plane| {
                let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(2));
                fieldcad_core::FieldBatch::new(
                    SampleGeometry::Plane {
                        plane: *plane,
                        lattice,
                    },
                    FieldColumn::vectors(vec![DVec3::X; 4]),
                    vec![SampleValidity::Exact; 4],
                )
                .unwrap()
            })
            .collect();
        let snapshot = FieldSnapshot {
            identity: SnapshotIdentity {
                session: SessionId::from_u128(1),
                sequence: 0,
                run_generation: 0,
                world_revision: WorldRevision::INITIAL,
                tick: 0,
                time_seconds: 0.0,
            },
            completeness: SnapshotCompleteness::Complete,
            domain: Domain::centred_cube(4.0, 4).unwrap(),
            plugins: Arc::from([]),
            channels: BTreeMap::from([(
                channel.clone(),
                fieldcad_core::ChannelSnapshot {
                    schema: Arc::new(ChannelSchema {
                        id: channel.clone(),
                        display_name: "Vector".to_owned(),
                        value_kind: FieldValueKind::Vector(Dimension::ELECTRIC_FIELD),
                    }),
                    provider: plugin,
                    batches: batches.into(),
                },
            )]),
            diagnostics: Arc::from([]),
        };
        (snapshot, channel, planes)
    }

    /// Two controls, two questions. Whether a field is drawn at all belongs to
    /// the view; whether *this* plane draws it belongs to the plane. Reaching one
    /// through the other is what made turning a field off on a plane turn it off
    /// everywhere.
    #[test]
    fn a_plane_hides_a_field_without_hiding_it_on_other_planes() {
        let (snapshot, channel, planes) = two_plane_snapshot();
        let arrows = |layers: BTreeMap<PlaneId, PlaneLayerSettings>| {
            field_geometry(
                &snapshot,
                &channel,
                FieldLayerSettings::default(),
                &layers,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .vector_lines
            .len()
        };

        let both = arrows(BTreeMap::new());
        assert!(both > 0, "an unconfigured plane draws the visible layer");

        let hidden_here = PlaneLayerSettings {
            visible: false,
            ..PlaneLayerSettings::default()
        };
        let one_hidden = arrows(BTreeMap::from([(planes[0], hidden_here)]));

        assert_eq!(
            one_hidden,
            both / 2,
            "hiding the field on one plane must leave the other drawing it"
        );
        assert_eq!(
            arrows(BTreeMap::from([
                (planes[0], hidden_here),
                (planes[1], hidden_here),
            ])),
            0
        );
    }

    /// A uniform 3×3×3 domain batch of unit +X vectors.
    fn uniform_domain(counts: u32) -> (GridLattice, Vec<DVec3>, Vec<SampleValidity>, Vec<Vec3>) {
        let lattice = GridLattice::new(DVec3::splat(-1.0), DVec3::splat(1.0), UVec3::splat(counts));
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        (lattice, values, validity, colors)
    }

    fn domain_arrows(display: VectorDisplay) -> Vec<ColoredVertex> {
        let (lattice, values, validity, colors) = uniform_domain(3);
        let mut lines = Vec::new();
        append_domain_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            MagnitudeScale::over(&values, &validity),
            display,
        );
        lines
    }

    /// The whole-domain view resamples the published lattice like a plane does,
    /// rather than drawing one arrow per transported sample. Asking for a
    /// legible volume must not mean asking the solver for less.
    #[test]
    fn domain_density_is_a_display_setting_not_the_published_sample_count() {
        // Nine samples per axis were published; four arrows per axis are drawn.
        let sparse = domain_arrows(VectorDisplay::new(true, 4));
        assert_eq!(sparse.len() / 6, 4 * 4 * 4);

        // And a density above the published one interpolates rather than
        // refusing — the same latitude a plane already has.
        let dense = domain_arrows(VectorDisplay::new(true, 5));
        assert_eq!(dense.len() / 6, 5 * 5 * 5);

        assert!(domain_arrows(VectorDisplay::new(true, 0)).is_empty());
    }

    #[test]
    fn domain_arrows_span_the_published_lattice_uniformly() {
        let arrows = domain_arrows(VectorDisplay::new(true, 3));
        let origins: Vec<_> = arrows
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();

        // The lattice runs from -1 to +1 on each axis in steps of one.
        assert!((origins[0].x + 1.0).abs() < 1.0e-5);
        assert!((origins[2].x - 1.0).abs() < 1.0e-5);
        assert!((origins[0].z + 1.0).abs() < 1.0e-5);
        assert!((origins[26].z - 1.0).abs() < 1.0e-5);
        assert!(origins.iter().all(|origin| origin.is_finite()));
    }

    /// Scale is a multiplier on the automatic length and nothing else: the same
    /// arrows, in the same places, drawn longer.
    #[test]
    fn the_scale_factor_lengthens_arrows_without_moving_them() {
        let plain = domain_arrows(VectorDisplay::new(true, 3));
        let doubled = domain_arrows(VectorDisplay {
            scale: 2.0,
            ..VectorDisplay::new(true, 3)
        });

        assert_eq!(plain.len(), doubled.len());
        for (plain, doubled) in plain.chunks_exact(6).zip(doubled.chunks_exact(6)) {
            assert_eq!(plain[0].position, doubled[0].position);
            let plain_length = plain[1].position.distance(plain[0].position);
            let doubled_length = doubled[1].position.distance(doubled[0].position);
            assert!((doubled_length - plain_length * 2.0).abs() < 1.0e-5);
        }
    }

    /// The same setting, applied the same way, wherever the arrows are drawn.
    #[test]
    fn the_scale_factor_applies_on_a_plane_too() {
        let lattice = PlaneLattice::new(DVec3::ZERO, DVec3::X, DVec3::Y, UVec2::splat(3));
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let field = uniform_plane_field(lattice, &values, &validity, &colors);

        let length = |scale: f32| {
            let mut lines = Vec::new();
            append_plane_vectors(
                &mut lines,
                &field,
                Vec3::ZERO,
                VectorDisplay {
                    scale,
                    ..VectorDisplay::new(true, 3)
                },
            );
            lines[1].position.distance(lines[0].position)
        };

        assert!((length(2.0) - length(1.0) * 2.0).abs() < 1.0e-5);
        assert!((length(0.5) - length(1.0) * 0.5).abs() < 1.0e-5);
    }

    #[test]
    fn magnitude_scale_ignores_undefined_placeholders() {
        // An undefined sample's stored number is a placeholder, not a
        // measurement. If it set the scale, every real sample would be coloured
        // and drawn against a magnitude nothing actually reported.
        let values = [DVec3::X * 1_000.0, DVec3::X];
        let validity = [
            SampleValidity::Undefined(UndefinedReason::NotConverged),
            SampleValidity::Exact,
        ];

        let scale = MagnitudeScale::over(&values, &validity);

        assert_eq!(scale.maximum, 1.0);
        assert_eq!(scale.normalized(1.0), 1.0);
        // Colour and glyph length agree because they share one scale.
        assert_eq!(scale.colors(&values, &validity)[0], Vec3::ZERO);
        assert_eq!(scale.glyph_length(1.0), 0.18 + 0.62);
    }

    #[test]
    fn plane_vectors_default_to_the_in_plane_component() {
        let value = DVec3::new(1.0, 2.0, 3.0);

        assert_eq!(
            displayed_plane_vector(value, Vec3::Z, PlaneVectorMode::InPlane),
            DVec3::new(1.0, 2.0, 0.0)
        );
        assert_eq!(
            displayed_plane_vector(value, Vec3::Z, PlaneVectorMode::Full3d),
            value
        );
    }

    /// A uniform 3×3×3 unrotated box batch of unit +X vectors, spanning -1..1.
    fn uniform_box(counts: u32) -> (BoxLattice, Vec<DVec3>, Vec<SampleValidity>, Vec<Vec3>) {
        let lattice = BoxLattice::new(
            DVec3::splat(-1.0),
            DVec3::X,
            DVec3::Y,
            DVec3::Z,
            UVec3::splat(counts),
        );
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        (lattice, values, validity, colors)
    }

    fn box_arrows(display: VectorDisplay) -> Vec<ColoredVertex> {
        let (lattice, values, validity, colors) = uniform_box(3);
        let mut lines = Vec::new();
        append_box_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            MagnitudeScale::over(&values, &validity),
            display,
        );
        lines
    }

    #[test]
    fn box_density_is_a_display_setting_not_the_published_sample_count() {
        let sparse = box_arrows(VectorDisplay::new(true, 4));
        assert_eq!(sparse.len() / 6, 4 * 4 * 4);

        let dense = box_arrows(VectorDisplay::new(true, 5));
        assert_eq!(dense.len() / 6, 5 * 5 * 5);

        assert!(box_arrows(VectorDisplay::new(true, 0)).is_empty());
    }

    #[test]
    fn box_arrows_span_the_published_lattice_uniformly() {
        let arrows = box_arrows(VectorDisplay::new(true, 3));
        let origins: Vec<_> = arrows
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();

        assert!((origins[0].x + 1.0).abs() < 1.0e-5);
        assert!((origins[2].x - 1.0).abs() < 1.0e-5);
        assert!((origins[0].z + 1.0).abs() < 1.0e-5);
        assert!((origins[26].z - 1.0).abs() < 1.0e-5);
        assert!(origins.iter().all(|origin| origin.is_finite()));
    }

    /// The lattice's u/v/w axes need not be axis-aligned; a rotated box still
    /// resamples uniformly across its actual (rotated) extent.
    #[test]
    fn box_arrows_follow_a_rotated_lattice() {
        // Local X maps to world Y, local Y maps to world -X: a 90 degree turn
        // about Z.
        let lattice = BoxLattice::new(DVec3::ZERO, DVec3::Y, DVec3::NEG_X, DVec3::Z, UVec3::splat(2));
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        let mut lines = Vec::new();

        append_box_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            MagnitudeScale::over(&values, &validity),
            VectorDisplay::new(true, 2),
        );

        let origins: Vec<_> = lines
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();
        assert!(origins.contains(&Vec3::Y));
        assert!(origins.contains(&Vec3::NEG_X));
    }

    fn uniform_sphere(counts_per_axis: u32, radius: f64) -> (SphereLattice, Vec<DVec3>, Vec<SampleValidity>, Vec<Vec3>) {
        let lattice = SphereLattice::new(
            DVec3::splat(-radius),
            DVec3::splat(2.0 * radius / f64::from(counts_per_axis.saturating_sub(1)).max(1.0)),
            UVec3::splat(counts_per_axis),
            DVec3::ZERO,
            radius,
        );
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let colors = vec![Vec3::ONE; lattice.len()];
        (lattice, values, validity, colors)
    }

    /// Corner samples of the bounding cube lie outside the inscribed sphere
    /// and must not be drawn, even though the solver evaluated them: this is
    /// what makes the drawn arrows fill a ball rather than a box.
    #[test]
    fn sphere_arrows_are_culled_to_the_inscribed_sphere() {
        let (lattice, values, validity, colors) = uniform_sphere(5, 2.0);
        let mut lines = Vec::new();
        append_sphere_vectors(
            &mut lines,
            lattice,
            &values,
            &validity,
            &colors,
            MagnitudeScale::over(&values, &validity),
            VectorDisplay::new(true, 5),
        );

        let origins: Vec<_> = lines
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();
        assert!(!origins.is_empty());
        assert!(
            origins
                .iter()
                .all(|origin| origin.distance(Vec3::ZERO) <= 2.0 + 1.0e-4)
        );
        // A dense sampling of a 5x5x5 cube has corners outside the sphere, so
        // strictly fewer arrows are drawn than the cube holds samples.
        assert!(origins.len() < 5 * 5 * 5);
    }
}
