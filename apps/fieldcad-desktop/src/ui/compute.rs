//! The read-only, per-frame view of what a data source is reporting, and the
//! formatting that turns it into text.
//!
//! Built once per frame so that panels take a plain value. Nothing here depends
//! on whether compute is local or remote, and nothing here can issue a command.

use std::collections::{BTreeMap, BTreeSet};

use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, ChannelId, DiagnosticSeverity, Domain, FieldValue,
    FieldValueKind, ObjectId, PluginId, SampleValidity, SimulationMode, SnapshotFreshness,
    UndefinedReason, WorldRevision, WorldSnapshot,
};
use fieldcad_simulation::{
    DataSourceStatus, EditHistoryStatus, FieldDataSource, FieldSystemStatus, Subscription,
};
use glam::DVec3;

/// A per-frame, read-only summary of what the data source is reporting.
///
/// Built once per frame so that panels take a plain value. Nothing here depends
/// on whether compute is local or remote.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputeView {
    pub description: String,
    pub status: DataSourceStatus,
    pub domain: Domain,
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
    /// All equation systems composed into the scene, including inactive ones.
    pub field_systems: Vec<FieldSystemStatus>,
    /// The physical fields this scene can have, and which model computes each.
    pub fields: Vec<FieldRow>,
    /// What undo and redo are currently offering, as the source reports it.
    pub edit_history: EditHistoryStatus,
    /// Channels a generic vector layer can draw, in published order.
    pub vector_channels: Vec<ChannelId>,
    /// What the source has acknowledged it is sampling.
    pub subscription: Subscription,
    pub diagnostics: Vec<String>,
    pub has_errors: bool,
    /// The dynamics system's summed force on every body it advanced at the
    /// most recent tick, for the inspector's read-only derived-values display.
    pub body_forces: BTreeMap<ObjectId, DVec3>,
}

