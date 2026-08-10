//! Saved probe/distance-probe recording history: plain, JSON-serializable
//! mirrors of `fieldcad_simulation::{ProbeHistory, DistanceHistory}`'s
//! internal ring buffers (see `view.rs` for why this crate keeps its own
//! mirror types rather than deriving `Serialize` on the live ones directly —
//! here that's doubly true, since a `BTreeMap` keyed by `(ProbeId,
//! ChannelId)` cannot round-trip through JSON, whose object keys must be
//! strings; each series is a flat `Vec` entry instead of a map).

use fieldcad_core::{
    ChannelId, DistanceProbeId, FieldValue, ProbeId, SampleValidity, WorldRevision,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeReadingRecord {
    pub tick: u64,
    pub time_seconds: f64,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: u64,
    pub value: FieldValue,
    pub validity: SampleValidity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeSeriesRecord {
    pub probe: ProbeId,
    pub channel: ChannelId,
    pub readings: Vec<ProbeReadingRecord>,
}

/// A saved probe recording: every probe/channel series with at least one
/// reading, at save time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeHistoryState {
    pub series: Vec<ProbeSeriesRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceReadingRecord {
    pub tick: u64,
    pub time_seconds: f64,
    pub world_revision: WorldRevision,
    pub snapshot_sequence: u64,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceSeriesRecord {
    pub probe: DistanceProbeId,
    pub readings: Vec<DistanceReadingRecord>,
}

/// A saved distance-probe recording — see [`ProbeHistoryState`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DistanceHistoryState {
    pub series: Vec<DistanceSeriesRecord>,
}
