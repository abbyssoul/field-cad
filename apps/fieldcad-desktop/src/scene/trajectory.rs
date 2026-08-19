//! Object trajectory trails: turning one object's recorded position/velocity
//! history into a smooth, optionally animated ribbon — the per-object
//! counterpart to [`super::flow_lines`]'s field-seeded streamlines. Both
//! trace a polyline into the same [`super::FlowRibbonVertex`] ribbon via
//! [`build_flow_ribbon`], but where a streamline integrates through a live
//! field from a seed, a trajectory replays what already happened: the
//! polyline comes from recorded [`BodySample`]s, not from sampling anything
//! this frame.

use fieldcad_core::SceneScale;
use fieldcad_simulation::BodySample;
use glam::{DVec3, Vec4};

use super::flow_lines::build_flow_ribbon;
use super::{FlowRibbonVertex, TrajectoryDisplay};

/// Smoothing substeps evaluated per recorded sample interval. Unlike a
/// field streamline's adaptive step (which is approximating an unknown
/// curve from a lattice interpolation), each interval here has an *exact*
/// Hermite fit — known position and velocity at both ends — so a small
/// fixed count is enough to read as smooth without letting the ribbon's
/// vertex count grow unboundedly as recorded history gets deeper.
const SUBSTEPS_PER_INTERVAL: usize = 8;

/// Alpha floor for the oldest point still drawn, so the tail thins toward
/// transparent instead of ending in a hard, high-contrast cutoff against
/// whatever is behind it in the scene.
const TAIL_ALPHA_FLOOR: f32 = 0.05;

/// The ribbon vertex count [`append_trajectory_geometry`] will produce once
/// `history` has filled to `capacity_samples` — its own `hermite_polyline`
/// and `build_flow_ribbon` math, run in reverse, so a caller can reserve a
/// trajectory ribbon buffer once, up front, instead of letting it regrow
/// tick by tick while history fills toward a capacity that
/// [`super::TrajectoryDisplay::required_body_history_capacity`] already
/// pins down exactly (it changes only when a user edits `trail_seconds` or
/// the session's own `dt`, not every tick).
pub(crate) fn max_ribbon_vertices(capacity_samples: usize) -> usize {
    let polyline_len = capacity_samples.saturating_sub(1) * SUBSTEPS_PER_INTERVAL + 1;
    polyline_len.saturating_sub(1) * 6
}

/// Append one object's trajectory ribbon to `output`, built from `history`
/// (oldest sample first, exactly as [`fieldcad_simulation::FieldDataSource::body_history`]
/// returns it).
///
/// `display.trail_seconds` trims how far back to draw, but never below the
/// two most recent recorded samples: a session whose `TimeStep` happens to
/// be coarser than the requested `trail_seconds` (an astronomical-scale
/// scene ticking in hours or days, say, against a trail asked to cover only
/// a handful of seconds) would otherwise have every sample but the newest
/// fall outside the cutoff and draw nothing at all, even though the body
/// visibly moves every tick. Showing the one most recent recorded leg of
/// motion — a few seconds' difference from what was asked for — reads as a
/// trail; showing nothing does not.
///
/// Appends nothing if the display is off, or if `history` itself has fewer
/// than two samples — there is no direction to draw a line in yet.
pub fn append_trajectory_geometry(
    output: &mut Vec<FlowRibbonVertex>,
    history: &[BodySample],
    display: TrajectoryDisplay,
    base_color: Vec4,
    scene_scale: SceneScale,
) {
    if !display.visible || history.len() < 2 {
        return;
    }
    let newest_time = history.last().unwrap().time_seconds;
    let cutoff = newest_time - f64::from(display.trail_seconds.max(0.0));
    let trim_start = history
        .iter()
        .rposition(|sample| sample.time_seconds < cutoff)
        .map_or(0, |index| index + 1)
        .min(history.len() - 2);
    let trimmed = &history[trim_start..];

    let polyline = hermite_polyline(trimmed);
    let colors = recency_fade(polyline.len(), base_color);
    build_flow_ribbon(&polyline, &colors, display.into(), scene_scale, output);
}

