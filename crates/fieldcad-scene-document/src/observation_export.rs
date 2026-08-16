//! The `fieldcad.observation-export/v1` file format: a portable, versioned
//! subset of a session's retained observations — one or more probe/channel
//! series, distance-probe series, mass-aggregate-probe series, and
//! optionally the current field snapshot — independent of a whole scene
//! document. Distinct from [`crate::SceneDocument`] the same way
//! [`crate::recording_file`] is: this describes a scoped slice of what a
//! session *observed*, not the authored world/domain/field-system state that
//! produced it, so sharing "here's what this run observed" never drags along
//! the rest of the scene. See `docs/tasks/observation-export.md`.
//!
//! Simple, single-write/single-read file I/O like
//! [`crate::save_recording_to_path`]/[`crate::load_recording_from_path`] —
//! not [`crate::save_to_path`]/[`crate::load_newest_valid`]'s
//! atomic-write-with-backup discipline, for the same reason: an export is
//! written once and read once, never repeatedly resaved over the same path.

use std::{fs, io, path::Path};

use fieldcad_core::FieldSnapshot;
use serde::{Deserialize, Serialize};

use crate::{DistanceHistoryState, MassAggregateHistoryState, ProbeHistoryState, rfc3339_now};

pub const EXPORT_FORMAT_ID: &str = "fieldcad.observation-export/v1";
pub const EXPORT_FORMAT_VERSION: u32 = 1;
/// File extension for a saved observation export (without the leading dot).
pub const EXPORT_EXTENSION: &str = "fcobservation";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportMetadata {
    /// Generator identity/version string (e.g. `"fieldcad-server/0.1.0"`),
    /// for support/debugging — not parsed on import.
    pub generated_by: String,
    /// RFC 3339, stamped once when the export is captured.
    pub exported_at: String,
}

/// A scoped, self-contained snapshot of retained observations.
///
/// Every field here is optional in the sense that an empty/absent value
/// simply means that part of the scope was never requested — see
/// [`Self::capture`]'s caller ([`fieldcad_server::HeadlessServer::export_observations`])
/// for how a scope selects what actually ends up populated.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservationExport {
    /// Must equal [`EXPORT_FORMAT_ID`].
    pub format: String,
    /// Loader rejects `format_version > `[`EXPORT_FORMAT_VERSION`] outright.
    pub format_version: u32,
    pub metadata: ExportMetadata,
    pub probe_history: ProbeHistoryState,
    pub distance_history: DistanceHistoryState,
    pub mass_aggregate_history: MassAggregateHistoryState,
    /// The current field snapshot, if the scope asked for one — see
    /// AGENTS.md's "publish immutable, versioned observations with validity
    /// and provenance" boundary: this is the raw published snapshot,
    /// unmodified, so its own validity/provenance travels with it exactly
    /// as a live reader would have seen it.
    pub snapshot: Option<FieldSnapshot>,
}

impl ObservationExport {
    /// Assemble an export from an already-scoped selection. Pure — no I/O.
    pub fn capture(
        generated_by: &str,
        probe_history: ProbeHistoryState,
        distance_history: DistanceHistoryState,
        mass_aggregate_history: MassAggregateHistoryState,
        snapshot: Option<FieldSnapshot>,
    ) -> Self {
        Self {
            format: EXPORT_FORMAT_ID.to_owned(),
            format_version: EXPORT_FORMAT_VERSION,
            metadata: ExportMetadata {
                generated_by: generated_by.to_owned(),
                exported_at: rfc3339_now(),
            },
            probe_history,
            distance_history,
            mass_aggregate_history,
            snapshot,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ObservationExportSaveError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ObservationExportLoadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error("expected format '{EXPORT_FORMAT_ID}', found '{found}'")]
    WrongFormat { found: String },
    #[error(
        "observation export format version {found} is newer than the {max_supported} this build supports"
    )]
    UnsupportedVersion { found: u32, max_supported: u32 },
}

pub fn save_observation_export_to_path(
    export: &ObservationExport,
    path: &Path,
) -> Result<(), ObservationExportSaveError> {
    let bytes = serde_json::to_vec_pretty(export)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn load_observation_export_from_path(
    path: &Path,
) -> Result<ObservationExport, ObservationExportLoadError> {
    let bytes = fs::read(path)?;
    let export: ObservationExport = serde_json::from_slice(&bytes)?;
    if export.format != EXPORT_FORMAT_ID {
        return Err(ObservationExportLoadError::WrongFormat {
            found: export.format,
        });
    }
    if export.format_version > EXPORT_FORMAT_VERSION {
        return Err(ObservationExportLoadError::UnsupportedVersion {
            found: export.format_version,
            max_supported: EXPORT_FORMAT_VERSION,
        });
    }
    Ok(export)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{ChannelId, Dimension, PluginId, ProbeId, Quantity, SampleValidity};
    use fieldcad_core::{FieldValue, WorldRevision};

    fn channel_id() -> ChannelId {
        ChannelId::new(PluginId::new("test").unwrap(), "scalar").unwrap()
    }

    fn sample_probe_history() -> ProbeHistoryState {
        ProbeHistoryState {
            series: vec![crate::ProbeSeriesRecord {
                probe: ProbeId::new(0),
                channel: channel_id(),
                readings: vec![crate::ProbeReadingRecord {
                    tick: 1,
                    time_seconds: 0.5,
                    world_revision: WorldRevision::INITIAL,
                    snapshot_sequence: 1,
                    value: FieldValue::Scalar(Quantity::new(3.0, Dimension::MASS).unwrap()),
                    validity: SampleValidity::Exact,
                }],
            }],
        }
    }

    #[test]
    fn a_saved_export_round_trips_through_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.fcobservation");
        let export = ObservationExport::capture(
            "test/0.0.0",
            sample_probe_history(),
            DistanceHistoryState::default(),
            MassAggregateHistoryState::default(),
            None,
        );

        save_observation_export_to_path(&export, &path).unwrap();
        let restored = load_observation_export_from_path(&path).unwrap();

        assert_eq!(restored.probe_history, export.probe_history);
        assert_eq!(restored.distance_history, export.distance_history);
        assert_eq!(
            restored.mass_aggregate_history,
            export.mass_aggregate_history
        );
        assert!(restored.snapshot.is_none() && export.snapshot.is_none());
        assert_eq!(restored.metadata.generated_by, export.metadata.generated_by);
    }

    #[test]
    fn a_file_with_an_unsupported_format_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.fcobservation");
        let mut export = ObservationExport::capture(
            "test/0.0.0",
            ProbeHistoryState::default(),
            DistanceHistoryState::default(),
            MassAggregateHistoryState::default(),
            None,
        );
        export.format_version = EXPORT_FORMAT_VERSION + 1;
        fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();

        let error = load_observation_export_from_path(&path).unwrap_err();

        assert!(matches!(
            error,
            ObservationExportLoadError::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn a_file_with_the_wrong_format_id_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.fcobservation");
        let mut export = ObservationExport::capture(
            "test/0.0.0",
            ProbeHistoryState::default(),
            DistanceHistoryState::default(),
            MassAggregateHistoryState::default(),
            None,
        );
        export.format = "something-else".to_owned();
        fs::write(&path, serde_json::to_vec(&export).unwrap()).unwrap();

        let error = load_observation_export_from_path(&path).unwrap_err();

        assert!(matches!(
            error,
            ObservationExportLoadError::WrongFormat { .. }
        ));
    }
}
