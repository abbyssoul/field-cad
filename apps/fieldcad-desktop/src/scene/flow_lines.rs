//! Streamline tracing: turning a sampled vector field into continuous flow
//! lines instead of discrete arrow glyphs.
//!
//! The tracer works in world space (SI metres), against whatever
//! interpolation [`super::interpolation`] already trusts for arrows — a
//! streamline and an arrow drawn from the same snapshot must never disagree
//! about what the field looks like at a point, and re-invoking the solver
//! mid-trace would defeat the batched-sampling design the rest of the
//! renderer already respects. Render-space conversion and ribbon-vertex
//! construction happen only once the polyline is final.

use fieldcad_core::{
    BoxLattice, GridLattice, PlaneLattice, SampleValidity, SceneScale, SphereLattice,
};
use glam::{DVec3, Vec3};

use super::interpolation::{
    MagnitudeScale, box_interpolation, field_color, grid_interpolation, plane_interpolation,
    uniform_axis, uniform_box_spacing, uniform_domain_spacing, uniform_glyph_spacing,
};
use super::{FlowLineDisplay, FlowRibbonVertex};

/// Hard cap on a streamline's reach in one direction from a seed, measured in
/// units of the *nominal* (seed-spacing-derived) step length rather than a
/// raw count of [`adaptive_step`] calls. Guards against a closed field line
/// (a dipole loop, for instance) that would otherwise never leave any
/// bounded region and never terminate on its own.
///
/// Counting nominal-step-equivalents instead of calls matters once
/// `adaptive_step` can shrink: a call that only advances by a tenth of the
/// nominal step (because the field turned sharply right there) must only
/// spend a tenth of the budget, or a trace that dips near a steep field
/// feature — a point charge's near-singular falloff, for instance — would
/// exhaust its whole reach resolving that one feature and never travel the
/// rest of the domain. A denser seeding (smaller nominal step) would then
/// make that exhaustion worse, not better, which is exactly backwards from
/// what raising density should do.
const MAX_STEPS: u32 = 500;

/// Hard ceiling on raw [`adaptive_step`] calls in one direction, independent
/// of the fractional budget above. Purely a compute-cost safety valve for a
/// pathological trace that keeps getting shrunk to a sliver near
/// [`MIN_STEP_FRACTION`] without leaving the region or exhausting its
/// distance budget; `64` is `1 / MIN_STEP_FRACTION`, the most a single step
/// can be shrunk, so this allows every one of `MAX_STEPS` budget units to be
/// spent at the smallest possible increment.
const MAX_ITERATIONS: u32 = MAX_STEPS * 64;

/// Hard cap on ribbon vertices produced by one shape's flow-line pass.
///
/// Seed count grows with `density` cubed for a volume, and each seed can cost
/// up to `2 * MAX_STEPS` points before this cap existed — a dense box or a
/// field that happens not to leave a large volume quickly can multiply those
/// into tens of millions of vertices well before hitting the GPU's own
/// buffer-size limit, stalling the frame (or, before the renderer's own
/// defensive clamp existed, crashing it outright). This cap stops *tracing*
/// once it is reached rather than relying solely on the renderer to discard
/// the excess, so an oversized request costs a bounded amount of CPU too.
/// 300,000 vertices is 50,000 ribbon segments — generous for what a scene can
/// usefully show at once, and small next to the renderer's own multi-million-
/// vertex ceiling.
const MAX_RIBBON_VERTICES: usize = 300_000;

/// Lower bound on how many points [`fair_share_points_per_direction`] may
/// cap a single trace direction to, however many seeds are sharing the
/// budget. Without a floor, a very dense seed grid would reduce every
/// streamline to a handful of points too short to read as a line at all.
const MIN_POINTS_PER_DIRECTION: usize = 32;

/// A fair per-seed share of [`MAX_RIBBON_VERTICES`], expressed as a cap on
/// how many points *one direction* of one seed's trace may output.
///
/// Without this, a seed whose trace lingers near a steep field feature —
/// every seed, eventually, in a field dominated by a single point charge,
/// since every backward trace converges on it — could spend its entire
/// reach budget on fine adaptive subdivision and alone produce tens of
/// thousands of points, exhausting the *global* vertex budget before most
/// of the seed grid was ever traced. Even combined with visiting seeds in
/// [`low_discrepancy_order`], that meant only the handful of seeds
/// processed before the budget ran out — often clustered near the same
/// expensive feature — ever appeared, which read as flow lines flattened
/// into one plane or bunched toward one direction instead of filling the
/// requested region. Capping each seed to a fair share of the budget keeps
/// one seed's cost from crowding out the rest of the grid; `12` converts a
/// vertex share to a points-per-direction share (`build_flow_ribbon`
/// produces `6` vertices per segment, and a seed's cost is split evenly
/// across its two trace directions).
fn fair_share_points_per_direction(seed_count: usize) -> usize {
    let fair_share_vertices = MAX_RIBBON_VERTICES / seed_count.max(1);
    (fair_share_vertices / 12).max(MIN_POINTS_PER_DIRECTION)
}

