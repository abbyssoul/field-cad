//! `HeadlessServer`'s `impl FieldDataSource` must not silently fall back to
//! the trait's defaults for `body_forces` and `drain_command_events` — both
//! compile fine either way, so only a test that actually exercises them
//! through `&dyn FieldDataSource`/`&mut dyn FieldDataSource` (not through
//! `HeadlessServer`'s own same-named inherent methods, which would pass even
//! if the trait impl were broken) catches a regression here.

use std::time::{Duration, Instant};

use fieldcad_core::{
    ObjectShape, ObjectSpec, Transform, WorldCommand,
    quantities::{ChargeCoulombs, MassKg, coulomb, kilogram},
};
use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_server::HeadlessServer;
use fieldcad_simulation::{
    CommandEvent, CommandPayload, CommandSequencer, FieldDataSource, LocalDataSource,
    RuntimeConfig, SimulationRuntime,
};
use fieldcad_sources::{
    inertial_mass_component_id, inertial_mass_properties, mass_component_schemas,
};
use glam::DVec3;

fn wait_for_event(server: &mut dyn FieldDataSource) -> CommandEvent {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        server.poll(Duration::ZERO).expect("poll succeeds");
        if let Some(event) = server.drain_command_events().into_iter().next() {
            return event;
        }
        assert!(Instant::now() < deadline, "worker did not respond");
        std::thread::yield_now();
    }
}

#[test]
fn body_forces_reach_the_trait_object_not_just_the_inherent_method() {
    let runtime = SimulationRuntime::new(
        RuntimeConfig::new(
            fieldcad_core::Domain::centred_cube(2.0, 4).unwrap(),
            fieldcad_core::TimeStep::from_seconds(0.1).unwrap(),
            fieldcad_core::SessionId::from_u128(0x70),
        )
        .with_plugin(Box::new(ElectrostaticsPlugin::new())),
    )
    .unwrap();
    let mut server = HeadlessServer::new(fieldcad_simulation::AsyncLocalDataSource::new(
        LocalDataSource::new(runtime),
    ));
    let mut sequencer = CommandSequencer::default();

    // Deliberately goes through `submit`, not a direct `AsyncLocalDataSource`
    // call: this is the same path a real transport uses.
    server
        .execute(
            sequencer.issue(CommandPayload::CommitWorld(
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
                                .with_transform(Transform::at_finite(DVec3::new(1.0, 0.0, 0.0)))
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
            )),
        )
        .unwrap();
    // `execute` only submits; the commit is applied asynchronously on the
    // worker thread (ADR 0011), so wait for it before reading `world()`.
    assert!(
        matches!(wait_for_event(&mut server), CommandEvent::Completed(_)),
        "authoring the scene is accepted"
    );

    // Access everything from here on *only* through the trait object, the
    // way `ComputeView::build` in the desktop app does.
    let source: &mut dyn FieldDataSource = &mut server;
    assert!(source.body_forces().is_empty(), "no tick has run yet");

    let free = *source
        .world()
        .objects()
        .iter()
        .find(|(_, object)| object.name == "free")
        .unwrap()
        .0;

    source
        .execute(sequencer.issue(CommandPayload::Step))
        .unwrap();
    assert!(
        matches!(wait_for_event(source), CommandEvent::Completed(_)),
        "the step is accepted once a scene exists"
    );

    let force = *source
        .body_forces()
        .get(&free)
        .expect("a dynamic body advanced by a tick has a force, through the trait object");
    assert!(force.x > 0.0, "expected repulsion along +x, got {force:?}");
}

#[test]
fn drain_command_events_reaches_the_trait_object_not_just_the_inherent_method() {
    let source = fieldcad_server::default_session().expect("default session builds");
    let mut server = HeadlessServer::new(source);
    let mut sequencer = CommandSequencer::default();

    // Removing an object that doesn't exist is rejected asynchronously
    // (ADR 0011: the worker validates a commit, not the initial `execute`
    // call), so this only surfaces via `drain_command_events`.
    server
        .execute(sequencer.issue(CommandPayload::CommitWorld(vec![
            WorldCommand::RemoveObject(fieldcad_core::ObjectId::new(0xdead_beef)),
        ])))
        .unwrap();

    let source: &mut dyn FieldDataSource = &mut server;
    let event = wait_for_event(source);
    assert!(
        matches!(event, CommandEvent::Failed { .. }),
        "removing a nonexistent object is rejected: {event:?}"
    );
}
