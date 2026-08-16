//! The `fieldcad.scene/v1` document format.
//!
//! A scene document is the round-trip file Field CAD opens and saves: durable
//! authored state (world, domain/time-step, field-system composition, the
//! paused command queue) with no transient solver memory, GPU resource, or
//! client-local presentation state. UI, MCP, and any future file codec are
//! adapters over this one declared type — none of them get a parallel
//! validation path (see `docs/tasks/product-capability-gaps.md`).
//!
//! This crate never constructs a plugin and never adopts a document into a
//! running session by itself: [`resolve_plugins`] only merges a document's
//! declared composition against a host-supplied catalog, and the caller
//! (desktop or MCP) is responsible for handing the result to
//! [`fieldcad_simulation::RuntimeConfig`]/`SimulationRuntime::new`, which is
//! where ADR-0007 validation actually happens.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use fieldcad_core::{
    Domain, PluginId, PluginVersion, PropertyBag, SceneScale, TimeStep, WorldDocument,
};
use fieldcad_dynamics::IntegrationScheme;
use fieldcad_simulation::{FieldSystemStatus, PlaybackSpeed, PluginRegistration, QueueDocument};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod history;
mod observation_export;
mod recording_file;
mod run_record;
mod view;
pub use history::{
    DistanceHistoryState, DistanceReadingRecord, DistanceSeriesRecord, MassAggregateHistoryState,
    MassAggregateReadingRecord, MassAggregateSeriesRecord, ProbeHistoryState, ProbeReadingRecord,
    ProbeSeriesRecord,
};
pub use observation_export::{
    EXPORT_EXTENSION, EXPORT_FORMAT_ID, EXPORT_FORMAT_VERSION, ExportMetadata, ObservationExport,
    ObservationExportLoadError, ObservationExportSaveError, load_observation_export_from_path,
    save_observation_export_to_path,
};
pub use recording_file::{
    RECORDING_EXTENSION, RECORDING_FORMAT_ID, RECORDING_FORMAT_VERSION, RecordingLoadError,
    RecordingSaveError, load_recording_from_path, save_recording_to_path,
};
pub use run_record::{
    ConfigurationDifference, DistanceSeriesComparison, MassAggregateSeriesComparison,
    ProbeSeriesComparison, RunComparison, RunRecord, RunRecordSummary, compare_run_records,
};
pub use view::{
    CameraProjection, CameraState, ChannelViewState, FieldLayerViewState, FlowLineDisplayState,
    GizmoDisplayState, PlaneVectorModeState, PlaneViewState, RegionViewState, SceneViewState,
    TrajectoryDisplayState, VectorDisplayState, ViewOptionsState,
};

/// Identifies this document format. Anything else in a candidate file is
/// rejected before any other field is even interpreted (US-02: incompatibility
/// reported, never silently adapted).
pub const FORMAT_ID: &str = "fieldcad.scene/v1";
/// The highest `format_version` this build can load. A document reporting a
/// higher version is rejected outright rather than partially interpreted.
/// Bumped 1 → 2 when `SceneDocument::view` was added, 2 → 3 when
/// `playback_speed`/`probe_history`/`distance_history` were added, 3 → 4
/// when `mass_aggregate_history` was added, 4 → 5 when
/// `document_entries`/`quick_add_hidden` were added, 5 → 6 when entries
/// and preferences became source-qualified, 6 → 7 when `run_records` was
/// added, 7 → 8 when authored expressions and constants were added, and 8 → 9
/// when queued world/expression edits gained one atomic scene envelope. Each field's own file
/// still loads fine on an older-format read (all `#[serde(default)]`), but
/// a build that only knows the prior version must refuse a newer file
/// outright rather than silently dropping that content on the next resave.
pub const FORMAT_VERSION: u32 = 9;
/// File extension for a saved scene document (without the leading dot).
pub const EXTENSION: &str = "fcscene";

