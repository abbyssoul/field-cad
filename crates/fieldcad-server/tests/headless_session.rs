//! Proves the model runs headless, end to end, through the same command
//! surface a future transport will use — no window, no GPU device anywhere
//! in this crate's dependency graph.

use std::time::{Duration, Instant};

use fieldcad_core::{
    ObjectShape, ObjectSpec, ProbeSpec, Transform, WorldCommand,
    quantities::{ChargeCoulombs, MassKg, coulomb, kilogram},
};
use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
use fieldcad_electrostatics::electric_field_channel_id;
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{CommandEvent, CommandPayload};
use fieldcad_sources::{inertial_mass_component_id, inertial_mass_properties};
use glam::DVec3;

/// [`HeadlessServer`] wraps a non-blocking source (ADR 0011): a submission
/// returns `Submitted` immediately and completes on a worker thread. Tests
/// pick up completion the same way any transport eventually will — poll and
/// drain events until one arrives.
fn submit_and_wait(server: &mut HeadlessServer, payload: CommandPayload) -> CommandEvent {
    server.submit(payload).expect("command is accepted");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        server.advance(Duration::ZERO).expect("advance succeeds");
        if let Some(event) = server.drain_events().into_iter().next() {
            return event;
        }
        assert!(Instant::now() < deadline, "worker did not respond");
        std::thread::yield_now();
    }
}

fn charge_and_probe() -> Vec<WorldCommand> {
    vec![
        WorldCommand::CreateObject(
            ObjectSpec::new("Point charge")
                .with_transform(Transform::at(DVec3::ZERO).unwrap())
                .with_shape(ObjectShape::point(0.1).unwrap())
                .with_component(
                    charge_component_id(),
                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                ),
        ),
        WorldCommand::CreateProbe(ProbeSpec::at(
            "Field probe",
            DVec3::new(1.0, 0.0, 0.0),
            vec![electric_field_channel_id()],
        )),
    ]
}

#[test]
fn a_headless_session_authors_a_scene_and_steps_with_no_gpu() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let before = server.simulation_status();
    assert_eq!(before.tick(), 0);

    let authored = submit_and_wait(&mut server, CommandPayload::CommitWorld(charge_and_probe()));
    assert!(
        matches!(authored, CommandEvent::Completed(_)),
        "authoring the default scene is accepted: {authored:?}"
    );

    let stepped = submit_and_wait(&mut server, CommandPayload::Step);
    assert!(
        matches!(stepped, CommandEvent::Completed(_)),
        "a step is accepted once a scene exists: {stepped:?}"
    );

    let after = server.simulation_status();
    assert_eq!(after.tick(), 1);

    let snapshot = server
        .latest_snapshot()
        .expect("a step produces a snapshot");
    assert!(snapshot.is_complete());
}

#[test]
fn capture_document_round_trips_through_the_async_worker() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let authored = submit_and_wait(&mut server, CommandPayload::CommitWorld(charge_and_probe()));
    assert!(matches!(authored, CommandEvent::Completed(_)));

    let (world, queue) = server
        .capture_document()
        .expect("capture succeeds against a healthy session");
    assert!(!queue.paused);
    assert!(queue.pending.is_empty());

    // The captured world must match what the ordinary read path reports —
    // proof the blocking round-trip actually reached the worker's live
    // session rather than returning stale/default data.
    let live = server.world();
    let reloaded = fieldcad_core::World::from_document(world);
    assert_eq!(reloaded.snapshot().objects(), live.objects());
    assert_eq!(reloaded.snapshot().probes(), live.probes());
}

#[test]
fn save_run_retains_a_copy_of_recorded_probe_history() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let authored = submit_and_wait(&mut server, CommandPayload::CommitWorld(charge_and_probe()));
    assert!(matches!(authored, CommandEvent::Completed(_)));
    let stepped = submit_and_wait(&mut server, CommandPayload::Step);
    assert!(matches!(stepped, CommandEvent::Completed(_)));

    let record = server.save_run("with-history".to_owned());

    assert!(
        !record.probe_history.series.is_empty(),
        "server-side history should have recorded the field probe's reading after a step"
    );
    assert_eq!(
        record.run_generation,
        server.simulation_status().run_generation
    );
}

