//! Snapshot batches to drawable colour and glyphs.
//!
//! Everything here works from declared channel schemas and per-sample validity,
//! never from a named equation system: a generic layer must be able to draw
//! whatever a plugin publishes.

use fieldcad_core::{
    BoxLattice, ChannelId, FieldBatch, FieldColumn, FieldSnapshot, GradientColumn, GridLattice,
    SampleGeometry, SampleValidity, SceneScale, SphereLattice, WorldSnapshot,
};
use glam::{DMat3, DVec3, Vec3};

use super::flow_lines::{
    trace_box_streamlines, trace_domain_streamlines, trace_plane_streamlines,
    trace_sphere_streamlines,
};
use super::interpolation::{
    MagnitudeScale, box_interpolation, grid_interpolation, lattice_normal, plane_interpolation,
    uniform_axis, uniform_box_spacing, uniform_domain_spacing, uniform_glyph_spacing,
};
use super::{
    ColoredVertex, FieldGeometry, FieldLayerSettings, PlaneVectorMode, RegionLayers,
    SceneVisibility, VectorDisplay, append_arrow,
};

/// Convert one vector channel's batches into generic coloured triangles and
/// line glyphs. Undefined samples are omitted rather than clamped into
/// something that looks measured.
///
/// The channel is a parameter rather than a constant: a snapshot declares the
/// shape and dimension of everything it publishes, which is all a generic glyph
/// layer needs, so this stays independent of which equation systems are loaded.
///
/// A batch whose region the believed `world` reports as hidden — or no longer
/// holds at all — is not drawn, however the batch arrived: what a retained
/// snapshot still contains is publication timing, and presentation must not
/// depend on it. This is the same per-entity check the authoring outline,
/// picking, and gizmo paths make.
pub fn field_geometry(
    snapshot: &FieldSnapshot,
    channel: &ChannelId,
    settings: FieldLayerSettings,
    layers: RegionLayers<'_>,
    show: SceneVisibility,
    world: &WorldSnapshot,
    scene_scale: SceneScale,
) -> FieldGeometry {
    let Some(channel) = snapshot.channel(channel) else {
        return FieldGeometry::default();
    };
    let mut output = FieldGeometry::default();
    for batch in channel.batches.iter() {
        let contribution = region_geometry(batch, settings, layers, show, world, scene_scale);
        output
            .surface_triangles
            .extend(contribution.surface_triangles);
        output.vector_lines.extend(contribution.vector_lines);
        output.flow_ribbons.extend(contribution.flow_ribbons);
    }
    output
}

