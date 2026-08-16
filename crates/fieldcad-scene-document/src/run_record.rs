//! Named, retained run records and their comparison.
//!
//! A run record is a user-named snapshot of "this numerical run": the
//! `run_generation` and configuration that produced it
//! ([`crate::FieldSystemComposition`], `Domain`, `TimeStep`) plus a copy of
//! the observation histories it produced. Re-running (changing a parameter
//! and letting the domain/field-system reconfigure, which bumps
//! `run_generation`) no longer overwrites what a modeller wants to keep —
//! saving a run under a name retains it independently, and two retained runs
//! can be compared. See `docs/tasks/run-records-and-comparison.md`.

use std::collections::BTreeSet;

use fieldcad_core::{ChannelId, Domain, PluginId, PropertyBag, TimeStep};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    DistanceHistoryState, DistanceReadingRecord, DistanceSeriesRecord, FieldSystemComposition,
    MassAggregateHistoryState, MassAggregateReadingRecord, MassAggregateSeriesRecord,
    ProbeHistoryState, ProbeReadingRecord, ProbeSeriesRecord, rfc3339_now,
};

/// A user-named, retained snapshot of one numerical run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Opaque stable identity — never reused, so a comparison or delete
    /// request always names exactly one record even after others are added
    /// or removed.
    pub id: Uuid,
    /// User-chosen label. Not required to be unique: two runs named the
    /// same thing are still two distinct records by `id`.
    pub name: String,
    /// RFC 3339, set once when the run is named.
    pub created_at: String,
    pub run_generation: u64,
    pub domain: Domain,
    pub time_step: TimeStep,
    pub field_systems: Vec<FieldSystemComposition>,
    pub probe_history: ProbeHistoryState,
    pub distance_history: DistanceHistoryState,
    pub mass_aggregate_history: MassAggregateHistoryState,
}

impl RunRecord {
    /// Name the current run. `created_at` is stamped as "now".
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        name: String,
        run_generation: u64,
        domain: Domain,
        time_step: TimeStep,
        field_systems: Vec<FieldSystemComposition>,
        probe_history: ProbeHistoryState,
        distance_history: DistanceHistoryState,
        mass_aggregate_history: MassAggregateHistoryState,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            created_at: rfc3339_now(),
            run_generation,
            domain,
            time_step,
            field_systems,
            probe_history,
            distance_history,
            mass_aggregate_history,
        }
    }

    pub fn summary(&self) -> RunRecordSummary {
        RunRecordSummary {
            id: self.id,
            name: self.name.clone(),
            created_at: self.created_at.clone(),
            run_generation: self.run_generation,
        }
    }
}

/// A run record's identity and label, without its configuration or
/// observation payload — what a "list retained runs" read returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecordSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub run_generation: u64,
}

/// One plugin's configuration differing (or being present in only one run)
/// between two compared run records. Only plugins that actually differ are
/// reported — a caller diffing two runs wants what changed, not a full
/// listing of every field system both share unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationDifference {
    pub plugin: PluginId,
    pub in_a: bool,
    pub in_b: bool,
    pub enabled_a: Option<bool>,
    pub enabled_b: Option<bool>,
    pub realtime_a: Option<bool>,
    pub realtime_b: Option<bool>,
    pub configuration_a: Option<PropertyBag>,
    pub configuration_b: Option<PropertyBag>,
}

/// One probe/channel series placed alongside its counterpart from the other
/// run, if any — empty on whichever side never recorded that probe/channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeSeriesComparison {
    pub probe: fieldcad_core::ProbeId,
    pub channel: ChannelId,
    pub a: Vec<ProbeReadingRecord>,
    pub b: Vec<ProbeReadingRecord>,
}

/// See [`ProbeSeriesComparison`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceSeriesComparison {
    pub probe: fieldcad_core::DistanceProbeId,
    pub a: Vec<DistanceReadingRecord>,
    pub b: Vec<DistanceReadingRecord>,
}

/// See [`ProbeSeriesComparison`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MassAggregateSeriesComparison {
    pub probe: fieldcad_core::MassAggregateProbeId,
    pub a: Vec<MassAggregateReadingRecord>,
    pub b: Vec<MassAggregateReadingRecord>,
}

