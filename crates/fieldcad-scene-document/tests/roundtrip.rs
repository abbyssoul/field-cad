//! End-to-end coverage for the `fieldcad.scene/v1` document: capture, save,
//! load, resolve, and rebuild should reproduce an equivalent session —
//! including identifiers that survive a delete in between, and rejection of
//! documents this build cannot represent.

use std::collections::BTreeMap;

use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, ComponentTypeId, Dimension, Domain, DomainBounds,
    FieldValueKind, ObjectSpec, PluginId, PluginVersion, Precision, ProbeSpec, PropertyBag,
    Resolution, SessionId, TimeStep, World, WorldCommand,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginError, PluginMetadata,
    SampledColumn, SolverContext, SolverKind,
};
use fieldcad_scene_document::{
    LoadError, LoadSource, ResolveError, SceneDocument, SceneDocumentInputs, resolve_plugins,
    save_to_path,
};
use fieldcad_simulation::{PluginRegistration, QueueDocument, RuntimeConfig, SimulationRuntime};
use fieldcad_test_field::TestFieldPlugin;
use glam::DVec3;

/// A minimal plugin that declares one component schema, so a round trip
/// exercises `RuntimeConfig::with_world` against a world that already has
/// that schema registered — the regression `SimulationRuntime::new`'s
/// schema-collision fix exists for.
#[derive(Clone, Copy, Debug, Default)]
struct MassPlugin;

fn mass_plugin_id() -> PluginId {
    PluginId::new("fieldcad.test-mass").unwrap()
}

fn mass_component_id() -> ComponentTypeId {
    ComponentTypeId::new(mass_plugin_id(), "mass").unwrap()
}

fn mass_channel_id() -> ChannelId {
    ChannelId::new(mass_plugin_id(), "mass-field").unwrap()
}

impl EquationSystemPlugin for MassPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: mass_plugin_id(),
            version: PluginVersion::new(1, 0, 0),
            display_name: "Test mass".to_owned(),
            description: "Declares a mass component for round-trip tests".to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        vec![ChannelSchema {
            id: mass_channel_id(),
            display_name: "Mass field".to_owned(),
            value_kind: FieldValueKind::Scalar(Dimension::MASS),
        }]
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        vec![ComponentSchema {
            id: mass_component_id(),
            display_name: "Mass".to_owned(),
            properties: Vec::new(),
        }]
    }

    fn create_solver(
        &self,
        _context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        Ok(Box::new(MassSolver))
    }
}

struct MassSolver;

impl EquationSystemSolver for MassSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }

    fn on_world_changed(
        &mut self,
        _world: &fieldcad_core::WorldSnapshot,
    ) -> Result<(), PluginError> {
        Ok(())
    }

    fn sample(
        &self,
        _channel: ChannelHandle,
        geometry: &fieldcad_core::SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let count = geometry.len();
        Ok(SampledColumn::new(
            fieldcad_core::FieldColumn::scalars(vec![0.0; count]),
            vec![fieldcad_core::SampleValidity::Exact; count],
        ))
    }
}

fn domain() -> Domain {
    Domain::new(
        DomainBounds::new(DVec3::splat(-2.0), DVec3::splat(2.0)).unwrap(),
        Resolution::new(8, 8, 8).unwrap(),
        fieldcad_core::BoundaryConditions::uniform(fieldcad_core::BoundaryCondition::Periodic),
        Precision::F32,
    )
}

fn catalog() -> Vec<PluginRegistration> {
    vec![
        PluginRegistration::with_default_configuration(Box::new(TestFieldPlugin)),
        PluginRegistration::with_default_configuration(Box::new(MassPlugin)),
    ]
}

fn build_runtime(world: World) -> SimulationRuntime {
    let config = RuntimeConfig::new(
        domain(),
        TimeStep::from_seconds(0.1).unwrap(),
        SessionId::from_u128(1),
    )
    .with_world(world);
    let config = catalog().into_iter().fold(config, |config, registration| {
        config.with_plugin_registration(registration)
    });
    SimulationRuntime::new(config).unwrap()
}

fn inputs(runtime: &SimulationRuntime, queue: QueueDocument) -> SceneDocumentInputs {
    SceneDocumentInputs {
        domain: *runtime.domain(),
        time_step: runtime.clock_snapshot().time_step(),
        scene_scale: runtime.scene_scale(),
        integration_scheme: runtime.integration_scheme(),
        field_systems: runtime.field_systems(),
        world: runtime.world_document(),
        queue,
    }
}