/// The field's normalized direction at a point — a streamline is a geometric
/// curve, not a trajectory, so only direction matters, never magnitude.
///
/// `direction_sign` is `1.0` to trace with the field, `-1.0` against it.
fn tangent(
    point: DVec3,
    direction_sign: f64,
    sample: &impl Fn(DVec3) -> Option<DVec3>,
) -> Option<DVec3> {
    let value = sample(point)?;
    let length = value.length();
    (length > 0.0 && length.is_finite()).then(|| value / length * direction_sign)
}

/// One RK4 step of `step_length` along the field's normalized direction.
/// Returns the stepped-to position together with the entry tangent sampled
/// at `position` (already computed as `k1`), so [`adaptive_step`] can judge
/// how sharply the field turned over the step without resampling it.
fn rk4_step(
    position: DVec3,
    step_length: f64,
    direction_sign: f64,
    sample: &impl Fn(DVec3) -> Option<DVec3>,
) -> Option<(DVec3, DVec3)> {
    let k1 = tangent(position, direction_sign, sample)?;
    let k2 = tangent(position + k1 * (step_length * 0.5), direction_sign, sample)?;
    let k3 = tangent(position + k2 * (step_length * 0.5), direction_sign, sample)?;
    let k4 = tangent(position + k3 * step_length, direction_sign, sample)?;
    let delta = (k1 + k2 * 2.0 + k3 * 2.0 + k4) * (step_length / 6.0);
    Some((position + delta, k1))
}

/// Cosine of the largest direction turn, entry to exit, an accepted step may
/// exhibit (`0.95` is roughly 18 degrees). A field resampled from a coarse
/// lattice near a steep feature — most commonly just outside a point
/// charge's exclusion radius, where the true field is an unclamped 1/r²
/// singularity — is poorly approximated by trilinear interpolation across
/// one cell; a full-length step through it produces jittery, tangled lines
/// rather than a smooth curve. [`adaptive_step`] retries such a step at half
/// the length instead of drawing it as-is.
const MIN_ACCEPTABLE_TURN_COSINE: f64 = 0.95;

/// Floor on how far [`adaptive_step`] may shrink a step, as a fraction of
/// the nominal (seed-spacing-derived) step length. Without a floor, tracing
/// that approaches a true singularity would keep halving without ever
/// bringing the turning angle under threshold, since direction changes
/// without bound arbitrarily close to the source.
const MIN_STEP_FRACTION: f64 = 1.0 / 64.0;

/// One logical step of a streamline: [`rk4_step`] at `step_length`, halved
/// and retried whenever the field turns more sharply than
/// [`MIN_ACCEPTABLE_TURN_COSINE`] allows, down to [`MIN_STEP_FRACTION`] of
/// the nominal length. Returns the accepted position and the step length
/// that produced it, so the caller can keep tracing at a smaller size while
/// the field stays sharp and recover back toward `step_length` once it eases.
fn adaptive_step(
    position: DVec3,
    step_length: f64,
    direction_sign: f64,
    sample: &impl Fn(DVec3) -> Option<DVec3>,
) -> Option<(DVec3, f64)> {
    let min_step = step_length * MIN_STEP_FRACTION;
    let mut trial_step = step_length;
    loop {
        let (next, entry_direction) = rk4_step(position, trial_step, direction_sign, sample)?;
        let accept = trial_step <= min_step || {
            let exit_direction = tangent(next, direction_sign, sample)?;
            entry_direction.dot(exit_direction) >= MIN_ACCEPTABLE_TURN_COSINE
        };
        if accept {
            return Some((next, trial_step));
        }
        trial_step *= 0.5;
    }
}

fn trace_direction(
    seed: DVec3,
    step_length: f64,
    direction_sign: f64,
    sample: &impl Fn(DVec3) -> Option<DVec3>,
    contains: &impl Fn(DVec3) -> bool,
    max_points: usize,
) -> Vec<DVec3> {
    let mut points = Vec::new();
    let mut current = seed;
    let mut step = step_length;
    let mut budget_spent = 0.0_f64;
    let mut iterations = 0u32;
    while budget_spent < f64::from(MAX_STEPS)
        && iterations < MAX_ITERATIONS
        && points.len() < max_points
    {
        iterations += 1;
        let Some((next, used_step)) = adaptive_step(current, step, direction_sign, sample) else {
            break;
        };
        if !contains(next) {
            break;
        }
        points.push(next);
        current = next;
        budget_spent += used_step / step_length;
        // Recover toward the nominal step size after a shrink, so a
        // once-sharp region doesn't permanently slow the rest of the trace.
        step = (used_step * 2.0).min(step_length);
    }
    points
}

/// Trace one streamline through `seed`, both with and against the field, and
/// stitch the result into a single polyline through the seed — the result
/// reads as one continuous thread through the field rather than a ray
/// sprouting from the seed grid.
///
/// `sample` returns `None` wherever the field is not usable at a point
/// (undefined validity, or too weak to mean anything — the caller decides
/// what "too weak" means); `contains` decides when the line has left the
/// region it is being drawn over. Returns an empty polyline if the seed
/// itself is not somewhere a line can start.
pub(super) fn trace_streamline(
    seed: DVec3,
    step_length: f64,
    sample: &impl Fn(DVec3) -> Option<DVec3>,
    contains: &impl Fn(DVec3) -> bool,
    max_points_per_direction: usize,
) -> Vec<DVec3> {
    let has_direction = sample(seed).is_some_and(|value| {
        let length = value.length();
        length > 0.0 && length.is_finite()
    });
    if step_length <= 0.0 || !contains(seed) || !has_direction {
        return Vec::new();
    }
    let mut backward = trace_direction(
        seed,
        step_length,
        -1.0,
        sample,
        contains,
        max_points_per_direction,
    );
    backward.reverse();
    let forward = trace_direction(
        seed,
        step_length,
        1.0,
        sample,
        contains,
        max_points_per_direction,
    );

    let mut polyline = backward;
    polyline.push(seed);
    polyline.extend(forward);
    polyline
}