/// The result of comparing two retained run records: what configuration
/// differs, and every observed series from either run placed side by side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunComparison {
    pub a: RunRecordSummary,
    pub b: RunRecordSummary,
    pub domain_changed: bool,
    pub time_step_changed: bool,
    pub configuration_differences: Vec<ConfigurationDifference>,
    pub probe_series: Vec<ProbeSeriesComparison>,
    pub distance_series: Vec<DistanceSeriesComparison>,
    pub mass_aggregate_series: Vec<MassAggregateSeriesComparison>,
}

/// Compare two run records: pure, no session dependency, so a caller (an
/// MCP tool, a desktop panel) can build a [`RunComparison`] from any two
/// records it already has in hand.
pub fn compare_run_records(a: &RunRecord, b: &RunRecord) -> RunComparison {
    let mut plugins: BTreeSet<&PluginId> = BTreeSet::new();
    plugins.extend(a.field_systems.iter().map(|entry| &entry.plugin));
    plugins.extend(b.field_systems.iter().map(|entry| &entry.plugin));

    let configuration_differences = plugins
        .into_iter()
        .filter_map(|plugin| {
            let in_a = a.field_systems.iter().find(|entry| &entry.plugin == plugin);
            let in_b = b.field_systems.iter().find(|entry| &entry.plugin == plugin);
            let differs = match (in_a, in_b) {
                (Some(x), Some(y)) => {
                    x.version != y.version
                        || x.enabled != y.enabled
                        || x.realtime != y.realtime
                        || x.configuration != y.configuration
                }
                _ => true,
            };
            differs.then(|| ConfigurationDifference {
                plugin: plugin.clone(),
                in_a: in_a.is_some(),
                in_b: in_b.is_some(),
                enabled_a: in_a.map(|entry| entry.enabled),
                enabled_b: in_b.map(|entry| entry.enabled),
                realtime_a: in_a.map(|entry| entry.realtime),
                realtime_b: in_b.map(|entry| entry.realtime),
                configuration_a: in_a.map(|entry| entry.configuration.clone()),
                configuration_b: in_b.map(|entry| entry.configuration.clone()),
            })
        })
        .collect();

    let mut probe_keys: BTreeSet<(fieldcad_core::ProbeId, ChannelId)> = BTreeSet::new();
    probe_keys.extend(
        a.probe_history
            .series
            .iter()
            .map(|series| (series.probe, series.channel.clone())),
    );
    probe_keys.extend(
        b.probe_history
            .series
            .iter()
            .map(|series| (series.probe, series.channel.clone())),
    );
    let probe_series = probe_keys
        .into_iter()
        .map(|(probe, channel)| ProbeSeriesComparison {
            probe,
            channel: channel.clone(),
            a: series_readings(&a.probe_history.series, probe, &channel),
            b: series_readings(&b.probe_history.series, probe, &channel),
        })
        .collect();

    let mut distance_probes: BTreeSet<fieldcad_core::DistanceProbeId> = BTreeSet::new();
    distance_probes.extend(a.distance_history.series.iter().map(|series| series.probe));
    distance_probes.extend(b.distance_history.series.iter().map(|series| series.probe));
    let distance_series = distance_probes
        .into_iter()
        .map(|probe| DistanceSeriesComparison {
            probe,
            a: distance_readings(&a.distance_history.series, probe),
            b: distance_readings(&b.distance_history.series, probe),
        })
        .collect();

    let mut mass_aggregate_probes: BTreeSet<fieldcad_core::MassAggregateProbeId> = BTreeSet::new();
    mass_aggregate_probes.extend(
        a.mass_aggregate_history
            .series
            .iter()
            .map(|series| series.probe),
    );
    mass_aggregate_probes.extend(
        b.mass_aggregate_history
            .series
            .iter()
            .map(|series| series.probe),
    );
    let mass_aggregate_series = mass_aggregate_probes
        .into_iter()
        .map(|probe| MassAggregateSeriesComparison {
            probe,
            a: mass_aggregate_readings(&a.mass_aggregate_history.series, probe),
            b: mass_aggregate_readings(&b.mass_aggregate_history.series, probe),
        })
        .collect();

    RunComparison {
        a: a.summary(),
        b: b.summary(),
        domain_changed: a.domain != b.domain,
        time_step_changed: a.time_step != b.time_step,
        configuration_differences,
        probe_series,
        distance_series,
        mass_aggregate_series,
    }
}

