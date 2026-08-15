//! The headless owner of the simulation model.
//!
//! Field CAD follows an Elm-style split: one authoritative model, and
//! commands that mutate it. The desktop UI is one source of commands; this
//! crate is where that boundary stops being desktop-shaped. [`HeadlessServer`]
//! owns the model — a [`SimulationRuntime`] behind an [`AsyncLocalDataSource`]
//! — with no window, no GPU device, nothing that requires a display. Any
//! transport (an embedded UI, MCP, or another network surface) drives it
//! through the same [`fieldcad_simulation::FieldDataSource`] contract ADR
//! 0001 already defines, so "remote and local sources behave identically" is a
//! property of this crate rather than a promise a transport has to keep.
//!
//! `fieldcad-mcp` is a working transport built on this crate, both as its
//! own standalone binary and embedded inside the desktop app, sharing one
//! session with the desktop UI's own commands.

use std::{collections::BTreeMap, collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use fieldcad_core::{
    CatalogEntryRef, CatalogLinkMode, Domain, FieldSnapshot, ObjectId, SceneScale, SessionId,
    TimeStep, TimeStepError, Transform, Velocity, WorldCommand, WorldSnapshot,
};
use fieldcad_electromagnetism::{ElectromagnetismPlugin, courant_limit};
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_simulation::{
    AsyncLocalDataSource, BodySample, Command, CommandDisposition, CommandEvent, CommandId,
    CommandPayload, CommandReceipt, CommandSequencer, DataSourceStatus, EditHistoryStatus,
    FieldDataSource, FieldSystemStatus, IntegrationScheme, LocalDataSource, PlaybackSpeed,
    PluginRegistration, PollOutcome, QueueStatus, QueueSummary, RuntimeConfig, RuntimeError,
    SimulationRuntime, SimulationStatus, SourceError, Subscription,
};
use glam::DVec3;
use tokio::sync::oneshot;

/// Return Field CAD's global catalog directory for this host.
pub fn catalog_directory() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "fieldcad").map(|dirs| dirs.config_dir().join("catalog"))
}

/// Catalog state owned with a session, rather than by a particular transport.
#[derive(Clone, Debug, Default)]
pub struct CatalogSession {
    root: Option<PathBuf>,
    report: fieldcad_catalog::CatalogLoadReport,
    file_states: fieldcad_catalog::DirectoryState,
    document_entries: Vec<fieldcad_scene_document::DocumentCatalogEntry>,
    quick_add_hidden: Vec<fieldcad_core::CatalogEntryRef>,
    /// Bumped every time `reload` runs. The sole change-detection signal a
    /// client needs: cheap to compare, and never wrong in the conservative
    /// direction (a caller that refetches on a revision bump that turned out
    /// unchanged wastes a read; a caller that trusted a stale mirror instead
    /// is the bug this exists to prevent). See
    /// `docs/tasks/server-authoritative-catalog.md`.
    revision: u64,
}

impl CatalogSession {
    fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    fn reload(
        &mut self,
        schemas: &BTreeMap<fieldcad_core::ComponentTypeId, fieldcad_core::ComponentSchema>,
    ) {
        let mut report = self
            .root
            .as_ref()
            .map_or_else(fieldcad_catalog::CatalogLoadReport::default, |root| {
                fieldcad_catalog::load_catalog_directory(root, schemas)
            });
        for entry in &self.document_entries {
            let reference = document_entry_ref(entry);
            let result = match fieldcad_catalog::resolve_availability(&entry.spec, schemas) {
                fieldcad_catalog::AvailabilityOutcome::Available => {
                    fieldcad_catalog::LoadResult::Available {
                        metadata: entry.metadata.clone(),
                        spec: entry.spec.clone(),
                    }
                }
                fieldcad_catalog::AvailabilityOutcome::Unavailable(reasons) => {
                    fieldcad_catalog::LoadResult::Unavailable {
                        metadata: entry.metadata.clone(),
                        spec: entry.spec.clone(),
                        reasons,
                    }
                }
            };
            report.entries.push(fieldcad_catalog::CatalogEntry {
                source: fieldcad_catalog::SourceLocation {
                    file: PathBuf::from("<scene document>"),
                    document_ordinal: fieldcad_catalog::DocumentOrdinal::new(0),
                },
                reference: Some(reference),
                identity: Some(entry.identity.clone()),
                result,
            });
        }
        self.file_states = self
            .root
            .as_ref()
            .map_or_else(fieldcad_catalog::DirectoryState::default, |root| {
                fieldcad_catalog::directory_state(root)
            });
        self.report = report;
        self.revision += 1;
    }
}