#[test]
fn a_recorded_session_replays_through_the_server_level_api() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    server.start_recording().expect("recording starts");
    submit_and_wait(&mut server, CommandPayload::CommitWorld(charge_and_probe()));
    submit_and_wait(&mut server, CommandPayload::Step);
    submit_and_wait(&mut server, CommandPayload::Step);
    let recording = server.stop_recording().expect("recording stops");
    assert_eq!(recording.events().len(), 3);

    // Replay into two independent fresh sessions and require the same
    // final observable state — the server-level equivalence property
    // `recording.rs`'s own synchronous-source tests establish, reproduced
    // here over the async transport this crate actually runs on.
    let replay_into_fresh_session =
        |recording: &fieldcad_simulation::recording::SessionRecording| {
            let source = fieldcad_server::default_session().expect("default session builds");
            let mut server = HeadlessServer::new(source);
            server.replay_recording(recording).expect("replay succeeds")
        };

    let first = replay_into_fresh_session(&recording);
    let second = replay_into_fresh_session(&recording);

    assert_eq!(first.len(), 3);
    assert_eq!(
        first, second,
        "replaying the same recording twice must agree"
    );
    let final_step = first.last().unwrap();
    assert_eq!(final_step.simulation.tick(), 2);
    assert!(matches!(
        final_step.command_event,
        Some(CommandEvent::Completed(_))
    ));
    assert!(
        !final_step.world.objects().is_empty(),
        "the replayed CommitWorld must have actually authored the charge/probe"
    );
}

#[test]
fn starting_a_recording_twice_is_a_structured_error() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    server.start_recording().unwrap();
    let error = server.start_recording().unwrap_err();

    assert!(matches!(
        error,
        fieldcad_server::RecordingError::AlreadyRecording
    ));
}

#[test]
fn stopping_a_recording_that_never_started_is_a_structured_error() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let error = server.stop_recording().unwrap_err();

    assert!(matches!(
        error,
        fieldcad_server::RecordingError::NotRecording
    ));
}

fn commit_charge_and_probe(server: &mut HeadlessServer) -> fieldcad_core::ProbeId {
    let authored = submit_and_wait(server, CommandPayload::CommitWorld(charge_and_probe()));
    let CommandEvent::Completed(receipt) = authored else {
        panic!("expected the authoring commit to complete: {authored:?}")
    };
    let probe = receipt.created.created_probes[0];
    submit_and_wait(server, CommandPayload::Step);
    probe
}

#[test]
fn export_observations_includes_only_the_requested_probe_and_channel() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let probe = commit_charge_and_probe(&mut server);
    let channel = electric_field_channel_id();

    let scope = fieldcad_server::ObservationExportScope {
        probes: vec![(probe, channel.clone())],
        ..Default::default()
    };
    let export = server.export_observations(&scope);

    assert_eq!(export.probe_history.series.len(), 1);
    assert_eq!(export.probe_history.series[0].probe, probe);
    assert_eq!(export.probe_history.series[0].channel, channel);
    assert!(!export.probe_history.series[0].readings.is_empty());
    assert!(export.distance_history.series.is_empty());
    assert!(export.mass_aggregate_history.series.is_empty());
    assert!(export.snapshot.is_none());
}

#[test]
fn export_observations_never_includes_an_unrequested_probe() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    commit_charge_and_probe(&mut server);

    // An empty scope names nothing — there is no "everything" shorthand, so
    // a session with real recorded readings must still export nothing when
    // asked for nothing.
    let export = server.export_observations(&fieldcad_server::ObservationExportScope::default());

    assert!(export.probe_history.series.is_empty());
    assert!(export.distance_history.series.is_empty());
    assert!(export.mass_aggregate_history.series.is_empty());
}