impl ComputeView {
    pub fn build(source: &dyn FieldDataSource, world: &WorldSnapshot) -> Self {
        let simulation = source.simulation_status();
        let domain = source.domain();
        let snapshot = source.latest_snapshot();

        let mut probe_readings = Vec::new();
        let mut diagnostics = Vec::new();
        let mut has_errors = false;
        let mut channel_names = BTreeMap::new();
        let mut vector_channels = Vec::new();
        let mut total_samples = 0;
        let mut domain_summary = "No data".to_owned();
        let field_systems = source.field_systems();
        let active_plugins: BTreeSet<_> = field_systems
            .iter()
            .filter(|system| system.enabled)
            .map(|system| system.plugin.id.clone())
            .collect();

        // Available field names outlive publication. This keeps inactive
        // channels identifiable in probe recorders and the scene inspector.
        for system in &field_systems {
            for channel in &system.channels {
                channel_names.insert(channel.id.clone(), channel.display_name.clone());
            }
        }

        if let Some(snapshot) = &snapshot {
            // A remote source can acknowledge composition before the replacement
            // snapshot arrives. Filter the retained complete snapshot by the
            // acknowledged system state so a disabled field never remains
            // visible during that delivery gap.
            total_samples = snapshot
                .channels
                .iter()
                .filter(|(_, channel)| active_plugins.contains(&channel.provider))
                .map(|(_, channel)| {
                    channel
                        .batches
                        .iter()
                        .map(fieldcad_core::FieldBatch::len)
                        .sum::<usize>()
                })
                .sum();
            vector_channels = snapshot
                .vector_channels()
                .filter(|channel| active_plugins.contains(&channel.provider))
                .map(|channel| channel.schema.id.clone())
                .collect();
            let cells = snapshot.domain.resolution().cells();
            domain_summary = format!(
                "{}×{}×{} = {} cells, {}, {}",
                cells.x,
                cells.y,
                cells.z,
                snapshot.domain.resolution().cell_count(),
                snapshot.domain.precision().label(),
                format_boundaries(snapshot.domain.boundaries()),
            );
            diagnostics = snapshot
                .diagnostics
                .iter()
                .filter(|diagnostic| active_plugins.contains(&diagnostic.plugin))
                .map(|diagnostic| {
                    has_errors |= diagnostic.severity == DiagnosticSeverity::Error;
                    format!("[{:?}] {}", diagnostic.severity, diagnostic.message)
                })
                .collect();

            for (channel_id, channel) in &snapshot.channels {
                // Which system produced a value, not which namespace names it:
                // a field channel is shared, so its identifier cannot say who
                // computed it and a retained snapshot must be filtered by the
                // provenance it carries.
                if !active_plugins.contains(&channel.provider) {
                    continue;
                }
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
            domain,
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
            fields: FieldRow::collect(&field_systems),
            field_systems,
            edit_history: source.edit_history(),
            vector_channels,
            subscription: source.subscription(),
            diagnostics,
            has_errors,
            body_forces: source.body_forces(),
        }
    }

    /// Transport controls are only meaningful against a connected source.
    pub fn accepts_commands(&self) -> bool {
        self.status == DataSourceStatus::Ready
    }

    /// Whether the scene can be stepped through its edit history right now.
    ///
    /// Paused, because an undo names a scene and a running clock is replacing
    /// that scene underneath it; the authoritative side refuses either way, and
    /// this is what lets the control say so before it is pressed.
    pub fn accepts_history_commands(&self) -> bool {
        self.accepts_commands() && self.mode == SimulationMode::Paused
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

fn format_boundaries(boundaries: BoundaryConditions) -> String {
    let label = |condition| match condition {
        BoundaryCondition::Periodic => "periodic",
        BoundaryCondition::Dirichlet => "Dirichlet",
        BoundaryCondition::Neumann => "Neumann",
        BoundaryCondition::Absorbing => "absorbing",
        BoundaryCondition::Open => "open",
    };
    if boundaries.x == boundaries.y && boundaries.y == boundaries.z {
        format!("{} boundaries", label(boundaries.x))
    } else {
        format!(
            "x {}, y {}, z {} boundaries",
            label(boundaries.x),
            label(boundaries.y),
            label(boundaries.z)
        )
    }
}

/// One physical field the scene can have, and the models that can compute it.
///
/// Built by asking which systems *declare* each channel rather than which are
/// publishing it, so a field with no active model is still listed — a scene that
/// could have a magnetic field but currently does not is worth saying, and a
/// control that only appears once you have already switched something on is
/// unreachable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldRow {
    pub channel: ChannelId,
    pub display_name: String,
    pub value_kind: FieldValueKind,
    /// Every composed system that can compute this field, in catalog order.
    pub candidates: Vec<PluginId>,
    /// The one currently computing it, if any.
    pub provider: Option<PluginId>,
}

impl FieldRow {
    fn collect(systems: &[FieldSystemStatus]) -> Vec<Self> {
        // A channel in a namespace no composed plugin claims is a quantity of
        // the scene, declared in a shared domain module so that several models
        // can compute it. A channel in its own plugin's namespace is that
        // method's own output — an energy density defined on a Yee lattice, a
        // divergence residual — and belongs with the system that defines it, not
        // in a list of fields the world has.
        let owned: BTreeSet<_> = systems.iter().map(|system| &system.plugin.id).collect();
        let mut rows: Vec<Self> = Vec::new();
        for system in systems {
            for channel in &system.channels {
                if owned.contains(channel.id.plugin()) {
                    continue;
                }
                let index = match rows.iter().position(|row| row.channel == channel.id) {
                    Some(index) => index,
                    None => {
                        rows.push(Self {
                            channel: channel.id.clone(),
                            display_name: channel.display_name.clone(),
                            value_kind: channel.value_kind,
                            candidates: Vec::new(),
                            provider: None,
                        });
                        rows.len() - 1
                    }
                };
                rows[index].candidates.push(system.plugin.id.clone());
                if system.enabled {
                    rows[index].provider = Some(system.plugin.id.clone());
                }
            }
        }
        rows.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        rows
    }

    /// Whether this field is a choice rather than a fixed consequence of which
    /// systems exist.
    pub fn has_alternatives(&self) -> bool {
        self.candidates.len() > 1
    }

    pub fn kind_label(&self) -> String {
        let kind = match self.value_kind {
            FieldValueKind::Scalar(_) => "scalar",
            FieldValueKind::Vector(_) => "vector",
        };
        format!("{kind} · {}", self.value_kind.dimension())
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
        SampleValidity::Undefined(UndefinedReason::AcrossPeriodicSeam) => {
            Some("undefined — across periodic seam")
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
    use fieldcad_test_field::{scalar_channel_id, vector_channel_id};

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
        assert!(view.domain_summary.contains("open boundaries"));
        assert_eq!(view.probe_readings.len(), 1);
        assert_eq!(view.probe_readings[0].probe_name, "Origin probe");
    }

    #[test]
    fn boundary_summary_reports_uniform_and_mixed_domains() {
        assert_eq!(
            format_boundaries(BoundaryConditions::uniform(BoundaryCondition::Periodic)),
            "periodic boundaries"
        );
        assert_eq!(
            format_boundaries(BoundaryConditions {
                x: BoundaryCondition::Periodic,
                y: BoundaryCondition::Absorbing,
                z: BoundaryCondition::Open,
            }),
            "x periodic, y absorbing, z open boundaries"
        );
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

    /// The defect this exists to prevent: the inspector listing "Electric field
    /// E" twice because two plugins each called their output that. A scene has
    /// one electric field, and the models of it are a choice.
    #[test]
    fn the_electric_field_appears_once_with_the_models_that_can_compute_it() {
        use fieldcad_electromagnetic_sources::{
            electric_field_channel_id, magnetic_field_channel_id,
        };
        use fieldcad_simulation::{PluginRegistration, RuntimeConfig, SimulationRuntime};

        let domain = fieldcad_core::Domain::new(
            fieldcad_core::DomainBounds::centred_cube(2.0).unwrap(),
            fieldcad_core::Resolution::uniform(8).unwrap(),
            fieldcad_core::BoundaryConditions::uniform(fieldcad_core::BoundaryCondition::Periodic),
            fieldcad_core::Precision::F64,
        );
        let step = fieldcad_core::TimeStep::from_seconds(
            fieldcad_electromagnetism::courant_limit(&domain) * 0.8,
        )
        .unwrap();
        let source = fieldcad_simulation::LocalDataSource::new(
            SimulationRuntime::new(
                RuntimeConfig::new(domain, step, fieldcad_core::SessionId::from_u128(0x90))
                    .with_plugin(Box::new(
                        fieldcad_electrostatics::ElectrostaticsPlugin::new(),
                    ))
                    .with_plugin_registration(
                        PluginRegistration::with_default_configuration(Box::new(
                            fieldcad_electromagnetism::ElectromagnetismPlugin::new(),
                        ))
                        .with_enabled(false),
                    ),
            )
            .unwrap(),
        );

        let view = ComputeView::build(&source, &fieldcad_core::World::new().snapshot());

        let electric: Vec<_> = view
            .fields
            .iter()
            .filter(|field| field.channel == electric_field_channel_id())
            .collect();
        assert_eq!(electric.len(), 1, "one field, not one per plugin");
        let electric = electric[0];
        assert_eq!(electric.display_name, "Electric field E");
        assert!(
            electric.has_alternatives(),
            "both composed systems must be offered as models of it"
        );
        assert_eq!(
            electric.provider.as_ref(),
            Some(&fieldcad_electrostatics::plugin_id())
        );

        // The magnetic field is listed even though nothing computes it, so the
        // control that would turn it on is reachable.
        let magnetic = view
            .fields
            .iter()
            .find(|field| field.channel == magnetic_field_channel_id())
            .expect("a composed system declares a magnetic field");
        assert_eq!(magnetic.provider, None);
        assert!(!magnetic.has_alternatives());

        // No field is listed twice, whatever the models.
        let mut seen: Vec<_> = view.fields.iter().map(|field| &field.channel).collect();
        let total = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), total);

        // A residual on a Yee lattice is not a field the world has. It stays
        // with the method that defines it rather than joining this list.
        assert!(
            !view.fields.iter().any(|field| {
                field.channel == fieldcad_electromagnetism::magnetic_divergence_channel_id()
            }),
            "method diagnostics must not be listed as fields of the scene: {:?}",
            view.fields
                .iter()
                .map(|field| &field.display_name)
                .collect::<Vec<_>>()
        );
        assert!(
            source.field_systems().iter().any(|system| {
                system.channels.iter().any(|channel| {
                    channel.id == fieldcad_electromagnetism::magnetic_divergence_channel_id()
                })
            }),
            "but it is still reachable through the system that publishes it"
        );
    }

    #[test]
    fn inactive_systems_and_their_available_fields_remain_visible_to_the_ui() {
        let world = seeded_world();
        let mut source = source();
        let system = source.field_systems()[0].plugin.id.clone();
        source
            .execute(fieldcad_simulation::CommandSequencer::default().issue(
                fieldcad_simulation::CommandPayload::SetFieldSystemEnabled {
                    plugin: system,
                    enabled: false,
                },
            ))
            .unwrap();

        let view = ComputeView::build(&source, &world.snapshot());

        assert_eq!(view.field_systems.len(), 1);
        assert!(!view.field_systems[0].enabled);
        assert_eq!(view.field_systems[0].configuration.len(), 1);
        assert!(view.channel_names.contains_key(&scalar_channel_id()));
        assert!(view.channel_names.contains_key(&vector_channel_id()));
        assert!(view.vector_channels.is_empty());
        assert!(view.probe_readings.is_empty());
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