/// Whether a fractional lattice coordinate still lies within `[0, count-1]`
/// on every axis it has — the tracer's exit test for grid/box/plane
/// lattices, whose `_interpolation` helpers clamp out-of-range coordinates
/// rather than reporting them, and whose fractional-index range is built to
/// span exactly the authored region's extent (`SlicePlane::lattice`,
/// `FieldBox::lattice`), so this is exactly the region's own boundary
/// expressed in index units instead of world units.
fn index_in_bounds(index: &[f64], counts: &[u32]) -> bool {
    index
        .iter()
        .zip(counts)
        .all(|(&value, &count)| (0.0..=f64::from(count.saturating_sub(1))).contains(&value))
}

/// Turn a traced world-space polyline into ribbon quads: two triangles per
/// segment, with `side = ±1` and the screen-space perpendicular resolved
/// later, in the vertex shader, from `neighbor`. `arclength` accumulates in
/// render-space units — the same units every other on-screen length in this
/// renderer works in — so the animation shader's scroll rate reads
/// consistently regardless of scene scale.
///
/// `side` is *not* simply "left/right of travel" at both ends of a segment:
/// the vertex shader computes its expansion direction from `neighbor - self`,
/// which points forward at a segment's start vertex but backward at its end
/// vertex. Left unaccounted for, that flips the expansion direction between
/// the two ends and crosses the ribbon into a bowtie instead of a strip. The
/// end vertices' `side` is therefore authored with the opposite sign of the
/// edge they represent, which cancels the flip and keeps both ends of an
/// edge expanding the same way.
fn build_flow_ribbon(
    polyline: &[DVec3],
    magnitudes: &[f64],
    scale: MagnitudeScale,
    display: FlowLineDisplay,
    scene_scale: SceneScale,
) -> Vec<FlowRibbonVertex> {
    if polyline.len() < 2 {
        return Vec::new();
    }
    let render: Vec<Vec3> = polyline
        .iter()
        .map(|&point| scene_scale.to_render_vec3(point))
        .collect();
    let colors: Vec<glam::Vec4> = magnitudes
        .iter()
        .map(|&magnitude| field_color(scale.normalized(magnitude)).extend(1.0))
        .collect();
    let speed = if display.animated { display.speed } else { 0.0 };

    let mut vertices = Vec::with_capacity((render.len() - 1) * 6);
    let mut arclength = 0.0_f32;
    for index in 0..render.len() - 1 {
        let from = render[index];
        let to = render[index + 1];
        let from_arclength = arclength;
        arclength += from.distance(to);
        let to_arclength = arclength;
        let from_color = colors[index];
        let to_color = colors[index + 1];

        let vertex =
            |position: Vec3, neighbor: Vec3, side: f32, arclength: f32, color: glam::Vec4| {
                FlowRibbonVertex {
                    position,
                    neighbor,
                    side,
                    arclength,
                    thickness_px: display.thickness_px,
                    speed,
                    color,
                }
            };
        let from_left = vertex(from, to, -1.0, from_arclength, from_color);
        let from_right = vertex(from, to, 1.0, from_arclength, from_color);
        // Sign flipped relative to `from_left`/`from_right` — see the doc
        // comment above.
        let to_right = vertex(to, from, -1.0, to_arclength, to_color);
        let to_left = vertex(to, from, 1.0, to_arclength, to_color);
        vertices.extend([
            from_left, from_right, to_right, //
            from_left, to_right, to_left,
        ]);
    }
    vertices
}

/// A permutation of `0..n` in which any *prefix* is already spread roughly
/// evenly across the whole range, rather than a contiguous run at the low
/// end — bit-reversal of the index within the smallest power of two
/// containing `n`, with the resulting out-of-range values dropped.
///
/// Every `trace_*_streamlines` function below can stop early once
/// [`MAX_RIBBON_VERTICES`] is spent, and a seed near a strong, localized
/// field feature — a point charge's near-singular falloff, for instance —
/// costs about the same number of vertices to resolve regardless of where in
/// the seed grid it happens to sit, since *every* backward trace in a
/// radial field passes close to the source eventually. Visiting seeds in
/// raster order (as a plain nested loop over `0..n` would), an early stop
/// after the first `k` seeds only ever samples one corner of the grid —
/// which reads as flow lines bunched into a single direction with the rest
/// of the region completely empty, not as a evenly-thinned-out version of
/// the full picture. Visiting axis indices in this order instead means an
/// early stop still leaves a spatially representative, if sparser, sample
/// of the whole region.
fn low_discrepancy_order(n: usize) -> Vec<usize> {
    if n <= 1 {
        return (0..n).collect();
    }
    let bits = usize::BITS - (n - 1).leading_zeros();
    let full = 1usize << bits;
    (0..full)
        .map(|index| index.reverse_bits() >> (usize::BITS - bits))
        .filter(|&index| index < n)
        .collect()
}

