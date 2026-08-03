//! The read-only, per-frame view of what a data source is reporting, and the
//! formatting that turns it into text.
//!
//! Built once per frame so that panels take a plain value. Nothing here depends
//! on whether compute is local or remote, and nothing here can issue a command.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, DiagnosticSeverity, FieldValue, SampleValidity, SimulationMode, SnapshotFreshness,
    UndefinedReason, WorldRevision, WorldSnapshot,
};
use fieldcad_simulation::{DataSourceStatus, FieldDataSource, Subscription};

/// A per-frame, read-only summary of what the data source is reporting.
///
/// Built once per frame so that panels take a plain value. Nothing here depends
/// on whether compute is local or remote.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputeView {
    pub description: String,
    pub status: DataSourceStatus,
    pub mode: SimulationMode,
    pub tick: u64,
    pub time_seconds: f64,
    pub time_step_seconds: f64,
    pub playback_speed: f64,
    pub pending_commands: usize,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: Option<u64>,
    pub freshness: Option<SnapshotFreshness>,
    pub total_samples: usize,
    pub domain_summary: String,
    pub probe_readings: Vec<ProbeRow>,
    pub channel_names: BTreeMap<ChannelId, String>,
    /// Channels a generic vector layer can draw, in published order.
    pub vector_channels: Vec<ChannelId>,
    /// What the source has acknowledged it is sampling.
    pub subscription: Subscription,
    pub diagnostics: Vec<String>,
    pub has_errors: bool,
}

impl ComputeView {
    pub fn build(source: &dyn FieldDataSource, world: &WorldSnapshot) -> Self {
        let simulation = source.simulation_status();
        let snapshot = source.latest_snapshot();

        let mut probe_readings = Vec::new();
        let mut diagnostics = Vec::new();
        let mut has_errors = false;
        let mut channel_names = BTreeMap::new();
        let mut vector_channels = Vec::new();
        let mut total_samples = 0;
        let mut domain_summary = "No data".to_owned();

        if let Some(snapshot) = &snapshot {
            total_samples = snapshot.total_samples();
            vector_channels = snapshot
                .vector_channels()
                .map(|channel| channel.schema.id.clone())
                .collect();
            let cells = snapshot.domain.resolution().cells();
            domain_summary = format!(
                "{}×{}×{} = {} cells, {}",
                cells.x,
                cells.y,
                cells.z,
                snapshot.domain.resolution().cell_count(),
                snapshot.domain.precision().label(),
            );
            diagnostics = snapshot
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    has_errors |= diagnostic.severity == DiagnosticSeverity::Error;
                    format!("[{:?}] {}", diagnostic.severity, diagnostic.message)
                })
                .collect();