fn series_readings(
    series: &[ProbeSeriesRecord],
    probe: fieldcad_core::ProbeId,
    channel: &ChannelId,
) -> Vec<ProbeReadingRecord> {
    series
        .iter()
        .find(|entry| entry.probe == probe && &entry.channel == channel)
        .map(|entry| entry.readings.clone())
        .unwrap_or_default()
}

fn distance_readings(
    series: &[DistanceSeriesRecord],
    probe: fieldcad_core::DistanceProbeId,
) -> Vec<DistanceReadingRecord> {
    series
        .iter()
        .find(|entry| entry.probe == probe)
        .map(|entry| entry.readings.clone())
        .unwrap_or_default()
}

fn mass_aggregate_readings(
    series: &[MassAggregateSeriesRecord],
    probe: fieldcad_core::MassAggregateProbeId,
) -> Vec<MassAggregateReadingRecord> {
    series
        .iter()
        .find(|entry| entry.probe == probe)
        .map(|entry| entry.readings.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{Dimension, PluginVersion, Quantity};

    fn field_system(plugin: &str, gain: f64) -> FieldSystemComposition {
        FieldSystemComposition {
            plugin: PluginId::new(plugin).unwrap(),
            version: PluginVersion::new(0, 1, 0),
            enabled: true,
            realtime: true,
            configuration: PropertyBag::from_iter([(
                fieldcad_core::PropertyId::new("gain").unwrap(),
                fieldcad_core::PropertyValue::Scalar(
                    Quantity::new(gain, Dimension::DIMENSIONLESS).unwrap(),
                ),
            )]),
        }
    }

    fn record(name: &str, generation: u64, gain: f64) -> RunRecord {
        RunRecord::capture(
            name.to_owned(),
            generation,
            Domain::centred_cube(2.0, 4).unwrap(),
            TimeStep::from_seconds(0.1).unwrap(),
            vec![field_system("gain-test", gain)],
            ProbeHistoryState::default(),
            DistanceHistoryState::default(),
            MassAggregateHistoryState::default(),
        )
    }

    #[test]
    fn comparing_two_runs_with_a_different_gain_surfaces_the_difference() {
        let a = record("baseline", 0, 1.0);
        let b = record("doubled-gain", 1, 2.0);

        let comparison = compare_run_records(&a, &b);

        assert_eq!(comparison.configuration_differences.len(), 1);
        let diff = &comparison.configuration_differences[0];
        assert_eq!(diff.plugin, PluginId::new("gain-test").unwrap());
        assert_ne!(diff.configuration_a, diff.configuration_b);
    }

    #[test]
    fn comparing_two_runs_with_identical_configuration_reports_no_differences() {
        let a = record("run-a", 0, 1.0);
        let b = record("run-b", 1, 1.0);

        let comparison = compare_run_records(&a, &b);

        assert!(comparison.configuration_differences.is_empty());
    }

    #[test]
    fn a_plugin_only_present_in_one_run_is_reported_as_a_difference() {
        let a = record("run-a", 0, 1.0);
        let mut b = record("run-b", 1, 1.0);
        b.field_systems
            .push(field_system("extra-plugin", 1.0));

        let comparison = compare_run_records(&a, &b);

        let extra = comparison
            .configuration_differences
            .iter()
            .find(|diff| diff.plugin == PluginId::new("extra-plugin").unwrap())
            .expect("extra plugin reported as a difference");
        assert!(!extra.in_a);
        assert!(extra.in_b);
    }

    #[test]
    fn round_trips_through_json() {
        let a = record("run-a", 0, 1.0);
        let json = serde_json::to_vec(&a).unwrap();
        let restored: RunRecord = serde_json::from_slice(&json).unwrap();
        assert_eq!(a, restored);
    }
}
