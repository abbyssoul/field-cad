//! The read-only, per-frame view of what a data source is reporting, and the
//! formatting that turns it into text.
//!
//! Built once per frame so that panels take a plain value. Nothing here depends
//! on whether compute is local or remote, and nothing here can issue a command.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, ChannelId, DiagnosticSeverity, Domain, FieldSnapshot,
    FieldValue, FieldValueKind, ObjectId, PluginId, SampleValidity, SceneScale, SimulationMode,
    SnapshotFreshness, UndefinedReason, WorldRevision, WorldSnapshot,
};
use fieldcad_simulation::{
    DataSourceStatus, EditHistoryStatus, FieldDataSource, FieldSystemStatus, QueueStatus,
    QueueSummary, Subscription,
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
    /// How many metres one render/camera unit represents. Drives the
    /// desktop viewport's world-to-render conversion — see
    /// [`fieldcad_core::SceneScale`].
    pub scene_scale: SceneScale,
    pub mode: SimulationMode,
    pub tick: u64,
    pub time_seconds: f64,
    pub time_step_seconds: f64,
    pub playback_speed: f64,
    pub pending_commands: usize,
    /// Authoritative queue state: paused flag, ordered pending commands, and
    /// recent terminal history — for the Queue panel to inspect and control.
    pub queue: QueueStatus,
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
    /// Active vector fields that their owning numerical solver permits painting.
    pub mutable_vector_channels: Vec<ChannelId>,
    /// What the source has acknowledged it is sampling.
    pub subscription: Subscription,
    pub diagnostics: Vec<String>,
    pub has_errors: bool,
    /// The dynamics system's summed force on every body it advanced at the
    /// most recent tick, for the inspector's read-only derived-values display.
    pub body_forces: BTreeMap<ObjectId, DVec3>,
    /// Wall-clock milliseconds the most recent simulation tick took to
    /// compute. Zero before the first tick.
    pub step_compute_ms: f32,
}

/// The fields [`ComputeView::build`] derives purely from the latest
/// snapshot (plus which field systems are active, which cannot change
/// without a new snapshot being published — see the note on `snapshot`
/// below) rather than being individually cheap to read from the source.
/// Bundled so [`ComputeView::build`] can reuse the whole group at once
/// when the snapshot it was built from is still current.
#[derive(Clone)]
struct SnapshotDerived {
    total_samples: usize,
    domain_summary: String,
    probe_readings: Vec<ProbeRow>,
    channel_names: BTreeMap<ChannelId, String>,
    vector_channels: Vec<ChannelId>,
    diagnostics: Vec<String>,
    has_errors: bool,
    /// Bundled here too: which field systems exist cannot change without a
    /// new snapshot being published (see the note on `build` below), so
    /// re-fetching it from the source — and re-deriving `fields` and
    /// `mutable_vector_channels` from it — is exactly as reusable as
    /// everything else in this group, not an unconditional per-frame cost.
    field_systems: Vec<FieldSystemStatus>,
    fields: Vec<FieldRow>,
    mutable_vector_channels: Vec<ChannelId>,
}

