//! The `fieldcad.recording/v1` file format: a saved
//! [`fieldcad_simulation::recording::SessionRecording`] — a command/wall-
//! clock-poll event log, not authored world/domain state — so a session
//! recorded in one process can be replayed in a later one. Deliberately its
//! own small format rather than a field on [`crate::SceneDocument`]: a
//! recording describes what was *done* to a session, independent of which
//! scene it started from, and a modeller sharing "here's what I did" should
//! not have to also share the whole authored world.
//!
//! Simpler than [`crate::save_to_path`]/[`crate::load_newest_valid`]'s
//! atomic-write-with-backup discipline on purpose: a recording is written
//! once, when [`fieldcad_server::HeadlessServer::stop_recording`] returns,
//! and read once, before a replay — not repeatedly resaved over the same
//! path the way a scene document is, so there is no in-place-overwrite
//! failure window worth guarding with a temp-file-plus-rename dance.

use std::{fs, io, path::Path};

use fieldcad_simulation::recording::SessionRecording;
use serde::{Deserialize, Serialize};

pub const RECORDING_FORMAT_ID: &str = "fieldcad.recording/v1";
pub const RECORDING_FORMAT_VERSION: u32 = 1;
/// File extension for a saved recording (without the leading dot).
pub const RECORDING_EXTENSION: &str = "fcrecording";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordingDocument {
    format: String,
    format_version: u32,
    recording: SessionRecording,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingSaveError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingLoadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error("expected format '{RECORDING_FORMAT_ID}', found '{found}'")]
    WrongFormat { found: String },
    #[error(
        "recording format version {found} is newer than the {max_supported} this build supports"
    )]
    UnsupportedVersion { found: u32, max_supported: u32 },
}

pub fn save_recording_to_path(
    recording: &SessionRecording,
    path: &Path,
) -> Result<(), RecordingSaveError> {
    let document = RecordingDocument {
        format: RECORDING_FORMAT_ID.to_owned(),
        format_version: RECORDING_FORMAT_VERSION,
        recording: recording.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&document)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn load_recording_from_path(path: &Path) -> Result<SessionRecording, RecordingLoadError> {
    let bytes = fs::read(path)?;
    let document: RecordingDocument = serde_json::from_slice(&bytes)?;
    if document.format != RECORDING_FORMAT_ID {
        return Err(RecordingLoadError::WrongFormat {
            found: document.format,
        });
    }
    if document.format_version > RECORDING_FORMAT_VERSION {
        return Err(RecordingLoadError::UnsupportedVersion {
            found: document.format_version,
            max_supported: RECORDING_FORMAT_VERSION,
        });
    }
    Ok(document.recording)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_simulation::CommandPayload;
    use std::time::Duration;

    #[test]
    fn a_saved_recording_round_trips_through_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.fcrecording");
        let recording = SessionRecording::new()
            .with_command(CommandPayload::Play)
            .with_poll(Duration::from_millis(16))
            .with_command(CommandPayload::Pause);

        save_recording_to_path(&recording, &path).unwrap();
        let restored = load_recording_from_path(&path).unwrap();

        assert_eq!(restored, recording);
    }

    #[test]
    fn a_file_with_an_unsupported_format_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.fcrecording");
        let document = RecordingDocument {
            format: RECORDING_FORMAT_ID.to_owned(),
            format_version: RECORDING_FORMAT_VERSION + 1,
            recording: SessionRecording::new(),
        };
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        let error = load_recording_from_path(&path).unwrap_err();

        assert!(matches!(
            error,
            RecordingLoadError::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn a_file_with_the_wrong_format_id_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-recording.fcrecording");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"format": "something-else"})).unwrap(),
        )
        .unwrap();

        let error = load_recording_from_path(&path).unwrap_err();

        assert!(matches!(error, RecordingLoadError::Decode(_)));
    }
}