#[test]
fn document_round_trips_objects_components_probes_and_planes() {
    let mut runtime = build_runtime(World::new());

    let mut components = BTreeMap::new();
    components.insert(mass_component_id(), PropertyBag::default());
    runtime
        .commit_world_commands(vec![
            WorldCommand::CreateObject(ObjectSpec::new("a")),
            WorldCommand::CreateObject(
                ObjectSpec::new("b").with_component(mass_component_id(), PropertyBag::default()),
            ),
            WorldCommand::CreateProbe(ProbeSpec::at("p", DVec3::X, Vec::new())),
        ])
        .unwrap();

    let document = SceneDocument::capture(inputs(&runtime, QueueDocument::default()), "test", None);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    save_to_path(&document, &path).unwrap();
    let outcome = fieldcad_scene_document::load_newest_valid(&path).unwrap();
    assert_eq!(outcome.source, LoadSource::Primary);

    let (plugins, warnings) = resolve_plugins(catalog(), &outcome.document.field_systems).unwrap();
    assert!(warnings.is_empty());
    let world = World::from_document(outcome.document.world);
    let config = RuntimeConfig::new(
        outcome.document.domain,
        outcome.document.time_step,
        SessionId::from_u128(2),
    )
    .with_world(world)
    .with_scene_scale(outcome.document.scene_scale)
    .with_integration_scheme(outcome.document.integration_scheme);
    let config = plugins.into_iter().fold(config, |config, registration| {
        config.with_plugin_registration(registration)
    });
    let reloaded = SimulationRuntime::new(config).unwrap();

    let original = runtime.world_snapshot();
    let restored = reloaded.world_snapshot();
    assert_eq!(original.objects(), restored.objects());
    assert_eq!(original.planes(), restored.planes());
    assert_eq!(original.probes(), restored.probes());
    assert_eq!(original.component_schemas(), restored.component_schemas());
}

#[test]
fn object_ids_survive_a_delete_between_save_and_load() {
    let mut runtime = build_runtime(World::new());
    let report = runtime
        .commit_world_commands(vec![
            WorldCommand::CreateObject(ObjectSpec::new("a")),
            WorldCommand::CreateObject(ObjectSpec::new("b")),
            WorldCommand::CreateObject(ObjectSpec::new("c")),
        ])
        .unwrap();
    let ids: Vec<_> = report.created_objects.clone();
    assert_eq!(ids.len(), 3);

    // Delete the middle object, so the surviving IDs have a gap — the case a
    // naive "replay CreateObject in order" load would renumber incorrectly.
    runtime
        .commit_world_commands(vec![WorldCommand::RemoveObject(ids[1])])
        .unwrap();

    let document = SceneDocument::capture(inputs(&runtime, QueueDocument::default()), "test", None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    save_to_path(&document, &path).unwrap();
    let outcome = fieldcad_scene_document::load_newest_valid(&path).unwrap();

    let world = World::from_document(outcome.document.world);
    assert_eq!(
        world
            .snapshot()
            .objects()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![ids[0], ids[2]],
        "surviving object IDs must be exactly the pre-save set, not renumbered"
    );

    // The next object created after loading must not reuse the deleted ID —
    // proof the counters, not just the map contents, round-tripped.
    let mut reloaded = build_runtime(world);
    let report = reloaded
        .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new("d"))])
        .unwrap();
    assert_eq!(
        report.created_objects[0],
        fieldcad_core::ObjectId::new(ids[2].get() + 1)
    );
}

#[test]
fn unknown_format_is_rejected_before_any_field_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    std::fs::write(&path, br#"{"format":"not.a.scene/v1","format_version":1}"#).unwrap();

    let error = fieldcad_scene_document::load_newest_valid(&path).unwrap_err();
    assert!(matches!(error, LoadError::NoValidCandidate));
}

#[test]
fn unsupported_format_version_is_rejected() {
    let mut runtime = build_runtime(World::new());
    let document = SceneDocument::capture(inputs(&runtime, QueueDocument::default()), "test", None);
    let mut value = serde_json::to_value(&document).unwrap();
    value["format_version"] = serde_json::json!(999);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    let error = fieldcad_scene_document::load_newest_valid(&path).unwrap_err();
    assert!(matches!(error, LoadError::NoValidCandidate));
    let _ = &mut runtime; // silence unused-mut if commit path above changes
}