            for (channel_id, channel) in &snapshot.channels {
                channel_names.insert(channel_id.clone(), channel.schema.display_name.clone());
                for probe in world.probes().values() {
                    if !probe.channels.contains(channel_id) {
                        continue;
                    }
                    let Some(sample) = channel.probe_sample(probe.id) else {
                        continue;
                    };
                    probe_readings.push(ProbeRow {
                        probe_name: probe.name.clone(),
                        channel_name: channel.schema.display_name.clone(),
                        value: format_value(sample.value),
                        validity: sample.validity,
                    });
                }
            }
        }

        Self {
            description: source.description().to_owned(),
            status: source.status(),
            mode: simulation.mode(),
            tick: simulation.tick(),
            time_seconds: simulation.time_seconds(),
            time_step_seconds: simulation.time_step().seconds(),
            playback_speed: source.playback_speed().multiplier(),
            pending_commands: source.pending_command_count(),
            world_revision: simulation.world_revision,
            snapshot_sequence: snapshot.as_ref().map(|snapshot| snapshot.identity.sequence),
            freshness: snapshot
                .as_ref()
                .map(|snapshot| snapshot.freshness_against(simulation.world_revision)),
            total_samples,
            domain_summary,
            probe_readings,
            channel_names,
            vector_channels,
            subscription: source.subscription(),
            diagnostics,
            has_errors,
        }
    }

    /// Transport controls are only meaningful against a connected source.
    pub fn accepts_commands(&self) -> bool {
        self.status == DataSourceStatus::Ready
    }

    pub fn workbench_state(&self) -> WorkbenchState {
        if self.has_errors
            || matches!(self.status, DataSourceStatus::Failed(_))
            || self.freshness == Some(SnapshotFreshness::Future)
        {
            return WorkbenchState::Invalid;
        }
        match self.status {
            DataSourceStatus::Connecting => WorkbenchState::Connecting,
            DataSourceStatus::Disconnected => WorkbenchState::Disconnected,
            DataSourceStatus::Failed(_) => WorkbenchState::Invalid,
            DataSourceStatus::Ready
                if self.pending_commands > 0
                    || self.snapshot_sequence.is_none()
                    || self.freshness == Some(SnapshotFreshness::Stale) =>
            {
                WorkbenchState::Solving
            }
            DataSourceStatus::Ready => match self.mode {
                SimulationMode::Running => WorkbenchState::Running,
                SimulationMode::Paused => WorkbenchState::Paused,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchState {
    Connecting,
    Solving,
    Running,
    Paused,
    Disconnected,
    Invalid,
}

impl WorkbenchState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Solving => "Solving",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Disconnected => "Disconnected",
            Self::Invalid => "Invalid",
        }
    }

    pub(super) fn color(self) -> egui::Color32 {
        match self {
            Self::Running => egui::Color32::from_rgb(90, 205, 125),
            Self::Paused => egui::Color32::from_rgb(120, 175, 235),
            Self::Solving | Self::Connecting => egui::Color32::from_rgb(235, 190, 75),
            Self::Disconnected | Self::Invalid => egui::Color32::from_rgb(235, 105, 90),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeRow {
    pub probe_name: String,
    pub channel_name: String,
    pub value: String,
    pub validity: SampleValidity,
}

fn format_value(value: FieldValue) -> String {
    match value {
        FieldValue::Scalar(value) => format!("{:.6} {}", value.si_value(), value.dimension()),
        FieldValue::Vector(value) => {
            let vector = value.si_value();
            format!(
                "({:.4}, {:.4}, {:.4}) {}",
                vector.x,
                vector.y,
                vector.z,
                value.dimension()
            )
        }
    }
}

/// A sample that is not defined must never be shown as though it were measured.
pub(super) fn validity_note(validity: SampleValidity) -> Option<&'static str> {
    match validity {
        SampleValidity::Exact => None,
        SampleValidity::Interpolated(_) => Some("interpolated"),
        SampleValidity::Undefined(UndefinedReason::InsideSourceRadius) => {
            Some("undefined — inside source radius")
        }
        SampleValidity::Undefined(UndefinedReason::OutsideDomain) => {
            Some("undefined — outside domain")
        }
        SampleValidity::Undefined(UndefinedReason::NotConverged) => {
            Some("undefined — not converged")
        }
        SampleValidity::Undefined(UndefinedReason::NumericalOverflow) => {
            Some("undefined — numerical overflow")
        }
    }
}

pub(super) fn format_simulation_time(seconds: f64) -> String {
    if seconds == 0.0 {
        "0 s".to_owned()
    } else {
        format_time_step(seconds)
    }
}

pub(super) fn time_step_drag_speed(seconds: f64) -> f64 {
    (seconds.abs() * 0.01).max(f64::from_bits(1))
}

pub(super) fn format_time_step(seconds: f64) -> String {
    let (factor, suffix) = if seconds >= 1.0 {
        (1.0, "s")
    } else if seconds >= 1.0e-3 {
        (1.0e-3, "ms")
    } else if seconds >= 1.0e-6 {
        (1.0e-6, "µs")
    } else if seconds >= 1.0e-9 {
        (1.0e-9, "ns")
    } else if seconds >= 1.0e-12 {
        (1.0e-12, "ps")
    } else {
        (1.0e-15, "fs")
    };
    format!("{} {suffix}", seconds / factor)
}

pub(super) fn parse_playback_speed(text: &str) -> Option<f64> {
    text.trim().trim_end_matches(['x', '×']).trim().parse().ok()
}

#[cfg(test)]
mod tests {

    use super::super::tests::{seeded_world, source};
    use super::*;

    #[test]
    fn the_compute_view_reports_provenance_from_the_source() {
        let world = seeded_world();
        let view = ComputeView::build(&source(), &world.snapshot());

        assert_eq!(view.mode, SimulationMode::Paused);
        assert_eq!(view.freshness, Some(SnapshotFreshness::Current));
        assert!(view.domain_summary.contains("512"));
        assert!(view.domain_summary.contains("f64"));
        assert_eq!(view.probe_readings.len(), 1);
        assert_eq!(view.probe_readings[0].probe_name, "Origin probe");
    }

    #[test]
    fn probe_values_are_shown_with_their_units() {
        let world = seeded_world();
        let view = ComputeView::build(&source(), &world.snapshot());

        // The probe sits at z = 0.6, so the linear scalar reads 3 * 0.6 m.
        assert!(view.probe_readings[0].value.starts_with("1.800000"));
        assert!(view.probe_readings[0].value.ends_with(" m"));
    }

    #[test]
    fn undefined_samples_are_labelled_rather_than_printed_as_numbers() {
        assert_eq!(validity_note(SampleValidity::Exact), None);
        assert_eq!(
            validity_note(SampleValidity::Undefined(
                UndefinedReason::InsideSourceRadius
            )),
            Some("undefined — inside source radius")
        );
    }

    #[test]
    fn a_disconnected_source_cannot_be_commanded_from_the_ui() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot());
        assert!(view.accepts_commands());

        view.status = DataSourceStatus::Disconnected;

        assert!(!view.accepts_commands());
        assert_eq!(view.status.label(), "Disconnected");
    }

    #[test]
    fn workbench_state_distinguishes_paused_solving_stale_and_invalid() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot());
        assert_eq!(view.workbench_state(), WorkbenchState::Paused);

        view.freshness = Some(SnapshotFreshness::Stale);
        assert_eq!(view.workbench_state(), WorkbenchState::Solving);

        view.has_errors = true;
        assert_eq!(view.workbench_state(), WorkbenchState::Invalid);

        view.has_errors = false;
        view.status = DataSourceStatus::Disconnected;
        assert_eq!(view.workbench_state(), WorkbenchState::Disconnected);
    }

    #[test]
    fn time_step_control_formats_values_at_a_readable_si_scale() {
        assert_eq!(format_time_step(432.0), "432 s");
        assert_eq!(format_time_step(1.23e-9), "1.23 ns");
        assert_eq!(format_time_step(4.43e-3), "4.43 ms");
        assert_eq!(format_time_step(7.3213e-7), "732.13 ns");
    }

    #[test]
    fn time_step_drag_speed_tracks_the_current_order_of_magnitude() {
        assert_eq!(time_step_drag_speed(1.0), 0.01);
        assert!((time_step_drag_speed(1.0e-9) - 1.0e-11).abs() < 1.0e-26);
        assert!(time_step_drag_speed(f64::from_bits(1)) > 0.0);
    }

    #[test]
    fn playback_speed_control_accepts_plain_and_multiplier_notation() {
        assert_eq!(parse_playback_speed("2"), Some(2.0));
        assert_eq!(parse_playback_speed("0.25×"), Some(0.25));
        assert_eq!(parse_playback_speed("1e2x"), Some(100.0));
        assert_eq!(parse_playback_speed("fast"), None);
    }
}