/// A complete, versioned Field CAD experiment document.
///
/// Deliberately excludes [`fieldcad_core::SessionId`] (names a live runtime,
/// minted fresh whenever a document is loaded, not a saved artifact) and
/// simulation run/pause mode (a loaded document always starts paused
/// regardless of whether the session was running when saved — see the
/// scene-lifecycle plan's rationale: a user must never open a file and have
/// it immediately start consuming machine resources).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneDocument {
    /// Must equal [`FORMAT_ID`].
    pub format: String,
    /// Loader rejects `format_version > `[`FORMAT_VERSION`] outright.
    pub format_version: u32,
    pub metadata: SceneMetadata,
    pub domain: Domain,
    pub time_step: TimeStep,
    /// Wall-clock playback rate. `#[serde(default)]` so a document saved
    /// before this field existed still loads, simply at the default 1×
    /// rather than whatever rate the session happened to be running at when
    /// saved.
    #[serde(default)]
    pub playback_speed: PlaybackSpeed,
    pub scene_scale: SceneScale,
    pub integration_scheme: IntegrationScheme,
    pub field_systems: Vec<FieldSystemComposition>,
    /// Opaque to this crate — see [`fieldcad_core::WorldDocument`].
    pub world: WorldDocument,
    /// Authored constants and property formulas. Resolved finite SI values
    /// remain in `world`; transient compiled graphs are rebuilt after load.
    #[serde(default)]
    pub expressions: fieldcad_expressions::ExpressionDocument,
    /// The paused-queue write-ahead log: world/domain edits accepted but not
    /// yet applied at save time. A user who paused the command queue mid-edit
    /// and saved must not lose that work.
    pub queue: QueueDocument,
    /// Camera framing, follow target, view toggles, and per-channel display
    /// settings — see [`SceneViewState`]. `#[serde(default)]` so a document
    /// saved before this field existed (`format_version` 1) still loads,
    /// simply with nothing to restore.
    #[serde(default)]
    pub view: SceneViewState,
    /// Recorded field-probe history, so a session's plots survive a save/
    /// reload instead of starting empty until the simulation runs again.
    /// `#[serde(default)]` for the same reason as `view`.
    #[serde(default)]
    pub probe_history: ProbeHistoryState,
    /// Recorded distance-probe history — see `probe_history`.
    #[serde(default)]
    pub distance_history: DistanceHistoryState,
    /// Recorded center-of-mass-probe history — see `probe_history`.
    #[serde(default)]
    pub mass_aggregate_history: MassAggregateHistoryState,
    /// Document-scoped catalog entries: templates created in-app that
    /// belong to this scene rather than a disk-based catalog directory.
    #[serde(default)]
    pub document_entries: Vec<DocumentCatalogEntry>,
    /// Source-qualified entries hidden from the quick-add menu in this scene.
    /// A new scene starts with an empty (all-showing) list. References must
    /// remain source-qualified because two catalog scopes may use the same
    /// user-authored template name.
    #[serde(default)]
    pub quick_add_hidden: Vec<fieldcad_core::CatalogEntryRef>,
    /// Named, retained run records — see [`RunRecord`]. `#[serde(default)]`
    /// for the same reason as `view`.
    #[serde(default)]
    pub run_records: Vec<RunRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneMetadata {
    /// Generator identity/version string (e.g. `"fieldcad-desktop/0.1.0"`),
    /// for support/debugging — not parsed by the loader.
    pub generated_by: String,
    /// RFC 3339. Preserved across re-saves of the same document.
    pub created_at: String,
    /// RFC 3339. Updated on every save.
    pub saved_at: String,
}

/// A catalog entry that belongs to a specific scene document — created
/// in-app rather than loaded from a disk-based catalog directory.
///
/// Travels with the scene and participates in the same additive/conflict
/// behaviour as disk-loaded entries. Resolved against the live component-
/// schema registry on load, the same way a disk entry is re-resolved on
/// every [`crate::load_newest_valid`] + rebuild cycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentCatalogEntry {
    /// Opaque stable identity for references and links. Names stay editable
    /// labels and may collide after a direct document edit.
    #[serde(default = "Uuid::new_v4")]
    pub entry_id: Uuid,
    pub identity: fieldcad_catalog::TemplateIdentity,
    pub metadata: fieldcad_catalog::TemplateMetadata,
    pub spec: fieldcad_catalog::TemplateSpec,
}