/// Streamlines seeded uniformly through the published domain lattice, the
/// same resampling `append_domain_vectors` already does for arrows.
pub(super) fn trace_domain_streamlines(
    lattice: GridLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    scale: MagnitudeScale,
    display: FlowLineDisplay,
    scene_scale: SceneScale,
) -> Vec<FlowRibbonVertex> {
    let floor = scale.noise_floor();
    let sample = |point: DVec3| -> Option<DVec3> {
        let index = lattice.fractional_coordinates(point);
        let interpolation = grid_interpolation(lattice, index.x, index.y, index.z, scene_scale)?;
        if !interpolation.is_usable(validity) {
            return None;
        }
        let value = interpolation.dvec3(values);
        (value.length() >= floor).then_some(value)
    };
    let contains = |point: DVec3| {
        let index = lattice.fractional_coordinates(point);
        let counts = lattice.counts();
        index_in_bounds(
            &[index.x, index.y, index.z],
            &[counts.x, counts.y, counts.z],
        )
    };

    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length =
        scene_scale.to_world(uniform_domain_spacing(lattice, &xs, &ys, &zs, scene_scale)) * 0.5;

    let x_order = low_discrepancy_order(xs.len());
    let y_order = low_discrepancy_order(ys.len());
    let z_order = low_discrepancy_order(zs.len());
    let max_points = fair_share_points_per_direction(xs.len() * ys.len() * zs.len());
    let mut vertices = Vec::new();
    'seeds: for &xi in &x_order {
        let x = xs[xi];
        for &yi in &y_order {
            let y = ys[yi];
            for &zi in &z_order {
                let z = zs[zi];
                let Some(seed_interp) = grid_interpolation(lattice, x, y, z, scene_scale) else {
                    continue;
                };
                let seed = scene_scale.to_world_vec3(seed_interp.position);
                let polyline = trace_streamline(seed, step_length, &sample, &contains, max_points);
                if polyline.len() < 2 {
                    continue;
                }
                let magnitudes: Vec<f64> = polyline
                    .iter()
                    .map(|&point| sample(point).map_or(0.0, |value| value.length()))
                    .collect();
                vertices.extend(build_flow_ribbon(
                    &polyline,
                    &magnitudes,
                    scale,
                    display,
                    scene_scale,
                ));
                if vertices.len() >= MAX_RIBBON_VERTICES {
                    break 'seeds;
                }
            }
        }
    }
    vertices
}

/// Streamlines seeded uniformly through an oriented field box, mirroring
/// `append_box_vectors`.
pub(super) fn trace_box_streamlines(
    lattice: BoxLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    scale: MagnitudeScale,
    display: FlowLineDisplay,
    scene_scale: SceneScale,
) -> Vec<FlowRibbonVertex> {
    let floor = scale.noise_floor();
    let sample = |point: DVec3| -> Option<DVec3> {
        let index = lattice.fractional_coordinates(point);
        let interpolation = box_interpolation(lattice, index.x, index.y, index.z, scene_scale)?;
        if !interpolation.is_usable(validity) {
            return None;
        }
        let value = interpolation.dvec3(values);
        (value.length() >= floor).then_some(value)
    };
    let contains = |point: DVec3| {
        let index = lattice.fractional_coordinates(point);
        let counts = lattice.counts();
        index_in_bounds(
            &[index.x, index.y, index.z],
            &[counts.x, counts.y, counts.z],
        )
    };

    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length =
        scene_scale.to_world(uniform_box_spacing(lattice, &xs, &ys, &zs, scene_scale)) * 0.5;

    let x_order = low_discrepancy_order(xs.len());
    let y_order = low_discrepancy_order(ys.len());
    let z_order = low_discrepancy_order(zs.len());
    let max_points = fair_share_points_per_direction(xs.len() * ys.len() * zs.len());
    let mut vertices = Vec::new();
    'seeds: for &xi in &x_order {
        let x = xs[xi];
        for &yi in &y_order {
            let y = ys[yi];
            for &zi in &z_order {
                let z = zs[zi];
                let Some(seed_interp) = box_interpolation(lattice, x, y, z, scene_scale) else {
                    continue;
                };
                let seed = scene_scale.to_world_vec3(seed_interp.position);
                let polyline = trace_streamline(seed, step_length, &sample, &contains, max_points);
                if polyline.len() < 2 {
                    continue;
                }
                let magnitudes: Vec<f64> = polyline
                    .iter()
                    .map(|&point| sample(point).map_or(0.0, |value| value.length()))
                    .collect();
                vertices.extend(build_flow_ribbon(
                    &polyline,
                    &magnitudes,
                    scale,
                    display,
                    scene_scale,
                ));
                if vertices.len() >= MAX_RIBBON_VERTICES {
                    break 'seeds;
                }
            }
        }
    }
    vertices
}

