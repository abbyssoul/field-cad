//! Headless simulation runtime and the field-data-source boundary.
//!
//! The visualizer talks to a [`FieldDataSource`], never to a solver. Two
//! implementations exist: [`LocalDataSource`] wrapping an in-process runtime, and
//! [`LoopbackDataSource`] standing in for a dedicated compute service. They are
//! required to be interchangeable, and the tests in this crate check that by
//! driving both through the same script.

pub mod async_source;
pub mod body_history;
pub mod history;
pub mod recording;
pub mod runtime;
pub mod source;

pub use async_source::{AsyncLocalDataSource, CommandEvent};
pub use body_history::{BodyHistory, BodySample};
pub use fieldcad_dynamics::IntegrationScheme;
pub use fieldcad_plugin_api::{FieldBrushFalloff, FieldBrushStroke};
pub use history::{DistanceHistory, DistanceReading, ProbeHistory, ProbeReading};
pub use recording::{RecordedEvent, ReplayObservation, SessionRecording};
pub use runtime::{
    DEFAULT_UNDO_DEPTH, EditHistoryStatus, FieldSystemStatus, PluginRegistration, RuntimeConfig,
    RuntimeError, SamplingBudget, SimulationRuntime, SimulationStatus, Subscription, TickDemand,
    TickPacer,
};
pub use source::{
    Command, CommandDisposition, CommandId, CommandKind, CommandLifecycle, CommandPayload,
    CommandReceipt, CommandRecord, CommandSequencer, DataSourceStatus, FieldDataSource,
    LocalDataSource, LoopbackDataSource, PlaybackSpeed, PlaybackSpeedError, PollOutcome,
    QueueDocument, QueueStatus, QueueSummary, SnapshotMailbox, SnapshotRejection, SourceError,
};

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, Domain, DomainBounds, ObjectId, ObjectShape,
        ObjectSpec, Precision, ProbeSpec, Resolution, SampleGeometry, SampleValidity, SessionId,
        SimulationMode, SlicePlaneSpec, SnapshotCompleteness, TimeStep, Transform, UndefinedReason,
        World, WorldCommand,
        quantities::{ChargeCoulombs, MassKg, coulomb, kilogram},
    };
    use fieldcad_electromagnetic_sources::{
        charge_component_id, charge_properties, charge_property_id,
    };
    use fieldcad_electromagnetism::{
        ElectromagnetismPlugin, courant_limit, electric_field_channel_id as maxwell_e_channel_id,
        energy_density_channel_id, magnetic_field_channel_id,
    };
    use fieldcad_electrostatics::{
        COULOMB_CONSTANT, ElectrostaticsPlugin, electric_field_channel_id,
        electric_potential_channel_id, plugin_id as electrostatics_plugin_id,
    };
    use fieldcad_particles::particle_component_id;
    use fieldcad_test_field::{TestFieldPlugin, scalar_channel_id, vector_channel_id};
    use glam::{DVec2, DVec3, UVec2};

    use super::*;

    fn time_step() -> TimeStep {
        TimeStep::from_seconds(0.1).unwrap()
    }

    fn domain() -> Domain {
        Domain::centred_cube(8.0, 8).unwrap()
    }

    fn seeded_world() -> World {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "origin probe",
                DVec3::new(1.0, 2.0, 3.0),
                vec![scalar_channel_id(), vector_channel_id()],
            ))])
            .unwrap();
        world
    }

    fn runtime() -> SimulationRuntime {
        SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(7))
                .with_world(seeded_world())
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap()
    }

    fn command(payload: CommandPayload) -> Command {
        CommandSequencer::default().issue(payload)
    }

    #[test]
    fn play_pause_and_step_are_deterministic() {
        let mut runtime = runtime();
        assert!(!runtime.advance_running().unwrap());
        runtime.step_once().unwrap();
        assert_eq!(runtime.clock_snapshot().tick(), 1);
        assert_eq!(runtime.clock_snapshot().time_seconds(), 0.1);

        runtime.play();
        assert!(runtime.advance_running().unwrap());
        runtime.pause();
        assert!(!runtime.advance_running().unwrap());
        assert_eq!(runtime.clock_snapshot().tick(), 2);
        assert_eq!(runtime.latest_snapshot().identity.sequence, 2);
    }

    #[test]
    fn maxwell_fields_publish_through_the_generic_runtime_contract() {
        let domain = Domain::new(
            DomainBounds::new(DVec3::ZERO, DVec3::ONE).unwrap(),
            Resolution::new(16, 2, 2).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let stable_step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "Maxwell recorder",
                DVec3::new(0.125, 0.5, 0.5),
                vec![
                    maxwell_e_channel_id(),
                    magnetic_field_channel_id(),
                    energy_density_channel_id(),
                ],
            ))])
            .unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, stable_step, SessionId::from_u128(0x5a))
                .with_world(world)
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        )
        .unwrap();

        let initial = runtime.latest_snapshot();
        assert!(initial.channel(&maxwell_e_channel_id()).is_some());
        assert!(initial.channel(&magnetic_field_channel_id()).is_some());
        assert!(initial.channel(&energy_density_channel_id()).is_some());

        runtime.step_once().unwrap();
        assert_eq!(runtime.latest_snapshot().identity.tick, 1);

        // Inactive time-stepped systems own no solver memory and do not advance.
        // Re-enabling recreates Maxwell at the current scene tick, so its next
        // step remains aligned with the snapshot time instead of resuming stale
        // tick-one state under a later timestamp.
        let maxwell = fieldcad_electromagnetism::plugin_id();
        runtime.set_field_system_enabled(&maxwell, false).unwrap();
        runtime.step_once().unwrap();
        assert!(runtime.latest_snapshot().channels.is_empty());
        runtime.set_field_system_enabled(&maxwell, true).unwrap();
        runtime.step_once().unwrap();
        assert_eq!(runtime.latest_snapshot().identity.tick, 3);

        let before = runtime.clock_snapshot().time_step();
        let rejected = TimeStep::from_seconds(courant_limit(&domain) * 1.01).unwrap();
        assert!(matches!(
            runtime.set_time_step(rejected),
            Err(RuntimeError::Plugin(_))
        ));
        assert_eq!(runtime.clock_snapshot().time_step(), before);

        // The desktop-facing adapter returns immediately, then reports the same
        // rejection as a final event without poisoning the source or its clock.
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime));
        let submitted = source
            .execute(command(CommandPayload::SetTimeStep(rejected)))
            .unwrap();
        assert_eq!(submitted.disposition, CommandDisposition::Submitted);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let event = loop {
            source.poll(Duration::ZERO).unwrap();
            if let Some(event) = source.drain_command_events().into_iter().next() {
                break event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not reject the unstable time step"
            );
            std::thread::yield_now();
        };
        assert!(matches!(
            event,
            CommandEvent::Failed {
                error: SourceError::Solver { ref code, .. },
                ..
            } if code == "plugin"
        ));
        assert_eq!(source.simulation_status().time_step(), before);
    }

    #[test]
    fn a_queued_command_reports_completion_only_after_its_tick_boundary() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        source.execute(command(CommandPayload::Play)).unwrap();
        // Wait for the worker to acknowledge Play (always `Applied`, never
        // queued) and discard its completion event -- irrelevant here.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            source.poll(Duration::ZERO).unwrap();
            if !source.drain_command_events().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never applied Play"
            );
            std::thread::yield_now();
        }

        let queued = source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        assert_eq!(queued.disposition, CommandDisposition::Submitted);

        // Let the worker acknowledge both requests without crossing a tick
        // boundary (elapsed = ZERO every poll). Once the edit is visibly
        // queued, this alone must have produced no terminal event yet --
        // the exact bug this task fixes.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            source.poll(Duration::ZERO).unwrap();
            if source.get_queue().pending.iter().any(|record| {
                record.kind == CommandKind::CommitWorld && record.state == CommandLifecycle::Queued
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never queued the running edit"
            );
            std::thread::yield_now();
        }
        assert!(
            source.drain_command_events().is_empty(),
            "a queued acknowledgement must not be reported as terminal completion"
        );

        // Now cross a tick boundary; only now must the terminal event appear.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let event = loop {
            source.poll(Duration::from_millis(100)).unwrap();
            if let Some(event) = source.drain_command_events().into_iter().next() {
                break event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the queued edit never completed at a tick boundary"
            );
            std::thread::yield_now();
        };
        assert!(matches!(
            event,
            CommandEvent::Completed(ref receipt) if receipt.disposition == CommandDisposition::Applied
        ));
    }

    /// BE-16 regression: `queue_summary` must always agree with what
    /// `get_queue` would report, since it exists specifically to answer the
    /// same four questions (paused, pending/history counts, newest history
    /// id) without cloning `pending`/`history` to get them.
    #[test]
    fn queue_summary_agrees_with_get_queue() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        source.execute(command(CommandPayload::Play)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();

        // Cross a tick boundary so the queued edit goes terminal, giving
        // `history` something too — not just `pending`.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            source.poll(Duration::from_millis(100)).unwrap();
            if !source.get_queue().history.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the queued edit never went terminal"
            );
            std::thread::yield_now();
        }

        let queue = source.get_queue();
        let summary = source.queue_summary();
        assert_eq!(summary.paused, queue.paused);
        assert_eq!(summary.pending_len, queue.pending.len());
        assert_eq!(summary.history_len, queue.history.len());
        assert_eq!(
            summary.newest_history,
            queue.history.last().map(|record| record.command)
        );
    }

    /// A command that flushes another, already-queued command as its own
    /// side effect (`pause` flushing a running edit) must report that other
    /// command's completion without requiring a subsequent `Poll` request —
    /// regression test for a bug where only `Poll`'s worker-side handling
    /// drained buffered terminal events, so a side-effect flush produced
    /// during `Execute` sat invisible on the worker thread until some
    /// *unrelated*, later, non-zero-elapsed poll happened to run.
    #[test]
    fn a_side_effect_flush_reports_completion_without_a_subsequent_poll() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        source.execute(command(CommandPayload::Play)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            source.poll(Duration::ZERO).unwrap();
            if !source.drain_command_events().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never applied Play"
            );
            std::thread::yield_now();
        }

        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            source.poll(Duration::ZERO).unwrap();
            if source.get_queue().pending.iter().any(|record| {
                record.kind == CommandKind::CommitWorld && record.state == CommandLifecycle::Queued
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker never queued the running edit"
            );
            std::thread::yield_now();
        }

        source.execute(command(CommandPayload::Pause)).unwrap();

        // Only zero-elapsed polls from here on: nothing may cross a real
        // tick boundary. `Pause`'s own flush already applied the queued
        // edit synchronously, on the worker thread, inside its own
        // `execute` call — a later, non-zero-elapsed poll must not be
        // necessary to observe it.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let mut events = Vec::new();
        loop {
            source.poll(Duration::ZERO).unwrap();
            events.extend(source.drain_command_events());
            if events.len() >= 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the side-effect-flushed edit's completion never surfaced without a real tick"
            );
            std::thread::yield_now();
        }

        assert_eq!(
            events.len(),
            2,
            "expected exactly the flushed edit's completion and pause's own: {events:?}"
        );
        assert!(
            events.iter().all(|event| matches!(
                event,
                CommandEvent::Completed(receipt) if receipt.disposition == CommandDisposition::Applied
            )),
            "expected both the flushed edit and pause itself to have applied cleanly: {events:?}"
        );
        assert_eq!(source.simulation_status().mode(), SimulationMode::Paused);
        assert!(source.get_queue().pending.is_empty());
    }

    #[test]
    fn stepping_while_running_is_refused() {
        let mut runtime = runtime();
        runtime.play();

        assert!(matches!(
            runtime.step_once(),
            Err(RuntimeError::CannotStepWhileRunning)
        ));
    }

    #[test]
    fn probes_receive_known_scalar_and_vector_values() {
        let runtime = runtime();
        let snapshot = runtime.latest_snapshot();
        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();

        let scalar = snapshot.probe_sample(&scalar_channel_id(), probe).unwrap();
        assert_eq!(scalar.value.magnitude(), 14.0);
        assert_eq!(scalar.position, DVec3::new(1.0, 2.0, 3.0));

        let vector = snapshot.probe_sample(&vector_channel_id(), probe).unwrap();
        assert_eq!(vector.value.magnitude(), DVec3::new(1.0, 2.0, 3.0).length());
    }

    #[test]
    fn a_rejected_edit_leaves_the_world_and_solvers_untouched() {
        let mut runtime = runtime();
        let revision = runtime.world_snapshot().revision();
        let sequence = runtime.latest_snapshot().identity.sequence;

        let error = runtime.commit_world_commands(vec![
            WorldCommand::CreateObject(ObjectSpec::new("discarded")),
            WorldCommand::RemoveObject(ObjectId::new(500)),
        ]);

        assert!(error.is_err());
        assert_eq!(runtime.world_snapshot().revision(), revision);
        // No snapshot was published for a revision that was never adopted, so
        // the visible result cannot be pinned as permanently stale.
        assert_eq!(runtime.latest_snapshot().identity.sequence, sequence);
        assert_eq!(
            runtime
                .latest_snapshot()
                .freshness_against(runtime.world_snapshot().revision()),
            fieldcad_core::SnapshotFreshness::Current
        );
    }

    #[test]
    fn inactive_field_system_stops_simulating_but_retains_its_object_schema() {
        let plugin = electrostatics_plugin_id();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x51))
                .with_plugin_registration(
                    PluginRegistration::with_default_configuration(Box::new(
                        ElectrostaticsPlugin::new(),
                    ))
                    .with_enabled(false),
                ),
        )
        .unwrap();

        assert!(
            runtime
                .world_snapshot()
                .component_schemas()
                .contains_key(&charge_component_id())
        );
        assert!(!runtime.field_systems()[0].enabled);
        assert!(runtime.latest_snapshot().plugins.is_empty());
        assert!(runtime.latest_snapshot().channels.is_empty());

        // A charged object can still carry the plugin-contributed property while
        // the solver is inactive. This object deliberately has a box shape,
        // which the electrostatics solver itself cannot represent.
        let report = runtime
            .commit_world_commands(vec![WorldCommand::CreateObject(
                ObjectSpec::new("unfinished charged object")
                    .with_shape(ObjectShape::boxed(glam::DVec3::ONE).unwrap())
                    .with_component(
                        charge_component_id(),
                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                    ),
            )])
            .unwrap();
        let object = report.created_objects[0];
        assert!(
            runtime.world_snapshot().object(object).unwrap().components[&charge_component_id()]
                .scalar(&charge_property_id())
                .is_some()
        );

        // Reactivation validates the scene before changing authoritative state.
        assert!(matches!(
            runtime.set_field_system_enabled(&plugin, true),
            Err(RuntimeError::Plugin(_))
        ));
        assert!(!runtime.field_systems()[0].enabled);

        runtime
            .commit_world_commands(vec![WorldCommand::SetShape {
                object,
                shape: Some(ObjectShape::point(0.1).unwrap()),
            }])
            .unwrap();
        runtime.set_field_system_enabled(&plugin, true).unwrap();

        assert!(runtime.field_systems()[0].enabled);
        assert_eq!(
            runtime.latest_snapshot().plugins[0].id,
            electrostatics_plugin_id()
        );
    }

    #[test]
    fn electrostatic_and_maxwell_plugins_compose_one_shared_charge_schema() {
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();

        let runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(0x52))
                .with_plugin(Box::new(ElectrostaticsPlugin::new()))
                // Composed as an available model of the same electric field,
                // and therefore not active alongside the one that is.
                .with_plugin_registration(
                    PluginRegistration::with_default_configuration(Box::new(
                        ElectromagnetismPlugin::new(),
                    ))
                    .with_enabled(false),
                ),
        )
        .unwrap();

        assert_eq!(runtime.field_systems().len(), 2);
        // Charge, inertial mass, gravitational mass, and catalog provenance.
        // Charge is declared by both plugins and registered once.
        assert_eq!(runtime.world_snapshot().component_schemas().len(), 4);
        for shared in [
            fieldcad_sources::inertial_mass_component_id(),
            fieldcad_sources::gravitational_mass_component_id(),
        ] {
            assert!(
                runtime
                    .world_snapshot()
                    .component_schemas()
                    .contains_key(&shared),
                "{shared} should be registered once for every consumer"
            );
        }
        assert!(
            runtime
                .world_snapshot()
                .component_schemas()
                .contains_key(&charge_component_id())
        );
        assert!(
            runtime
                .world_snapshot()
                .component_schemas()
                .contains_key(&particle_component_id())
        );
    }

    /// Every component the inspector's "+ Add" menu can offer must survive the
    /// solvers that consume it, not merely its own schema.
    ///
    /// Schema validation only checks dimensions, so a mass of zero passes it and
    /// is then rejected by the pusher that has to divide by it — which made the
    /// menu item impossible to use. The gap is between the schema and its
    /// consumer, so the test has to cross that boundary: it commits the same
    /// `AttachComponent` the menu issues and lets validate-before-adopt rule.
    #[test]
    fn every_offered_component_can_actually_be_attached_to_a_bare_object() {
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(0x53))
                .with_plugin(Box::new(ElectrostaticsPlugin::new()))
                .with_plugin_registration(
                    PluginRegistration::with_default_configuration(Box::new(
                        ElectromagnetismPlugin::new(),
                    ))
                    .with_enabled(false),
                ),
        )
        .unwrap();

        let schemas: Vec<_> = runtime
            .world_snapshot()
            .component_schemas()
            .values()
            .cloned()
            .collect();
        assert!(!schemas.is_empty(), "no component schemas to exercise");

        for schema in schemas {
            let report = runtime
                .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new(format!(
                    "bare {}",
                    schema.display_name
                )))])
                .unwrap_or_else(|error| panic!("creating a bare object failed: {error}"));
            let object = report.created_objects[0];
            let properties = schema.default_properties().unwrap_or_else(|error| {
                panic!(
                    "{} has no attachable defaults: {error}",
                    schema.display_name
                )
            });

            runtime
                .commit_world_commands(vec![WorldCommand::AttachComponent {
                    object,
                    component: schema.id.clone(),
                    properties,
                }])
                .unwrap_or_else(|error| {
                    panic!(
                        "attaching {} with its own defaults was rejected: {error}",
                        schema.display_name
                    )
                });
        }
    }

    /// A heavy pinned source and a light free body to its right, both
    /// charged alike so the free body is repelled straight along +x. Shared
    /// by every test below that needs a real, nonzero force to exercise the
    /// dynamics coupling with.
    fn repelling_charges_scene(scheme: IntegrationScheme) -> (SimulationRuntime, ObjectId) {
        use fieldcad_sources::{
            inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
        };

        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x60))
                .with_plugin(Box::new(ElectrostaticsPlugin::new()))
                .with_integration_scheme(scheme),
        )
        .unwrap();
        let report = runtime
            .commit_world_commands(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema)
                    .chain([
                        WorldCommand::CreateObject(
                            ObjectSpec::new("source")
                                .with_pinned(true)
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                        WorldCommand::CreateObject(
                            ObjectSpec::new("free")
                                .with_transform(Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap())
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9))
                                        .unwrap(),
                                )
                                .with_component(
                                    inertial_mass_component_id(),
                                    inertial_mass_properties(MassKg::new::<kilogram>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                    ])
                    .collect(),
            )
            .unwrap();
        let free = report.created_objects[1];
        (runtime, free)
    }

    /// The whole coupling path in one test: an electrostatic field produces a
    /// force, the dynamics system turns it into motion, and the runtime adopts
    /// the result — with no equation system integrating anything itself.
    ///
    /// Pinned to Symplectic Euler: this test is about whether the field's
    /// force actually reaches the body in one step, not about how many ticks
    /// Velocity Verlet needs to warm its force cache (see
    /// `velocity_verlet_moves_a_body_once_its_force_cache_is_warm` below).
    #[test]
    fn a_field_moves_a_body_through_the_dynamics_system() {
        let (mut runtime, free) = repelling_charges_scene(IntegrationScheme::SymplecticEuler);

        runtime.step_once().unwrap();
        let moved = runtime.world_snapshot().object(free).unwrap().clone();

        // Like charges repel, so the free body accelerates away along +x and
        // nowhere else.
        assert!(
            moved.velocity.linear.x > 0.0,
            "expected repulsion, got {:?}",
            moved.velocity.linear
        );
        assert!(moved.transform.translation.x > 1.0);
        assert!(moved.velocity.linear.y.abs() < 1.0e-15);
        assert!(moved.velocity.linear.z.abs() < 1.0e-15);
    }

    #[test]
    fn velocity_verlet_moves_a_body_once_its_force_cache_is_warm() {
        // Velocity Verlet's default cold start: with no prior tick's force to
        // half-kick with, the first tick is velocity-only (the true force
        // still lands, evaluated at the pre-tick position). Position only
        // starts advancing once the cache is warm, from the second tick on.
        let (mut runtime, free) = repelling_charges_scene(IntegrationScheme::VelocityVerlet);
        assert_eq!(
            runtime.integration_scheme(),
            IntegrationScheme::VelocityVerlet,
            "Velocity Verlet is the new default"
        );

        runtime.step_once().unwrap();
        let after_first = runtime.world_snapshot().object(free).unwrap().clone();
        assert!(
            after_first.velocity.linear.x > 0.0,
            "expected repulsion, got {:?}",
            after_first.velocity.linear
        );
        assert_eq!(
            after_first.transform.translation.x, 1.0,
            "no prior force to half-kick with yet, so position hasn't moved"
        );

        runtime.step_once().unwrap();
        let after_second = runtime.world_snapshot().object(free).unwrap().clone();
        assert!(
            after_second.transform.translation.x > 1.0,
            "the cache warmed by the first tick should now move the body"
        );
    }

    #[test]
    fn switching_integration_scheme_clears_cached_dynamics_state() {
        let (mut runtime, free) = repelling_charges_scene(IntegrationScheme::VelocityVerlet);
        runtime.step_once().unwrap();
        assert!(runtime.body_force(free).is_some());
        assert!(runtime.body_history(free).count() > 0);

        runtime.set_integration_scheme(IntegrationScheme::SymplecticEuler);

        assert_eq!(
            runtime.integration_scheme(),
            IntegrationScheme::SymplecticEuler
        );
        assert!(
            runtime.body_force(free).is_none(),
            "a force cached under the previous scheme must not be reused"
        );
        assert_eq!(runtime.body_history(free).count(), 0);
    }

    /// The force an inspector would show for a body is a byproduct of the same
    /// tick that moved it, not a second computation — so it has to agree with
    /// the direction the body actually accelerated in, and be absent before
    /// any tick supplied one.
    #[test]
    fn body_force_reflects_the_most_recent_ticks_dynamics() {
        use fieldcad_sources::{
            inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
        };

        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x67))
                .with_plugin(Box::new(ElectrostaticsPlugin::new())),
        )
        .unwrap();
        let report = runtime
            .commit_world_commands(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema)
                    .chain([
                        WorldCommand::CreateObject(
                            ObjectSpec::new("source")
                                .with_pinned(true)
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                        WorldCommand::CreateObject(
                            ObjectSpec::new("free")
                                .with_transform(Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap())
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9))
                                        .unwrap(),
                                )
                                .with_component(
                                    inertial_mass_component_id(),
                                    inertial_mass_properties(MassKg::new::<kilogram>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                    ])
                    .collect(),
            )
            .unwrap();
        let free = report.created_objects[1];
        let source = report.created_objects[0];

        assert_eq!(runtime.body_force(free), None, "no tick has run yet");

        runtime.step_once().unwrap();

        let force = runtime
            .body_force(free)
            .expect("a dynamic body advanced by a tick has a force");
        assert!(force.x > 0.0, "expected repulsion along +x, got {force:?}");
        assert!(force.y.abs() < 1.0e-15);
        assert!(force.z.abs() < 1.0e-15);
        // A pinned body is never a dynamics-system input, so it never gets an
        // entry — not zero, absent.
        assert_eq!(runtime.body_force(source), None);
    }

    /// Inertia is the only thing dividing the force, so doubling it must halve
    /// the acceleration — whatever field supplied the force.
    #[test]
    fn inertial_mass_alone_sets_the_response_to_a_force() {
        use fieldcad_sources::{
            inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
        };

        let velocity_after_one_step = |mass_kg: f64| {
            let mut runtime = SimulationRuntime::new(
                RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x61))
                    .with_plugin(Box::new(ElectrostaticsPlugin::new())),
            )
            .unwrap();
            let report = runtime
                .commit_world_commands(
                    mass_component_schemas()
                        .into_iter()
                        .map(WorldCommand::RegisterComponentSchema)
                        .chain([
                            WorldCommand::CreateObject(
                                ObjectSpec::new("source")
                                    .with_pinned(true)
                                    .with_shape(ObjectShape::point(0.05).unwrap())
                                    .with_component(
                                        charge_component_id(),
                                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6))
                                            .unwrap(),
                                    ),
                            ),
                            WorldCommand::CreateObject(
                                ObjectSpec::new("free")
                                    .with_transform(
                                        Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap(),
                                    )
                                    .with_shape(ObjectShape::point(0.05).unwrap())
                                    .with_component(
                                        charge_component_id(),
                                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9))
                                            .unwrap(),
                                    )
                                    .with_component(
                                        inertial_mass_component_id(),
                                        inertial_mass_properties(MassKg::new::<kilogram>(mass_kg))
                                            .unwrap(),
                                    ),
                            ),
                        ])
                        .collect(),
                )
                .unwrap();
            let free = report.created_objects[1];
            runtime.step_once().unwrap();
            runtime
                .world_snapshot()
                .object(free)
                .unwrap()
                .velocity
                .linear
                .x
        };

        let light = velocity_after_one_step(1.0e-6);
        let heavy = velocity_after_one_step(2.0e-6);

        assert!(
            (light / heavy - 2.0).abs() < 1.0e-6,
            "doubling inertia should halve the response: {light} vs {heavy}"
        );
    }

    /// A charged body with no inertial mass is a source, not a projectile.
    #[test]
    fn a_body_without_inertial_mass_is_never_moved() {
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x62))
                .with_plugin(Box::new(ElectrostaticsPlugin::new())),
        )
        .unwrap();
        let report = runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    ObjectSpec::new("source")
                        .with_shape(ObjectShape::point(0.05).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("massless")
                        .with_transform(Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap())
                        .with_shape(ObjectShape::point(0.05).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
            ])
            .unwrap();
        let massless = report.created_objects[1];
        let before = runtime.world_snapshot().object(massless).unwrap().clone();

        runtime.step_once().unwrap();
        let after = runtime.world_snapshot().object(massless).unwrap().clone();

        assert_eq!(before.transform, after.transform);
        assert_eq!(before.velocity, after.velocity);
    }

    #[test]
    fn maxwell_alone_registers_and_consumes_the_shared_charge_schema() {
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(0x53))
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        )
        .unwrap();

        runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    ObjectSpec::new("charge")
                        .with_shape(ObjectShape::point(0.1).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
                WorldCommand::CreateProbe(ProbeSpec::at(
                    "Maxwell E",
                    DVec3::X,
                    vec![maxwell_e_channel_id()],
                )),
            ])
            .unwrap();

        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();
        let electric = runtime
            .latest_snapshot()
            .probe_sample(&maxwell_e_channel_id(), probe)
            .unwrap();
        assert!(electric.value.magnitude() > 0.0);
    }

    #[test]
    fn field_system_activation_is_a_transport_command() {
        let plugin = fieldcad_test_field::plugin_id();
        let mut source = LocalDataSource::new(runtime());

        source
            .execute(command(CommandPayload::SetFieldSystemEnabled {
                plugin: plugin.clone(),
                enabled: false,
            }))
            .unwrap();

        assert_eq!(source.field_systems()[0].plugin.id, plugin);
        assert!(!source.field_systems()[0].enabled);
        assert_eq!(
            source.field_systems()[0]
                .configuration_schema
                .properties
                .len(),
            1
        );
        assert_eq!(source.field_systems()[0].configuration.len(), 1);
        assert!(source.latest_snapshot().unwrap().channels.is_empty());
    }

    #[test]
    fn an_accepted_edit_is_observed_atomically_at_its_revision() {
        let mut runtime = runtime();

        let report = runtime
            .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new("source"))])
            .unwrap();
        let snapshot = runtime.latest_snapshot();

        assert_eq!(snapshot.identity.world_revision, report.revision);
        assert!(snapshot.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "observed-world-revision"
                && diagnostic.message.contains(&report.revision.to_string())
        }));
    }

    #[test]
    fn snapshots_carry_the_domain_that_produced_them() {
        let runtime = runtime();
        let snapshot = runtime.latest_snapshot();

        assert_eq!(snapshot.domain, domain());
        assert_eq!(snapshot.domain.resolution().cell_count(), 512);
        assert_eq!(snapshot.domain.precision(), fieldcad_core::Precision::F64);
    }

    #[test]
    fn visualization_density_changes_samples_but_not_the_domain() {
        let mut world = seeded_world();
        world
            .commit([WorldCommand::CreatePlane(
                SlicePlaneSpec::new("xy", DVec3::ZERO, DVec3::Z).unwrap(),
            )])
            .unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(9))
                .with_world(world)
                .with_subscription(Subscription::PROBES_ONLY.with_planes(UVec2::splat(4)))
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap();

        let coarse = runtime.latest_snapshot();
        let coarse_samples = coarse.total_samples();

        runtime
            .set_subscription(Subscription::PROBES_ONLY.with_planes(UVec2::splat(16)))
            .unwrap();
        let fine = runtime.latest_snapshot();

        assert!(fine.total_samples() > coarse_samples);
        // The physical configuration is untouched, and so is the world revision.
        assert_eq!(fine.domain, coarse.domain);
        assert_eq!(fine.identity.world_revision, coarse.identity.world_revision);
    }

    #[test]
    fn whole_domain_subscription_is_decimated_not_one_glyph_per_cell() {
        let runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(11))
                .with_world(seeded_world())
                .with_subscription(Subscription::PROBES_ONLY.with_domain_stride(4))
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap();
        let snapshot = runtime.latest_snapshot();

        let grid_samples: usize = snapshot
            .channels
            .values()
            .flat_map(|channel| channel.batches.iter())
            .filter(|batch| matches!(batch.geometry(), fieldcad_core::SampleGeometry::Grid(_)))
            .map(fieldcad_core::FieldBatch::len)
            .sum();

        // Eight glyphs per channel from a 512-cell domain.
        assert_eq!(grid_samples, 16);
        assert_eq!(snapshot.domain.resolution().cell_count(), 512);
    }

    #[test]
    fn samples_outside_the_domain_are_marked_undefined_not_clamped() {
        let mut world = World::new();
        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "far away",
                DVec3::splat(1_000.0),
                vec![scalar_channel_id()],
            ))])
            .unwrap();
        let runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(13))
                .with_world(world)
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap();

        let snapshot = runtime.latest_snapshot();
        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();
        let sample = snapshot.probe_sample(&scalar_channel_id(), probe).unwrap();

        assert_eq!(
            sample.validity,
            SampleValidity::Undefined(UndefinedReason::OutsideDomain)
        );
        assert!(!sample.validity.is_usable());
    }

    #[test]
    fn mailbox_keeps_the_newest_complete_snapshot_of_one_session() {
        let runtime = runtime();
        let current = runtime.latest_snapshot();
        let mut mailbox = SnapshotMailbox::default();

        assert!(mailbox.offer(Arc::clone(&current)).unwrap());
        // Re-offering the same result is normal under backpressure, not an error.
        assert!(!mailbox.offer(Arc::clone(&current)).unwrap());

        let mut partial = (*current).clone();
        partial.identity.sequence += 1;
        partial.completeness = SnapshotCompleteness::Partial;
        assert_eq!(
            mailbox.offer(Arc::new(partial)),
            Err(SnapshotRejection::Incomplete)
        );

        let mut foreign = (*current).clone();
        foreign.identity.sequence += 1;
        foreign.identity.session = SessionId::from_u128(999);
        assert_eq!(
            mailbox.offer(Arc::new(foreign)),
            Err(SnapshotRejection::UnexpectedSession)
        );

        assert_eq!(mailbox.sequence(), Some(current.identity.sequence));
    }

    /// Drive any data source through one script, and describe what a consumer
    /// would have seen. Used to check that swapping sources changes nothing.
    fn observed_script(source: &mut dyn FieldDataSource) -> Vec<String> {
        let mut sequencer = CommandSequencer::default();
        let mut history = ProbeHistory::new(64);
        let mut log = Vec::new();

        let mut record = |source: &mut dyn FieldDataSource, history: &mut ProbeHistory| {
            if let Some(snapshot) = source.latest_snapshot() {
                history.record(&snapshot);
                let status = source.simulation_status();
                let world = source.world();
                log.push(format!(
                    "seq={} rev={} tick={} mode={} samples={} freshness={} objects={} world_rev={}",
                    snapshot.identity.sequence,
                    snapshot.identity.world_revision,
                    snapshot.identity.tick,
                    status.mode().label(),
                    snapshot.total_samples(),
                    snapshot.freshness_against(status.world_revision).label(),
                    world.objects().len(),
                    world.revision(),
                ));
            } else {
                log.push("no data".to_owned());
            }
        };

        // Poll until the source has settled, so a source that delivers over a
        // link reaches the same visible state as one that does not.
        let settle = |source: &mut dyn FieldDataSource| {
            for _ in 0..4 {
                source.poll(Duration::ZERO).unwrap();
            }
        };

        settle(source);
        record(source, &mut history);

        let receipt = source
            .execute(sequencer.issue(CommandPayload::Step))
            .unwrap();
        assert_eq!(receipt.command, CommandId::new(0));
        settle(source);
        record(source, &mut history);

        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("added")),
            ])))
            .unwrap();
        settle(source);
        record(source, &mut history);

        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();
        source.poll(Duration::from_millis(250)).unwrap();
        settle(source);
        record(source, &mut history);

        source
            .execute(sequencer.issue(CommandPayload::Pause))
            .unwrap();
        settle(source);
        record(source, &mut history);

        let probe = ProbeHistory::tracked(&history)
            .map(|(probe, _)| probe)
            .next()
            .expect("the script records probe history");
        log.push(format!(
            "history={} first={:?}",
            history.len(probe, &scalar_channel_id()),
            history
                .readings(probe, &scalar_channel_id())
                .next()
                .map(|reading| reading.value.magnitude())
        ));
        log
    }

    #[test]
    fn local_and_loopback_sources_are_interchangeable_for_consumers() {
        let mut local = LocalDataSource::new(runtime());
        let mut remote = LoopbackDataSource::new(
            SimulationRuntime::new(
                RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(7))
                    .with_world(seeded_world())
                    .with_plugin(Box::new(TestFieldPlugin)),
            )
            .unwrap(),
        );

        assert_eq!(observed_script(&mut local), observed_script(&mut remote));
    }

    #[test]
    fn acknowledgements_are_correlated_with_their_commands() {
        let mut source = LocalDataSource::new(runtime());
        let mut sequencer = CommandSequencer::default();

        let first = sequencer.issue(CommandPayload::Step);
        let second = sequencer.issue(CommandPayload::Pause);
        let first_receipt = source.execute(first.clone()).unwrap();
        let second_receipt = source.execute(second.clone()).unwrap();

        assert_eq!(first_receipt.command, first.id);
        assert_eq!(second_receipt.command, second.id);
        assert_ne!(first.id, second.id);
        assert_eq!(first_receipt.tick, 1);
    }

    #[test]
    fn a_remote_acknowledgement_precedes_the_snapshot_it_describes() {
        let mut remote = LoopbackDataSource::new(runtime());
        remote.poll(Duration::ZERO).unwrap();

        let receipt = remote.execute(command(CommandPayload::Step)).unwrap();

        // The server has acknowledged the tick, but the client has not yet
        // received the snapshot for it.
        assert_eq!(receipt.tick, 1);
        assert_eq!(remote.latest_snapshot().unwrap().identity.tick, 0);

        remote.poll(Duration::ZERO).unwrap();
        assert_eq!(remote.latest_snapshot().unwrap().identity.tick, 1);
    }

    #[test]
    fn asynchronous_local_commands_complete_without_blocking_submission() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        let receipt = source.execute(command(CommandPayload::Step)).unwrap();

        assert_eq!(receipt.disposition, CommandDisposition::Submitted);
        assert_eq!(receipt.snapshot_sequence, None);
        assert_eq!(source.simulation_status().tick(), 0);

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let completed = loop {
            source.poll(Duration::ZERO).unwrap();
            if let Some(event) = source.drain_command_events().into_iter().next() {
                break event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not respond"
            );
            std::thread::yield_now();
        };

        let CommandEvent::Completed(receipt) = completed else {
            panic!("the valid step command must complete");
        };
        assert_eq!(receipt.disposition, CommandDisposition::Applied);
        assert_eq!(receipt.snapshot_sequence, Some(1));
        assert_eq!(source.simulation_status().tick(), 1);
        assert_eq!(source.latest_snapshot().unwrap().identity.tick, 1);
    }

    #[test]
    fn disconnect_retains_the_last_complete_snapshot_and_marks_it_stale() {
        let mut remote = LoopbackDataSource::new(runtime());
        remote.poll(Duration::ZERO).unwrap();
        let before = remote.latest_snapshot().unwrap().identity.sequence;

        remote.disconnect();

        assert_eq!(remote.status(), DataSourceStatus::Disconnected);
        assert!(matches!(
            remote.execute(command(CommandPayload::Step)),
            Err(SourceError::Disconnected)
        ));
        // The image stays up, identified as not current.
        assert_eq!(remote.latest_snapshot().unwrap().identity.sequence, before);
        assert_eq!(remote.poll(Duration::ZERO).unwrap(), PollOutcome::default());

        remote.reconnect();
        assert_eq!(remote.status(), DataSourceStatus::Ready);
        assert!(remote.execute(command(CommandPayload::Step)).is_ok());
    }

    /// The same body-force byproduct the runtime test checks directly, this
    /// time read through the `FieldDataSource` trait a real UI would use —
    /// the seam most likely to have a wiring bug, since the runtime method
    /// itself is a one-line map lookup.
    #[test]
    fn a_local_source_reports_body_force_through_the_trait_boundary() {
        use fieldcad_sources::{
            inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
        };

        let mut source = LocalDataSource::new(
            SimulationRuntime::new(
                RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x68))
                    .with_plugin(Box::new(ElectrostaticsPlugin::new())),
            )
            .unwrap(),
        );
        source
            .execute(command(CommandPayload::CommitWorld(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema)
                    .chain([
                        WorldCommand::CreateObject(
                            ObjectSpec::new("source")
                                .with_pinned(true)
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                        WorldCommand::CreateObject(
                            ObjectSpec::new("free")
                                .with_transform(Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap())
                                .with_shape(ObjectShape::point(0.05).unwrap())
                                .with_component(
                                    charge_component_id(),
                                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9))
                                        .unwrap(),
                                )
                                .with_component(
                                    inertial_mass_component_id(),
                                    inertial_mass_properties(MassKg::new::<kilogram>(1.0e-6))
                                        .unwrap(),
                                ),
                        ),
                    ])
                    .collect(),
            )))
            .unwrap();
        let free = *source
            .world()
            .objects()
            .iter()
            .find(|(_, object)| object.name == "free")
            .unwrap()
            .0;

        assert!(source.body_forces().is_empty(), "no tick has run yet");

        source.execute(command(CommandPayload::Step)).unwrap();

        let force = *source
            .body_forces()
            .get(&free)
            .expect("a dynamic body advanced by a tick has a force");
        assert!(force.x > 0.0, "expected repulsion along +x, got {force:?}");
    }

    #[test]
    fn wall_clock_time_advances_ticks_without_changing_dt() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();

        let outcome = source.poll(Duration::from_millis(350)).unwrap();

        assert_eq!(outcome.ticks_advanced, 3);
        assert_eq!(source.simulation_status().time_step(), time_step());
        assert_eq!(source.simulation_status().tick(), 3);
        // Exactly `tick * dt`, bit for bit. 0.1 s is not representable in
        // binary, so this is 0.30000000000000004 and not 0.3 — reproducibly so,
        // which is what the invariant actually promises.
        assert_eq!(
            source.simulation_status().time_seconds(),
            3.0 * time_step().seconds()
        );
    }

    #[test]
    fn playback_speed_changes_wall_clock_pacing_without_changing_dt() {
        let mut source = LocalDataSource::new(runtime());
        let speed = PlaybackSpeed::from_multiplier(2.0).unwrap();
        source
            .execute(command(CommandPayload::SetPlaybackSpeed(speed)))
            .unwrap();
        source.execute(command(CommandPayload::Play)).unwrap();

        let outcome = source.poll(Duration::from_millis(150)).unwrap();

        assert_eq!(outcome.ticks_advanced, 3);
        assert_eq!(source.playback_speed(), speed);
        assert_eq!(source.simulation_status().time_step(), time_step());
        assert_eq!(
            source.simulation_status().time_seconds(),
            3.0 * time_step().seconds()
        );
    }

    /// A subscription change is a visualization command: it is applied at once
    /// even while running, because it cannot make a solver observe half an edit.
    fn subscriptions_change_density_but_not_physics(source: &mut dyn FieldDataSource) {
        source.poll(Duration::ZERO).unwrap();
        source.execute(command(CommandPayload::Play)).unwrap();
        let before = source.latest_snapshot().unwrap();

        let receipt = source
            .execute(command(CommandPayload::SetSubscription(
                Subscription::PROBES_ONLY.with_domain_stride(2),
            )))
            .unwrap();
        // A loopback source delivers over a link, so drain it before comparing.
        for _ in 0..4 {
            source.poll(Duration::ZERO).unwrap();
        }
        let after = source.latest_snapshot().unwrap();

        assert_eq!(receipt.disposition, CommandDisposition::Applied);
        assert_eq!(source.pending_command_count(), 0);
        assert_eq!(
            source.subscription(),
            Subscription::PROBES_ONLY.with_domain_stride(2)
        );
        assert!(after.total_samples() > before.total_samples());
        // Denser observation, same world and same numerical configuration.
        assert_eq!(
            after.identity.world_revision,
            before.identity.world_revision
        );
        assert_eq!(after.domain, before.domain);
    }

    #[test]
    fn local_subscriptions_change_density_but_not_physics() {
        subscriptions_change_density_but_not_physics(&mut LocalDataSource::new(runtime()));
    }

    #[test]
    fn loopback_subscriptions_change_density_but_not_physics() {
        subscriptions_change_density_but_not_physics(&mut LoopbackDataSource::new(runtime()));
    }

    #[test]
    fn a_rejected_subscription_does_not_replace_the_acknowledged_one() {
        let mut source = LocalDataSource::new(runtime());
        let before = source.subscription();
        let sequence = source.latest_snapshot().unwrap().identity.sequence;

        let result = source.execute(command(CommandPayload::SetSubscription(
            Subscription::PROBES_ONLY.with_planes(UVec2::splat(1_025)),
        )));

        assert!(matches!(
            result,
            Err(SourceError::Solver { ref code, .. }) if code == "invalid-subscription"
        ));
        assert_eq!(source.subscription(), before);
        assert_eq!(
            source.latest_snapshot().unwrap().identity.sequence,
            sequence
        );
    }

    #[test]
    fn the_authoritative_source_enforces_a_total_sampling_budget() {
        let source_runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(31))
                .with_world(seeded_world())
                .with_sampling_budget(SamplingBudget {
                    max_plane_samples_per_axis: 32,
                    max_samples_per_snapshot: 1,
                })
                .with_plugin(Box::new(TestFieldPlugin)),
        );

        assert!(matches!(
            source_runtime,
            Err(RuntimeError::SamplingBudgetExceeded {
                requested: 2,
                limit: 1
            })
        ));
    }

    #[test]
    fn invalid_playback_speeds_are_rejected() {
        for multiplier in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(PlaybackSpeed::from_multiplier(multiplier).is_err());
        }
    }

    fn running_edit_is_applied_at_next_tick_boundary(source: &mut dyn FieldDataSource) {
        // Deliver a loopback source's initial snapshot before beginning the
        // script so local and remote presentation state start at the same point.
        source.poll(Duration::ZERO).unwrap();
        let initial_revision = source.world().revision();
        source.execute(command(CommandPayload::Play)).unwrap();

        let receipt = source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Queued);
        assert_eq!(source.pending_command_count(), 1);
        assert_eq!(source.world().revision(), initial_revision);
        assert!(source.world().objects().is_empty());

        let before_boundary = source.poll(Duration::from_millis(99)).unwrap();
        assert_eq!(before_boundary.ticks_advanced, 0);
        assert_eq!(before_boundary.commands_applied, 0);
        assert!(source.world().objects().is_empty());

        let at_boundary = source.poll(Duration::from_millis(1)).unwrap();
        assert_eq!(at_boundary.ticks_advanced, 1);
        assert_eq!(at_boundary.commands_applied, 1);
        assert_eq!(source.pending_command_count(), 0);
        assert_eq!(source.world().objects().len(), 1);
        assert_eq!(source.simulation_status().tick(), 1);
        assert_eq!(
            source.latest_snapshot().unwrap().identity.world_revision,
            source.world().revision()
        );
    }

    #[test]
    fn local_running_edits_are_queued_for_a_fixed_tick_boundary() {
        running_edit_is_applied_at_next_tick_boundary(&mut LocalDataSource::new(runtime()));
    }

    #[test]
    fn loopback_running_edits_are_queued_for_a_fixed_tick_boundary() {
        running_edit_is_applied_at_next_tick_boundary(&mut LoopbackDataSource::new(runtime()));
    }

    #[test]
    fn pausing_flushes_a_queued_edit_at_the_current_boundary() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();

        let receipt = source.execute(command(CommandPayload::Pause)).unwrap();

        assert_eq!(receipt.disposition, CommandDisposition::Applied);
        assert_eq!(source.pending_command_count(), 0);
        assert_eq!(source.world().objects().len(), 1);
        assert_eq!(source.simulation_status().tick(), 0);
        assert_eq!(source.simulation_status().mode(), SimulationMode::Paused);
    }

    #[test]
    fn pausing_the_queue_holds_a_running_edit_across_tick_boundaries() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();

        let receipt = source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Queued);

        let pause_receipt = source.execute(command(CommandPayload::PauseQueue)).unwrap();
        assert_eq!(pause_receipt.disposition, CommandDisposition::Applied);
        assert!(source.get_queue().paused);

        for _ in 0..5 {
            let outcome = source.poll(Duration::from_millis(100)).unwrap();
            assert_eq!(outcome.commands_applied, 0);
        }
        assert!(source.world().objects().is_empty());
        assert_eq!(source.get_queue().pending.len(), 1);

        let resume_receipt = source
            .execute(command(CommandPayload::ResumeQueue))
            .unwrap();
        assert_eq!(resume_receipt.disposition, CommandDisposition::Applied);
        assert!(!source.get_queue().paused);

        let outcome = source.poll(Duration::from_millis(100)).unwrap();
        assert_eq!(outcome.commands_applied, 1);
        assert_eq!(source.world().objects().len(), 1);
        assert!(source.get_queue().pending.is_empty());
    }

    #[test]
    fn resuming_a_paused_queue_applies_pending_edits_in_submission_order() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();

        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("first")),
            ])))
            .unwrap();
        source.execute(command(CommandPayload::PauseQueue)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("second")),
            ])))
            .unwrap();
        assert_eq!(source.get_queue().pending.len(), 2);

        source
            .execute(command(CommandPayload::ResumeQueue))
            .unwrap();
        source.poll(Duration::from_millis(100)).unwrap();

        assert!(source.get_queue().pending.is_empty());
        assert_eq!(source.world().objects().len(), 2);

        let history = source.get_queue().history;
        let commit_history: Vec<_> = history
            .iter()
            .filter(|record| record.kind == CommandKind::CommitWorld)
            .collect();
        assert_eq!(commit_history.len(), 2);
        assert!(commit_history[0].sequence < commit_history[1].sequence);
        assert_eq!(commit_history[0].state, CommandLifecycle::Applied);
        assert_eq!(commit_history[1].state, CommandLifecycle::Applied);
    }

    /// ADR 0011 says an edit submitted while `Paused` is immediate — true
    /// with an idle queue, but a mutation queue the user has explicitly
    /// paused is a stronger promise than "no tick boundary to wait for".
    /// Regression for the desktop's viewport-drag deferral: a plain-object
    /// move released while both the simulation and the queue are paused
    /// must sit exactly like a `Running` one would, not slip through the
    /// mode check and apply on the spot.
    #[test]
    fn pausing_the_queue_holds_a_paused_simulations_edit_too() {
        let mut source = LocalDataSource::new(runtime());
        assert_eq!(source.simulation_status().mode(), SimulationMode::Paused);

        source.execute(command(CommandPayload::PauseQueue)).unwrap();
        let receipt = source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("held while paused")),
            ])))
            .unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Queued);
        assert!(source.world().objects().is_empty());
        assert_eq!(source.get_queue().pending.len(), 1);
    }

    /// Unlike the `Running` case (where a resumed queue waits for the next
    /// tick boundary — see `resuming_a_paused_queue_applies_pending_edits_in_submission_order`),
    /// a `Paused` simulation has no boundary to wait for. But `ResumeQueue`
    /// itself does not flush a `Paused` simulation's held backlog
    /// synchronously either (that would report the whole backlog as one
    /// final state, losing the same per-edit feedback a live edit gets —
    /// see `flush_one_pending_mutation`): it drains one held edit per
    /// `poll` instead, so resuming must not leave anything stranded past
    /// the very next poll.
    #[test]
    fn resuming_the_queue_drains_a_paused_simulations_held_edit_on_the_next_poll() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::PauseQueue)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("held while paused")),
            ])))
            .unwrap();

        source
            .execute(command(CommandPayload::ResumeQueue))
            .unwrap();

        // Not yet flushed by `ResumeQueue` itself...
        assert_eq!(source.get_queue().pending.len(), 1);
        assert!(source.world().objects().is_empty());

        // ...but drains on the very next poll, with no unrelated command
        // needed to flush it.
        source.poll(Duration::ZERO).unwrap();
        assert!(source.get_queue().pending.is_empty());
        assert_eq!(source.world().objects().len(), 1);
    }

    /// The heart of the fix: a multi-edit backlog drains exactly one edit
    /// per `poll`, not all of it on the first poll after resuming — a real
    /// UI polling once per frame therefore sees the object move through
    /// its held positions one at a time, the same way it would have while
    /// live-dragging, instead of freezing and then jumping straight to the
    /// final position.
    #[test]
    fn resuming_the_queue_drains_a_multi_edit_backlog_one_poll_at_a_time() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::PauseQueue))
            .unwrap();

        let names = ["first", "second", "third"];
        for name in names {
            source
                .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
                    WorldCommand::CreateObject(ObjectSpec::new(name)),
                ])))
                .unwrap();
        }
        assert_eq!(source.get_queue().pending.len(), names.len());

        source
            .execute(sequencer.issue(CommandPayload::ResumeQueue))
            .unwrap();

        for expected_objects in 1..=names.len() {
            assert_eq!(
                source.get_queue().pending.len(),
                names.len() - expected_objects + 1,
                "still {} left pending before this poll",
                names.len() - expected_objects + 1
            );
            source.poll(Duration::ZERO).unwrap();
            assert_eq!(
                source.world().objects().len(),
                expected_objects,
                "poll #{expected_objects} should apply exactly one more held edit, not the whole backlog"
            );
        }
        assert!(source.get_queue().pending.is_empty());
    }

    #[test]
    fn cancelling_a_queued_command_prevents_its_application() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let queued = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("queued object")),
        ]));
        let queued_id = queued.id;
        let receipt = source.execute(queued).unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Queued);

        let cancel_receipt = source
            .execute(sequencer.issue(CommandPayload::CancelQueuedCommand(queued_id)))
            .unwrap();
        assert_eq!(cancel_receipt.disposition, CommandDisposition::Applied);

        let queue = source.get_queue();
        assert!(queue.pending.is_empty());
        let cancelled = queue
            .history
            .iter()
            .find(|record| record.command == queued_id)
            .expect("the cancelled command is retained in terminal history");
        assert_eq!(cancelled.state, CommandLifecycle::Cancelled);

        source.poll(Duration::from_millis(100)).unwrap();
        assert!(source.world().objects().is_empty());
    }

    #[test]
    fn cancelling_an_unknown_or_already_applied_command_is_refused() {
        let mut source = LocalDataSource::new(runtime());
        let result = source.execute(command(CommandPayload::CancelQueuedCommand(
            CommandId::new(999),
        )));
        assert!(matches!(result, Err(SourceError::CommandNotQueued(_))));
    }

    /// BE-6 regression: cancelling a command that is still in flight (in the
    /// mpsc channel, not yet acknowledged by the worker) must return a clear
    /// `CommandInFlight` error, not a misleading "not found" error.
    #[test]
    fn cancelling_a_command_that_is_still_in_flight_returns_command_in_flight() {
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));
        source.execute(command(CommandPayload::Play)).unwrap();

        let world_cmd = command(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("test object")),
        ]));
        let target_id = world_cmd.id;
        let receipt = source.execute(world_cmd).unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Submitted);

        // drain_worker_events only runs on poll(), not on execute(), so
        // the target is still in submitted_commands at this point even if
        // the worker already acknowledged it — the events buffer hasn't
        // been drained yet.
        let result = source.execute(command(CommandPayload::CancelQueuedCommand(target_id)));
        assert!(
            matches!(result, Err(SourceError::CommandInFlight(id)) if id == target_id),
            "expected CommandInFlight({target_id:?}), got {result:?}"
        );

        // Clean up: drain events so the worker doesn't stall.
        source.poll(Duration::ZERO).unwrap();
    }

    fn queue_paused_conflict(payload: CommandPayload) -> Result<CommandReceipt, SourceError> {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        source.execute(command(CommandPayload::PauseQueue)).unwrap();
        source.execute(command(payload))
    }

    /// BE-7 regression: a queue-paused rejection must record a `Rejected`
    /// terminal record and emit `CommandEvent::Failed`, so clients can find
    /// the outcome through queue history rather than seeing a silent error.
    #[test]
    fn a_queue_paused_rejection_leaves_a_terminal_history_entry() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();
        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        source
            .execute(sequencer.issue(CommandPayload::PauseQueue))
            .unwrap();

        let step = sequencer.issue(CommandPayload::Step);
        let step_id = step.id;
        let result = source.execute(step);
        assert!(matches!(result, Err(SourceError::QueuePaused { .. })));

        // The rejected command must appear in queue history.
        let queue = source.get_queue();
        let rejected = queue
            .history
            .iter()
            .find(|record| record.command == step_id)
            .expect("the rejected step must appear in terminal history");
        assert_eq!(rejected.state, CommandLifecycle::Rejected);
        assert!(
            rejected.error.is_some(),
            "a rejected terminal record carries an error message"
        );

        // A CommandEvent::Failed must have been emitted.
        let events = source.drain_command_events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                CommandEvent::Failed { command, .. } if *command == step_id
            )),
            "a CommandEvent::Failed must be emitted for the rejected command"
        );
    }

    /// BE-7 regression for the async path: a queue-paused rejection must emit
    /// `CommandEvent::Failed` exactly once, not twice (once from the worker's
    /// `terminal` drain and once from the `CommandFailed` wrapper).
    #[test]
    fn a_queue_paused_rejection_emits_failed_exactly_once_via_async_source() {
        let mut sequencer = CommandSequencer::default();
        let mut source = AsyncLocalDataSource::new(LocalDataSource::new(runtime()));

        // Helper: pump until pending_command_count drops to the expected
        // level, meaning the worker has caught up.
        let wait_for_pending = |source: &mut AsyncLocalDataSource, expected: usize| {
            for _ in 0..1000 {
                source.poll(Duration::from_millis(1)).unwrap();
                if source.pending_command_count() == expected {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!(
                "worker never caught up: pending = {}, expected {expected}",
                source.pending_command_count()
            );
        };

        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();
        wait_for_pending(&mut source, 0);

        source
            .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("queued object")),
            ])))
            .unwrap();
        wait_for_pending(&mut source, 1);

        source
            .execute(sequencer.issue(CommandPayload::PauseQueue))
            .unwrap();
        wait_for_pending(&mut source, 1);

        source.drain_command_events();

        let step = sequencer.issue(CommandPayload::Step);
        let step_id = step.id;
        let result = source.execute(step);
        assert!(result.is_ok(), "async execute returns Submitted, not Err");

        // Pump until the worker processes the Step and we can drain events.
        for _ in 0..1000 {
            source.poll(Duration::from_millis(1)).unwrap();
            let events = source.drain_command_events();
            if !events.is_empty() {
                let failed_count = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            CommandEvent::Failed { command, .. } if *command == step_id
                        )
                    })
                    .count();
                assert_eq!(
                    failed_count, 1,
                    "CommandEvent::Failed must be emitted exactly once, not {failed_count}"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("worker never processed the Step command");
    }

    #[test]
    fn pause_step_redo_are_refused_while_the_queue_is_paused_with_pending_work() {
        for payload in [
            CommandPayload::Pause,
            CommandPayload::Step,
            CommandPayload::Redo,
        ] {
            let result = queue_paused_conflict(payload);
            assert!(
                matches!(result, Err(SourceError::QueuePaused { .. })),
                "expected a queue-paused conflict, got {result:?}"
            );
        }
    }

    /// Undo does not share `Pause`/`Step`/`Redo`'s rejection: a still-pending
    /// edit has not been recorded in `EditHistory` yet, so "the last action
    /// I took hasn't landed" is cancelled instead of erroring.
    #[test]
    fn undo_cancels_the_most_recently_queued_command_when_the_queue_is_paused() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let queued = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("queued object")),
        ]));
        let queued_id = queued.id;
        assert_eq!(
            source.execute(queued).unwrap().disposition,
            CommandDisposition::Queued
        );

        source
            .execute(sequencer.issue(CommandPayload::PauseQueue))
            .unwrap();

        let receipt = source
            .execute(sequencer.issue(CommandPayload::Undo))
            .unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Applied);

        let queue = source.get_queue();
        assert!(queue.pending.is_empty());
        let cancelled = queue
            .history
            .iter()
            .find(|record| record.command == queued_id)
            .expect("the cancelled command is retained in terminal history");
        assert_eq!(cancelled.state, CommandLifecycle::Cancelled);

        let events = source.drain_command_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CommandEvent::Cancelled(id) if *id == queued_id)),
            "a CommandEvent::Cancelled must be emitted for the undone pending command"
        );

        source.poll(Duration::from_millis(100)).unwrap();
        assert!(source.world().objects().is_empty());
    }

    /// Undo picks the newest pending record (LIFO), leaving earlier ones
    /// still queued rather than cancelling everything.
    #[test]
    fn undo_cancels_only_the_most_recently_queued_command_leaving_earlier_ones_pending() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let first = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("first queued object")),
        ]));
        let first_id = first.id;
        assert_eq!(
            source.execute(first).unwrap().disposition,
            CommandDisposition::Queued
        );

        let second = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("second queued object")),
        ]));
        let second_id = second.id;
        assert_eq!(
            source.execute(second).unwrap().disposition,
            CommandDisposition::Queued
        );

        let receipt = source
            .execute(sequencer.issue(CommandPayload::Undo))
            .unwrap();
        assert_eq!(receipt.disposition, CommandDisposition::Applied);

        let queue = source.get_queue();
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(
            queue.pending[0].command, first_id,
            "only the newest pending record is cancelled"
        );

        let cancelled = queue
            .history
            .iter()
            .find(|record| record.command == second_id)
            .expect("the cancelled command is retained in terminal history");
        assert_eq!(cancelled.state, CommandLifecycle::Cancelled);
    }

    fn flush_rejected_conflict(
        payload: CommandPayload,
    ) -> (
        LocalDataSource,
        CommandId,
        Result<CommandReceipt, SourceError>,
    ) {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let invalid = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveObject(ObjectId::new(500)),
        ]));
        assert_eq!(
            source.execute(invalid).unwrap().disposition,
            CommandDisposition::Queued
        );

        let valid = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::CreateObject(ObjectSpec::new("queued object")),
        ]));
        let valid_id = valid.id;
        assert_eq!(
            source.execute(valid).unwrap().disposition,
            CommandDisposition::Queued
        );

        let result = source.execute(sequencer.issue(payload));
        (source, valid_id, result)
    }

    /// `Pause`/`Step`/`Undo`/`Redo` all flush the pending queue before doing
    /// anything else. If that flush rejects a mutation, the command must not
    /// proceed on top of a boundary whose preceding queued edit never
    /// actually landed — regression test for a flush that was briefly
    /// infallible and silently ignored at these four call sites.
    #[test]
    fn pause_step_redo_are_refused_when_their_own_flush_rejects_a_mutation() {
        for payload in [
            CommandPayload::Pause,
            CommandPayload::Step,
            CommandPayload::Redo,
        ] {
            let (source, valid_id, result) = flush_rejected_conflict(payload);
            assert!(
                matches!(result, Err(SourceError::FlushRejected { .. })),
                "expected a flush-rejected conflict, got {result:?}"
            );
            // The mode must be left unchanged so the still-queued valid
            // mutation gets another flush attempt on the next tick, instead
            // of being stranded by a state change to non-`Running`.
            assert_eq!(source.simulation_status().mode(), SimulationMode::Running);
            let queue = source.get_queue();
            assert_eq!(queue.pending.len(), 1);
            assert_eq!(queue.pending[0].command, valid_id);
        }
    }

    /// A flush rejection vetoes this cycle's ticks, but the wall-clock
    /// budget those ticks were paid for must be handed back to the pacer —
    /// otherwise every rejected flush silently discards up to a whole
    /// poll's worth of simulation time (BE-10). Regression test.
    #[test]
    fn a_rejected_flush_hands_its_tick_budget_back() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        // An invalid mutation the flush will reject (removing an object
        // that does not exist), queued while running.
        let invalid = sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveObject(ObjectId::new(500)),
        ]));
        assert_eq!(
            source.execute(invalid).unwrap().disposition,
            CommandDisposition::Queued
        );

        // Three ticks' worth of wall clock: the flush rejects, so no ticks
        // run this cycle. dt is 0.1s and the per-poll budget is 8 ticks.
        let rejected = source.poll(Duration::from_millis(300)).unwrap();
        assert_eq!(rejected.ticks_advanced, 0);

        // The rejection terminal-recorded and removed the bad mutation, so
        // the next poll's flush is clean — and the three ticks the rejected
        // cycle was paid for are still owed.
        let catch_up = source.poll(Duration::ZERO).unwrap();
        assert_eq!(catch_up.ticks_advanced, 3);

        // Normal pacing continues from there.
        let next = source.poll(Duration::from_millis(100)).unwrap();
        assert_eq!(next.ticks_advanced, 1);
    }

    #[test]
    fn pause_step_undo_redo_proceed_normally_when_the_queue_is_paused_but_empty() {
        for payload in [
            CommandPayload::Pause,
            CommandPayload::Step,
            CommandPayload::Undo,
            CommandPayload::Redo,
        ] {
            let mut source = LocalDataSource::new(runtime());
            source.execute(command(CommandPayload::PauseQueue)).unwrap();
            let result = source.execute(command(payload));
            assert!(
                !matches!(result, Err(SourceError::QueuePaused { .. })),
                "an idle paused queue must not block commands, got {result:?}"
            );
        }
    }

    #[test]
    fn terminal_history_evicts_its_oldest_entry_past_256_records() {
        let mut sequencer = CommandSequencer::default();
        let mut source = LocalDataSource::new(runtime());
        source
            .execute(sequencer.issue(CommandPayload::Play))
            .unwrap();

        let mut ids = Vec::new();
        for index in 0..300 {
            let queued = sequencer.issue(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new(format!("object {index}"))),
            ]));
            ids.push(queued.id);
            let receipt = source.execute(queued).unwrap();
            assert_eq!(receipt.disposition, CommandDisposition::Queued);
        }
        // A single flush (triggered here by `Pause`, which is never itself
        // queued) applies every one of the 300 queued mutations in one pass.
        source
            .execute(sequencer.issue(CommandPayload::Pause))
            .unwrap();

        let queue = source.get_queue();
        assert!(queue.pending.is_empty());
        assert_eq!(queue.history.len(), 256);
        assert_eq!(queue.history.first().unwrap().command, ids[300 - 256]);
        assert_eq!(queue.history.last().unwrap().command, *ids.last().unwrap());
    }

    fn milestone_four_recording() -> SessionRecording {
        SessionRecording::new()
            .with_poll(Duration::ZERO)
            .with_command(CommandPayload::SetPlaybackSpeed(
                PlaybackSpeed::from_multiplier(2.0).unwrap(),
            ))
            .with_command(CommandPayload::Play)
            .with_poll(Duration::from_millis(49))
            .with_command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("recorded object")),
            ]))
            .with_poll(Duration::from_millis(1))
            .with_poll(Duration::from_millis(250))
            .with_command(CommandPayload::Pause)
            .with_command(CommandPayload::SetTimeStep(
                TimeStep::from_seconds(0.025).unwrap(),
            ))
            .with_command(CommandPayload::Step)
            .with_poll(Duration::ZERO)
    }

    #[test]
    fn a_recorded_command_sequence_replays_bit_identically() {
        let recording = milestone_four_recording();
        let first = recording
            .replay(&mut LocalDataSource::new(runtime()))
            .unwrap();
        let second = recording
            .replay(&mut LocalDataSource::new(runtime()))
            .unwrap();

        assert_eq!(first, second);
        let final_state = first.last().unwrap();
        assert_eq!(final_state.simulation.tick(), 7);
        assert_eq!(
            final_state.simulation.time_seconds(),
            6.0 * time_step().seconds() + 0.025
        );
        assert_eq!(final_state.simulation.time_step().seconds(), 0.025);
        assert_eq!(final_state.pending_commands, 0);
    }

    #[test]
    fn loopback_replay_is_deterministic_despite_deferred_snapshots() {
        let recording = milestone_four_recording();
        let first = recording
            .replay(&mut LoopbackDataSource::new(runtime()))
            .unwrap();
        let second = recording
            .replay(&mut LoopbackDataSource::new(runtime()))
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.last().unwrap().simulation.tick(), 7);
    }

    #[test]
    fn a_source_that_falls_behind_reports_it_instead_of_stretching_dt() {
        let mut source = LocalDataSource::new(runtime());
        source.execute(command(CommandPayload::Play)).unwrap();

        let outcome = source.poll(Duration::from_secs(30)).unwrap();

        assert!(outcome.fell_behind);
        assert_eq!(source.simulation_status().time_step(), time_step());
        // Exactly the budgeted number of whole ticks were taken.
        assert_eq!(
            source.simulation_status().time_seconds(),
            f64::from(outcome.ticks_advanced) * time_step().seconds()
        );
    }

    #[test]
    fn changing_dt_while_paused_does_not_rewrite_recorded_history() {
        let mut source = LocalDataSource::new(runtime());
        let mut history = ProbeHistory::new(16);

        source.execute(command(CommandPayload::Step)).unwrap();
        history.record(&source.latest_snapshot().unwrap());
        let recorded = source.latest_snapshot().unwrap().identity.time_seconds;

        source
            .execute(command(CommandPayload::SetTimeStep(
                TimeStep::from_seconds(0.5).unwrap(),
            )))
            .unwrap();
        source.execute(command(CommandPayload::Step)).unwrap();
        history.record(&source.latest_snapshot().unwrap());

        let probe = *source
            .runtime()
            .world_snapshot()
            .probes()
            .keys()
            .next()
            .unwrap();
        let times: Vec<_> = history
            .readings(probe, &scalar_channel_id())
            .map(|reading| reading.time_seconds)
            .collect();

        assert_eq!(recorded, 0.1);
        assert_eq!(times, vec![0.1, 0.6]);
    }

    #[test]
    fn probe_history_is_bounded_and_does_not_duplicate_snapshots() {
        let mut source = LocalDataSource::new(runtime());
        let mut history = ProbeHistory::new(3);
        let probe = *source
            .runtime()
            .world_snapshot()
            .probes()
            .keys()
            .next()
            .unwrap();

        for _ in 0..10 {
            source.execute(command(CommandPayload::Step)).unwrap();
            history.record(&source.latest_snapshot().unwrap());
            // Recording the same snapshot again must not add a sample.
            history.record(&source.latest_snapshot().unwrap());
        }

        assert_eq!(history.len(probe, &scalar_channel_id()), 3);
        let ticks: Vec<_> = history
            .readings(probe, &scalar_channel_id())
            .map(|reading| reading.tick)
            .collect();
        assert_eq!(ticks, vec![8, 9, 10]);
    }

    #[test]
    fn probe_readings_carry_their_snapshot_provenance() {
        let mut source = LocalDataSource::new(runtime());
        let mut history = ProbeHistory::new(8);
        source.execute(command(CommandPayload::Step)).unwrap();
        let snapshot = source.latest_snapshot().unwrap();
        history.record(&snapshot);

        let probe = *source
            .runtime()
            .world_snapshot()
            .probes()
            .keys()
            .next()
            .unwrap();
        let reading = history
            .readings(probe, &scalar_channel_id())
            .next()
            .unwrap();

        assert_eq!(reading.snapshot_sequence, snapshot.identity.sequence);
        assert_eq!(reading.world_revision, snapshot.identity.world_revision);
        assert_eq!(reading.tick, snapshot.identity.tick);
        assert_eq!(reading.value.magnitude(), 14.0);
    }

    #[test]
    fn an_analytic_only_runtime_declares_it_does_not_evolve() {
        assert!(!runtime().has_time_dependent_solver());
    }

    fn electrostatic_runtime() -> SimulationRuntime {
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(
                Domain::centred_cube(4.0, 16).unwrap(),
                time_step(),
                SessionId::from_u128(21),
            )
            .with_subscription(
                Subscription::PROBES_ONLY
                    .with_planes(UVec2::splat(5))
                    .with_domain_stride(8),
            )
            .with_plugin(Box::new(ElectrostaticsPlugin::new())),
        )
        .unwrap();
        runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    ObjectSpec::new("positive point charge")
                        .with_transform(Transform::at(DVec3::ZERO).unwrap())
                        .with_shape(ObjectShape::point(0.1).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
                WorldCommand::CreateProbe(ProbeSpec::at(
                    "one metre on x",
                    DVec3::X,
                    vec![electric_field_channel_id(), electric_potential_channel_id()],
                )),
                WorldCommand::CreatePlane(
                    SlicePlaneSpec::new("XY field", DVec3::ZERO, DVec3::Z).unwrap(),
                ),
            ])
            .unwrap();
        // Setting the fixture up is not something a user did.
        runtime.clear_edit_history();
        runtime
    }

    #[test]
    fn electrostatic_channels_publish_probe_plane_and_grid_batches() {
        let runtime = electrostatic_runtime();
        let snapshot = runtime.latest_snapshot();
        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();

        let field = snapshot
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap();
        let potential = snapshot
            .probe_sample(&electric_potential_channel_id(), probe)
            .unwrap();
        assert!((field.value.magnitude() - COULOMB_CONSTANT * 1.0e-9).abs() < 1.0e-12);
        assert!((potential.value.magnitude() - COULOMB_CONSTANT * 1.0e-9).abs() < 1.0e-12);

        for channel in snapshot.channels.values() {
            assert!(
                channel
                    .batches
                    .iter()
                    .any(|batch| matches!(batch.geometry(), SampleGeometry::Probes { .. }))
            );
            assert!(
                channel
                    .batches
                    .iter()
                    .any(|batch| matches!(batch.geometry(), SampleGeometry::Plane { .. }))
            );
            assert!(
                channel
                    .batches
                    .iter()
                    .any(|batch| matches!(batch.geometry(), SampleGeometry::Grid(_)))
            );
        }
    }

    #[test]
    fn electrostatic_plane_marks_the_point_source_exclusion_without_non_finite_data() {
        let runtime = electrostatic_runtime();
        let snapshot = runtime.latest_snapshot();
        let channel = &snapshot.channels[&electric_field_channel_id()];
        let plane = channel
            .batches
            .iter()
            .find(|batch| matches!(batch.geometry(), SampleGeometry::Plane { .. }))
            .unwrap();

        assert!(plane.validity().contains(&SampleValidity::Undefined(
            UndefinedReason::InsideSourceRadius
        )));
        assert!(plane.values().first_non_finite().is_none());
    }

    #[test]
    fn analytic_electrostatic_plane_remains_sampled_after_it_grows_beyond_the_grid_domain() {
        let mut runtime = electrostatic_runtime();
        let plane_id = *runtime.world_snapshot().planes().keys().next().unwrap();
        let plane = runtime.world_snapshot().planes()[&plane_id].clone();

        runtime
            .commit_world_commands(vec![WorldCommand::SetPlane {
                plane: plane_id,
                spec: SlicePlaneSpec::from_plane(&plane)
                    .with_half_extent(DVec2::splat(8.0))
                    .unwrap(),
            }])
            .unwrap();

        let snapshot = runtime.latest_snapshot();
        let batch = snapshot.channels[&electric_field_channel_id()]
            .batches
            .iter()
            .find(|batch| batch.geometry().plane_id() == Some(plane_id))
            .unwrap();
        let SampleGeometry::Plane { lattice, .. } = batch.geometry() else {
            panic!("plane identity must resolve to plane geometry");
        };
        let far_corner = lattice.len() - 1;

        let corner = lattice.position(far_corner).unwrap();
        assert_eq!(corner.abs(), DVec3::new(8.0, 8.0, 0.0));
        assert!(batch.validity()[far_corner].is_usable());
    }

    #[test]
    fn moving_a_charge_invalidates_and_recomputes_published_samples() {
        let mut runtime = electrostatic_runtime();
        let before = runtime.latest_snapshot();
        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();
        let object = *runtime.world_snapshot().objects().keys().next().unwrap();
        let before_field = before
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value
            .magnitude();

        runtime
            .commit_world_commands(vec![WorldCommand::SetTransform {
                object,
                transform: Transform::at(DVec3::X * 0.5).unwrap(),
            }])
            .unwrap();
        let after = runtime.latest_snapshot();
        let after_field = after
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value
            .magnitude();

        assert_eq!(after.identity.sequence, before.identity.sequence + 1);
        assert_eq!(
            after.identity.world_revision,
            before.identity.world_revision.next()
        );
        assert!((after_field - before_field * 4.0).abs() < 1.0e-12);
        assert!(!Arc::ptr_eq(
            &before.channels[&electric_field_channel_id()].batches,
            &after.channels[&electric_field_channel_id()].batches
        ));
    }

    /// A system that declares one arbitrary channel and computes nothing.
    ///
    /// Enough to stand in for a third-party model of a field this scene already
    /// has, which is what the composition rules have to be checked against.
    struct ChannelPlugin(fieldcad_core::ChannelSchema);

    impl fieldcad_plugin_api::EquationSystemPlugin for ChannelPlugin {
        fn metadata(&self) -> fieldcad_plugin_api::PluginMetadata {
            fieldcad_plugin_api::PluginMetadata {
                id: fieldcad_core::PluginId::new("fieldcad.impostor").unwrap(),
                version: fieldcad_core::PluginVersion::new(0, 1, 0),
                display_name: "Impostor".to_owned(),
                description: "Declares one channel for composition tests".to_owned(),
            }
        }

        fn channels(&self) -> Vec<fieldcad_core::ChannelSchema> {
            vec![self.0.clone()]
        }

        fn create_solver(
            &self,
            _context: fieldcad_plugin_api::SolverContext<'_>,
        ) -> Result<
            Box<dyn fieldcad_plugin_api::EquationSystemSolver>,
            fieldcad_plugin_api::PluginError,
        > {
            Ok(Box::new(ChannelSolver))
        }
    }

    struct ChannelSolver;

    impl fieldcad_plugin_api::EquationSystemSolver for ChannelSolver {
        fn on_world_changed(
            &mut self,
            _world: &fieldcad_core::WorldSnapshot,
        ) -> Result<(), fieldcad_plugin_api::PluginError> {
            Ok(())
        }

        fn sample(
            &self,
            channel: fieldcad_plugin_api::ChannelHandle,
            _geometry: &SampleGeometry,
        ) -> Result<fieldcad_plugin_api::SampledColumn, fieldcad_plugin_api::PluginError> {
            Err(fieldcad_plugin_api::PluginError::UnknownChannel(
                channel.index(),
            ))
        }
    }

    /// A scene composing both models of the electric field, with one active.
    fn two_model_runtime(session: u128) -> SimulationRuntime {
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(session))
                .with_plugin(Box::new(ElectrostaticsPlugin::new()))
                .with_plugin_registration(
                    PluginRegistration::with_default_configuration(Box::new(
                        ElectromagnetismPlugin::new(),
                    ))
                    .with_enabled(false),
                ),
        )
        .unwrap();
        runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    ObjectSpec::new("charge")
                        .with_shape(ObjectShape::point(0.1).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        ),
                ),
                // Both fields are recorded from the start. A probe names
                // channels, not systems, so it keeps asking for the magnetic
                // field across a change of model — and simply receives nothing
                // while no active model computes one.
                WorldCommand::CreateProbe(ProbeSpec::at(
                    "field recorder",
                    DVec3::X * 0.5,
                    vec![
                        electric_field_channel_id(),
                        fieldcad_electromagnetism::magnetic_field_channel_id(),
                    ],
                )),
            ])
            .unwrap();
        runtime
    }

    /// The point of the whole arrangement: a scene has *one* electric field.
    /// Two systems that model it declare the same channel, so the identity a
    /// probe records and a layer draws does not change when the model does.
    #[test]
    fn both_models_of_the_electric_field_declare_the_same_field() {
        assert_eq!(
            fieldcad_electrostatics::electric_field_channel_id(),
            fieldcad_electromagnetism::electric_field_channel_id(),
            "electrostatics and Maxwell must compute one electric field, not two"
        );

        let runtime = two_model_runtime(0x80);
        let electric: Vec<_> = runtime
            .field_systems()
            .iter()
            .filter(|system| {
                system
                    .channels
                    .iter()
                    .any(|channel| channel.id == electric_field_channel_id())
            })
            .map(|system| system.plugin.id.clone())
            .collect();

        assert_eq!(electric.len(), 2, "both systems offer the same field");
        // And exactly one of them is computing it.
        assert_eq!(
            runtime.field_provider(&electric_field_channel_id()),
            Some(electrostatics_plugin_id())
        );
    }

    /// Two active models of one field would publish contradictory values under
    /// one name and each contribute the force their own version exerts. The
    /// composition is refused rather than double-solved.
    #[test]
    fn one_field_cannot_be_computed_by_two_active_models() {
        let domain = Domain::new(
            DomainBounds::centred_cube(2.0).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();

        let refused = SimulationRuntime::new(
            RuntimeConfig::new(domain, step, SessionId::from_u128(0x81))
                .with_plugin(Box::new(ElectrostaticsPlugin::new()))
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        );

        match refused {
            Ok(_) => panic!("two active models of one field must not compose"),
            Err(error) => assert_eq!(error.code(), "conflicting-field-provider"),
        }

        // Nor by switching the second on afterwards.
        let mut runtime = two_model_runtime(0x82);
        let maxwell = fieldcad_electromagnetism::plugin_id();
        let error = runtime
            .set_field_system_enabled(&maxwell, true)
            .unwrap_err();

        assert_eq!(error.code(), "conflicting-field-provider");
        assert_eq!(
            runtime.field_provider(&electric_field_channel_id()),
            Some(electrostatics_plugin_id()),
            "a refused activation leaves the model that was computing the field"
        );
    }

    /// Choosing a model swaps which system computes the field, in one step, and
    /// brings whatever else that model computes with it.
    #[test]
    fn choosing_a_model_replaces_the_one_computing_the_field() {
        let mut source = LocalDataSource::new(two_model_runtime(0x83));
        let maxwell = fieldcad_electromagnetism::plugin_id();
        let magnetic = fieldcad_electromagnetism::magnetic_field_channel_id();
        let probe = *source.world().probes().keys().next().unwrap();
        let analytic = source
            .latest_snapshot()
            .unwrap()
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value
            .magnitude();
        assert!(analytic > 0.0);
        assert!(
            source
                .latest_snapshot()
                .unwrap()
                .channel(&magnetic)
                .is_none()
        );

        source
            .execute(command(CommandPayload::SetFieldModel {
                channel: electric_field_channel_id(),
                provider: Some(maxwell.clone()),
            }))
            .unwrap();

        // Same field identity, different model — and the magnetic field the
        // chosen model also computes has arrived with it.
        let snapshot = source.latest_snapshot().unwrap();
        let channel = snapshot.channel(&electric_field_channel_id()).unwrap();
        assert_eq!(channel.provider, maxwell);
        assert!(snapshot.channel(&magnetic).is_some());
        assert_eq!(
            source
                .field_systems()
                .iter()
                .filter(|system| system.enabled)
                .count(),
            1,
            "choosing a model must not leave the previous one also active"
        );

        // And back again, without the electric field ever becoming two things.
        source
            .execute(command(CommandPayload::SetFieldModel {
                channel: electric_field_channel_id(),
                provider: Some(electrostatics_plugin_id()),
            }))
            .unwrap();
        let snapshot = source.latest_snapshot().unwrap();
        assert_eq!(
            snapshot
                .channel(&electric_field_channel_id())
                .unwrap()
                .provider,
            electrostatics_plugin_id()
        );
        assert!(snapshot.channel(&magnetic).is_none());
    }

    /// One solver computes all of its fields or none of them — Maxwell cannot
    /// advance `E` without `B` — so asking for a magnetic field takes the
    /// electric one with it. Refusing instead would leave the magnetic field
    /// unreachable from its own control, since its only model overlaps the
    /// active one.
    #[test]
    fn choosing_a_model_from_one_field_takes_the_rest_of_that_model_with_it() {
        let mut runtime = two_model_runtime(0x86);
        let maxwell = fieldcad_electromagnetism::plugin_id();
        let magnetic = fieldcad_electromagnetism::magnetic_field_channel_id();
        assert_eq!(
            runtime.field_provider(&electric_field_channel_id()),
            Some(electrostatics_plugin_id())
        );
        assert_eq!(runtime.field_provider(&magnetic), None);

        runtime.set_field_model(&magnetic, Some(&maxwell)).unwrap();

        assert_eq!(runtime.field_provider(&magnetic), Some(maxwell.clone()));
        assert_eq!(
            runtime.field_provider(&electric_field_channel_id()),
            Some(maxwell),
            "the model that computes B is now the model of E too"
        );
        // Which is a swap, not an addition.
        assert_eq!(
            runtime
                .field_systems()
                .iter()
                .filter(|system| system.enabled)
                .count(),
            1
        );
    }

    /// A field may also have no model, which is a scene with no such field
    /// rather than a scene with a broken one.
    #[test]
    fn a_field_can_be_left_uncomputed() {
        let mut runtime = two_model_runtime(0x84);

        runtime
            .set_field_model(&electric_field_channel_id(), None)
            .unwrap();

        assert_eq!(runtime.field_provider(&electric_field_channel_id()), None);
        assert!(runtime.latest_snapshot().channels.is_empty());
        assert!(runtime.field_systems().iter().all(|system| !system.enabled));
    }

    /// Systems declaring one field must describe it identically, or they are
    /// not models of the same thing. Caught before any solver exists.
    #[test]
    fn systems_disagreeing_about_a_field_do_not_compose() {
        let mismatched = fieldcad_core::ChannelSchema {
            id: electric_field_channel_id(),
            display_name: "Electric field, but in different units".to_owned(),
            value_kind: fieldcad_core::FieldValueKind::Scalar(fieldcad_core::Dimension::CHARGE),
        };
        let config = RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x85))
            .with_plugin(Box::new(ElectrostaticsPlugin::new()))
            .with_plugin(Box::new(ChannelPlugin(mismatched)));

        match SimulationRuntime::new(config) {
            Ok(_) => panic!("incompatible descriptions of one field must not compose"),
            Err(error) => assert_eq!(error.code(), "conflicting-channel-schema"),
        }
    }

    /// The base case, and the invariant that shapes everything else: undo
    /// restores *contents*, and does so as a new revision. A revision is a point
    /// in history, not a place to go back to.
    #[test]
    fn an_authored_edit_is_stepped_back_and_forward_by_moving_history_forward() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        let before = source.world().revision();
        assert!(!source.edit_history().can_undo());

        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("second charge")),
            ])))
            .unwrap();
        let added = source.world().revision();
        assert_eq!(source.world().objects().len(), 2);
        assert_eq!(source.edit_history().undo.as_deref(), Some("Add object"));

        source.execute(command(CommandPayload::Undo)).unwrap();

        assert_eq!(source.world().objects().len(), 1);
        assert!(
            source.world().revision() > added,
            "undo must not rewind the revision"
        );
        assert_eq!(source.edit_history().redo.as_deref(), Some("Add object"));
        assert!(!source.edit_history().can_undo());

        source.execute(command(CommandPayload::Redo)).unwrap();

        assert_eq!(source.world().objects().len(), 2);
        assert!(source.edit_history().can_undo());
        assert!(!source.edit_history().can_redo());
        assert!(source.world().revision() > before);
    }

    /// Undoing a creation frees nothing. If the next object could take the
    /// undone one's identifier, anything keyed by identifier — a probe
    /// attachment, a recorded series — would silently inherit its past.
    #[test]
    fn undo_does_not_recycle_identifiers() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("first")),
            ])))
            .unwrap();
        let first = *source.world().objects().keys().next_back().unwrap();

        source.execute(command(CommandPayload::Undo)).unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("second")),
            ])))
            .unwrap();
        let second = *source.world().objects().keys().next_back().unwrap();

        assert_ne!(first, second);
    }

    /// A drag submits an edit every frame. Undo has to reverse the gesture the
    /// user made, not the last mouse position they passed through.
    #[test]
    fn one_interactive_edit_is_one_undo_step() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        let object = *source.world().objects().keys().next().unwrap();
        let start = source.world().object(object).unwrap().transform.translation;

        source
            .execute(command(CommandPayload::SetInteractiveEdit(true)))
            .unwrap();
        for x in [0.1, 0.2, 0.3, 0.4, 0.5] {
            drag_charge_to(&mut source, x);
        }
        source
            .execute(command(CommandPayload::SetInteractiveEdit(false)))
            .unwrap();
        assert_eq!(source.edit_history().undo_depth, 1);

        source.execute(command(CommandPayload::Undo)).unwrap();

        assert_eq!(
            source.world().object(object).unwrap().transform.translation,
            start,
            "one gesture must step back to where the drag began"
        );
        assert!(!source.edit_history().can_undo());
    }

    /// A step back followed by a new edit is a new branch. Offering the
    /// abandoned one would reapply an edit that no longer follows from anything.
    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("abandoned")),
            ])))
            .unwrap();
        source.execute(command(CommandPayload::Undo)).unwrap();
        assert!(source.edit_history().can_redo());

        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateProbe(ProbeSpec::at("kept", DVec3::X, Vec::new())),
            ])))
            .unwrap();

        assert!(!source.edit_history().can_redo());
        assert_eq!(source.edit_history().undo.as_deref(), Some("Add probe"));
    }

    /// A rejected edit changed nothing, so offering to undo it would step the
    /// user back past an edit they did make.
    #[test]
    fn a_rejected_edit_leaves_nothing_to_undo() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        assert!(!source.edit_history().can_undo());

        let rejected = source.execute(command(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveObject(ObjectId::new(500)),
        ])));

        assert!(rejected.is_err());
        assert!(!source.edit_history().can_undo());
    }

    /// An undo names a scene. While the clock advances, the scene it names is
    /// being replaced underneath it, so the answer is to pause — the same one
    /// click single-stepping already needs.
    #[test]
    fn stepping_through_history_while_running_is_refused() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::CreateObject(ObjectSpec::new("added")),
            ])))
            .unwrap();
        source.execute(command(CommandPayload::Play)).unwrap();

        assert!(matches!(
            source.execute(command(CommandPayload::Undo)),
            Err(SourceError::Solver { ref code, .. })
                if code == "cannot-edit-history-while-running"
        ));

        source.execute(command(CommandPayload::Pause)).unwrap();
        assert!(source.execute(command(CommandPayload::Undo)).is_ok());
        assert_eq!(source.world().objects().len(), 1);
    }

    /// Once a solver has moved a body, the authored scene the entries describe
    /// is not the world any more. Restoring one would drag every integrated body
    /// back without rewinding the clock, which is not an undo of anything a user
    /// did.
    #[test]
    fn solver_motion_discards_the_authored_history() {
        use fieldcad_sources::{
            inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
        };

        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x70))
                .with_plugin(Box::new(ElectrostaticsPlugin::new())),
        )
        .unwrap();
        runtime
            .commit_world_commands(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema)
                    .collect(),
            )
            .unwrap();
        runtime
            .commit_world_commands(vec![
                WorldCommand::CreateObject(
                    ObjectSpec::new("source")
                        .with_pinned(true)
                        .with_shape(ObjectShape::point(0.05).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-6)).unwrap(),
                        ),
                ),
                WorldCommand::CreateObject(
                    ObjectSpec::new("free")
                        .with_transform(Transform::at(DVec3::X).unwrap())
                        .with_shape(ObjectShape::point(0.05).unwrap())
                        .with_component(
                            charge_component_id(),
                            charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                        )
                        .with_component(
                            inertial_mass_component_id(),
                            inertial_mass_properties(MassKg::new::<kilogram>(1.0e-6)).unwrap(),
                        ),
                ),
            ])
            .unwrap();
        assert!(runtime.edit_history().can_undo());

        runtime.step_once().unwrap();

        assert!(
            !runtime.edit_history().can_undo(),
            "a tick that moves a body leaves no authored scene to return to"
        );
    }

    /// The depth is a bound, not a promise to retain everything.
    #[test]
    fn the_undo_stack_is_bounded_and_drops_its_oldest_entries() {
        let mut runtime = SimulationRuntime::new(
            RuntimeConfig::new(domain(), time_step(), SessionId::from_u128(0x71))
                .with_undo_depth(2)
                .with_plugin(Box::new(TestFieldPlugin)),
        )
        .unwrap();

        for index in 0..5 {
            runtime
                .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new(format!(
                    "object {index}"
                )))])
                .unwrap();
        }

        assert_eq!(runtime.edit_history().undo_depth, 2);
        runtime.undo().unwrap();
        runtime.undo().unwrap();
        assert!(!runtime.edit_history().can_undo());
        // Two of five creations were stepped back; the rest are beyond the bound.
        assert_eq!(runtime.world_snapshot().objects().len(), 3);
    }

    /// Read one probe's electric field magnitude out of the published snapshot.
    fn published_field(source: &dyn FieldDataSource) -> f64 {
        let world = source.world();
        let probe = *world.probes().keys().next().unwrap();
        source
            .latest_snapshot()
            .unwrap()
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value
            .magnitude()
    }

    /// Move the charge to `x`, as one frame of a drag would.
    fn drag_charge_to(source: &mut dyn FieldDataSource, x: f64) {
        let object = *source.world().objects().keys().next().unwrap();
        source
            .execute(command(CommandPayload::CommitWorld(vec![
                WorldCommand::SetTransform {
                    object,
                    transform: Transform::at(DVec3::X * x).unwrap(),
                },
            ])))
            .unwrap();
    }

    /// The default: every intermediate pose of a drag is solved, which is what
    /// makes a cheap analytic field feel attached to the object being moved.
    #[test]
    fn a_realtime_system_follows_every_intermediate_value_of_an_edit() {
        let mut source = LocalDataSource::new(electrostatic_runtime());
        assert!(source.field_systems()[0].realtime);
        let before = published_field(&source);

        source
            .execute(command(CommandPayload::SetInteractiveEdit(true)))
            .unwrap();
        drag_charge_to(&mut source, 0.5);

        assert!(
            (published_field(&source) - before * 4.0).abs() < 1.0e-12,
            "a realtime system must republish mid-gesture"
        );
    }

    /// Turning realtime update off is a cost choice, not a physical one: the
    /// same committed world has to produce the same field, just once instead of
    /// once per frame of the gesture.
    #[test]
    fn a_non_realtime_system_holds_its_result_until_the_edit_is_committed() {
        let plugin = electrostatics_plugin_id();
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::SetFieldSystemRealtime {
                plugin,
                realtime: false,
            }))
            .unwrap();
        let before = published_field(&source);
        let sequence = source.latest_snapshot().unwrap().identity.sequence;

        source
            .execute(command(CommandPayload::SetInteractiveEdit(true)))
            .unwrap();
        for x in [0.9, 0.8, 0.7, 0.6, 0.5] {
            drag_charge_to(&mut source, x);
            assert_eq!(
                published_field(&source),
                before,
                "an intermediate pose of a gesture must not be solved for"
            );
        }
        // The world itself moved all along — only the solving waited.
        assert!(source.latest_snapshot().unwrap().identity.sequence > sequence);
        assert_eq!(
            source
                .world()
                .objects()
                .values()
                .next()
                .unwrap()
                .transform
                .translation,
            DVec3::X * 0.5
        );
        assert!(
            source
                .latest_snapshot()
                .unwrap()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "deferred-during-edit"),
            "a held result must say that it is held"
        );

        source
            .execute(command(CommandPayload::SetInteractiveEdit(false)))
            .unwrap();

        // Exactly what continuous update would have arrived at: half the
        // distance is four times the field.
        assert!((published_field(&source) - before * 4.0).abs() < 1.0e-12);
        assert!(
            !source
                .latest_snapshot()
                .unwrap()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "deferred-during-edit")
        );
    }

    /// A gesture is not a licence to accept a world the solver cannot represent.
    /// Deferral skips the recompute, never the validation.
    #[test]
    fn an_edit_a_deferred_solver_cannot_represent_is_still_rejected_mid_gesture() {
        let plugin = electrostatics_plugin_id();
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::SetFieldSystemRealtime {
                plugin,
                realtime: false,
            }))
            .unwrap();
        source
            .execute(command(CommandPayload::SetInteractiveEdit(true)))
            .unwrap();
        let object = *source.world().objects().keys().next().unwrap();
        let revision = source.world().revision();

        let rejected = source.execute(command(CommandPayload::CommitWorld(vec![
            WorldCommand::SetShape {
                object,
                shape: Some(ObjectShape::boxed(DVec3::ONE).unwrap()),
            },
        ])));

        assert!(rejected.is_err());
        assert_eq!(source.world().revision(), revision);
    }

    /// A drag that never moved anything, or a value typed and left unchanged,
    /// must not cost a solve on release.
    #[test]
    fn a_gesture_that_commits_nothing_republishes_nothing() {
        let plugin = electrostatics_plugin_id();
        let mut source = LocalDataSource::new(electrostatic_runtime());
        source
            .execute(command(CommandPayload::SetFieldSystemRealtime {
                plugin,
                realtime: false,
            }))
            .unwrap();
        source
            .execute(command(CommandPayload::SetInteractiveEdit(true)))
            .unwrap();
        let sequence = source.latest_snapshot().unwrap().identity.sequence;

        source
            .execute(command(CommandPayload::SetInteractiveEdit(false)))
            .unwrap();

        assert_eq!(
            source.latest_snapshot().unwrap().identity.sequence,
            sequence
        );
    }

    #[test]
    fn hiding_a_charge_changes_presentation_not_physics() {
        let mut runtime = electrostatic_runtime();
        let probe = *runtime.world_snapshot().probes().keys().next().unwrap();
        let object = *runtime.world_snapshot().objects().keys().next().unwrap();
        let before = runtime
            .latest_snapshot()
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value;

        runtime
            .commit_world_commands(vec![WorldCommand::SetObjectVisible {
                object,
                visible: false,
            }])
            .unwrap();

        let after = runtime
            .latest_snapshot()
            .probe_sample(&electric_field_channel_id(), probe)
            .unwrap()
            .value;
        assert_eq!(before, after);
        assert!(!runtime.world_snapshot().object(object).unwrap().visible);
    }

    #[test]
    fn analytic_batches_are_reused_across_ticks_until_an_input_changes() {
        let mut runtime = runtime();
        let before = runtime.latest_snapshot();
        let before_batches = &before.channels[&scalar_channel_id()].batches;

        runtime.step_once().unwrap();
        let after = runtime.latest_snapshot();
        let after_batches = &after.channels[&scalar_channel_id()].batches;

        assert_eq!(after.identity.tick, before.identity.tick + 1);
        assert_eq!(after.identity.sequence, before.identity.sequence + 1);
        assert!(Arc::ptr_eq(before_batches, after_batches));
    }

    #[test]
    fn simulation_mode_is_reported_through_the_source_boundary() {
        let mut source = LocalDataSource::new(runtime());
        assert_eq!(source.simulation_status().mode(), SimulationMode::Paused);

        source.execute(command(CommandPayload::Play)).unwrap();
        assert_eq!(source.simulation_status().mode(), SimulationMode::Running);

        source.execute(command(CommandPayload::Pause)).unwrap();
        assert_eq!(source.simulation_status().mode(), SimulationMode::Paused);
    }
}