impl ComputeView {
    /// Built once per frame so that panels take a plain value — but
    /// building it is not free (`total_samples`, `probe_readings`, and
    /// `diagnostics` scale with channels and probes, and `queue` clones up
    /// to 256 terminal records), and a redraw can run at up to the
    /// display's refresh rate while the source itself changes far less
    /// often. `previous` is last frame's view, if there was one: its
    /// snapshot-derived fields are reused verbatim when `source.latest_snapshot()`
    /// reports the same `identity.sequence` (every path that changes them —
    /// a committed world edit, a field system enabled/disabled, a field
    /// brush stroke — publishes a new snapshot; see
    /// `SimulationRuntime::commit_world_commands`/`set_field_system_enabled`),
    /// and its `queue` is reused when `source.queue_summary()` agrees with
    /// it. Everything else here is already an O(1) or small, bounded read,
    /// so it stays unconditional.
    pub fn build(
        source: &dyn FieldDataSource,
        world: &WorldSnapshot,
        previous: Option<&Self>,
    ) -> Self {
        let simulation = source.simulation_status();
        let domain = source.domain();
        let snapshot = source.latest_snapshot();
        let snapshot_sequence = snapshot.as_ref().map(|snapshot| snapshot.identity.sequence);

        let reusable = previous.filter(|previous| previous.snapshot_sequence == snapshot_sequence);
        let SnapshotDerived {
            total_samples,
            domain_summary,
            probe_readings,
            channel_names,
            vector_channels,
            diagnostics,
            has_errors,
            field_systems,
            fields,
            mutable_vector_channels,
        } = match reusable {
            Some(previous) => SnapshotDerived {
                total_samples: previous.total_samples,
                domain_summary: previous.domain_summary.clone(),
                probe_readings: previous.probe_readings.clone(),
                channel_names: previous.channel_names.clone(),
                vector_channels: previous.vector_channels.clone(),
                diagnostics: previous.diagnostics.clone(),
                has_errors: previous.has_errors,
                field_systems: previous.field_systems.clone(),
                fields: previous.fields.clone(),
                mutable_vector_channels: previous.mutable_vector_channels.clone(),
            },
            None => snapshot_derived(source, &snapshot, world),
        };

        let queue = match previous {
            Some(previous) if queue_matches_summary(&previous.queue, source.queue_summary()) => {
                previous.queue.clone()
            }
            _ => source.get_queue(),
        };

        Self {
            description: source.description().to_owned(),
            status: source.status(),
            domain,
            scene_scale: source.scene_scale(),
            mode: simulation.mode(),
            tick: simulation.tick(),
            time_seconds: simulation.time_seconds(),
            time_step_seconds: simulation.time_step().seconds(),
            playback_speed: source.playback_speed().multiplier(),
            pending_commands: source.pending_command_count(),
            queue,
            world_revision: simulation.world_revision,
            snapshot_sequence,
            freshness: snapshot
                .as_ref()
                .map(|snapshot| snapshot.freshness_against(simulation.world_revision)),
            total_samples,
            domain_summary,
            probe_readings,
            channel_names,
            fields,
            field_systems,
            edit_history: source.edit_history(),
            vector_channels,
            mutable_vector_channels,
            subscription: source.subscription(),
            diagnostics,
            has_errors,
            body_forces: source.body_forces(),
            step_compute_ms: source.step_compute_ms(),
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

/// The expensive part of [`ComputeView::build`]: everything that scales
/// with the number of published channels and probes, computed fresh only
/// when there is no snapshot to reuse it from — including `field_systems`
/// itself, which clones a `ChannelSchema` per channel per system and is no
/// cheaper to ask for than the rest of this group.
fn snapshot_derived(
    source: &dyn FieldDataSource,
    snapshot: &Option<Arc<FieldSnapshot>>,
    world: &WorldSnapshot,
) -> SnapshotDerived {
    let field_systems = source.field_systems();
    let mutable_vector_channels = field_systems
        .iter()
        .filter(|system| system.enabled)
        .flat_map(|system| system.mutable_vector_channels.iter().cloned())
        .collect();
    let fields = FieldRow::collect(&field_systems);

    let mut probe_readings = Vec::new();
    let mut diagnostics = Vec::new();
    let mut has_errors = false;
    let mut channel_names = BTreeMap::new();
    let mut vector_channels = Vec::new();
    let mut total_samples = 0;
    let mut domain_summary = "No data".to_owned();

    // Available field names outlive publication. This keeps inactive
    // channels identifiable in probe recorders and the scene inspector.
    for system in &field_systems {
        for channel in &system.channels {
            channel_names.insert(channel.id.clone(), channel.display_name.clone());
        }
    }

    if let Some(snapshot) = snapshot {
        let active_plugins: BTreeSet<_> = field_systems
            .iter()
            .filter(|system| system.enabled)
            .map(|system| system.plugin.id.clone())
            .collect();

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
            // Which system produced a value, not which namespace names it: a
            // field channel is shared, so its identifier cannot say who
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

    SnapshotDerived {
        total_samples,
        domain_summary,
        probe_readings,
        channel_names,
        vector_channels,
        diagnostics,
        has_errors,
        field_systems,
        fields,
        mutable_vector_channels,
    }
}

/// Whether `cached` (last frame's `get_queue()` result) already reflects
/// what `summary` (this frame's cheap [`FieldDataSource::queue_summary`])
/// reports — checked without touching `cached.pending`/`.history`'s
/// contents, only their lengths and the newest history id.
fn queue_matches_summary(cached: &QueueStatus, summary: QueueSummary) -> bool {
    cached.paused == summary.paused
        && cached.pending.len() == summary.pending_len
        && cached.history.len() == summary.history_len
        && cached.history.last().map(|record| record.command) == summary.newest_history
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

/// A general-purpose numeric display for values that can range over many
/// orders of magnitude (particle masses, scene scale, …): plain decimal
/// within a normal-looking range, scientific notation outside it — so a
/// value like `1.0` or `2.0` reads as `1.0000`/`2.0000` rather than the
/// unconditional `1e0`/`2e0` a bare `{:e}` format would produce.
pub(super) fn format_engineering(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let magnitude = value.abs();
    if (1.0e-3..1.0e6).contains(&magnitude) {
        format!("{value:.4}")
    } else {
        format!("{value:.6e}")
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_test_field::{scalar_channel_id, vector_channel_id};

    use super::super::tests::{seeded_world, source};
    use super::*;

    #[test]
    fn the_compute_view_reports_provenance_from_the_source() {
        let world = seeded_world();
        let view = ComputeView::build(&source(), &world.snapshot(), None);

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
        let view = ComputeView::build(&source(), &world.snapshot(), None);

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
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot(), None);
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

        let view = ComputeView::build(&source, &fieldcad_core::World::new().snapshot(), None);

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

        let view = ComputeView::build(&source, &world.snapshot(), None);

        assert_eq!(view.field_systems.len(), 1);
        assert!(!view.field_systems[0].enabled);
        assert_eq!(view.field_systems[0].configuration.len(), 1);
        assert!(view.channel_names.contains_key(&scalar_channel_id()));
        assert!(view.channel_names.contains_key(&vector_channel_id()));
        assert!(view.vector_channels.is_empty());
        assert!(view.probe_readings.is_empty());
    }

    /// Regression for the `field_systems()` per-frame-clone fix: reusing
    /// `field_systems`/`fields`/`mutable_vector_channels` from `previous`
    /// whenever `snapshot_sequence` is unchanged is only correct if every
    /// realtime flip actually publishes a new snapshot — not just the
    /// mid-gesture catch-up path `set_field_system_realtime` used to gate
    /// it behind. Before that companion fix, toggling realtime outside an
    /// edit changed `slot.realtime` without bumping `snapshot_sequence`, so
    /// this view would have shown the stale value from `previous` forever.
    #[test]
    fn toggling_realtime_outside_a_gesture_is_visible_on_the_next_frame() {
        let world = seeded_world();
        let mut source = source();
        let system = source.field_systems()[0].plugin.id.clone();
        let before = ComputeView::build(&source, &world.snapshot(), None);
        assert!(
            before.field_systems[0].realtime,
            "test assumes realtime starts on"
        );

        source
            .execute(fieldcad_simulation::CommandSequencer::default().issue(
                fieldcad_simulation::CommandPayload::SetFieldSystemRealtime {
                    plugin: system,
                    realtime: false,
                },
            ))
            .unwrap();

        let after = ComputeView::build(&source, &world.snapshot(), Some(&before));

        assert_ne!(
            after.snapshot_sequence, before.snapshot_sequence,
            "a realtime flip outside a gesture must still publish, or a cache keyed \
             on snapshot_sequence shows a stale field_systems() forever"
        );
        assert!(!after.field_systems[0].realtime);
    }

    #[test]
    fn workbench_state_distinguishes_paused_solving_stale_and_invalid() {
        let mut view = ComputeView::build(&source(), &seeded_world().snapshot(), None);
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

    #[test]
    fn queue_matches_summary_agrees_only_when_shape_and_newest_entry_match() {
        use fieldcad_simulation::{CommandId, CommandKind, CommandRecord};

        let cached = QueueStatus {
            paused: false,
            pending: vec![CommandRecord::submitted(
                CommandId::new(1),
                CommandKind::Play,
                1,
            )],
            history: vec![CommandRecord::submitted(
                CommandId::new(0),
                CommandKind::Pause,
                0,
            )],
        };
        let matching = QueueSummary {
            paused: false,
            pending_len: 1,
            history_len: 1,
            newest_history: Some(CommandId::new(0)),
        };
        assert!(queue_matches_summary(&cached, matching));

        assert!(!queue_matches_summary(
            &cached,
            QueueSummary {
                paused: true,
                ..matching
            }
        ));
        assert!(!queue_matches_summary(
            &cached,
            QueueSummary {
                pending_len: 2,
                ..matching
            }
        ));
        assert!(!queue_matches_summary(
            &cached,
            QueueSummary {
                history_len: 0,
                ..matching
            }
        ));
        assert!(!queue_matches_summary(
            &cached,
            QueueSummary {
                newest_history: Some(CommandId::new(7)),
                ..matching
            }
        ));
    }

    /// The regression this guards: [`ComputeView::build`] must not simply
    /// recompute its snapshot- and queue-derived fields identically every
    /// frame (that would make `previous` a no-op) nor hand back `previous`'s
    /// fields once they are actually out of date. Poisoning `previous` with
    /// values a real rebuild would never produce, then asserting they either
    /// survive or are discarded, tells the two cases apart in a way that
    /// comparing two honest rebuilds never could — those always agree.
    #[test]
    fn build_reuses_snapshot_derived_fields_until_the_snapshot_changes() {
        let world = seeded_world();
        let baseline = ComputeView::build(&source(), &world.snapshot(), None);

        let mut poisoned = baseline.clone();
        poisoned.domain_summary = "POISONED".to_owned();

        let source = source();
        let reused = ComputeView::build(&source, &world.snapshot(), Some(&poisoned));
        assert_eq!(
            reused.domain_summary, "POISONED",
            "unchanged snapshot sequence must reuse the cached snapshot-derived fields verbatim"
        );

        let mut source = source;
        let system = source.field_systems()[0].plugin.id.clone();
        source
            .execute(fieldcad_simulation::CommandSequencer::default().issue(
                fieldcad_simulation::CommandPayload::SetFieldSystemEnabled {
                    plugin: system,
                    enabled: false,
                },
            ))
            .unwrap();

        let rebuilt = ComputeView::build(&source, &world.snapshot(), Some(&poisoned));
        assert_eq!(
            rebuilt.domain_summary, baseline.domain_summary,
            "a new snapshot must discard the stale cache and recompute rather than propagate it"
        );
    }

    /// Mirrors the snapshot-sequence test above but for the queue, which is
    /// gated on [`FieldDataSource::queue_summary`] instead: an issued command
    /// changes the queue without necessarily publishing a new snapshot (see
    /// `SimulationRuntime::set_field_system_realtime`), so the two caches must
    /// invalidate independently.
    #[test]
    fn build_reuses_the_queue_until_it_changes_shape() {
        use fieldcad_simulation::{CommandKind, CommandPayload, CommandSequencer};

        let world = seeded_world();
        let mut source = source();
        // A `CommitWorld` only reaches `history` once flushed: submit it
        // while running (so it queues instead of applying immediately) and
        // then pause, which flushes the queue.
        let mut sequencer = CommandSequencer::default();
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();
        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(Vec::new())))
            .unwrap();
        source
            .execute(sequencer.issue(CommandPayload::Pause))
            .unwrap();
        let baseline = ComputeView::build(&source, &world.snapshot(), None);
        assert_eq!(
            baseline.queue.history.last().map(|record| record.kind),
            Some(CommandKind::CommitWorld),
            "test setup: expected the flushed `CommitWorld` command to land in history"
        );

        // `kind` plays no part in `queue_summary`/`queue_matches_summary`, so
        // it survives a correct reuse untouched — unlike `paused` or a
        // length, which the comparator would only ever agree on by
        // reflecting the real, current queue.
        let mut poisoned = baseline.clone();
        poisoned.queue.history.last_mut().unwrap().kind = CommandKind::Pause;

        // Nothing about the queue has changed since `baseline`, so the
        // poisoned `kind` must come back verbatim.
        let reused = ComputeView::build(&source, &world.snapshot(), Some(&poisoned));
        assert_eq!(
            reused.queue.history.last().map(|record| record.kind),
            Some(CommandKind::Pause),
            "an unchanged queue summary must reuse the cached queue verbatim"
        );

        // Pausing the queue changes `queue_summary().paused`, which must be
        // caught independently of the snapshot sequence (nothing here
        // publishes a new snapshot).
        source
            .execute(CommandSequencer::default().issue(CommandPayload::PauseQueue))
            .unwrap();

        let rebuilt = ComputeView::build(&source, &world.snapshot(), Some(&poisoned));
        assert_ne!(
            rebuilt.queue.history.last().map(|record| record.kind),
            Some(CommandKind::Pause),
            "a changed queue summary must discard the stale cache and read the real queue"
        );
    }
}