/// One field system's composition and configuration, as declared by a
/// document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldSystemComposition {
    pub plugin: PluginId,
    pub version: PluginVersion,
    pub enabled: bool,
    pub realtime: bool,
    pub configuration: PropertyBag,
}

/// Everything [`SceneDocument::capture`] needs, gathered by the caller from
/// whatever accessor surface its host exposes.
///
/// A plain input struct rather than `capture` taking
/// `&SimulationRuntime` directly: the desktop and MCP hosts both reach their
/// session through an `AsyncLocalDataSource`/`HeadlessServer` boundary that
/// does not expose the runtime itself (by design, ADR 0001) — `world`/`queue`
/// in particular come from a dedicated capture round-trip
/// (`AsyncLocalDataSource::capture_document`), not from a live
/// `&SimulationRuntime`. A synchronous caller (this crate's own tests, a
/// headless `LocalDataSource`) assembles the same struct from
/// `SimulationRuntime`'s ordinary accessors.
#[derive(Clone, Debug)]
pub struct SceneDocumentInputs {
    pub domain: Domain,
    pub time_step: TimeStep,
    pub playback_speed: PlaybackSpeed,
    pub scene_scale: SceneScale,
    pub integration_scheme: IntegrationScheme,
    pub field_systems: Vec<FieldSystemStatus>,
    pub world: WorldDocument,
    pub expressions: fieldcad_expressions::ExpressionDocument,
    pub queue: QueueDocument,
    pub view: SceneViewState,
    pub probe_history: ProbeHistoryState,
    pub distance_history: DistanceHistoryState,
    pub mass_aggregate_history: MassAggregateHistoryState,
    pub document_entries: Vec<DocumentCatalogEntry>,
    pub quick_add_hidden: Vec<fieldcad_core::CatalogEntryRef>,
    pub run_records: Vec<RunRecord>,
}

impl SceneDocument {
    /// Assemble a document from a session's current state. Pure — no I/O.
    ///
    /// `created_at`: pass the prior document's `created_at` on a re-save (not
    /// "Save As") so it survives across saves; `None` for a brand-new
    /// document, in which case `created_at` and `saved_at` are both "now".
    pub fn capture(
        inputs: SceneDocumentInputs,
        generated_by: &str,
        created_at: Option<String>,
    ) -> Self {
        let now = rfc3339_now();
        Self {
            format: FORMAT_ID.to_owned(),
            format_version: FORMAT_VERSION,
            metadata: SceneMetadata {
                generated_by: generated_by.to_owned(),
                created_at: created_at.unwrap_or_else(|| now.clone()),
                saved_at: now,
            },
            domain: inputs.domain,
            time_step: inputs.time_step,
            playback_speed: inputs.playback_speed,
            scene_scale: inputs.scene_scale,
            integration_scheme: inputs.integration_scheme,
            field_systems: inputs
                .field_systems
                .into_iter()
                .map(|status| FieldSystemComposition {
                    plugin: status.plugin.id,
                    version: status.plugin.version,
                    enabled: status.enabled,
                    realtime: status.realtime,
                    configuration: status.configuration,
                })
                .collect(),
            world: inputs.world,
            expressions: inputs.expressions,
            queue: inputs.queue,
            view: inputs.view,
            probe_history: inputs.probe_history,
            distance_history: inputs.distance_history,
            mass_aggregate_history: inputs.mass_aggregate_history,
            document_entries: inputs.document_entries,
            quick_add_hidden: inputs.quick_add_hidden,
            run_records: inputs.run_records,
        }
    }

