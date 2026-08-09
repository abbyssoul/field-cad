//! Proves the model runs headless, end to end, through the same command
//! surface a future transport will use — no window, no GPU device anywhere
//! in this crate's dependency graph.

use std::time::{Duration, Instant};

use fieldcad_core::{
    ObjectShape, ObjectSpec, ProbeSpec, Transform, WorldCommand,
    quantities::{ChargeCoulombs, coulomb},
};
use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
use fieldcad_electrostatics::electric_field_channel_id;
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{CommandEvent, CommandPayload};
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