/// Streamlines seeded uniformly through a field sphere's bounding cube,
/// culled to the inscribed sphere the same way `append_sphere_vectors` culls
/// its arrows.
pub(super) fn trace_sphere_streamlines(
    lattice: SphereLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    scale: MagnitudeScale,
    display: FlowLineDisplay,
    scene_scale: SceneScale,
) -> Vec<FlowRibbonVertex> {
    let grid = lattice.grid();
    let floor = scale.noise_floor();
    let sample = |point: DVec3| -> Option<DVec3> {
        let index = grid.fractional_coordinates(point);
        let interpolation = grid_interpolation(grid, index.x, index.y, index.z, scene_scale)?;
        if !interpolation.is_usable(validity) {
            return None;
        }
        let value = interpolation.dvec3(values);
        (value.length() >= floor).then_some(value)
    };
    // The sphere, not its bounding cube: this is what keeps the traced lines
    // filling a ball rather than a box, exactly as `append_sphere_vectors`
    // culls its arrows.
    let contains = |point: DVec3| lattice.contains(point);

    let counts = grid.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let zs = uniform_axis(counts.z, display.density);
    let step_length =
        scene_scale.to_world(uniform_domain_spacing(grid, &xs, &ys, &zs, scene_scale)) * 0.5;

    let x_order = low_discrepancy_order(xs.len());
    let y_order = low_discrepancy_order(ys.len());
    let z_order = low_discrepancy_order(zs.len());
    let max_points = fair_share_points_per_direction(xs.len() * ys.len() * zs.len());
    let mut vertices = Vec::new();
    'seeds: for &xi in &x_order {
        let x = xs[xi];
        for &yi in &y_order {
            let y = ys[yi];
            for &zi in &z_order {
                let z = zs[zi];
                let Some(seed_interp) = grid_interpolation(grid, x, y, z, scene_scale) else {
                    continue;
                };
                let seed = scene_scale.to_world_vec3(seed_interp.position);
                if !lattice.contains(seed) {
                    continue;
                }
                let polyline = trace_streamline(seed, step_length, &sample, &contains, max_points);
                if polyline.len() < 2 {
                    continue;
                }
                let magnitudes: Vec<f64> = polyline
                    .iter()
                    .map(|&point| sample(point).map_or(0.0, |value| value.length()))
                    .collect();
                vertices.extend(build_flow_ribbon(
                    &polyline,
                    &magnitudes,
                    scale,
                    display,
                    scene_scale,
                ));
                if vertices.len() >= MAX_RIBBON_VERTICES {
                    break 'seeds;
                }
            }
        }
    }
    vertices
}