    /// The lowest `snapshot_sequence` a freshly resumed session's live
    /// snapshot producer must start counting from, so that every snapshot it
    /// publishes after a load has a sequence number greater than every
    /// reading already recorded in this document's saved histories.
    ///
    /// A live history's `record` only accepts a reading whose incoming
    /// snapshot sequence is strictly greater than the last one it already
    /// holds (see `fieldcad_simulation::{ProbeHistory, DistanceHistory,
    /// MassAggregateHistory}::record`) — a guard against double-recording a
    /// snapshot polled more than once, not a save/load concern. A fresh
    /// [`fieldcad_simulation::SimulationRuntime`] otherwise always starts
    /// counting from 0 regardless of source, so without this, every reading
    /// this document restores into a resumed session's histories poisons
    /// that guard: every new snapshot's sequence is *lower* than the
    /// restored max until the counter climbs back past it, and every
    /// plot/live-value bound to history — though not a value read straight
    /// off the latest snapshot, which is why only the plot appears frozen —
    /// stalls for exactly that many ticks.
    pub fn next_snapshot_sequence(&self) -> u64 {
        [
            self.probe_history.max_snapshot_sequence(),
            self.distance_history.max_snapshot_sequence(),
            self.mass_aggregate_history.max_snapshot_sequence(),
        ]
        .into_iter()
        .flatten()
        .max()
        .map_or(0, |max| max + 1)
    }
}

/// A whole-seconds-precision RFC 3339 UTC timestamp, without pulling in a
/// date/time crate for one field: `SystemTime` plus fixed civil-calendar math
/// is enough for a "when was this saved" label nothing parses back into a
/// `SystemTime`.
pub(crate) fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    civil_from_unix(secs)
}

fn civil_from_unix(unix_secs: u64) -> String {
    // Howard Hinnant's days_from_civil / civil_from_days algorithm, run in
    // reverse — public domain, standard technique for calendar math without a
    // dependency.
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z",)
}

/// Turns a document's declared field-system composition into a concrete
/// plugin set, given what this host has linked in.
///
/// Never constructs a plugin — `catalog` is one default-configuration
/// [`PluginRegistration`] per plugin the host links in (what a host's own
/// session-construction helper builds today, minus any templated demo-scene
/// content). For each catalog entry, if the document names that plugin,
/// `enabled`/`realtime`/`configuration` are overridden from the document
/// (configuration validated against the plugin's own declared schema — the
/// same check `SimulationRuntime::new` runs, not re-derived here). A catalog
/// plugin the document never mentions keeps the host's own default
/// (forward-compatible: a document saved before a plugin existed just gets
/// that plugin's off-by-default host policy, whatever it is).
pub fn resolve_plugins(
    catalog: Vec<PluginRegistration>,
    field_systems: &[FieldSystemComposition],
) -> Result<(Vec<PluginRegistration>, Vec<ResolveWarning>), ResolveError> {
    let mut warnings = Vec::new();
    let mut resolved = Vec::with_capacity(catalog.len());
    for mut registration in catalog {
        let metadata = registration.plugin.metadata();
        let Some(declared) = field_systems
            .iter()
            .find(|entry| entry.plugin == metadata.id)
        else {
            resolved.push(registration);
            continue;
        };
        if declared.version.major != metadata.version.major {
            return Err(ResolveError::IncompatiblePluginVersion {
                plugin: metadata.id.clone(),
                document_version: declared.version,
                linked_version: metadata.version,
            });
        }
        if declared.version != metadata.version {
            warnings.push(ResolveWarning {
                plugin: metadata.id.clone(),
                document_version: declared.version,
                linked_version: metadata.version,
            });
        }
        registration
            .plugin
            .configuration_schema()
            .validate(&declared.configuration)
            .map_err(|source| ResolveError::InvalidConfiguration {
                plugin: metadata.id.clone(),
                source,
            })?;
        registration.configuration = declared.configuration.clone();
        registration.enabled = declared.enabled;
        registration.realtime = declared.realtime;
        resolved.push(registration);
    }

    // A document plugin absent from the host's catalog names a system this
    // build cannot construct at all — reported, not silently dropped.
    for declared in field_systems {
        if !resolved
            .iter()
            .any(|registration| registration.plugin.metadata().id == declared.plugin)
        {
            return Err(ResolveError::UnknownPlugin {
                plugin: declared.plugin.clone(),
                version: declared.version,
            });
        }
    }

    Ok((resolved, warnings))
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(
        "document references plugin '{plugin}' version {version}, which is not linked into this build"
    )]
    UnknownPlugin {
        plugin: PluginId,
        version: PluginVersion,
    },
    #[error(
        "document references plugin '{plugin}' major version {document_version}, linked build has {linked_version}; configuration is not compatible"
    )]
    IncompatiblePluginVersion {
        plugin: PluginId,
        document_version: PluginVersion,
        linked_version: PluginVersion,
    },
    #[error("document's configuration for plugin '{plugin}' is invalid: {source}")]
    InvalidConfiguration {
        plugin: PluginId,
        #[source]
        source: fieldcad_core::SchemaError,
    },
}