/// Build a durable catalog reference for a scene-local entry.
pub fn document_entry_ref(
    entry: &fieldcad_scene_document::DocumentCatalogEntry,
) -> fieldcad_core::CatalogEntryRef {
    fieldcad_core::CatalogEntryRef {
        catalog: entry.identity.catalog.as_str().to_owned(),
        template: entry.identity.template.as_str().to_owned(),
        origin: fieldcad_core::CatalogOrigin::Document {
            entry_id: entry.entry_id,
        },
        api_version: fieldcad_catalog::API_VERSION.to_owned(),
        fingerprint: fieldcad_catalog::template_fingerprint(
            &entry.identity,
            &entry.metadata,
            &entry.spec,
        ),
    }
}

/// Failure while changing a catalog source or its session-local entries.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog configuration directory is unavailable")]
    DirectoryUnavailable,
    #[error("catalog entry disappeared")]
    EntryNotFound,
    #[error("{0} already exists in the effective catalog")]
    IdentityCollision(fieldcad_catalog::TemplateIdentity),
    #[error("catalog entry is invalid and cannot be edited structurally")]
    InvalidEntry,
    #[error("catalog source changed or disappeared: {0}")]
    SourceUnavailable(PathBuf),
    #[error("catalog entry is unavailable or invalid: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Unavailable(Vec<fieldcad_catalog::AvailabilityReason>),
    #[error("no matching tracking objects")]
    NoMatchingObjects,
    #[error(transparent)]
    Write(#[from] fieldcad_catalog::WriteError),
}

mod event_hub;
pub use event_hub::{EventHub, EventWatcher, SessionEvent, WatchEvent};

/// Builds the default session: a numerical domain and the same solver
/// composition rule the desktop app uses (one electric field, two candidate
/// models — electrostatics active, Maxwell composed but inactive), starting
/// from an empty world.
///
/// Deliberately empty rather than pre-populated: what scene to author is a
/// client decision (desktop's demo scene is a UI convenience, not part of the
/// server's contract), and a remote client must be able to build up a scene
/// through the same commands a local one would use.
pub fn default_session() -> Result<AsyncLocalDataSource, SessionError> {
    let domain = Domain::centred_cube(5.0, 32)?;
    let time_step = TimeStep::from_seconds(courant_limit(&domain) * 0.8)?;
    let config = server_plugin_catalog().into_iter().fold(
        RuntimeConfig::new(domain, time_step, SessionId::from_u128(1)),
        |config, registration| config.with_plugin_registration(registration),
    );
    let runtime = SimulationRuntime::new(config)?;
    Ok(AsyncLocalDataSource::new(LocalDataSource::new(runtime)))
}

