//! The headless owner of the simulation model.
//!
//! Field CAD follows an Elm-style split: one authoritative model, and
//! commands that mutate it. The desktop UI is one source of commands; this
//! crate is where that boundary stops being desktop-shaped. [`HeadlessServer`]
//! owns the model — a [`SimulationRuntime`] behind an [`AsyncLocalDataSource`]
//! — with no window, no GPU device, nothing that requires a display. Any
//! transport (an embedded UI, and later MCP or another network surface) drives
//! it through the same [`fieldcad_simulation::FieldDataSource`] contract ADR
//! 0001 already defines, so "remote and local sources behave identically" is a
//! property of this crate rather than a promise a transport has to keep.
//!
//! No transport is wired up yet — see `docs/mcp-plan.md` phase 3 onward. This
//! crate proves the model can run detached from the desktop app first.

use std::{collections::BTreeMap, collections::HashMap, sync::Arc, time::Duration};

use fieldcad_core::{
    Domain, FieldSnapshot, ObjectId, SessionId, TimeStep, TimeStepError, WorldSnapshot,
};
use fieldcad_electromagnetism::{ElectromagnetismPlugin, courant_limit};
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_simulation::{
    AsyncLocalDataSource, Command, CommandDisposition, CommandEvent, CommandId, CommandPayload,
    CommandReceipt, CommandSequencer, DataSourceStatus, EditHistoryStatus, FieldDataSource,
    FieldSystemStatus, LocalDataSource, PlaybackSpeed, PluginRegistration, PollOutcome,
    RuntimeConfig, RuntimeError, SimulationRuntime, SimulationStatus, SourceError, Subscription,
};
use glam::DVec3;
use tokio::sync::oneshot;

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
    let runtime = SimulationRuntime::new(
        RuntimeConfig::new(domain, time_step, SessionId::from_u128(1))
            .with_plugin(Box::new(ElectrostaticsPlugin::new()))
            .with_plugin_registration(
                PluginRegistration::with_default_configuration(Box::new(
                    ElectromagnetismPlugin::new(),
                ))
                .with_enabled(false),
            ),
    )?;
    Ok(AsyncLocalDataSource::new(LocalDataSource::new(runtime)))
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
    sequencer: CommandSequencer,
    /// Registered by [`submit_and_await`](Self::submit_and_await), fulfilled
    /// by [`drain_events`](Self::drain_events) — whichever transport happens
    /// to call `drain_events` next completes every pending waiter it finds,
    /// not only its own. That is what makes it safe for more than one
    /// transport to poll the same session concurrently.
    waiters: HashMap<CommandId, oneshot::Sender<CommandEvent>>,
}

impl HeadlessServer {
    pub fn new(source: AsyncLocalDataSource) -> Self {
        Self {
            source,
            sequencer: CommandSequencer::default(),
            waiters: HashMap::new(),
        }
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
        self.source.execute(command)
    }

    /// Mint a command identity, submit it, and register interest in its
    /// completion — atomically, under one call, so no [`drain_events`] can
    /// land between submission and registration and fulfill the waiter
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
        self.source.poll(elapsed)
    }

    /// Completion/rejection events for commands submitted non-blockingly.
    ///
    /// The only call site of [`AsyncLocalDataSource::drain_command_events`]
    /// in this crate — every transport's completion events, and every
    /// registered [`submit_and_await`](Self::submit_and_await) waiter, are
    /// resolved from here, whichever transport happens to call it. A
    /// transport that only wants "did my command finish" does not need to
    /// call this at all; a transport that wants a running log of everything
    /// (the desktop UI's per-frame diagnostics) still gets the full list
    /// unchanged.
    pub fn drain_events(&mut self) -> Vec<CommandEvent> {
        let events = self.source.drain_command_events();
        for event in &events {
            let id = match event {
                CommandEvent::Completed(receipt) => receipt.command,
                CommandEvent::Failed { command, .. } => *command,
            };
            if let Some(waiter) = self.waiters.remove(&id) {
                let _ = waiter.send(event.clone());
            }
        }
        events
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

    fn subscription(&self) -> Subscription {
        self.source.subscription()
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

    fn execute(&mut self, command: Command) -> Result<CommandReceipt, SourceError> {
        self.source.execute(command)
    }

    fn poll(&mut self, elapsed: Duration) -> Result<PollOutcome, SourceError> {
        self.source.poll(elapsed)
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