/// A document plugin's version differs from what this build has linked in,
/// but only in a minor/patch way — no frozen plugin contract exists yet
/// (ADR 0005), so this is surfaced rather than treated as an error.
#[derive(Clone, Debug)]
pub struct ResolveWarning {
    pub plugin: PluginId,
    pub document_version: PluginVersion,
    pub linked_version: PluginVersion,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error("expected format '{FORMAT_ID}', found '{found}'")]
    WrongFormat { found: String },
    #[error(
        "document format version {found} is newer than the {max_supported} this build supports"
    )]
    UnsupportedVersion { found: u32, max_supported: u32 },
    #[error("no valid document found at, or alongside, the given path")]
    NoValidCandidate,
}

/// Which of the three durable-write-protocol candidates a load actually used.
/// Anything other than `Primary` is worth surfacing to the caller as a
/// warning — it means the primary file was missing or failed to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadSource {
    Primary,
    Backup,
    RecoveredTemp,
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub document: SceneDocument,
    pub source: LoadSource,
}

fn backup_path(path: &Path) -> PathBuf {
    append_extension(path, "bak")
}

fn tmp_path(path: &Path) -> PathBuf {
    append_extension(path, "tmp")
}

fn append_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

fn decode(bytes: &[u8]) -> Result<SceneDocument, LoadError> {
    let document: SceneDocument = serde_json::from_slice(bytes)?;
    if document.format != FORMAT_ID {
        return Err(LoadError::WrongFormat {
            found: document.format,
        });
    }
    if document.format_version > FORMAT_VERSION {
        return Err(LoadError::UnsupportedVersion {
            found: document.format_version,
            max_supported: FORMAT_VERSION,
        });
    }
    Ok(document)
}

fn try_load(path: &Path) -> Option<SceneDocument> {
    let bytes = fs::read(path).ok()?;
    decode(&bytes).ok()
}

/// Write `<path>.tmp`, fsync, atomically rename over `path`. Before the
/// rename, if `path` already exists and is independently valid (parses as a
/// well-formed, compatible document), copy it to `<path>.bak` first — "retain
/// one `.bak` of the previous *verified* document," not whatever bytes
/// happened to be on disk.
pub fn save_to_path(document: &SceneDocument, path: &Path) -> Result<(), SaveError> {
    let bytes = serde_json::to_vec_pretty(document)?;
    let tmp = tmp_path(path);
    {
        let mut file = fs::File::create(&tmp)?;
        io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
    }
    if try_load(path).is_some() {
        fs::copy(path, backup_path(path))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Try primary, then `.bak`, then `.tmp` (the `.tmp` only exists if a prior
/// write was interrupted after the write but before the rename), in that
/// order. Each candidate is independently deserialized and
/// format/version-checked; the first valid one wins.
pub fn load_newest_valid(path: &Path) -> Result<LoadOutcome, LoadError> {
    if let Some(document) = try_load(path) {
        return Ok(LoadOutcome {
            document,
            source: LoadSource::Primary,
        });
    }
    if let Some(document) = try_load(&backup_path(path)) {
        return Ok(LoadOutcome {
            document,
            source: LoadSource::Backup,
        });
    }
    if let Some(document) = try_load(&tmp_path(path)) {
        return Ok(LoadOutcome {
            document,
            source: LoadSource::RecoveredTemp,
        });
    }
    Err(LoadError::NoValidCandidate)
}