/// One batch's own drawable contribution — the per-region unit
/// [`field_geometry`]'s loop already treats each batch as. Extracted so a
/// caller that already knows only one region's batch changed (see
/// `app.rs`'s per-region geometry cache) can rebuild just that region
/// instead of every batch in the channel.
pub(crate) fn region_geometry(
    batch: &FieldBatch,
    settings: FieldLayerSettings,
    layers: RegionLayers<'_>,
    show: SceneVisibility,
    world: &WorldSnapshot,
    scene_scale: SceneScale,
) -> FieldGeometry {
    let mut output = FieldGeometry::default();
    let FieldColumn::Vector(values) = batch.values() else {
        return output;
    };
    // Extracted once per batch, not once per glyph/streamline sample:
    // whether a batch carries a gradient is a per-batch fact, and every
    // consumer below falls back to today's trilinear/bilinear
    // reconstruction when it is absent.
    let gradient: Option<&[DMat3]> = match batch.gradient() {
        Some(GradientColumn::Vector(gradient)) => Some(gradient.as_ref()),
        _ => None,
    };
    match batch.geometry() {
        SampleGeometry::Plane { plane, lattice } => {
            if !show.planes {
                return output;
            }
            // The entity's own visibility, from the believed world — the
            // same gate the plane's authoring outline answers to.
            if !world.planes().get(plane).is_some_and(|plane| plane.visible) {
                return output;
            }
            let plane_settings = layers.planes.get(plane).copied().unwrap_or_default();
            // Whether this plane draws this field is the plane's own
            // setting. The channel's visibility is checked by the caller,
            // which is what keeps the two independent: hiding a field here
            // leaves every other plane showing it.
            if !plane_settings.visible {
                return output;
            }
            let normal = lattice_normal(*lattice);
            let displayed_values: Vec<_> = values
                .iter()
                .map(|value| displayed_plane_vector(*value, normal, plane_settings.vector_mode))
                .collect();
            // The projection applied to a value is linear, so the same
            // projection applied to its Jacobian keeps the two
            // consistent (∇(Pv) = P∇v for a constant projection P).
            let displayed_gradient: Option<Vec<DMat3>> = gradient.map(|gradient| {
                gradient
                    .iter()
                    .map(|matrix| {
                        displayed_plane_gradient(*matrix, normal, plane_settings.vector_mode)
                    })
                    .collect()
            });
            let displayed_gradient = displayed_gradient.as_deref();
            let scale = MagnitudeScale::over(&displayed_values, batch.validity());
            let colors = scale.colors(&displayed_values, batch.validity());
            let field = PlaneField {
                lattice: *lattice,
                values: &displayed_values,
                gradient: displayed_gradient,
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
                    scene_scale,
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
                    scene_scale,
                );
            }
            if plane_settings.flow_lines.visible {
                // Traces the same in-plane-projected values the arrows
                // above draw: a 2D streamline cannot depict an
                // out-of-plane component either.
                output.flow_ribbons.extend(trace_plane_streamlines(
                    *lattice,
                    &displayed_values,
                    batch.validity(),
                    scale,
                    plane_settings.flow_lines,
                    scene_scale,
                    displayed_gradient,
                ));
            }
        }
        SampleGeometry::Grid(lattice)
            if settings.vectors.visible || settings.flow_lines.visible =>
        {
            let scale = MagnitudeScale::over(values, batch.validity());
            if settings.vectors.visible {
                let colors = scale.colors(values, batch.validity());
                append_domain_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    settings.vectors,
                    scene_scale,
                    gradient,
                );
            }
            if settings.flow_lines.visible {
                output.flow_ribbons.extend(trace_domain_streamlines(
                    *lattice,
                    values,
                    batch.validity(),
                    scale,
                    settings.flow_lines,
                    scene_scale,
                    gradient,
                ));
            }
        }
        SampleGeometry::Box { region, lattice } => {
            if !show.boxes {
                return output;
            }
            if !world
                .boxes()
                .get(region)
                .is_some_and(|region| region.visible)
            {
                return output;
            }
            let box_settings = layers.boxes.get(region).copied().unwrap_or_default();
            if !box_settings.visible
                || (!box_settings.vectors.visible && !box_settings.flow_lines.visible)
            {
                return output;
            }
            let scale = MagnitudeScale::over(values, batch.validity());
            if box_settings.vectors.visible {
                let colors = scale.colors(values, batch.validity());
                append_box_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    box_settings.vectors,
                    scene_scale,
                    gradient,
                );
            }
            if box_settings.flow_lines.visible {
                output.flow_ribbons.extend(trace_box_streamlines(
                    *lattice,
                    values,
                    batch.validity(),
                    scale,
                    box_settings.flow_lines,
                    scene_scale,
                    gradient,
                ));
            }
        }
        SampleGeometry::Sphere { region, lattice } => {
            if !show.spheres {
                return output;
            }
            if !world
                .spheres()
                .get(region)
                .is_some_and(|region| region.visible)
            {
                return output;
            }
            let sphere_settings = layers.spheres.get(region).copied().unwrap_or_default();
            if !sphere_settings.visible
                || (!sphere_settings.vectors.visible && !sphere_settings.flow_lines.visible)
            {
                return output;
            }
            let scale = MagnitudeScale::over(values, batch.validity());
            if sphere_settings.vectors.visible {
                let colors = scale.colors(values, batch.validity());
                append_sphere_vectors(
                    &mut output.vector_lines,
                    *lattice,
                    values,
                    batch.validity(),
                    &colors,
                    scale,
                    sphere_settings.vectors,
                    scene_scale,
                    gradient,
                );
            }
            if sphere_settings.flow_lines.visible {
                output.flow_ribbons.extend(trace_sphere_streamlines(
                    *lattice,
                    values,
                    batch.validity(),
                    scale,
                    sphere_settings.flow_lines,
                    scene_scale,
                    gradient,
                ));
            }
        }
        _ => {}
    }
    output
}

