//! The bounded probe-history plot.
//!
//! Painted directly rather than through a plotting crate: the traces are few and
//! the axes must state simulation time and units exactly, so the small amount of
//! drawing here is cheaper than adapting a general-purpose plot widget.

use fieldcad_core::{
    ChannelId, DistanceProbeId, FieldValue, MassAggregateProbeId, ProbeId, SampleValidity,
};
use fieldcad_simulation::{
    DistanceHistory, DistanceReading, MassAggregateHistory, MassAggregateReading, ProbeHistory,
};

use super::compute::ComputeView;
use super::{DistanceProbeSeries, FrameContext, MassAggregateProbeSeries, UiModel};

pub(super) fn probe_history_plots(
    ui: &mut egui::Ui,
    probe: &fieldcad_core::Probe,
    compute: &ComputeView,
    history: &ProbeHistory,
) {
    // The caller owns the fold. This used to open its own collapsing header,
    // which became a second one nested inside the inspector's History section.
    ui.small(format!(
        "Bounded to {} samples per channel",
        history.capacity()
    ));
    for channel in &probe.channels {
        probe_channel_plot(ui, probe.id, channel, compute, history);
    }
}

/// Draw every pinned probe recorder independently of current scene selection.
pub(super) fn floating_probe_plots(
    context: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
) {
    let mut closed = Vec::new();
    for (probe_id, plot) in &mut model.probe_plots {
        let Some(probe) = frame.world.probe(*probe_id) else {
            closed.push(*probe_id);
            continue;
        };
        let mut open = true;
        egui::Window::new(format!("Probe plot · {}", probe.name))
            .id(egui::Id::new(("probe_plot", probe.id)))
            .open(&mut open)
            .default_size(egui::vec2(460.0, 320.0))
            .resizable(true)
            .collapsible(true)
            .show(context, |ui| {
                let position = frame
                    .world
                    .resolve_probe_position(probe)
                    .unwrap_or_default();
                ui.small(format!(
                    "Recorder at ({:.4}, {:.4}, {:.4}) m · tick {}",
                    position.x, position.y, position.z, frame.compute.tick
                ));
                ui.horizontal_wrapped(|ui| {
                    ui.label("Channels");
                    for channel in &probe.channels {
                        let mut selected = plot.channels.contains(channel);
                        let label = channel_label(channel, frame.compute);
                        if ui.checkbox(&mut selected, label).changed() {
                            if selected {
                                plot.channels.insert(channel.clone());
                            } else {
                                plot.channels.remove(channel);
                            }
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut drew_channel = false;
                    for channel in &probe.channels {
                        if plot.channels.contains(channel) {
                            probe_channel_plot(
                                ui,
                                probe.id,
                                channel,
                                frame.compute,
                                frame.probe_history,
                            );
                            drew_channel = true;
                        }
                    }
                    if !drew_channel {
                        ui.weak("Select at least one recorded channel.");
                    }
                });
            });
        if !open {
            closed.push(*probe_id);
        }
    }
    for probe in closed {
        model.probe_plots.remove(&probe);
    }
}

fn probe_channel_plot(
    ui: &mut egui::Ui,
    probe: ProbeId,
    channel: &ChannelId,
    compute: &ComputeView,
    history: &ProbeHistory,
) {
    let readings: Vec<_> = history.readings(probe, channel).copied().collect();
    let mut title = channel_label(channel, compute);
    if let Some(reading) = readings.last() {
        title.push_str(&format!(" [{}]", reading.value.dimension()));
    }
    ui.label(title);
    if readings.is_empty() {
        ui.weak("No samples yet");
    } else {
        paint_probe_plot(ui, &readings);
    }
}

/// Draw every pinned distance-probe recorder independently of current scene
/// selection — see [`floating_probe_plots`].
pub(super) fn floating_distance_probe_plots(
    context: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
) {
    // Collected up front rather than iterated as `&model.distance_probe_plots`:
    // each window body below also needs a mutable borrow of
    // `model.distance_probe_series`, and an owned id list is what lets that
    // borrow and this one coexist.
    let probe_ids: Vec<DistanceProbeId> = model.distance_probe_plots.iter().copied().collect();
    let mut closed = Vec::new();
    for probe_id in probe_ids {
        let Some(probe) = frame.world.distance_probe(probe_id) else {
            closed.push(probe_id);
            continue;
        };
        let mut open = true;
        let series = model.distance_probe_series.entry(probe_id).or_default();
        egui::Window::new(format!("Distance plot · {}", probe.name))
            .id(egui::Id::new(("distance_probe_plot", probe.id)))
            .open(&mut open)
            .default_size(egui::vec2(460.0, 220.0))
            .resizable(true)
            .collapsible(true)
            .show(context, |ui| {
                distance_history_plot(ui, probe.id, frame.distance_history, series);
            });
        if !open {
            closed.push(probe_id);
        }
    }
    for probe in closed {
        model.distance_probe_plots.remove(&probe);
    }
}

/// A distance probe's history as one or two scalar traces — see
/// [`history_plot`], the same single-trace painter the diagnostics panel
/// uses. A distance never carries a unit-of-measure ambiguity the way a
/// field probe's [`FieldValue`] can, so this needs none of
/// `probe_channel_plot`'s trace/validity machinery.
///
/// Draws its own distance/rate-of-change checkboxes and mutates `series`
/// directly, so the inline inspector plot and the floating window share one
/// implementation rather than two copies that could disagree.
pub(super) fn distance_history_plot(
    ui: &mut egui::Ui,
    probe: DistanceProbeId,
    history: &DistanceHistory,
    series: &mut DistanceProbeSeries,
) {
    ui.small(format!("Bounded to {} samples", history.capacity()));
    ui.horizontal(|ui| {
        ui.checkbox(&mut series.distance, "Distance");
        ui.checkbox(&mut series.rate_of_change, "Rate of change");
    });
    if !series.distance && !series.rate_of_change {
        ui.weak("Select at least one series.");
        return;
    }
    let readings: Vec<DistanceReading> = history.readings(probe).copied().collect();
    if readings.is_empty() {
        ui.weak("No samples yet");
        return;
    }
    if series.distance {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.distance as f32)
            .collect();
        ui.label(format!("Distance · {:.4} m", values.last().unwrap()));
        history_plot(ui, &values, egui::Color32::from_rgb(245, 205, 75), "m");
    }
    if series.rate_of_change {
        let rates = distance_rate_of_change(&readings);
        if rates.is_empty() {
            ui.weak("Not enough samples yet for a rate of change");
        } else {
            ui.label(format!("Rate of change · {:.4} m/s", rates.last().unwrap()));
            history_plot(ui, &rates, egui::Color32::from_rgb(100, 155, 245), "m/s");
        }
    }
}

/// A discrete derivative of distance over time: one value per adjacent pair
/// of readings, using each pair's actual `time_seconds` gap rather than
/// assuming a fixed tick rate. Samples with a non-positive gap (a snapshot
/// sequence tie, or a rewound clock) are skipped rather than producing an
/// infinite or reversed rate.
fn distance_rate_of_change(readings: &[DistanceReading]) -> Vec<f32> {
    readings
        .windows(2)
        .filter_map(|pair| {
            let dt = pair[1].time_seconds - pair[0].time_seconds;
            (dt > 0.0).then(|| ((pair[1].distance - pair[0].distance) / dt) as f32)
        })
        .collect()
}

/// Draw every pinned mass-aggregate-probe recorder independently of current
/// scene selection — see [`floating_probe_plots`].
pub(super) fn floating_mass_aggregate_probe_plots(
    context: &egui::Context,
    model: &mut UiModel,
    frame: &FrameContext<'_>,
) {
    let probe_ids: Vec<MassAggregateProbeId> =
        model.mass_aggregate_probe_plots.iter().copied().collect();
    let mut closed = Vec::new();
    for probe_id in probe_ids {
        let Some(probe) = frame.world.mass_aggregate_probe(probe_id) else {
            closed.push(probe_id);
            continue;
        };
        let mut open = true;
        let series = model
            .mass_aggregate_probe_series
            .entry(probe_id)
            .or_default();
        egui::Window::new(format!("Center of mass plot · {}", probe.name))
            .id(egui::Id::new(("mass_aggregate_probe_plot", probe.id)))
            .open(&mut open)
            .default_size(egui::vec2(460.0, 320.0))
            .resizable(true)
            .collapsible(true)
            .show(context, |ui| {
                mass_aggregate_history_plot(ui, probe.id, frame.mass_aggregate_history, series);
            });
        if !open {
            closed.push(probe_id);
        }
    }
    for probe in closed {
        model.mass_aggregate_probe_plots.remove(&probe);
    }
}

/// A mass-aggregate probe's history as up to five scalar traces — the
/// centroid's distance from the origin, and the magnitudes of velocity,
/// momentum, and angular momentum, plus kinetic energy directly. Same shape
/// as
/// [`distance_history_plot`]: draws its own checkboxes and mutates `series`
/// directly, so the inline inspector plot and the floating window share one
/// implementation.
pub(super) fn mass_aggregate_history_plot(
    ui: &mut egui::Ui,
    probe: MassAggregateProbeId,
    history: &MassAggregateHistory,
    series: &mut MassAggregateProbeSeries,
) {
    ui.small(format!("Bounded to {} samples", history.capacity()));
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut series.center_of_mass, "Position");
        ui.checkbox(&mut series.velocity, "Velocity");
        ui.checkbox(&mut series.momentum, "Momentum");
        ui.checkbox(&mut series.angular_momentum, "Angular momentum");
        ui.checkbox(&mut series.kinetic_energy, "Kinetic energy");
    });
    if !series.center_of_mass
        && !series.velocity
        && !series.momentum
        && !series.angular_momentum
        && !series.kinetic_energy
    {
        ui.weak("Select at least one series.");
        return;
    }
    let readings: Vec<MassAggregateReading> = history.readings(probe).copied().collect();
    if readings.is_empty() {
        ui.weak("No samples yet");
        return;
    }
    if series.center_of_mass {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.center_of_mass.length() as f32)
            .collect();
        ui.label(format!(
            "Distance from origin · {:.4} m",
            values.last().unwrap()
        ));
        history_plot(ui, &values, egui::Color32::from_rgb(245, 205, 75), "m");
    }
    if series.velocity {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.velocity.length() as f32)
            .collect();
        ui.label(format!("Speed · {:.4} m/s", values.last().unwrap()));
        history_plot(ui, &values, egui::Color32::from_rgb(100, 155, 245), "m/s");
    }
    if series.momentum {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.total_momentum.length() as f32)
            .collect();
        ui.label(format!(
            "Momentum magnitude · {:.4} kg·m/s",
            values.last().unwrap()
        ));
        history_plot(
            ui,
            &values,
            egui::Color32::from_rgb(120, 220, 150),
            "kg·m/s",
        );
    }
    if series.angular_momentum {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.angular_momentum.length() as f32)
            .collect();
        ui.label(format!(
            "Angular momentum magnitude · {:.4} kg·m²/s",
            values.last().unwrap()
        ));
        history_plot(
            ui,
            &values,
            egui::Color32::from_rgb(235, 160, 60),
            "kg·m²/s",
        );
    }
    if series.kinetic_energy {
        let values: Vec<f32> = readings
            .iter()
            .map(|reading| reading.total_kinetic_energy_j as f32)
            .collect();
        ui.label(format!("Kinetic energy · {:.4} J", values.last().unwrap()));
        history_plot(ui, &values, egui::Color32::from_rgb(220, 130, 220), "J");
    }
}