/// Cubic Hermite spline through consecutive samples, using each sample's own
/// recorded velocity as the tangent there. This is the "use historical
/// velocity for smoothness" of it: the curve matches the body's actual
/// recorded motion at each sample instead of faceting at every turn the way
/// a straight-line polyline through raw positions would.
fn hermite_polyline(samples: &[BodySample]) -> Vec<DVec3> {
    let mut points = Vec::with_capacity((samples.len() - 1) * SUBSTEPS_PER_INTERVAL + 1);
    points.push(samples[0].position);
    for pair in samples.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let dt = end.time_seconds - start.time_seconds;
        if dt <= 0.0 {
            // A non-advancing or out-of-order pair (most plausibly a
            // history gap left by an integration-scheme switch, which
            // clears and restarts recording) has no meaningful tangent
            // scale to fit against — fall back to a straight sub-segment
            // rather than dividing by zero or extrapolating backwards.
            points.push(end.position);
            continue;
        }
        for step in 1..=SUBSTEPS_PER_INTERVAL {
            let t = step as f64 / SUBSTEPS_PER_INTERVAL as f64;
            points.push(hermite_point(start, end, dt, t));
        }
    }
    points
}

/// One Hermite-spline evaluation at `t` in `0..=1` across `[start, end]`,
/// `dt` seconds apart. Standard cubic Hermite basis: value and tangent
/// (scaled by the interval length) fixed at both ends.
fn hermite_point(start: BodySample, end: BodySample, dt: f64, t: f64) -> DVec3 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    start.position * h00
        + start.velocity * (dt * h10)
        + end.position * h01
        + end.velocity * (dt * h11)
}