#[test]
fn a_run_generation_reset_clears_server_side_observation_histories() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let probe = commit_charge_and_probe(&mut server);
    let channel = electric_field_channel_id();
    let before_count = server.probe_history().readings(probe, &channel).count();
    assert!(
        before_count >= 2,
        "the initial commit and the step must each have published a reading: {before_count}"
    );
    let generation_before = server.simulation_status().run_generation;

    // A coarser domain (same extent, fewer cells) raises the Courant limit,
    // so the session's existing time step stays valid — this only needs to
    // trigger a numerical run reset, not actually change resolution
    // meaningfully.
    let coarser = fieldcad_core::Domain::centred_cube(5.0, 16).unwrap();
    let reconfigured = submit_and_wait(&mut server, CommandPayload::ReconfigureDomain(coarser));
    assert!(
        matches!(reconfigured, CommandEvent::Completed(_)),
        "reconfiguring the domain must succeed: {reconfigured:?}"
    );
    assert!(
        server.simulation_status().run_generation > generation_before,
        "a domain reconfiguration must bump run_generation"
    );

    // Reconfiguring publishes a fresh tick-0 snapshot for the new run, so
    // the probe legitimately gets one new reading right away — but it must
    // be the *only* one: the prior run's tick-1 reading must be gone, not
    // sitting alongside it as if both belonged to the same series.
    let after: Vec<_> = server.probe_history().readings(probe, &channel).collect();
    assert_eq!(
        after.len(),
        1,
        "a run-generation reset must discard every reading from the prior run: {after:?}"
    );
    assert_eq!(
        after[0].tick, 0,
        "the sole remaining reading must be the new run's own tick-0 publish, not the old tick-1 one"
    );
}

#[test]
fn exported_probe_history_round_trips_through_a_file_bit_for_bit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("observations.fcobservation");
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let probe = commit_charge_and_probe(&mut server);
    let channel = electric_field_channel_id();
    let scope = fieldcad_server::ObservationExportScope {
        probes: vec![(probe, channel)],
        ..Default::default()
    };
    let export = server.export_observations(&scope);

    fieldcad_scene_document::save_observation_export_to_path(&export, &path).unwrap();
    let restored = fieldcad_scene_document::load_observation_export_from_path(&path).unwrap();

    assert_eq!(restored.probe_history, export.probe_history);
    assert_eq!(restored.format, fieldcad_scene_document::EXPORT_FORMAT_ID);
}

#[test]
fn server_side_probe_history_honors_each_probes_own_declared_capacity() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let channel = electric_field_channel_id();

    let commands = vec![
        WorldCommand::CreateObject(
            ObjectSpec::new("Point charge")
                .with_transform(Transform::at(DVec3::ZERO).unwrap())
                .with_shape(ObjectShape::point(0.1).unwrap())
                .with_component(
                    charge_component_id(),
                    charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9)).unwrap(),
                ),
        ),
        WorldCommand::CreateProbe(
            ProbeSpec::at("Small", DVec3::new(1.0, 0.0, 0.0), vec![channel.clone()])
                .with_history_capacity(2),
        ),
        WorldCommand::CreateProbe(ProbeSpec::at(
            "Large",
            DVec3::new(2.0, 0.0, 0.0),
            vec![channel.clone()],
        )),
    ];
    let authored = submit_and_wait(&mut server, CommandPayload::CommitWorld(commands));
    let CommandEvent::Completed(receipt) = authored else {
        panic!("expected the authoring commit to complete: {authored:?}")
    };
    let [small, large] = receipt.created.created_probes[..] else {
        panic!(
            "expected exactly two created probes: {:?}",
            receipt.created.created_probes
        );
    };

    for _ in 0..5 {
        submit_and_wait(&mut server, CommandPayload::Step);
    }

    assert_eq!(
        server.probe_history().len(small, &channel),
        2,
        "the probe's own declared history_capacity=2 must bound its series, \
         independent of the session's default capacity"
    );
    assert!(
        server.probe_history().len(large, &channel) > 2,
        "a probe with no declared override must still use the session default, \
         unaffected by the other probe's narrower capacity"
    );
}

/// A pinned charge source and a free (unpinned) charged, massive body a
/// tick's dynamics integration actually pushes — the same fixture
/// `field_data_source_delegation.rs::body_forces_reach_the_trait_object_not_just_the_inherent_method`
/// uses to get a real, moving dynamic body. Unlike that test, this doesn't
/// register the mass component schema first: `default_session` already has
/// it registered (registering it again is rejected as a duplicate).
fn commit_charge_source_and_free_body(server: &mut HeadlessServer) -> fieldcad_core::ObjectId {
    let commands = vec![
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
                .with_transform(Transform::at(DVec3::new(1.0, 0.0, 0.0)).unwrap())
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
    ];
    let authored = submit_and_wait(server, CommandPayload::CommitWorld(commands));
    let CommandEvent::Completed(receipt) = authored else {
        panic!("expected the authoring commit to complete: {authored:?}")
    };
    receipt.created.created_objects[1]
}