/// Streamlines seeded uniformly across a slice plane, mirroring
/// `append_plane_vectors`.
///
/// `values` is expected to already be the in-plane projection the caller
/// draws (see `displayed_plane_vector`): a 2D streamline cannot depict an
/// out-of-plane component, so a plane's flow lines always trace that
/// projection regardless of the plane's arrow `vector_mode`.
pub(super) fn trace_plane_streamlines(
    lattice: PlaneLattice,
    values: &[DVec3],
    validity: &[SampleValidity],
    scale: MagnitudeScale,
    display: FlowLineDisplay,
    scene_scale: SceneScale,
) -> Vec<FlowRibbonVertex> {
    let floor = scale.noise_floor();
    let sample = |point: DVec3| -> Option<DVec3> {
        let index = lattice.fractional_coordinates(point);
        let interpolation = plane_interpolation(lattice, index.x, index.y, scene_scale)?;
        if !interpolation.is_usable(validity) {
            return None;
        }
        let value = interpolation.dvec3(values);
        (value.length() >= floor).then_some(value)
    };
    let contains = |point: DVec3| {
        let index = lattice.fractional_coordinates(point);
        let counts = lattice.counts();
        index_in_bounds(&[index.x, index.y], &[counts.x, counts.y])
    };

    let counts = lattice.counts();
    let xs = uniform_axis(counts.x, display.density);
    let ys = uniform_axis(counts.y, display.density);
    let step_length =
        scene_scale.to_world(uniform_glyph_spacing(lattice, &xs, &ys, scene_scale)) * 0.5;

    let x_order = low_discrepancy_order(xs.len());
    let y_order = low_discrepancy_order(ys.len());
    let max_points = fair_share_points_per_direction(xs.len() * ys.len());
    let mut vertices = Vec::new();
    'seeds: for &xi in &x_order {
        let x = xs[xi];
        for &yi in &y_order {
            let y = ys[yi];
            let Some(seed_interp) = plane_interpolation(lattice, x, y, scene_scale) else {
                continue;
            };
            let seed = scene_scale.to_world_vec3(seed_interp.position);
            let polyline = trace_streamline(seed, step_length, &sample, &contains, max_points);
            if polyline.len() < 2 {
                continue;
            }
            let magnitudes: Vec<f64> = polyline
                .iter()
                .map(|&point| sample(point).map_or(0.0, |value| value.length()))
                .collect();
            vertices.extend(build_flow_ribbon(
                &polyline,
                &magnitudes,
                scale,
                display,
                scene_scale,
            ));
            if vertices.len() >= MAX_RIBBON_VERTICES {
                break 'seeds;
            }
        }
    }
    vertices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_discrepancy_order_is_a_permutation() {
        for n in [0, 1, 2, 3, 5, 12, 16, 100] {
            let mut order = low_discrepancy_order(n);
            assert_eq!(order.len(), n, "n={n}");
            order.sort_unstable();
            assert_eq!(order, (0..n).collect::<Vec<_>>(), "n={n}");
        }
    }

    /// Regression: seeding a box/domain/sphere/plane in raster order meant
    /// an early stop at [`MAX_RIBBON_VERTICES`] only ever sampled one corner
    /// of the seed grid — every backward trace in a radial field passes
    /// close to the source and costs about the same number of vertices to
    /// resolve regardless of where its seed sits, so the vertex budget ran
    /// out after the first few `x` values and the rest of the grid (most
    /// directions around the source) was silently never traced at all. This
    /// showed up as flow lines bunching into one direction with the rest of
    /// a field box or domain left completely empty, and it got *more*
    /// visible after adaptive stepping started spending more vertices per
    /// seed near sharp features. A representative prefix of the order must
    /// span close to the full range, not cluster at the low end.
    #[test]
    fn low_discrepancy_order_prefix_spans_the_full_range() {
        let n = 100;
        let order = low_discrepancy_order(n);
        let prefix: Vec<usize> = order.into_iter().take(10).collect();

        let min = *prefix.iter().min().unwrap();
        let max = *prefix.iter().max().unwrap();
        assert!(
            max - min > n / 2,
            "the first 10 of {n} indices should already spread across most \
             of the range, not cluster together: {prefix:?}"
        );
    }

    /// A uniform +X field: the streamline through any seed is a straight
    /// line, with no discontinuity between the backward and forward halves.
    #[test]
    fn a_uniform_field_traces_a_straight_line_through_its_seed() {
        let seed = DVec3::new(0.3, 0.0, 0.0);
        let polyline = trace_streamline(
            seed,
            0.1,
            &|_point| Some(DVec3::X),
            &|point| point.x.abs() <= 1.0,
            usize::MAX,
        );

        assert!(polyline.len() > 10);
        assert!(polyline.iter().all(|point| point.y.abs() < 1.0e-9));
        assert!(polyline.iter().all(|point| point.z.abs() < 1.0e-9));
        // Monotonically increasing in x: the backward half approaches -1 and
        // the forward half approaches +1, meeting at the seed in between.
        for pair in polyline.windows(2) {
            assert!(pair[1].x > pair[0].x);
        }
        assert!(polyline.first().unwrap().x < seed.x);
        assert!(polyline.last().unwrap().x > seed.x);
    }

    /// A rigid-rotation field around Z (`v = -y, x, 0`) produces closed
    /// circular field lines. Without the step cap this would trace forever;
    /// with it, the line still traces a bounded arc in each direction.
    #[test]
    fn a_solid_body_rotation_field_traces_a_closed_loop_and_terminates() {
        let radius = 2.0;
        let seed = DVec3::new(radius, 0.0, 0.0);
        let sample = |point: DVec3| Some(DVec3::new(-point.y, point.x, 0.0));
        let polyline = trace_streamline(seed, 0.05, &sample, &|_point| true, usize::MAX);

        assert_eq!(
            polyline.len(),
            (MAX_STEPS as usize) * 2 + 1,
            "an unbounded closed loop always exhausts the step cap in both directions"
        );
        assert!(
            polyline
                .iter()
                .all(|point| (point.length() - radius).abs() < 1.0e-2),
            "every point stays on the circle the seed started on"
        );
    }

    /// A radial field traced outward from a seed must stop exactly where the
    /// caller's `contains` predicate says it should, not run past it.
    #[test]
    fn tracing_stops_at_the_containment_boundary() {
        let seed = DVec3::new(0.1, 0.0, 0.0);
        let sample = |point: DVec3| {
            let length = point.length();
            (length > 0.0).then_some(point / length)
        };
        let polyline = trace_streamline(
            seed,
            0.1,
            &sample,
            &|point| point.length() <= 1.0,
            usize::MAX,
        );

        assert!(polyline.iter().all(|point| point.length() <= 1.0 + 1.0e-9));
        assert!(
            polyline.last().unwrap().length() > 0.9,
            "the forward trace should reach close to the boundary before stopping"
        );
    }

    /// A field that vanishes at the seed cannot start a line: there is no
    /// direction to follow.
    #[test]
    fn a_zero_field_at_the_seed_produces_no_polyline() {
        let polyline = trace_streamline(
            DVec3::ZERO,
            0.1,
            &|_point| Some(DVec3::ZERO),
            &|_| true,
            usize::MAX,
        );
        assert!(polyline.is_empty());
    }

    /// A sample that goes undefined partway along must stop the line there
    /// rather than propagate a placeholder value.
    #[test]
    fn tracing_stops_where_the_sample_becomes_undefined() {
        let seed = DVec3::ZERO;
        let sample = |point: DVec3| (point.x < 0.5).then_some(DVec3::X);
        let polyline = trace_streamline(seed, 0.1, &sample, &|_| true, usize::MAX);

        assert!(polyline.iter().all(|point| point.x < 0.5 + 1.0e-9));
        assert!(polyline.last().unwrap().x > 0.35);
    }

    /// A field with no curvature never needs the step shrunk — the common
    /// case, and the one every other test in this file implicitly relies on
    /// staying exactly at the requested step length.
    #[test]
    fn adaptive_step_keeps_the_full_step_length_when_the_field_does_not_curve() {
        let sample = |_point: DVec3| Some(DVec3::X);
        let (_, used_step) = adaptive_step(DVec3::ZERO, 0.5, 1.0, &sample).unwrap();
        assert_eq!(used_step, 0.5);
    }

    /// Regression for the tangled, jittery lines a fixed-length RK4 step
    /// produced near a point charge: the true field there is an unclamped
    /// 1/r^2 singularity, poorly approximated by the lattice's trilinear
    /// interpolation over one step-sized span, so a full step through it
    /// turned far more sharply than the curve it was meant to follow. A
    /// rigid-rotation field at a small radius stands in for that same
    /// "turns much faster than the nominal step expects" shape without
    /// needing a real singularity: `adaptive_step` must shrink the step and,
    /// in doing so, turn less sharply than an unshrunk `rk4_step` would have.
    #[test]
    fn adaptive_step_reduces_turning_error_that_a_fixed_step_would_have_taken() {
        let seed = DVec3::new(0.05, 0.0, 0.0);
        let sample = |point: DVec3| Some(DVec3::new(-point.y, point.x, 0.0));
        let entry = tangent(seed, 1.0, &sample).unwrap();

        let (naive_next, _) = rk4_step(seed, 0.5, 1.0, &sample).unwrap();
        let naive_exit = tangent(naive_next, 1.0, &sample).unwrap();

        let (adaptive_next, used_step) = adaptive_step(seed, 0.5, 1.0, &sample).unwrap();
        let adaptive_exit = tangent(adaptive_next, 1.0, &sample).unwrap();

        assert!(
            used_step < 0.5,
            "step should have shrunk near the tight curvature"
        );
        assert!(
            entry.dot(adaptive_exit) > entry.dot(naive_exit),
            "adaptive stepping should turn far less sharply than an unshrunk full step would"
        );
    }

    /// Regression: a sharply turning patch near the seed used to be able to
    /// exhaust a trace's *entire* reach before it ever got past that patch,
    /// and the effect got worse — not better — as seed density (and so the
    /// nominal step length) increased, because the old budget was a raw
    /// count of accepted [`adaptive_step`] calls: a patch that needed many
    /// shrunk micro-steps to resolve spent the same budget as if each of
    /// those micro-steps had been a full nominal step, e.g. a plane's
    /// streamlines bunching up right around a point charge instead of
    /// reaching across the rest of the plane at high display density. The
    /// budget must instead be spent in nominal-step-equivalents, so a patch
    /// resolved with many small steps costs proportionally little, leaving
    /// the rest of the domain still reachable.
    #[test]
    fn a_sharply_turning_patch_near_the_seed_does_not_exhaust_the_reach_budget() {
        let nominal_step = 0.01;
        let sample = |point: DVec3| {
            // Oscillates far more sharply than a full nominal step could
            // follow near x = 0, smoothly decaying to a plain +x drift by
            // about x = 0.1 — continuous throughout, like a real
            // interpolated field, unlike a hard cutoff between "sharp" and
            // "straight" that could never be smoothly crossed at all.
            let decay = (-point.x / 0.02).exp();
            Some(DVec3::new(1.0, (point.x * 400.0).sin() * 3.0 * decay, 0.0))
        };
        let polyline = trace_streamline(DVec3::ZERO, nominal_step, &sample, &|_| true, usize::MAX);

        assert!(
            polyline.len() > MAX_STEPS as usize,
            "resolving the sharp patch should need many more than {MAX_STEPS} small \
             accepted steps: {}",
            polyline.len()
        );
        assert!(
            polyline.last().unwrap().x > 0.2,
            "the trace should reach well past the patch and into the straight \
             region beyond it, not stall trying to resolve the patch alone: {}",
            polyline.last().unwrap().x
        );
    }

    /// Regression: a dense box (or sphere/domain) whose field doesn't leave
    /// its bounds quickly used to multiply seed count by up-to-`2*MAX_STEPS`
    /// vertices per seed, reaching tens of millions of vertices before the
    /// renderer ever saw them — enough to crash on `Device::create_buffer`
    /// (a real 268 MiB `wgpu` buffer-size validation panic) rather than just
    /// running slowly. Tracing must stop itself once the shared budget is
    /// spent, not rely solely on the renderer discarding the excess.
    #[test]
    fn trace_box_streamlines_stops_at_the_global_vertex_budget() {
        use glam::UVec3;

        // A uniform +X field through a large box: every one of the 20^3
        // seeds travels a long way before its containment check trips, so an
        // unbounded tracer would produce roughly
        // 8_000 seeds * ~90 segments * 6 vertices ≈ 4,300,000 vertices —
        // more than fourteen times the budget.
        let half = 50.0;
        let counts = UVec3::splat(20);
        let step = 2.0 * half / 19.0;
        let lattice = BoxLattice::new(
            DVec3::splat(-half),
            DVec3::new(step, 0.0, 0.0),
            DVec3::new(0.0, step, 0.0),
            DVec3::new(0.0, 0.0, step),
            counts,
        );
        let values = vec![DVec3::X; lattice.len()];
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let scale = MagnitudeScale::over(&values, &validity);
        let display = FlowLineDisplay::new(true, 20);

        let vertices = trace_box_streamlines(
            lattice,
            &values,
            &validity,
            scale,
            display,
            SceneScale::metre(),
        );

        assert!(
            !vertices.is_empty(),
            "test setup: seeds must actually trace"
        );
        // One seed's worst-case contribution (2 * MAX_STEPS segments, 6
        // vertices each — looser than what `fair_share_points_per_direction`
        // actually allows for this seed count, but still a valid upper
        // bound) can land after the budget check passes, so the final count
        // overshoots the budget by at most that much.
        let worst_case_overshoot = (2 * MAX_STEPS as usize) * 6;
        assert!(
            vertices.len() <= MAX_RIBBON_VERTICES + worst_case_overshoot,
            "tracing must stop near the budget instead of exhausting every seed: {}",
            vertices.len()
        );
    }

    /// Regression: even visiting seeds in `low_discrepancy_order`, a handful
    /// of seeds whose trace lingers near a tight-curvature feature could
    /// each spend their entire reach on fine adaptive subdivision and alone
    /// produce tens of thousands of points, exhausting the shared vertex
    /// budget before most of the seed grid was ever traced — which read as
    /// flow lines flattened toward whichever few expensive seeds happened to
    /// be visited first, instead of filling the requested box.
    #[test]
    fn expensive_seeds_near_a_curvature_feature_do_not_crowd_out_the_rest_of_the_box() {
        use glam::UVec3;

        let half = 1.0;
        let counts = UVec3::splat(4);
        let step = 2.0 * half / 3.0;
        let lattice = BoxLattice::new(
            DVec3::splat(-half),
            DVec3::new(step, 0.0, 0.0),
            DVec3::new(0.0, step, 0.0),
            DVec3::new(0.0, 0.0, step),
            counts,
        );
        // A rigid-rotation field around Z: curvature is 1/radius, so seeds
        // near the rotation axis need heavy adaptive subdivision (expensive)
        // while seeds near the box's corners barely need to shrink at all
        // (cheap). Crucially v_z = 0 everywhere, so a traced point's z never
        // moves from its seed's z — the spread of z across the resulting
        // vertices is a clean proxy for "how much of the seed grid actually
        // got traced," independent of how the tracer curves in x/y. Since
        // the field is linear in x and y, trilinear interpolation of this
        // coarse lattice reconstructs it exactly.
        let values: Vec<DVec3> = (0..lattice.len())
            .map(|index| {
                let position = lattice.position(index).unwrap();
                DVec3::new(-position.y, position.x, 0.0)
            })
            .collect();
        let validity = vec![SampleValidity::Exact; lattice.len()];
        let scale = MagnitudeScale::over(&values, &validity);
        let display = FlowLineDisplay::new(true, 10);

        let vertices = trace_box_streamlines(
            lattice,
            &values,
            &validity,
            scale,
            display,
            SceneScale::metre(),
        );

        assert!(!vertices.is_empty(), "test setup: seeds must actually trace");
        let z_min = vertices
            .iter()
            .map(|vertex| vertex.position.z)
            .fold(f32::MAX, f32::min);
        let z_max = vertices
            .iter()
            .map(|vertex| vertex.position.z)
            .fold(f32::MIN, f32::max);
        assert!(
            f64::from(z_max - z_min) > half,
            "seeds from across the box's full z-extent should be represented, \
             not crowded out by a few expensive ones near the rotation axis: \
             z spans [{z_min}, {z_max}] out of [-{half}, {half}]"
        );
    }

    #[test]
    fn index_in_bounds_matches_the_published_extent() {
        assert!(index_in_bounds(&[0.0, 4.0], &[5, 5]));
        assert!(index_in_bounds(&[4.0, 4.0], &[5, 5]));
        assert!(!index_in_bounds(&[4.01, 0.0], &[5, 5]));
        assert!(!index_in_bounds(&[-0.01, 0.0], &[5, 5]));
    }

    #[test]
    fn build_flow_ribbon_alternates_sides_and_accumulates_arclength() {
        let polyline = vec![DVec3::ZERO, DVec3::X, DVec3::new(2.0, 0.0, 0.0)];
        let magnitudes = vec![1.0, 1.0, 1.0];
        let vertices = build_flow_ribbon(
            &polyline,
            &magnitudes,
            MagnitudeScale { maximum: 1.0 },
            FlowLineDisplay::new(true, 4),
            SceneScale::metre(),
        );

        assert_eq!(vertices.len(), 12, "two segments, six vertices each");
        let sides: Vec<f32> = vertices.iter().map(|vertex| vertex.side).collect();
        assert!(sides.contains(&-1.0) && sides.contains(&1.0));
        assert!(vertices.iter().all(|vertex| vertex.arclength >= 0.0));
        // The second segment's vertices start where the first segment ends.
        let first_segment_end = vertices[..6]
            .iter()
            .map(|vertex| vertex.arclength)
            .fold(0.0_f32, f32::max);
        let second_segment_start = vertices[6..]
            .iter()
            .map(|vertex| vertex.arclength)
            .fold(f32::MAX, f32::min);
        assert!((first_segment_end - second_segment_start).abs() < 1.0e-5);
    }

    #[test]
    fn build_flow_ribbon_is_empty_for_a_degenerate_polyline() {
        assert!(
            build_flow_ribbon(
                &[DVec3::ZERO],
                &[1.0],
                MagnitudeScale { maximum: 1.0 },
                FlowLineDisplay::new(true, 4),
                SceneScale::metre(),
            )
            .is_empty()
        );
    }
}