fn channel_label(channel: &ChannelId, compute: &ComputeView) -> String {
    compute
        .channel_names
        .get(channel)
        .map_or_else(|| channel.to_string(), Clone::clone)
}

#[derive(Clone, Copy)]
struct ProbeTrace {
    label: &'static str,
    color: egui::Color32,
    component: fn(FieldValue) -> Option<f64>,
}

fn paint_probe_plot(ui: &mut egui::Ui, readings: &[fieldcad_simulation::ProbeReading]) {
    let vector = readings
        .iter()
        .map(|reading| match reading.value {
            FieldValue::Vector(_) => true,
            FieldValue::Scalar(_) => false,
        })
        .next()
        .unwrap_or(false);
    let traces: &[ProbeTrace] = if vector {
        &[
            ProbeTrace {
                label: "x",
                color: egui::Color32::from_rgb(235, 90, 90),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().x),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "y",
                color: egui::Color32::from_rgb(95, 210, 120),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().y),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "z",
                color: egui::Color32::from_rgb(100, 155, 245),
                component: |value| match value {
                    FieldValue::Vector(value) => Some(value.si_value().z),
                    FieldValue::Scalar(_) => None,
                },
            },
            ProbeTrace {
                label: "|v|",
                color: egui::Color32::from_rgb(245, 205, 75),
                component: |value| Some(value.magnitude()),
            },
        ]
    } else {
        &[ProbeTrace {
            label: "value",
            color: egui::Color32::from_rgb(245, 205, 75),
            component: |value| match value {
                FieldValue::Scalar(value) => Some(value.si_value()),
                FieldValue::Vector(_) => None,
            },
        }]
    };

    let values: Vec<_> = traces
        .iter()
        .flat_map(|trace| {
            readings.iter().filter_map(move |reading| {
                is_plot_valid(reading.validity).then(|| (trace.component)(reading.value))?
            })
        })
        .collect();
    let Some((mut y_min, mut y_max)) = value_bounds(values.iter().copied()) else {
        ui.colored_label(
            egui::Color32::from_rgb(230, 150, 60),
            "Samples are currently undefined",
        );
        return;
    };
    let (display_min, display_max) = (y_min, y_max);
    if y_min == y_max {
        let padding = y_min.abs().max(1.0) * 0.05;
        y_min -= padding;
        y_max += padding;
    }
    let x_min = readings.first().map_or(0.0, |reading| reading.time_seconds);
    let x_max = readings
        .last()
        .map_or(x_min, |reading| reading.time_seconds);

    let (rect, plot, painter) = plot_frame(ui, 150.0);

    if y_min <= 0.0 && y_max >= 0.0 {
        let y = remap(0.0, y_min, y_max, plot.bottom(), plot.top());
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(65)),
        );
    }

    for trace in traces {
        let points: Vec<_> = readings
            .iter()
            .filter(|reading| is_plot_valid(reading.validity))
            .filter_map(|reading| {
                let value = (trace.component)(reading.value)?;
                let x = if x_min == x_max {
                    plot.center().x
                } else {
                    remap(
                        reading.time_seconds,
                        x_min,
                        x_max,
                        plot.left(),
                        plot.right(),
                    )
                };
                let y = remap(value, y_min, y_max, plot.bottom(), plot.top());
                Some(egui::pos2(x, y))
            })
            .collect();
        if points.len() == 1 {
            painter.circle_filled(points[0], 2.0, trace.color);
        } else if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.4, trace.color),
            ));
        }
    }

    let mut legend_x = plot.left();
    for trace in traces {
        painter.text(
            egui::pos2(legend_x, rect.top() + 3.0),
            egui::Align2::LEFT_TOP,
            trace.label,
            egui::FontId::monospace(10.0),
            trace.color,
        );
        legend_x += 30.0;
    }
    painter.text(
        rect.right_top() + egui::vec2(-4.0, 3.0),
        egui::Align2::RIGHT_TOP,
        format!("max {display_max:.3e}"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.right_top() + egui::vec2(-4.0, 14.0),
        egui::Align2::RIGHT_TOP,
        format!("min {display_min:.3e}"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -3.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{x_min:.3e} s"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.right_bottom() + egui::vec2(-4.0, -3.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{x_max:.3e} s"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
}

/// The chrome every plot in this app shares: a filled background, an inset
/// bordered plotting rect, and a painter scoped to the outer rect. Kept in
/// one place so a probe trace and a diagnostics sparkline read as the same
/// kind of object rather than two widgets that happen to sit near each
/// other.
///
/// Returns `(outer, inner, painter)` — `outer` for corner-anchored labels,
/// `inner` for the traces themselves.
fn plot_frame(ui: &mut egui::Ui, height: f32) -> (egui::Rect, egui::Rect, egui::Painter) {
    let desired = egui::vec2(ui.available_width().max(120.0), height);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let plot = rect.shrink2(egui::vec2(8.0, 20.0));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(45));
    painter.rect_stroke(
        plot,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(75)),
        egui::StrokeKind::Inside,
    );
    (rect, plot, painter)
}

