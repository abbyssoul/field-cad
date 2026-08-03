//! Headless simulation runtime and the field-data-source boundary.
//!
//! The visualizer talks to a [`FieldDataSource`], never to a solver. Two
//! implementations exist: [`LocalDataSource`] wrapping an in-process runtime, and
//! [`LoopbackDataSource`] standing in for a dedicated compute service. They are
//! required to be interchangeable, and the tests in this crate check that by
//! driving both through the same script.

pub mod async_source;
pub mod history;
pub mod recording;
pub mod runtime;
pub mod source;

pub use async_source::{AsyncLocalDataSource, CommandEvent};
pub use history::{ProbeHistory, ProbeReading};
pub use recording::{RecordedEvent, ReplayObservation, SessionRecording};
pub use runtime::{
    FieldSystemStatus, PluginRegistration, RuntimeConfig, RuntimeError, SamplingBudget,
    SimulationRuntime, SimulationStatus, Subscription, TickDemand, TickPacer,
};
pub use source::{
    Command, CommandDisposition, CommandId, CommandPayload, CommandReceipt, CommandSequencer,
    DataSourceStatus, FieldDataSource, LocalDataSource, LoopbackDataSource, PlaybackSpeed,
    PlaybackSpeedError, PollOutcome, SnapshotMailbox, SnapshotRejection, SourceError,
};

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, Domain, DomainBounds, ObjectId, ObjectShape,
        ObjectSpec, Precision, ProbeSpec, Resolution, SampleGeometry, SampleValidity, SessionId,
        SimulationMode, SlicePlaneSpec, SnapshotCompleteness, TimeStep, Transform, UndefinedReason,
        World, WorldCommand,
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
        // the solver is inactive. This object deliberately has no shape, which
        // the electrostatics solver itself cannot represent.
        let report = runtime
            .commit_world_commands(vec![WorldCommand::CreateObject(
                ObjectSpec::new("unfinished charged object")
                    .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
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
                .with_plugin(Box::new(ElectromagnetismPlugin::new())),
        )
        .unwrap();

        assert_eq!(runtime.field_systems().len(), 2);
        assert_eq!(runtime.world_snapshot().component_schemas().len(), 2);
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
                        .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
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
                        .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
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
