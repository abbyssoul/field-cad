use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    ChannelId, ChannelSchema, Domain, FieldBatch, PluginId, PluginVersion, ProbeId, Sample,
    WorldRevision,
};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SessionId(pub [u8; 16]);

impl SessionId {
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotCompleteness {
    Complete,
    /// Some requested data is missing. A partial snapshot may be shown as
    /// progress, but it must never replace the last complete result.
    Partial,
}

/// Everything needed to say which computation produced a value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotIdentity {
    pub session: SessionId,
    pub sequence: u64,
    pub world_revision: WorldRevision,
    pub tick: u64,
    pub time_seconds: f64,
}

impl SnapshotIdentity {
    pub fn freshness_against(self, revision: WorldRevision) -> SnapshotFreshness {
        match self.world_revision.cmp(&revision) {
            std::cmp::Ordering::Less => SnapshotFreshness::Stale,
            std::cmp::Ordering::Equal => SnapshotFreshness::Current,
            std::cmp::Ordering::Greater => SnapshotFreshness::Future,
        }
    }

    /// Whether two snapshots describe the same computation. Chunks from
    /// different identities must never be combined.
    pub fn same_result_as(self, other: Self) -> bool {
        self.session == other.session && self.sequence == other.sequence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotFreshness {
    Current,
    Stale,
    Future,
}

impl SnapshotFreshness {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Current => "Current",
            Self::Stale => "Stale",
            Self::Future => "Future revision",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginProvenance {
    pub id: PluginId,
    pub version: PluginVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolverDiagnostic {
    pub plugin: PluginId,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

/// One channel's published data: its schema, and the batches produced for it.
///
/// A channel may be published over several geometries at once — probe points, a
/// slice plane, a decimated whole-domain grid — so batches is a list rather than
/// a single payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    pub schema: Arc<ChannelSchema>,
    pub batches: Arc<[FieldBatch]>,
}

impl ChannelSnapshot {
    pub fn probe_sample(&self, probe: ProbeId) -> Option<Sample> {
        let dimension = self.schema.dimension();
        self.batches.iter().find_map(|batch| {
            let index = batch.geometry().probe_index(probe)?;
            batch.sample(index, dimension)
        })
    }

    pub fn sample_count(&self) -> usize {
        self.batches.iter().map(FieldBatch::len).sum()
    }
}

/// Immutable solver output for one simulation time and world revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldSnapshot {
    pub identity: SnapshotIdentity,
    pub completeness: SnapshotCompleteness,
    /// The numerical region and configuration this result was computed over.
    pub domain: Domain,
    pub plugins: Arc<[PluginProvenance]>,
    pub channels: BTreeMap<ChannelId, ChannelSnapshot>,
    pub diagnostics: Arc<[SolverDiagnostic]>,
}

impl FieldSnapshot {
    pub fn channel(&self, id: &ChannelId) -> Option<&ChannelSnapshot> {
        self.channels.get(id)
    }

    pub fn probe_sample(&self, channel: &ChannelId, probe: ProbeId) -> Option<Sample> {
        self.channel(channel)?.probe_sample(probe)
    }

    pub fn freshness_against(&self, revision: WorldRevision) -> SnapshotFreshness {
        self.identity.freshness_against(revision)
    }

    pub const fn is_complete(&self) -> bool {
        matches!(self.completeness, SnapshotCompleteness::Complete)
    }

    /// The published channels whose values are vectors, in channel-ID order.
    ///
    /// A generic glyph or streamline layer needs to know what it *can* draw
    /// without naming an equation system. Declared channel schemas already say
    /// so; asking the snapshot is what keeps the renderer independent of which
    /// plugins are loaded.
    pub fn vector_channels(&self) -> impl Iterator<Item = &ChannelSnapshot> {
        self.channels
            .values()
            .filter(|channel| matches!(channel.schema.value_kind, crate::FieldValueKind::Vector(_)))
    }

    pub fn total_samples(&self) -> usize {
        self.channels
            .values()
            .map(ChannelSnapshot::sample_count)
            .sum()
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::{Dimension, FieldColumn, FieldValueKind, SampleGeometry, SampleValidity};

    fn identity(sequence: u64, revision: WorldRevision) -> SnapshotIdentity {
        SnapshotIdentity {
            session: SessionId::from_u128(1),
            sequence,
            world_revision: revision,
            tick: 2,
            time_seconds: 0.2,
        }
    }

    #[test]
    fn revision_freshness_is_explicit() {
        let identity = identity(3, WorldRevision::INITIAL);

        assert_eq!(
            identity.freshness_against(WorldRevision::INITIAL.next()),
            SnapshotFreshness::Stale
        );
        assert_eq!(
            identity.freshness_against(WorldRevision::INITIAL),
            SnapshotFreshness::Current
        );
    }

    #[test]
    fn snapshots_from_different_sequences_are_different_results() {
        assert!(
            !identity(1, WorldRevision::INITIAL)
                .same_result_as(identity(2, WorldRevision::INITIAL))
        );
        assert!(
            identity(1, WorldRevision::INITIAL).same_result_as(identity(1, WorldRevision::INITIAL))
        );
    }

    #[test]
    fn probe_samples_are_found_across_batches() {
        let schema = Arc::new(ChannelSchema {
            id: ChannelId::new(PluginId::new("test").unwrap(), "potential").unwrap(),
            display_name: "Potential".to_owned(),
            value_kind: FieldValueKind::Scalar(Dimension::ELECTRIC_POTENTIAL),
        });
        let batch = FieldBatch::new(
            SampleGeometry::probes(vec![ProbeId::new(4)], vec![DVec3::X]).unwrap(),
            FieldColumn::scalars(vec![12.0]),
            vec![SampleValidity::Exact],
        )
        .unwrap();
        let channel = ChannelSnapshot {
            schema,
            batches: Arc::from([batch]),
        };

        let sample = channel.probe_sample(ProbeId::new(4)).unwrap();
        assert_eq!(sample.value.magnitude(), 12.0);
        assert_eq!(sample.value.dimension(), Dimension::ELECTRIC_POTENTIAL);
        assert!(channel.probe_sample(ProbeId::new(5)).is_none());
    }
}