/// A single-trace time series, oldest sample first. Used by the diagnostics
/// panel for frame time, memory, and CPU, and by probe plots for distance,
/// velocity, momentum, and similar recorded quantities — anything that is
/// one number sampled repeatedly, as opposed to a probe's multi-component
/// field reading. `unit` labels the min/max/avg readout (e.g. `"ms"`,
/// `"MiB"`, `"m/s"`); pass `""` for an already-dimensionless series.
pub(super) fn history_plot(ui: &mut egui::Ui, values: &[f32], color: egui::Color32, unit: &str) {
    if values.len() < 2 {
        ui.weak("Not enough samples yet");
        return;
    }
    let (mut y_min, mut y_max) = values
        .iter()
        .copied()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), value| {
            (lo.min(value), hi.max(value))
        });
    let (display_min, display_max) = (y_min, y_max);
    let display_avg = values.iter().copied().sum::<f32>() / values.len() as f32;
    if y_min == y_max {
        let padding = y_min.abs().max(1.0) * 0.05;
        y_min -= padding;
        y_max += padding;
    }

    let (rect, plot, painter) = plot_frame(ui, 60.0);

    let last_index = (values.len() - 1) as f64;
    let points: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(i, &value)| {
            let x = remap(i as f64, 0.0, last_index, plot.left(), plot.right());
            let y = remap(
                value as f64,
                y_min as f64,
                y_max as f64,
                plot.bottom(),
                plot.top(),
            );
            egui::pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.4, color)));

    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -3.0),
        egui::Align2::LEFT_BOTTOM,
        "oldest",
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.right_bottom() + egui::vec2(-4.0, -3.0),
        egui::Align2::RIGHT_BOTTOM,
        "now",
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.left_top() + egui::vec2(4.0, 3.0),
        egui::Align2::LEFT_TOP,
        format!("max {display_max:.2} {unit}"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.right_top() + egui::vec2(-4.0, 3.0),
        egui::Align2::RIGHT_TOP,
        format!("min {display_min:.2} {unit}"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
    painter.text(
        rect.center_top() + egui::vec2(0.0, 3.0),
        egui::Align2::CENTER_TOP,
        format!("avg {display_avg:.2} {unit}"),
        egui::FontId::monospace(9.0),
        egui::Color32::GRAY,
    );
}

fn is_plot_valid(validity: SampleValidity) -> bool {
    !matches!(validity, SampleValidity::Undefined(_))
}

fn value_bounds(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    values
        .filter(|value| value.is_finite())
        .fold(None, |bounds, value| {
            Some(match bounds {
                Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                None => (value, value),
            })
        })
}

fn remap(value: f64, from_min: f64, from_max: f64, to_min: f32, to_max: f32) -> f32 {
    let fraction = ((value - from_min) / (from_max - from_min)) as f32;
    to_min + fraction * (to_max - to_min)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn probe_plot_bounds_include_negative_components() {
        assert_eq!(
            value_bounds([-4.0, 2.0, 9.0, -1.0].into_iter()),
            Some((-4.0, 9.0))
        );
        assert_eq!(value_bounds([f64::NAN].into_iter()), None);
    }

    fn reading(time_seconds: f64, distance: f64) -> DistanceReading {
        DistanceReading {
            tick: 0,
            time_seconds,
            world_revision: fieldcad_core::WorldRevision::INITIAL,
            snapshot_sequence: 0,
            distance,
        }
    }

    #[test]
    fn distance_rate_of_change_uses_the_actual_time_gap_between_samples() {
        let readings = [reading(0.0, 1.0), reading(0.5, 2.0), reading(1.5, 2.0)];
        // (2.0 - 1.0) / 0.5 = 2.0 m/s, then (2.0 - 2.0) / 1.0 = 0.0 m/s.
        assert_eq!(distance_rate_of_change(&readings), vec![2.0, 0.0]);
    }

    #[test]
    fn distance_rate_of_change_skips_a_non_positive_time_gap() {
        let readings = [reading(1.0, 1.0), reading(1.0, 5.0), reading(2.0, 7.0)];
        // The first pair's zero gap is skipped rather than producing an
        // infinite rate; the second pair still reports normally.
        assert_eq!(distance_rate_of_change(&readings), vec![2.0]);
    }

    #[test]
    fn distance_rate_of_change_is_empty_for_a_single_reading() {
        assert!(distance_rate_of_change(&[reading(0.0, 1.0)]).is_empty());
    }
}
