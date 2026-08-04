//! The bounded probe-history plot.
//!
//! Painted directly rather than through a plotting crate: the traces are few and
//! the axes must state simulation time and units exactly, so the small amount of
//! drawing here is cheaper than adapting a general-purpose plot widget.

use fieldcad_core::{ChannelId, FieldValue, ProbeId, SampleValidity};
use fieldcad_simulation::ProbeHistory;

use super::compute::ComputeView;
use super::{FrameContext, UiModel};

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
    if y_min == y_max {
        let padding = y_min.abs().max(1.0) * 0.05;
        y_min -= padding;
        y_max += padding;
    }
    let x_min = readings.first().map_or(0.0, |reading| reading.time_seconds);
    let x_max = readings
        .last()
        .map_or(x_min, |reading| reading.time_seconds);

    let desired = egui::vec2(ui.available_width().max(120.0), 150.0);
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
}