#[test]
fn trajectory_returns_the_free_bodys_recorded_kinematics() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let free = commit_charge_source_and_free_body(&mut server);

    for _ in 0..3 {
        submit_and_wait(&mut server, CommandPayload::Step);
    }

    let samples = server.trajectory(free).expect("trajectory read succeeds");
    assert_eq!(samples.len(), 3, "one sample per tick");
    assert!(
        samples.iter().any(|sample| sample.force.x > 0.0),
        "the free body must have recorded a real repulsive force: {samples:?}"
    );
}

#[test]
fn trajectory_for_an_object_with_no_recorded_ticks_is_empty_not_an_error() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);

    let samples = server
        .trajectory(fieldcad_core::ObjectId::new(0xdead_beef))
        .expect("an unknown object id is an empty result, not an error");

    assert!(samples.is_empty());
}

#[test]
fn recorded_observations_lists_exactly_what_was_actually_recorded() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let probe = commit_charge_and_probe(&mut server);
    let free = commit_charge_source_and_free_body(&mut server);
    submit_and_wait(&mut server, CommandPayload::Step);

    let inventory = server
        .recorded_observations()
        .expect("inventory read succeeds");

    let channel = electric_field_channel_id();
    assert!(
        inventory.probes.contains(&(probe, channel)),
        "the recorded probe/channel must be listed: {:?}",
        inventory.probes
    );
    assert!(inventory.distance_probes.is_empty());
    assert!(inventory.mass_aggregate_probes.is_empty());
    assert!(
        inventory.objects.contains(&free),
        "the free body's trajectory must be listed: {:?}",
        inventory.objects
    );
}

#[test]
fn observation_history_survives_replace_source_via_restore_observation_history() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let probe = commit_charge_and_probe(&mut server);
    let channel = electric_field_channel_id();

    let (probe_history, distance_history, mass_aggregate_history) =
        server.capture_observation_history();
    assert!(
        !probe_history.series.is_empty(),
        "there must be something real to restore"
    );
    let (world_document, _queue) = server.capture_document().unwrap();

    // Rebuild a fresh session from the same world (as loading the scene
    // document that was saved alongside this history would), replace the
    // session, then restore the captured history into it — mirroring
    // `open_scene`/`replace_session`'s call sequence exactly.
    let world = fieldcad_core::World::from_document(world_document);
    let domain = fieldcad_core::Domain::centred_cube(5.0, 32).unwrap();
    let time_step = fieldcad_core::TimeStep::from_seconds(
        fieldcad_electromagnetism::courant_limit(&domain) * 0.8,
    )
    .unwrap();
    let config = fieldcad_simulation::RuntimeConfig::new(
        domain,
        time_step,
        fieldcad_core::SessionId::from_u128(2),
    )
    .with_world(world);
    let config = fieldcad_server::server_plugin_catalog()
        .into_iter()
        .fold(config, |config, registration| {
            config.with_plugin_registration(registration)
        });
    let runtime = fieldcad_simulation::SimulationRuntime::new(config).unwrap();
    let new_source = fieldcad_simulation::AsyncLocalDataSource::new(
        fieldcad_simulation::LocalDataSource::new(runtime),
    );

    // A freshly replaced session may already have recorded its own tick-0
    // publish for `probe` (the new world already contains it) — `restore`
    // must still wholesale-replace that with the captured history, not
    // merge with it.
    server.replace_source(new_source);

    server.restore_observation_history(
        probe_history.clone(),
        distance_history,
        mass_aggregate_history,
    );

    let restored = server
        .probe_history_series(probe, &channel)
        .expect("the restored series must be readable after replace_source");
    assert_eq!(
        restored.readings.len(),
        probe_history.series[0].readings.len()
    );

    // Also readable through export, the other read path this task added.
    let export = server.export_observations(&fieldcad_server::ObservationExportScope {
        probes: vec![(probe, channel)],
        ..Default::default()
    });
    assert_eq!(export.probe_history, probe_history);
}