fn displayed_plane_vector(value: DVec3, normal: Vec3, mode: PlaneVectorMode) -> DVec3 {
    match mode {
        PlaneVectorMode::InPlane => value - normal.as_dvec3() * value.dot(normal.as_dvec3()),
        PlaneVectorMode::Full3d => value,
    }
}

/// The Jacobian counterpart of [`displayed_plane_vector`]: `InPlane`
/// projects out the normal component of the *value*, a linear operation
/// `P = I - n⊗n`, so the consistent gradient is `P` applied to the Jacobian
/// (`∇(Pv) = P∇v` for a constant `P`), not the raw published Jacobian.
fn displayed_plane_gradient(gradient: DMat3, normal: Vec3, mode: PlaneVectorMode) -> DMat3 {
    match mode {
        PlaneVectorMode::InPlane => {
            let n = normal.as_dvec3();
            let projection = DMat3::IDENTITY - DMat3::from_cols(n.x * n, n.y * n, n.z * n);
            projection * gradient
        }
        PlaneVectorMode::Full3d => gradient,
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
    gradient: Option<&'a [DMat3]>,
    validity: &'a [SampleValidity],
    colors: &'a [Vec3],
    scale: MagnitudeScale,
}

fn append_plane_surface(
    triangles: &mut Vec<ColoredVertex>,
    field: &PlaneField<'_>,
    offset: Vec3,
    density: u32,
    scene_scale: SceneScale,
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
                let interpolation = plane_interpolation(lattice, u, v, scene_scale)?;
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
    scene_scale: SceneScale,
) {
    let PlaneField {
        lattice,
        values,
        gradient,
        validity,
        colors,
        scale,
    } = *field;
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let step_length = uniform_glyph_spacing(lattice, &xs, &ys, scene_scale);
    for &y in &ys {
        for &x in &xs {
            let Some(interpolation) = plane_interpolation(lattice, x, y, scene_scale) else {
                continue;
            };
            if !interpolation.is_usable(validity) {
                continue;
            }
            let vector = interpolation.dvec3(values, gradient).as_vec3();
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
#[allow(clippy::too_many_arguments)]
fn append_domain_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: GridLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
    scene_scale: SceneScale,
    gradient: Option<&[DMat3]>,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_domain_spacing(lattice, &xs, &ys, &zs, scene_scale);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = grid_interpolation(lattice, x, y, z, scene_scale) else {
                    continue;
                };
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values, gradient).as_vec3();
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
#[allow(clippy::too_many_arguments)]
fn append_box_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: BoxLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
    scene_scale: SceneScale,
    gradient: Option<&[DMat3]>,
) {
    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_box_spacing(lattice, &xs, &ys, &zs, scene_scale);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = box_interpolation(lattice, x, y, z, scene_scale) else {
                    continue;
                };
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values, gradient).as_vec3();
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
#[allow(clippy::too_many_arguments)]
fn append_sphere_vectors(
    lines: &mut Vec<ColoredVertex>,
    lattice: SphereLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    colors: &[Vec3],
    scale: MagnitudeScale,
    display: VectorDisplay,
    scene_scale: SceneScale,
    gradient: Option<&[DMat3]>,
) {
    let grid = lattice.grid();
    let counts = grid.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length = uniform_domain_spacing(grid, &xs, &ys, &zs, scene_scale);
    for &z in &zs {
        for &y in &ys {
            for &x in &xs {
                let Some(interpolation) = grid_interpolation(grid, x, y, z, scene_scale) else {
                    continue;
                };
                // `interpolation.position` is render-space; the sphere lattice's
                // own containment test is defined in world (SI-metre) space, so
                // it needs the inverse conversion, not a bare widening cast.
                if !lattice.contains(scene_scale.to_world_vec3(interpolation.position)) {
                    continue;
                }
                if !interpolation.is_usable(validity) {
                    continue;
                }
                let vector = interpolation.dvec3(values, gradient).as_vec3();
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fieldcad_core::{PlaneId, PlaneLattice, SampleValidity, UndefinedReason};
    use glam::{DVec3, UVec2, UVec3};

    use super::*;
    use crate::scene::{FlowLineDisplay, PlaneLayerSettings};

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
            gradient: None,
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
            SceneScale::metre(),
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
            SceneScale::metre(),
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
            SceneScale::metre(),
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
            SceneScale::metre(),
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

        append_plane_vectors(
            &mut lines,
            &field,
            Vec3::ZERO,
            VectorDisplay::new(true, 0),
            SceneScale::metre(),
        );
        assert!(lines.is_empty());

        append_plane_vectors(
            &mut lines,
            &field,
            Vec3::ZERO,
            VectorDisplay::new(true, 8),
            SceneScale::metre(),
        );
        assert_eq!(lines.len() / 6, 8 * 8);
    }

    /// A snapshot publishing one vector channel over the given batches.
    fn single_vector_channel_snapshot(
        batches: Vec<fieldcad_core::FieldBatch>,
    ) -> (FieldSnapshot, ChannelId) {
        use fieldcad_core::{
            ChannelSchema, Dimension, Domain, FieldValueKind, PluginId, SessionId,
            SnapshotCompleteness, SnapshotIdentity, WorldRevision,
        };
        use std::sync::Arc;

        let plugin = PluginId::new("test").unwrap();
        let channel = ChannelId::new(plugin.clone(), "vector").unwrap();
        let snapshot = FieldSnapshot {
            identity: SnapshotIdentity {
                session: SessionId::from_u128(1),
                sequence: 0,
                run_generation: 0,
                world_revision: WorldRevision::INITIAL,
                tick: 0,
                time_seconds: 0.0,
            },
            expression_graph_hash: None,
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
            distances: Arc::from([]),
            mass_aggregates: Arc::from([]),
        };
        (snapshot, channel)
    }

    /// A world holding two visible slice planes, so the batch ids in a
    /// snapshot and the world's ids agree — the same correspondence the
    /// runtime's own publication guarantees.
    fn world_with_two_planes() -> (fieldcad_core::World, [PlaneId; 2]) {
        let mut world = fieldcad_core::World::new();
        let report = world
            .commit([
                fieldcad_core::WorldCommand::CreatePlane(
                    fieldcad_core::SlicePlaneSpec::new("Plane A", DVec3::ZERO, DVec3::Z).unwrap(),
                ),
                fieldcad_core::WorldCommand::CreatePlane(
                    fieldcad_core::SlicePlaneSpec::new("Plane B", DVec3::ZERO, DVec3::Z).unwrap(),
                ),
            ])
            .unwrap();
        (world, [report.created_planes[0], report.created_planes[1]])
    }

    /// A snapshot publishing one vector channel on the world's two slice
    /// planes.
    fn two_plane_snapshot(planes: [PlaneId; 2]) -> (FieldSnapshot, ChannelId) {
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
        single_vector_channel_snapshot(batches)
    }

    /// Two controls, two questions. Whether a field is drawn at all belongs to
    /// the view; whether *this* plane draws it belongs to the plane. Reaching one
    /// through the other is what made turning a field off on a plane turn it off
    /// everywhere.
    #[test]
    fn a_plane_hides_a_field_without_hiding_it_on_other_planes() {
        let (world, planes) = world_with_two_planes();
        let (snapshot, channel) = two_plane_snapshot(planes);
        let arrows = |layers: BTreeMap<PlaneId, PlaneLayerSettings>| {
            field_geometry(
                &snapshot,
                &channel,
                FieldLayerSettings::default(),
                RegionLayers {
                    planes: &layers,
                    boxes: &BTreeMap::new(),
                    spheres: &BTreeMap::new(),
                },
                SceneVisibility::ALL,
                &world.snapshot(),
                SceneScale::metre(),
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

    #[test]
    fn hiding_planes_hides_their_field_geometry_too() {
        let (world, planes) = world_with_two_planes();
        let (snapshot, channel) = two_plane_snapshot(planes);
        let geometry = field_geometry(
            &snapshot,
            &channel,
            FieldLayerSettings::default(),
            RegionLayers {
                planes: &BTreeMap::new(),
                boxes: &BTreeMap::new(),
                spheres: &BTreeMap::new(),
            },
            SceneVisibility {
                planes: false,
                ..SceneVisibility::ALL
            },
            &world.snapshot(),
            SceneScale::metre(),
        );

        assert!(geometry.surface_triangles.is_empty());
        assert!(geometry.vector_lines.is_empty());
    }

    /// UI-4 regression: the entity's own `visible` flag gates its field
    /// geometry exactly the way it gates the authoring outline, picking, and
    /// the gizmo. Until this check existed, a hidden plane's arrows and
    /// magnitude mesh kept drawing from a retained snapshot for as long as no
    /// republication happened to drop its batches — presentation depending on
    /// publication timing, which is not the desktop's to control.
    #[test]
    fn a_hidden_plane_draws_no_field_geometry() {
        let (mut world, planes) = world_with_two_planes();
        let (snapshot, channel) = two_plane_snapshot(planes);
        let arrows = |world: &fieldcad_core::World| {
            field_geometry(
                &snapshot,
                &channel,
                FieldLayerSettings::default(),
                RegionLayers {
                    planes: &BTreeMap::new(),
                    boxes: &BTreeMap::new(),
                    spheres: &BTreeMap::new(),
                },
                SceneVisibility::ALL,
                &world.snapshot(),
                SceneScale::metre(),
            )
            .vector_lines
            .len()
        };

        let both = arrows(&world);
        assert!(both > 0, "test setup: two visible planes draw arrows");

        world
            .commit([fieldcad_core::WorldCommand::SetPlaneVisible {
                plane: planes[0],
                visible: false,
            }])
            .unwrap();
        assert_eq!(
            arrows(&world),
            both / 2,
            "a hidden plane draws nothing, even with its layer settings still \
             showing the field"
        );

        world
            .commit([fieldcad_core::WorldCommand::SetPlaneVisible {
                plane: planes[1],
                visible: false,
            }])
            .unwrap();
        assert_eq!(arrows(&world), 0);
    }

    #[test]
    fn a_hidden_box_draws_no_field_geometry() {
        let mut world = fieldcad_core::World::new();
        let report = world
            .commit([fieldcad_core::WorldCommand::CreateBox(
                fieldcad_core::FieldBoxSpec::new("Box", DVec3::ZERO, DVec3::splat(1.0)).unwrap(),
            )])
            .unwrap();
        let region = report.created_boxes[0];
        let (snapshot, channel) = single_vector_channel_snapshot(vec![
            fieldcad_core::FieldBatch::new(
                SampleGeometry::Box {
                    region,
                    lattice: BoxLattice::new(
                        DVec3::splat(-1.0),
                        DVec3::X,
                        DVec3::Y,
                        DVec3::Z,
                        UVec3::splat(2),
                    ),
                },
                FieldColumn::vectors(vec![DVec3::X; 8]),
                vec![SampleValidity::Exact; 8],
            )
            .unwrap(),
        ]);
        let arrows = |world: &fieldcad_core::World| {
            field_geometry(
                &snapshot,
                &channel,
                FieldLayerSettings::default(),
                RegionLayers {
                    planes: &BTreeMap::new(),
                    boxes: &BTreeMap::new(),
                    spheres: &BTreeMap::new(),
                },
                SceneVisibility::ALL,
                &world.snapshot(),
                SceneScale::metre(),
            )
            .vector_lines
            .len()
        };

        assert!(arrows(&world) > 0, "test setup: a visible box draws arrows");

        world
            .commit([fieldcad_core::WorldCommand::SetBoxVisible {
                region,
                visible: false,
            }])
            .unwrap();
        assert_eq!(arrows(&world), 0, "a hidden box draws nothing");
    }

    #[test]
    fn a_hidden_sphere_draws_no_field_geometry() {
        let mut world = fieldcad_core::World::new();
        let report = world
            .commit([fieldcad_core::WorldCommand::CreateSphere(
                fieldcad_core::FieldSphereSpec::new("Sphere", DVec3::ZERO, 1.0).unwrap(),
            )])
            .unwrap();
        let region = report.created_spheres[0];
        let (snapshot, channel) = single_vector_channel_snapshot(vec![
            fieldcad_core::FieldBatch::new(
                SampleGeometry::Sphere {
                    region,
                    lattice: SphereLattice::new(
                        DVec3::splat(-1.0),
                        DVec3::splat(1.0),
                        UVec3::splat(3),
                        DVec3::ZERO,
                        1.0,
                    ),
                },
                FieldColumn::vectors(vec![DVec3::X; 27]),
                vec![SampleValidity::Exact; 27],
            )
            .unwrap(),
        ]);
        let arrows = |world: &fieldcad_core::World| {
            field_geometry(
                &snapshot,
                &channel,
                FieldLayerSettings::default(),
                RegionLayers {
                    planes: &BTreeMap::new(),
                    boxes: &BTreeMap::new(),
                    spheres: &BTreeMap::new(),
                },
                SceneVisibility::ALL,
                &world.snapshot(),
                SceneScale::metre(),
            )
            .vector_lines
            .len()
        };

        assert!(
            arrows(&world) > 0,
            "test setup: a visible sphere draws arrows"
        );

        world
            .commit([fieldcad_core::WorldCommand::SetSphereVisible {
                sphere: region,
                visible: false,
            }])
            .unwrap();
        assert_eq!(arrows(&world), 0, "a hidden sphere draws nothing");
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
            SceneScale::metre(),
            None,
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
                SceneScale::metre(),
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
            SceneScale::metre(),
            None,
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
        let lattice = BoxLattice::new(
            DVec3::ZERO,
            DVec3::Y,
            DVec3::NEG_X,
            DVec3::Z,
            UVec3::splat(2),
        );
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
            SceneScale::metre(),
            None,
        );

        let origins: Vec<_> = lines
            .chunks_exact(6)
            .map(|arrow| arrow[0].position)
            .collect();
        assert!(origins.contains(&Vec3::Y));
        assert!(origins.contains(&Vec3::NEG_X));
    }

    fn uniform_sphere(
        counts_per_axis: u32,
        radius: f64,
    ) -> (SphereLattice, Vec<DVec3>, Vec<SampleValidity>, Vec<Vec3>) {
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
            SceneScale::metre(),
            None,
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

    /// Flow lines and arrows are independent controls: a region can draw
    /// either, both, or neither, and one being off must not silence the
    /// other.
    #[test]
    fn domain_flow_lines_are_independent_of_arrow_visibility() {
        let (lattice, values, validity, _colors) = uniform_domain(5);
        let scale = MagnitudeScale::over(&values, &validity);
        // `visible` is a caller-side gate, the same convention arrows already
        // follow (`append_domain_vectors` does not itself re-check
        // `VectorDisplay::visible` either) — checked at the `field_geometry`
        // level below, not inside the tracer.
        let ribbons = trace_domain_streamlines(
            lattice,
            &values,
            &validity,
            scale,
            FlowLineDisplay::new(true, 3),
            SceneScale::metre(),
            None,
        );
        assert!(
            !ribbons.is_empty(),
            "a uniform field seeds and traces at least one streamline"
        );
        assert!(
            ribbons.len().is_multiple_of(6),
            "ribbons are built from whole quads"
        );

        // The same field_geometry() entry point draws flow lines with arrows
        // switched off, and vice versa.
        let (snapshot, channel) = single_vector_channel_snapshot(vec![
            fieldcad_core::FieldBatch::exact(
                SampleGeometry::Grid(lattice),
                FieldColumn::vectors(values.clone()),
            )
            .unwrap(),
        ]);
        let world = fieldcad_core::World::new();
        let geometry = |settings: FieldLayerSettings| {
            field_geometry(
                &snapshot,
                &channel,
                settings,
                RegionLayers {
                    planes: &BTreeMap::new(),
                    boxes: &BTreeMap::new(),
                    spheres: &BTreeMap::new(),
                },
                SceneVisibility::ALL,
                &world.snapshot(),
                SceneScale::metre(),
            )
        };

        let flow_only = geometry(FieldLayerSettings {
            vectors: VectorDisplay::new(false, 0),
            flow_lines: FlowLineDisplay::new(true, 3),
        });
        assert!(flow_only.vector_lines.is_empty());
        assert!(!flow_only.flow_ribbons.is_empty());

        let arrows_only = geometry(FieldLayerSettings {
            vectors: VectorDisplay::new(true, 3),
            flow_lines: FlowLineDisplay::new(false, 3),
        });
        assert!(!arrows_only.vector_lines.is_empty());
        assert!(arrows_only.flow_ribbons.is_empty());
    }

    /// A plane's flow lines trace the in-plane projection even when its
    /// arrows are configured to show the full 3D vector — a 2D streamline
    /// cannot depict an out-of-plane component either.
    #[test]
    fn plane_flow_lines_trace_the_in_plane_projection_regardless_of_vector_mode() {
        let lattice = PlaneLattice::new(
            DVec3::new(-2.0, -2.0, 0.0),
            DVec3::new(0.5, 0.0, 0.0),
            DVec3::new(0.0, 0.5, 0.0),
            UVec2::splat(9),
        );
        // A field with a large out-of-plane component: if flow lines ever
        // saw the raw value instead of the projection, tracing would leave
        // the plane immediately and produce no visible line.
        let values = vec![DVec3::new(1.0, 0.0, 50.0); lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];

        let (world, planes) = {
            let mut world = fieldcad_core::World::new();
            let report = world
                .commit([fieldcad_core::WorldCommand::CreatePlane(
                    fieldcad_core::SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
                )])
                .unwrap();
            (world, [report.created_planes[0]])
        };
        let (snapshot, channel) = single_vector_channel_snapshot(vec![
            fieldcad_core::FieldBatch::new(
                SampleGeometry::Plane {
                    plane: planes[0],
                    lattice,
                },
                FieldColumn::vectors(values),
                validity,
            )
            .unwrap(),
        ]);

        let mut layers = BTreeMap::new();
        layers.insert(
            planes[0],
            PlaneLayerSettings {
                vector_mode: PlaneVectorMode::Full3d,
                flow_lines: FlowLineDisplay::new(true, 5),
                ..PlaneLayerSettings::default()
            },
        );
        let geometry = field_geometry(
            &snapshot,
            &channel,
            FieldLayerSettings::default(),
            RegionLayers {
                planes: &layers,
                boxes: &BTreeMap::new(),
                spheres: &BTreeMap::new(),
            },
            SceneVisibility::ALL,
            &world.snapshot(),
            SceneScale::metre(),
        );

        assert!(
            !geometry.flow_ribbons.is_empty(),
            "the in-plane component (1, 0, 0) is enough to trace a line even \
             though the raw value points mostly out of the plane"
        );
    }
}