/// Per-vertex colour along the polyline: alpha rises from
/// [`TAIL_ALPHA_FLOOR`] at the oldest point to `base_color`'s own alpha at
/// the newest (the object's current position), so the trail reads as
/// fading into the past. No rendering changes needed for this: the
/// flow-line pipeline is already alpha-blended.
fn recency_fade(point_count: usize, base_color: Vec4) -> Vec<Vec4> {
    if point_count <= 1 {
        return vec![base_color; point_count];
    }
    (0..point_count)
        .map(|index| {
            let age = index as f32 / (point_count - 1) as f32;
            let alpha = (TAIL_ALPHA_FLOOR + (1.0 - TAIL_ALPHA_FLOOR) * age) * base_color.w;
            base_color.truncate().extend(alpha)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use fieldcad_core::WorldRevision;

    use super::*;

    fn sample(time_seconds: f64, position: DVec3, velocity: DVec3) -> BodySample {
        BodySample {
            tick: (time_seconds * 10.0) as u64,
            time_seconds,
            world_revision: WorldRevision::INITIAL,
            position,
            velocity,
            force: DVec3::ZERO,
        }
    }

    #[test]
    fn a_history_shorter_than_two_samples_draws_nothing() {
        let mut output = Vec::new();
        append_trajectory_geometry(
            &mut output,
            &[sample(0.0, DVec3::ZERO, DVec3::X)],
            TrajectoryDisplay::new(true, 5.0),
            Vec4::ONE,
            SceneScale::metre(),
        );
        assert!(output.is_empty());
    }

    #[test]
    fn a_hidden_display_draws_nothing_even_with_history() {
        let mut output = Vec::new();
        let history = [
            sample(0.0, DVec3::ZERO, DVec3::X),
            sample(1.0, DVec3::X, DVec3::X),
        ];
        append_trajectory_geometry(
            &mut output,
            &history,
            TrajectoryDisplay::new(false, 5.0),
            Vec4::ONE,
            SceneScale::metre(),
        );
        assert!(output.is_empty());
    }

    /// Constant velocity, evenly spaced samples: the Hermite fit degenerates
    /// to the same straight line a raw polyline through the positions would
    /// draw, so this doubles as a check that the spline doesn't introduce
    /// spurious curvature where none exists.
    #[test]
    fn constant_velocity_history_produces_a_straight_ribbon() {
        let history: Vec<BodySample> = (0..5)
            .map(|tick| {
                let t = f64::from(tick) * 0.1;
                sample(t, DVec3::new(t, 0.0, 0.0), DVec3::X)
            })
            .collect();
        let polyline = hermite_polyline(&history);

        assert!(polyline.len() > history.len());
        for point in &polyline {
            assert!(point.y.abs() < 1.0e-9);
            assert!(point.z.abs() < 1.0e-9);
        }
        // Monotonically increasing in x, matching the recorded motion.
        for pair in polyline.windows(2) {
            assert!(pair[1].x > pair[0].x);
        }
    }

    #[test]
    fn trail_seconds_trims_samples_older_than_the_cutoff() {
        let history = [
            sample(0.0, DVec3::ZERO, DVec3::X),
            sample(1.0, DVec3::X, DVec3::X),
            sample(2.0, DVec3::new(2.0, 0.0, 0.0), DVec3::X),
            sample(10.0, DVec3::new(10.0, 0.0, 0.0), DVec3::X),
        ];
        // trail_seconds=1: cutoff is t=9, which only the newest sample
        // (t=10) clears. Trimming still keeps its immediate predecessor
        // (t=2) too — see `append_trajectory_geometry`'s doc comment — so a
        // trail_seconds shorter than the runtime's actual sample interval
        // draws the one most recent leg of motion instead of nothing.
        let mut output = Vec::new();
        append_trajectory_geometry(
            &mut output,
            &history,
            TrajectoryDisplay::new(true, 1.0),
            Vec4::ONE,
            SceneScale::metre(),
        );
        assert!(
            !output.is_empty(),
            "a trail_seconds narrower than one recorded interval should still draw the \
             most recent leg of motion, not nothing"
        );

        // A wide enough trail_seconds keeps every sample and draws something.
        let mut output = Vec::new();
        append_trajectory_geometry(
            &mut output,
            &history,
            TrajectoryDisplay::new(true, 20.0),
            Vec4::ONE,
            SceneScale::metre(),
        );
        assert!(!output.is_empty());
    }

    /// Regression for the reported bug: an astronomical-scale scene ticking
    /// in units far coarser than the requested `trail_seconds` (hours or
    /// days per tick, against a trail asked to cover only 30 seconds) used
    /// to leave every recorded sample but the newest outside the cutoff, so
    /// `trimmed.len() < 2` and nothing ever drew — even though the body
    /// visibly moved every tick.
    #[test]
    fn a_time_step_coarser_than_trail_seconds_still_draws_the_latest_leg() {
        let history = [
            sample(0.0, DVec3::ZERO, DVec3::X),
            sample(86_400.0, DVec3::new(1.0e6, 0.0, 0.0), DVec3::X),
        ];
        let mut output = Vec::new();
        append_trajectory_geometry(
            &mut output,
            &history,
            TrajectoryDisplay::new(true, 30.0),
            Vec4::ONE,
            SceneScale::metre(),
        );
        assert!(
            !output.is_empty(),
            "a one-day tick interval against a 30-second requested trail_seconds should \
             still draw the only leg of motion recorded, not nothing"
        );
    }

    #[test]
    fn recency_fade_rises_monotonically_from_the_tail_floor_to_full_alpha() {
        let colors = recency_fade(4, Vec4::ONE);
        assert_eq!(colors.first().unwrap().w, TAIL_ALPHA_FLOOR);
        assert_eq!(colors.last().unwrap().w, 1.0);
        for pair in colors.windows(2) {
            assert!(pair[1].w > pair[0].w);
        }
    }
}
