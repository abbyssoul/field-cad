//! Snapshot batches to drawable colour and glyphs.
//!
//! Everything here works from declared channel schemas and per-sample validity,
//! never from a named equation system: a generic layer must be able to draw
//! whatever a plugin publishes.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, FieldColumn, FieldSnapshot, PlaneId, SampleGeometry, SampleValidity,
};
use glam::{DVec3, Vec3};

use super::{
    ColoredVertex, FieldGeometry, FieldLayerSettings, PlaneLayerSettings, PlaneVectorMode,
    append_arrow,
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
                if plane_settings.vectors_visible {
                    // Lifted clear of the colour mesh so the arrows are not
                    // z-fighting with the surface they describe.
                    append_plane_vectors(
                        &mut output.vector_lines,
                        &field,
                        offset + normal * 0.008,
                        plane_settings.vector_density,
                    );
                }
            }
            SampleGeometry::Grid(lattice) if settings.domain_vectors => {
                let scale = MagnitudeScale::over(values, batch.validity());
                let colors = scale.colors(values, batch.validity());
                let characteristic_length = lattice.step().min_element().abs() as f32;
                for index in 0..lattice.len() {
                    if !batch.validity()[index].is_usable() {
                        continue;
                    }
                    let vector = values[index].as_vec3();
                    append_arrow(
                        &mut output.vector_lines,
                        lattice
                            .position(index)
                            .expect("index is inside lattice")
                            .as_vec3(),
                        vector,
                        characteristic_length * scale.glyph_length(vector.length()),
                        colors[index].extend(1.0),
                    );
                }
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
    density: u32,
) {
    let PlaneField {
        lattice,
        values,
        validity,
        colors,
        scale,
    } = *field;
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, density);
    let ys = uniform_axis(counts.y, density);
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
                step_length * scale.glyph_length(vector.length()),
                interpolation.vec3(colors).extend(1.0),
            );
        }
    }
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
    use glam::{DVec3, UVec2};

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
            25,
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

        append_plane_vectors(&mut lines, &field, Vec3::ZERO, 0);
        assert!(lines.is_empty());

        append_plane_vectors(&mut lines, &field, Vec3::ZERO, 8);
        assert_eq!(lines.len() / 6, 8 * 8);
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
}