/// The headless server's CPU-only plugin composition: one electric field,
/// two candidate models — electrostatics active, Maxwell composed but
/// inactive. Factored out of [`default_session`] so a scene-lifecycle
/// loader (new/save/load, `fieldcad-scene-document`) can rebuild a session
/// from this same composition rather than a hardcoded one, the way the
/// desktop app's GPU-backed catalog does for its own host.
pub fn server_plugin_catalog() -> Vec<PluginRegistration> {
    vec![
        PluginRegistration::with_default_configuration(Box::new(ElectrostaticsPlugin::new())),
        PluginRegistration::with_default_configuration(Box::new(ElectromagnetismPlugin::new()))
            .with_enabled(false),
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Domain(#[from] fieldcad_core::DomainError),
    #[error(transparent)]
    TimeStep(#[from] TimeStepError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// The model plus the bookkeeping any command source needs: a place to mint
/// [`CommandId`](fieldcad_simulation::CommandId)s and a wall-clock pacer.
///
/// One `HeadlessServer` is one session. Multiple transports driving the same
/// session share one `HeadlessServer`, the way the desktop app's UI is the
/// sole caller of its own `AsyncLocalDataSource` today — and, once more than
/// one transport shares a session (an embedded UI plus MCP, say), it is the
/// reason two transports can safely mint commands and learn of their
/// completion without racing each other: there is exactly one
/// `CommandSequencer` and exactly one place that drains
/// [`AsyncLocalDataSource::drain_command_events`], no matter how many
/// transports are attached.
pub struct HeadlessServer {
    source: AsyncLocalDataSource,
    catalog: CatalogSession,
    sequencer: CommandSequencer,
    /// Registered by [`submit_and_await`](Self::submit_and_await), fulfilled
    /// by [`publish`](Self::publish) the moment a command actually goes
    /// terminal — not by whichever transport next happens to call
    /// [`drain_events`](Self::drain_events), which is what used to make two
    /// concurrent transports race for the same waiter.
    waiters: HashMap<CommandId, oneshot::Sender<CommandEvent>>,
    /// The broadcast hub every transport subscribes to independently. See
    /// `docs/tasks/session-events-and-queue-control.md`.
    hub: EventHub,
    /// Buffered for [`drain_events`](Self::drain_events), refilled by
    /// [`publish`](Self::publish) — the sole call site of
    /// [`AsyncLocalDataSource::drain_command_events`] in this crate, keeping
    /// this crate's one-canonical-drain discipline for the *inner* source
    /// even though publication now also happens here.
    events: Vec<CommandEvent>,
}

impl HeadlessServer {
    pub fn new(source: AsyncLocalDataSource) -> Self {
        Self::with_catalog_root(source, catalog_directory())
    }

    /// Construct a session with an explicit global catalog root. Hosts use this
    /// to share their normal user configuration directory without letting a
    /// transport choose arbitrary filesystem locations.
    pub fn with_catalog_root(source: AsyncLocalDataSource, root: Option<PathBuf>) -> Self {
        let mut server = Self {
            source,
            catalog: CatalogSession::new(root),
            sequencer: CommandSequencer::default(),
            waiters: HashMap::new(),
            hub: EventHub::default(),
            events: Vec::new(),
        };
        server.reload_catalog();
        server
    }

    /// Snapshot of global plus document-scoped catalog entries.
    pub fn catalog(&self) -> &fieldcad_catalog::CatalogLoadReport {
        &self.catalog.report
    }

    /// Re-read the configured global source and re-resolve document entries.
    /// Every catalog mutation on this type funnels through here, so this is
    /// the single place that bumps the catalog revision and notifies
    /// subscribers — no mutation method needs to remember to do so itself.
    pub fn reload_catalog(&mut self) {
        self.catalog.reload(self.source.world().component_schemas());
        self.hub.publish_catalog_updated(self.catalog.revision);
    }

    /// Monotonically increasing revision, bumped on every catalog change.
    /// Cheap enough to poll: a client compares this before refetching the
    /// full report rather than diffing the report itself.
    pub fn catalog_revision(&self) -> u64 {
        self.catalog.revision
    }

    /// Scene-local templates persisted with this session's scene document.
    pub fn document_entries(&self) -> &[fieldcad_scene_document::DocumentCatalogEntry] {
        &self.catalog.document_entries
    }

    /// Scene-local quick-add visibility preferences.
    pub fn quick_add_hidden(&self) -> &[fieldcad_core::CatalogEntryRef] {
        &self.catalog.quick_add_hidden
    }

    /// Replace scene-local catalog state wholesale — a new/loaded scene's
    /// document-scoped entries and quick-add preferences, atomically with
    /// the rest of scene lifecycle restore (see [`Self::replace_source`]).
    ///
    /// This is deliberately not exposed as a general-purpose sync path: an
    /// embedded MCP client and the desktop UI share one `HeadlessServer`, so
    /// a caller that re-pushed its own possibly-stale copy of document
    /// entries on every reload could silently discard the other transport's
    /// concurrent edits. Steady-state document-entry/quick-add changes go
    /// through [`Self::create_catalog_entry`], [`Self::update_catalog_entry`],
    /// [`Self::delete_catalog_entry`], and [`Self::set_quick_add_visibility`]
    /// instead, each of which mutates exactly the one entry named.
    pub fn restore_document_catalog(
        &mut self,
        entries: Vec<fieldcad_scene_document::DocumentCatalogEntry>,
        quick_add_hidden: Vec<fieldcad_core::CatalogEntryRef>,
    ) {
        self.catalog.document_entries = entries;
        self.catalog.quick_add_hidden = quick_add_hidden;
        self.reload_catalog();
    }

    /// Show or hide one catalog entry in the quick-add menu — a narrow,
    /// single-entry alternative to replacing the whole `quick_add_hidden`
    /// list via [`Self::restore_document_catalog`].
    pub fn set_quick_add_visibility(&mut self, entry: fieldcad_core::CatalogEntryRef, hidden: bool) {
        if hidden {
            if !self.catalog.quick_add_hidden.contains(&entry) {
                self.catalog.quick_add_hidden.push(entry);
            }
        } else {
            self.catalog.quick_add_hidden.retain(|current| current != &entry);
        }
        self.reload_catalog();
    }

    /// Resolve one available catalog entry into a `CreateObject` command at
    /// the origin, the way the normal authoritative `CreateObject`
    /// transaction expects it. Placement remains an instance concern and is
    /// never decided here — a caller (desktop viewport, MCP) that wants a
    /// different placement submits this command then moves the result, the
    /// same way the object inspector already handles placement for any
    /// other object.
    ///
    /// Returns the command rather than submitting it: desktop callers route
    /// it through their own edit-gesture/undo pipeline; MCP submits and
    /// awaits it directly. This is the single implementation both share —
    /// see `docs/tasks/server-authoritative-catalog.md`.
    pub fn resolve_catalog_instantiation(
        &self,
        entry: &CatalogEntryRef,
        display_name: Option<String>,
    ) -> Result<WorldCommand, CatalogError> {
        let candidate = self
            .catalog
            .report
            .entries
            .iter()
            .find(|candidate| candidate.reference.as_ref() == Some(entry))
            .ok_or(CatalogError::EntryNotFound)?;
        let fieldcad_catalog::LoadResult::Available { spec, .. } = &candidate.result else {
            return Err(CatalogError::Unavailable(Vec::new()));
        };
        let world = self.source.world();
        let display_name = display_name.unwrap_or_else(|| {
            fieldcad_catalog::suggest_display_name(
                &entry.template,
                world.objects().values().map(|object| object.name.as_str()),
            )
        });
        let object = fieldcad_catalog::instantiate_template(
            spec,
            entry,
            world.component_schemas(),
            fieldcad_catalog::InstantiationPlacement {
                display_name,
                transform: Transform::default(),
                velocity: Velocity::default(),
                pinned: false,
                fallback_shape_radius: 0.15,
            },
        )
        .map_err(CatalogError::Unavailable)?;
        Ok(WorldCommand::CreateObject(object))
    }

    /// World objects tracking `entry`'s source origin — non-mutating. A
    /// caller previews before calling
    /// [`apply_catalog_propagation`](Self::apply_catalog_propagation), which
    /// uses the identical matching rule (link mode `Tracking` and matching
    /// origin, independent of a possibly-renamed identity), so the count
    /// shown here is exactly what apply will act on.
    pub fn preview_catalog_propagation(&self, entry: &CatalogEntryRef) -> Vec<ObjectId> {
        self.source
            .world()
            .objects()
            .values()
            .filter(|object| {
                object.catalog_link.as_ref().is_some_and(|link| {
                    link.mode == CatalogLinkMode::Tracking
                        && link
                            .entry
                            .as_ref()
                            .is_some_and(|linked| linked.origin == entry.origin)
                })
            })
            .map(|object| object.id)
            .collect()
    }

    /// Resolve `entry`'s current template into one `ApplyCatalogTemplate`
    /// command per previewed tracking object, or only `selection` when
    /// non-empty — for a caller to submit as one atomic transaction (normal
    /// undo/history semantics apply). A catalog save or reload never calls
    /// this itself; a caller (desktop propagation dialog, MCP
    /// `apply_catalog_propagation`) resolves and submits explicitly after
    /// the user confirms.
    pub fn resolve_catalog_propagation(
        &self,
        entry: &CatalogEntryRef,
        selection: &[ObjectId],
    ) -> Result<Vec<WorldCommand>, CatalogError> {
        let candidate = self
            .catalog
            .report
            .entries
            .iter()
            .find(|candidate| candidate.reference.as_ref() == Some(entry))
            .ok_or(CatalogError::EntryNotFound)?;
        let fieldcad_catalog::LoadResult::Available { spec, .. } = &candidate.result else {
            return Err(CatalogError::Unavailable(Vec::new()));
        };
        let world = self.source.world();
        let mut commands = Vec::new();
        for object in world.objects().values() {
            let Some(link) = &object.catalog_link else {
                continue;
            };
            if link.mode != CatalogLinkMode::Tracking
                || !link
                    .entry
                    .as_ref()
                    .is_some_and(|linked| linked.origin == entry.origin)
            {
                continue;
            }
            if !selection.is_empty() && !selection.contains(&object.id) {
                continue;
            }
            let replacement = fieldcad_catalog::instantiate_template(
                spec,
                entry,
                world.component_schemas(),
                fieldcad_catalog::InstantiationPlacement {
                    display_name: object.name.clone(),
                    transform: object.transform,
                    velocity: object.velocity,
                    pinned: object.pinned,
                    fallback_shape_radius: 0.15,
                },
            )
            .map_err(CatalogError::Unavailable)?;
            commands.push(WorldCommand::ApplyCatalogTemplate {
                object: object.id,
                expected_entry: link
                    .entry
                    .clone()
                    .expect("tracking links from catalog have an entry"),
                shape: replacement.shape,
                components: replacement.components,
                link: replacement
                    .catalog_link
                    .expect("catalog instantiation stamps provenance"),
            });
        }
        if commands.is_empty() {
            return Err(CatalogError::NoMatchingObjects);
        }
        Ok(commands)
    }

    /// Create a template in the requested catalog scope.
    pub fn create_catalog_entry(
        &mut self,
        document_scope: bool,
        identity: fieldcad_catalog::TemplateIdentity,
        metadata: fieldcad_catalog::TemplateMetadata,
        spec: fieldcad_catalog::TemplateSpec,
    ) -> Result<fieldcad_core::CatalogEntryRef, CatalogError> {
        if self
            .catalog
            .report
            .entries
            .iter()
            .any(|entry| entry.identity.as_ref() == Some(&identity))
        {
            return Err(CatalogError::IdentityCollision(identity));
        }
        let reference = if document_scope {
            let entry = fieldcad_scene_document::DocumentCatalogEntry {
                entry_id: uuid::Uuid::new_v4(),
                identity,
                metadata,
                spec,
            };
            let reference = document_entry_ref(&entry);
            self.catalog.document_entries.push(entry);
            reference
        } else {
            let root = self
                .catalog
                .root
                .as_ref()
                .ok_or(CatalogError::DirectoryUnavailable)?;
            let path = root.join(format!("{}.yaml", identity.template.as_str()));
            fieldcad_catalog::create_entry(&path, &identity, &metadata, &spec)?;
            fieldcad_core::CatalogEntryRef {
                catalog: identity.catalog.as_str().to_owned(),
                template: identity.template.as_str().to_owned(),
                origin: fieldcad_core::CatalogOrigin::Global {
                    relative_path: path
                        .file_name()
                        .expect("catalog filename")
                        .to_string_lossy()
                        .into_owned(),
                    document_ordinal: 1,
                },
                api_version: fieldcad_catalog::API_VERSION.to_owned(),
                fingerprint: String::new(),
            }
        };
        self.reload_catalog();
        self.catalog
            .report
            .entries
            .iter()
            .find_map(|entry| {
                entry
                    .reference
                    .as_ref()
                    .filter(|current| {
                        current.origin == reference.origin
                            && current.catalog == reference.catalog
                            && current.template == reference.template
                    })
                    .cloned()
            })
            .ok_or(CatalogError::EntryNotFound)
    }

    /// Update one source-qualified template, preserving its source document
    /// (and a document entry UUID) while allowing its identity to change.
    pub fn update_catalog_entry(
        &mut self,
        entry: &fieldcad_core::CatalogEntryRef,
        identity: fieldcad_catalog::TemplateIdentity,
        metadata: fieldcad_catalog::TemplateMetadata,
        spec: fieldcad_catalog::TemplateSpec,
    ) -> Result<fieldcad_core::CatalogEntryRef, CatalogError> {
        if self.catalog.report.entries.iter().any(|candidate| {
            candidate.identity.as_ref() == Some(&identity)
                && candidate.reference.as_ref() != Some(entry)
        }) {
            return Err(CatalogError::IdentityCollision(identity));
        }
        match &entry.origin {
            fieldcad_core::CatalogOrigin::Document { entry_id } => {
                let document = self
                    .catalog
                    .document_entries
                    .iter_mut()
                    .find(|document| document.entry_id == *entry_id)
                    .ok_or(CatalogError::EntryNotFound)?;
                document.identity = identity.clone();
                document.metadata = metadata;
                document.spec = spec;
            }
            fieldcad_core::CatalogOrigin::Global {
                relative_path,
                document_ordinal,
            } => {
                let root = self
                    .catalog
                    .root
                    .as_ref()
                    .ok_or(CatalogError::DirectoryUnavailable)?;
                let path = root.join(relative_path);
                let expected = self
                    .catalog
                    .file_states
                    .files
                    .get(&path)
                    .and_then(|state| state.as_ref().ok())
                    .ok_or_else(|| CatalogError::SourceUnavailable(path.clone()))?;
                let old_identity = self
                    .catalog
                    .report
                    .entries
                    .iter()
                    .find(|candidate| candidate.reference.as_ref() == Some(entry))
                    .and_then(|candidate| candidate.identity.clone())
                    .ok_or(CatalogError::EntryNotFound)?;
                fieldcad_catalog::save_entry_at(
                    &fieldcad_catalog::SourceTarget {
                        path,
                        document_ordinal: fieldcad_catalog::DocumentOrdinal::new(
                            document_ordinal.saturating_sub(1),
                        ),
                        identity: old_identity,
                    },
                    &identity,
                    &metadata,
                    &spec,
                    expected,
                )?;
            }
        }
        self.reload_catalog();
        self.catalog
            .report
            .entries
            .iter()
            .find_map(|candidate| {
                candidate
                    .reference
                    .as_ref()
                    .filter(|current| {
                        current.origin == entry.origin
                            && current.catalog == identity.catalog.as_str()
                            && current.template == identity.template.as_str()
                    })
                    .cloned()
            })
            .ok_or(CatalogError::EntryNotFound)
    }

    /// Delete one source-qualified template.
    pub fn delete_catalog_entry(
        &mut self,
        entry: &fieldcad_core::CatalogEntryRef,
    ) -> Result<(), CatalogError> {
        match &entry.origin {
            fieldcad_core::CatalogOrigin::Document { entry_id } => {
                let before = self.catalog.document_entries.len();
                self.catalog
                    .document_entries
                    .retain(|document| document.entry_id != *entry_id);
                if self.catalog.document_entries.len() == before {
                    return Err(CatalogError::EntryNotFound);
                }
            }
            fieldcad_core::CatalogOrigin::Global {
                relative_path,
                document_ordinal,
            } => {
                let root = self
                    .catalog
                    .root
                    .as_ref()
                    .ok_or(CatalogError::DirectoryUnavailable)?;
                let path = root.join(relative_path);
                let expected = self
                    .catalog
                    .file_states
                    .files
                    .get(&path)
                    .and_then(|state| state.as_ref().ok())
                    .ok_or_else(|| CatalogError::SourceUnavailable(path.clone()))?;
                let identity = self
                    .catalog
                    .report
                    .entries
                    .iter()
                    .find(|candidate| candidate.reference.as_ref() == Some(entry))
                    .and_then(|candidate| candidate.identity.clone())
                    .ok_or(CatalogError::EntryNotFound)?;
                fieldcad_catalog::remove_entry_at(
                    &fieldcad_catalog::SourceTarget {
                        path,
                        document_ordinal: fieldcad_catalog::DocumentOrdinal::new(
                            document_ordinal.saturating_sub(1),
                        ),
                        identity,
                    },
                    expected,
                )?;
            }
        }
        self.reload_catalog();
        Ok(())
    }

    /// Mint a command identity and submit it. See
    /// [`FieldDataSource::execute`] for what the receipt's disposition means.
    pub fn submit(&mut self, payload: CommandPayload) -> Result<CommandReceipt, SourceError> {
        let command = self.sequencer.issue(payload);
        self.execute(command)
    }

    /// Submit a command whose identity was already minted by the caller —
    /// for a transport that tracks its own client-issued ids rather than
    /// this server's sequencer.
    pub fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        let receipt = self.source.execute(command)?;
        self.publish();
        Ok(receipt)
    }

    /// Mint a command identity, submit it, and register interest in its
    /// completion — atomically, under one call, so no [`publish`](Self::publish)
    /// can land between submission and registration and fulfill the waiter
    /// before anyone is listening for it.
    ///
    /// Returns `None` in the receiver position when the command was applied
    /// or queued rather than submitted non-blockingly: there is nothing to
    /// wait for, the receipt already says everything the caller needs.
    pub fn submit_and_await(
        &mut self,
        payload: CommandPayload,
    ) -> Result<(CommandReceipt, Option<oneshot::Receiver<CommandEvent>>), SourceError> {
        let receipt = self.submit(payload)?;
        if receipt.disposition != CommandDisposition::Submitted {
            return Ok((receipt, None));
        }
        let (tx, rx) = oneshot::channel();
        self.waiters.insert(receipt.command, tx);
        Ok((receipt, Some(rx)))
    }

    /// Advance the model by wall-clock time. Call this on a fixed cadence
    /// (a run loop, a timer) — the numerical `dt` is the model's own
    /// business and never changes to compensate for a slow caller.
    pub fn advance(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        let outcome = self.source.poll(elapsed)?;
        self.publish();
        Ok(outcome)
    }

    /// The one place events leave the inner source and fan out: to whichever
    /// `submit_and_await` waiter is registered for that id, to the broadcast
    /// hub, and into `self.events` for `drain_events()`. Waiter resolution no
    /// longer depends on anyone calling `drain_events()`, which is what
    /// removes "whichever transport calls it next completes every pending
    /// waiter" as a race entirely — both [`execute`](Self::execute) and
    /// [`advance`](Self::advance) fold through here, whether the caller
    /// reached them through this type's own methods or through its
    /// [`FieldDataSource`] impl.
    fn publish(&mut self) {
        for event in self.source.drain_command_events() {
            if let Some(waiter) = self.waiters.remove(&event.command_id()) {
                let _ = waiter.send(event.clone());
            }
            self.hub.publish_command_event(&event);
            self.events.push(event);
        }
        // Prune waiters whose receiver was dropped (caller timed out,
        // disconnected, etc.) — the only removal path was the
        // `waiters.remove` above, which only fires for a completed
        // command.  Without this, an MCP 30 s timeout that drops the
        // receiver leaves the sender in the map forever (BE-8).
        self.waiters.retain(|_id, sender| !sender.is_closed());
        self.hub.publish_state(&self.source);
    }

    /// Completion/rejection/cancellation events for commands submitted
    /// non-blockingly.
    ///
    /// Every transport's completion events, and every registered
    /// [`submit_and_await`](Self::submit_and_await) waiter, are resolved by
    /// [`publish`](Self::publish), not by this call — draining here only
    /// hands back what already accumulated. A transport that only wants "did
    /// my command finish" does not need to call this at all; a transport
    /// that wants a running log of everything (the desktop UI's per-frame
    /// diagnostics) still gets the full list unchanged.
    pub fn drain_events(&mut self) -> Vec<CommandEvent> {
        std::mem::take(&mut self.events)
    }

    /// An independent, non-destructive subscription to this session's
    /// events — any number of callers may hold one at once without
    /// competing with [`drain_events`](Self::drain_events) or with each
    /// other.
    pub fn subscribe_events(&self) -> EventWatcher {
        self.hub.subscribe()
    }

    /// Authoritative queue state: paused flag, ordered pending commands, and
    /// recent terminal history.
    pub fn get_queue(&self) -> QueueStatus {
        self.source.get_queue()
    }

    /// The number of unresolved [`submit_and_await`](Self::submit_and_await)
    /// waiters.
    pub fn waiter_count(&self) -> usize {
        self.waiters.len()
    }

    /// The queue's shape without its contents — see
    /// [`FieldDataSource::queue_summary`]. Delegates to
    /// `AsyncLocalDataSource`'s own cheap implementation rather than the
    /// trait's default (which would derive this from [`Self::get_queue`],
    /// defeating the point).
    pub fn queue_summary(&self) -> QueueSummary {
        self.source.queue_summary()
    }

    pub fn status(&self) -> DataSourceStatus {
        self.source.status()
    }

    pub fn simulation_status(&self) -> SimulationStatus {
        self.source.simulation_status()
    }

    pub fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.source.latest_snapshot()
    }

    pub fn world(&self) -> WorldSnapshot {
        self.source.world()
    }

    pub fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.source.field_systems()
    }

    pub fn edit_history(&self) -> EditHistoryStatus {
        self.source.edit_history()
    }

    pub fn subscription(&self) -> Subscription {
        self.source.subscription()
    }

    pub fn scene_scale(&self) -> SceneScale {
        self.source.scene_scale()
    }

    /// The current authoritative numerical time step.
    pub fn time_step(&self) -> TimeStep {
        self.source.simulation_status().time_step()
    }

    /// Synchronously read the current session's full world contents (with
    /// identifier counters) and pending-queue contents, for durable
    /// storage — see [`fieldcad_core::WorldDocument`] and
    /// [`fieldcad_simulation::QueueDocument`]. A rare, explicit, one-shot
    /// save action; blocking on the compute worker is deliberate, the same
    /// way `AsyncLocalDataSource::capture_document` documents.
    pub fn capture_document(
        &mut self,
    ) -> Result<
        (
            fieldcad_core::WorldDocument,
            fieldcad_simulation::QueueDocument,
        ),
        SourceError,
    > {
        self.source.capture_document()
    }

    /// Ask the worker for `object`'s current recorded kinematics history —
    /// see `AsyncLocalDataSource::request_body_history`. Not part of
    /// `FieldDataSource`: it queues a fetch rather than reading anything,
    /// so it does not belong next to that trait's other read-only
    /// accessors (`body_history` among them).
    pub fn request_body_history(&mut self, object: ObjectId) {
        self.source.request_body_history(object);
    }

    /// Override how many samples `object`'s recorded history keeps — see
    /// `AsyncLocalDataSource::set_body_history_capacity`. Same "not part of
    /// `FieldDataSource`" reasoning as `request_body_history` above: this
    /// sets something rather than reading it.
    pub fn set_body_history_capacity(&mut self, object: ObjectId, capacity: usize) {
        self.source.set_body_history_capacity(object, capacity);
    }

    /// Replace the inner session in place — a new/loaded scene replaces the
    /// world, domain, and field-system composition without disturbing the
    /// `Arc<Mutex<HeadlessServer>>` every attached transport (desktop UI,
    /// embedded MCP) already holds a clone of.
    ///
    /// Every waiter registered by [`submit_and_await`](Self::submit_and_await)
    /// against the *old* session can never resolve — drop their senders so
    /// an awaiting caller gets a clean disconnect instead of hanging
    /// forever. Reset the event hub's change-detection cache: the new
    /// session's first `SnapshotIdentity`/`SimulationStatus` can
    /// coincidentally equal cached values from the old session (both
    /// commonly start at sequence 0 / tick 0), which would otherwise
    /// suppress the first post-replace publish and leave subscribers on
    /// stale state.
    pub fn replace_source(&mut self, source: AsyncLocalDataSource) {
        self.source = source;
        self.waiters.clear();
        self.events.clear();
        self.hub.reset();
        self.reload_catalog();
        self.publish();
    }
}

impl FieldDataSource for HeadlessServer {
    fn description(&self) -> &str {
        self.source.description()
    }

    fn status(&self) -> DataSourceStatus {
        self.source.status()
    }

    fn simulation_status(&self) -> SimulationStatus {
        self.source.simulation_status()
    }

    fn domain(&self) -> Domain {
        self.source.domain()
    }

    fn playback_speed(&self) -> PlaybackSpeed {
        self.source.playback_speed()
    }

    fn pending_command_count(&self) -> usize {
        self.source.pending_command_count()
    }

    fn get_queue(&self) -> QueueStatus {
        HeadlessServer::get_queue(self)
    }

    fn queue_summary(&self) -> QueueSummary {
        HeadlessServer::queue_summary(self)
    }

    fn subscription(&self) -> Subscription {
        self.source.subscription()
    }

    fn scene_scale(&self) -> SceneScale {
        self.source.scene_scale()
    }

    fn integration_scheme(&self) -> IntegrationScheme {
        self.source.integration_scheme()
    }

    fn field_systems(&self) -> Vec<FieldSystemStatus> {
        self.source.field_systems()
    }

    fn edit_history(&self) -> EditHistoryStatus {
        self.source.edit_history()
    }

    fn world(&self) -> WorldSnapshot {
        self.source.world()
    }

    // Not the trait default: a source that actually holds body forces must
    // say so, or an inspector reading this through `&dyn FieldDataSource`
    // (as the desktop UI does) sees an empty map forever.
    fn body_forces(&self) -> BTreeMap<ObjectId, DVec3> {
        self.source.body_forces()
    }

    // Not the trait default, for the same reason as `body_forces` above.
    fn step_compute_ms(&self) -> f32 {
        self.source.step_compute_ms()
    }

    // Not the trait default, for the same reason as `body_forces` above.
    fn body_history(&self, object: ObjectId) -> Vec<BodySample> {
        self.source.body_history(object)
    }

    // Not `self.source.execute(command)` directly: this must go through
    // `Self::execute` above, which also calls `publish()` — the desktop's
    // per-frame pump reaches this crate only through this trait, and
    // publication (waiter resolution, the broadcast hub) must not be
    // bypassable by that path.
    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        HeadlessServer::execute(self, command)
    }

    // Not `self.source.poll(elapsed)` directly, for the same reason as
    // `execute` above.
    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        HeadlessServer::advance(self, elapsed)
    }

    fn latest_snapshot(&self) -> Option<Arc<FieldSnapshot>> {
        self.source.latest_snapshot()
    }

    // Not the trait default, and not `self.source.drain_command_events()`
    // either: this must go through `Self::drain_events` above, the one
    // canonical drain point that also resolves `submit_and_await` waiters.
    // Calling the inner source's drain directly here would split the drain
    // across two code paths again — the exact race this type exists to
    // prevent.
    fn drain_command_events(&mut self) -> Vec<CommandEvent> {
        self.drain_events()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(catalog: &str, template: &str) -> fieldcad_catalog::TemplateIdentity {
        fieldcad_catalog::TemplateIdentity {
            catalog: fieldcad_catalog::CatalogScopeName::new(catalog).unwrap(),
            template: fieldcad_catalog::TemplateName::new(template).unwrap(),
        }
    }

    fn empty_metadata() -> fieldcad_catalog::TemplateMetadata {
        fieldcad_catalog::TemplateMetadata {
            description: None,
            author: None,
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
        }
    }

    fn empty_spec() -> fieldcad_catalog::TemplateSpec {
        fieldcad_catalog::TemplateSpec {
            object_kind: "world-object".to_owned(),
            shape: None,
            components: Vec::new(),
        }
    }

    #[test]
    fn document_catalog_edits_keep_the_entry_uuid_when_renamed() {
        let source = default_session().unwrap();
        let mut server = HeadlessServer::with_catalog_root(source, None);
        let entry = server
            .create_catalog_entry(
                true,
                identity("test", "alpha"),
                empty_metadata(),
                empty_spec(),
            )
            .unwrap();
        let renamed = server
            .update_catalog_entry(
                &entry,
                identity("test", "beta"),
                empty_metadata(),
                empty_spec(),
            )
            .unwrap();
        assert_eq!(entry.origin, renamed.origin);
        assert_eq!(renamed.template, "beta");
        assert_eq!(server.document_entries().len(), 1);
    }

    #[test]
    fn global_catalog_edits_retain_the_selected_yaml_document() {
        let root = tempfile::tempdir().unwrap();
        let source = default_session().unwrap();
        let mut server = HeadlessServer::with_catalog_root(source, Some(root.path().to_path_buf()));
        let entry = server
            .create_catalog_entry(
                false,
                identity("test", "alpha"),
                empty_metadata(),
                empty_spec(),
            )
            .unwrap();
        let renamed = server
            .update_catalog_entry(
                &entry,
                identity("test", "beta"),
                empty_metadata(),
                empty_spec(),
            )
            .unwrap();
        assert_eq!(entry.origin, renamed.origin);
        assert_eq!(renamed.template, "beta");
        assert!(root.path().join("alpha.yaml").exists());
    }
}