#[test]
fn unknown_plugin_is_rejected_not_silently_dropped() {
    let mut runtime = build_runtime(World::new());
    runtime
        .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new("a"))])
        .unwrap();
    let document = SceneDocument::capture(inputs(&runtime, QueueDocument::default()), "test", None);

    // Resolve against a catalog missing MassPlugin entirely.
    let bare_catalog = vec![PluginRegistration::with_default_configuration(Box::new(
        TestFieldPlugin,
    ))];
    let error = match resolve_plugins(bare_catalog, &document.field_systems) {
        Ok(_) => panic!("must reject a document naming a plugin absent from the catalog"),
        Err(error) => error,
    };
    assert!(matches!(error, ResolveError::UnknownPlugin { .. }));
}

#[test]
fn corrupt_primary_falls_back_to_backup() {
    let mut runtime = build_runtime(World::new());
    runtime
        .commit_world_commands(vec![WorldCommand::CreateObject(ObjectSpec::new("a"))])
        .unwrap();
    let document = SceneDocument::capture(inputs(&runtime, QueueDocument::default()), "test", None);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    save_to_path(&document, &path).unwrap();
    // A second save with a corrupted primary write is simulated by writing
    // garbage directly, leaving the `.bak` from the first save intact.
    std::fs::copy(&path, dir.path().join("scene.fcscene.bak")).unwrap();
    std::fs::write(&path, b"not json").unwrap();

    let outcome = fieldcad_scene_document::load_newest_valid(&path).unwrap();
    assert_eq!(outcome.source, LoadSource::Backup);
}

fn issue(
    source: &mut fieldcad_simulation::LocalDataSource,
    next_id: &mut u64,
    payload: fieldcad_simulation::CommandPayload,
) -> fieldcad_simulation::CommandReceipt {
    use fieldcad_simulation::{Command, FieldDataSource};
    let id = fieldcad_simulation::CommandId::new(*next_id);
    *next_id += 1;
    source.execute(Command { id, payload }).unwrap()
}

#[test]
fn paused_queue_round_trips_and_restores_unapplied() {
    use fieldcad_simulation::{CommandPayload, FieldDataSource, LocalDataSource};

    let mut source = LocalDataSource::new(build_runtime(World::new()));
    let mut next_id = 0u64;

    issue(&mut source, &mut next_id, CommandPayload::PauseQueue);
    let receipt = issue(
        &mut source,
        &mut next_id,
        CommandPayload::CommitWorld(vec![WorldCommand::CreateObject(ObjectSpec::new(
            "queued-a",
        ))]),
    );
    assert_eq!(
        receipt.disposition,
        fieldcad_simulation::CommandDisposition::Queued
    );
    // Nothing applied yet: the object must not be visible in the world.
    assert!(source.world().objects().is_empty());

    let queue = source.queue_document();
    assert!(queue.paused);
    assert_eq!(queue.pending.len(), 1);

    let document = SceneDocument::capture(inputs(source.runtime(), queue), "test", None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scene.fcscene");
    save_to_path(&document, &path).unwrap();
    let outcome = fieldcad_scene_document::load_newest_valid(&path).unwrap();

    // Rebuild a fresh session from the loaded document, exactly as
    // `build_session` (desktop/MCP) would, then replay the saved queue.
    let (plugins, _) = resolve_plugins(catalog(), &outcome.document.field_systems).unwrap();
    let world = World::from_document(outcome.document.world);
    let config = RuntimeConfig::new(
        outcome.document.domain,
        outcome.document.time_step,
        SessionId::from_u128(3),
    )
    .with_world(world);
    let config = plugins.into_iter().fold(config, |config, registration| {
        config.with_plugin_registration(registration)
    });
    let mut reloaded = LocalDataSource::new(SimulationRuntime::new(config).unwrap());

    // The session must come back paused (never auto-running) regardless of
    // the saved queue state, and the world must be empty until the queue is
    // explicitly resumed.
    assert_eq!(
        reloaded.simulation_status().mode(),
        fieldcad_core::SimulationMode::Paused
    );
    assert!(reloaded.world().objects().is_empty());

    let queue = outcome.document.queue;
    let mut next_id = 100u64;
    if queue.paused {
        issue(&mut reloaded, &mut next_id, CommandPayload::PauseQueue);
    }
    for payload in queue.pending {
        issue(&mut reloaded, &mut next_id, payload);
    }

    assert!(
        reloaded.world().objects().is_empty(),
        "restored edit must land back in the queue, unapplied"
    );
    assert_eq!(reloaded.get_queue().pending.len(), 1);

    // Resuming applies exactly the edit that was pending at save time.
    issue(&mut reloaded, &mut next_id, CommandPayload::ResumeQueue);
    reloaded.poll(std::time::Duration::ZERO).unwrap();
    assert_eq!(reloaded.world().objects().len(), 1);
}
