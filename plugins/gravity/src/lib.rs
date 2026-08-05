//! Analytic Newtonian gravity over the reusable backend-neutral kernel.

use std::sync::{Arc, Mutex};

use fieldcad_core::{
    ChannelId, ChannelSchema, ComponentSchema, DiagnosticSeverity, Dimension, Domain, FieldColumn,
    FieldValueKind, PluginId, PluginVersion, Precision, SampleGeometry, SampleValidity,
    SolverDiagnostic, WorldSnapshot,
};
use fieldcad_mass_sources::{MassSource, collect_mass_sources, mass_component_schemas};
use fieldcad_newtonian_gravity::{NewtonianSample, evaluate_geometry, evaluate_sources};
use fieldcad_plugin_api::{
    ChannelHandle, DynamicBody, EquationSystemPlugin, EquationSystemSolver, PluginError,
    PluginMetadata, SampledColumn, SolverContext, SolverKind,
};
use glam::DVec3;

pub const PLUGIN_ID: &str = "fieldcad.gravity";
pub const GRAVITATIONAL_ACCELERATION: &str = "gravitational-acceleration";
pub const GRAVITATIONAL_POTENTIAL: &str = "gravitational-potential";
pub const GRAVITATIONAL_ACCELERATION_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const GRAVITATIONAL_POTENTIAL_HANDLE: ChannelHandle = ChannelHandle::new(1);
const POTENTIAL_DIMENSION: Dimension = Dimension::new(0, 2, -2, 0, 0, 0, 0);
const POISONED_CACHE: &str = "Newtonian gravity evaluation cache is poisoned";

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}
pub fn gravitational_acceleration_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_ACCELERATION).expect("static channel ID is valid")
}
pub fn gravitational_potential_channel_id() -> ChannelId {
    ChannelId::new(plugin_id(), GRAVITATIONAL_POTENTIAL).expect("static channel ID is valid")
}

fn channels() -> Vec<ChannelSchema> {
    vec![
        ChannelSchema {
            id: gravitational_acceleration_channel_id(),
            display_name: "Gravitational acceleration g".to_owned(),
            value_kind: FieldValueKind::Vector(Dimension::ACCELERATION),
        },
        ChannelSchema {
            id: gravitational_potential_channel_id(),
            display_name: "Gravitational potential Φ".to_owned(),
            value_kind: FieldValueKind::Scalar(POTENTIAL_DIMENSION),
        },
    ]
}

/// Analytic, static Newtonian gravity. It is intentionally a thin adapter over
/// `fieldcad-newtonian-gravity`; a distributed backend can use that crate
/// directly without loading Field CAD's plugin/runtime interface.
#[derive(Clone, Copy, Debug, Default)]
pub struct NewtonianGravityPlugin;

impl EquationSystemPlugin for NewtonianGravityPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Newtonian gravity".to_owned(),
            description:
                "Analytic Newtonian gravitational acceleration and potential with superposition"
                    .to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        channels()
    }
    fn component_schemas(&self) -> Vec<ComponentSchema> {
        mass_component_schemas()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        Ok(Box::new(NewtonianGravitySolver {
            domain: *context.domain,
            sources: sources(context.world)?,
            world_revision: context.world.revision(),
            cache: Mutex::new(Vec::new()),
        }))
    }
}

fn sources(world: &WorldSnapshot) -> Result<Vec<MassSource>, PluginError> {
    collect_mass_sources(world).map_err(|error| PluginError::UnsupportedWorld(error.to_string()))
}

struct NewtonianGravitySolver {
    domain: Domain,
    sources: Vec<MassSource>,
    world_revision: fieldcad_core::WorldRevision,
    cache: Mutex<Vec<(SampleGeometry, Arc<[NewtonianSample]>)>>,
}

impl EquationSystemSolver for NewtonianGravitySolver {
    fn kind(&self) -> SolverKind {
        SolverKind::Analytic
    }
    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        sources(world).map(|_| ())
    }
    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.sources = sources(world)?;
        self.world_revision = world.revision();
        self.cache
            .get_mut()
            .map_err(|_| PluginError::Solver(POISONED_CACHE.to_owned()))?
            .clear();
        Ok(())
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let samples = self.samples_for(geometry)?;
        let validity = samples.iter().map(|sample| sample.validity).collect();
        match channel {
            GRAVITATIONAL_ACCELERATION_HANDLE => Ok(SampledColumn::new(
                FieldColumn::vectors(samples.iter().map(|sample| sample.acceleration).collect()),
                validity,
            )),
            GRAVITATIONAL_POTENTIAL_HANDLE => Ok(SampledColumn::new(
                FieldColumn::scalars(samples.iter().map(|sample| sample.potential).collect()),
                validity,
            )),
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn forces(&self, bodies: &[DynamicBody]) -> Result<Vec<DVec3>, PluginError> {
        bodies
            .iter()
            .map(|body| {
                let mass = self
                    .sources
                    .iter()
                    .find(|source| source.object == body.object)
                    .and_then(|source| source.gravitational_mass_kg)
                    .unwrap_or(0.0);
                if mass == 0.0 {
                    return Ok(DVec3::ZERO);
                }
                let sources: Vec<_> = self
                    .sources
                    .iter()
                    .copied()
                    .filter(|source| source.object != body.object)
                    .collect();
                let sample = evaluate_sources(&sources, body.position);
                match sample.validity {
                    SampleValidity::Exact => Ok(sample.acceleration * mass),
                    _ => Ok(DVec3::ZERO),
                }
            })
            .collect()
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        vec![SolverDiagnostic {
            plugin: plugin_id(),
            severity: DiagnosticSeverity::Info,
            code: "newtonian-gravity-source-count".to_owned(),
            message: format!(
                "{} mass source(s), analytic f64 kernel, world revision {}",
                self.sources.len(),
                self.world_revision
            ),
        }]
    }
}

impl NewtonianGravitySolver {
    fn samples_for(
        &self,
        geometry: &SampleGeometry,
    ) -> Result<Arc<[NewtonianSample]>, PluginError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| PluginError::Solver(POISONED_CACHE.to_owned()))?;
        if let Some((_, samples)) = cache.iter().find(|(cached, _)| cached == geometry) {
            return Ok(Arc::clone(samples));
        }
        let samples: Arc<[NewtonianSample]> = evaluate_geometry(&self.sources, geometry)
            .into_iter()
            .map(|sample| quantize(sample, self.domain.precision()))
            .collect::<Vec<_>>()
            .into();
        if cache.len() >= 16 {
            cache.remove(0);
        }
        cache.push((geometry.clone(), Arc::clone(&samples)));
        Ok(samples)
    }
}

fn quantize(sample: NewtonianSample, precision: Precision) -> NewtonianSample {
    if precision == Precision::F64 {
        return sample;
    }
    NewtonianSample {
        acceleration: sample.acceleration.as_vec3().as_dvec3(),
        potential: f64::from(sample.potential as f32),
        validity: sample.validity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fieldcad_core::{
        ObjectShape, ObjectSpec, ProbeId, StepContext, TimeStep, Transform, World, WorldCommand,
    };
    use fieldcad_mass_sources::{
        gravitational_mass_component_id, inertial_mass_component_id, inertial_mass_properties,
        linked_gravitational_mass_properties,
    };

    fn solver() -> Box<dyn EquationSystemSolver> {
        let plugin = NewtonianGravityPlugin;
        let mut world = World::new();
        world
            .commit(
                mass_component_schemas()
                    .into_iter()
                    .map(WorldCommand::RegisterComponentSchema),
            )
            .unwrap();
        world
            .commit([WorldCommand::CreateObject(
                ObjectSpec::new("source")
                    .with_transform(Transform::at(DVec3::ZERO).unwrap())
                    .with_shape(ObjectShape::point(0.01).unwrap())
                    .with_component(
                        inertial_mass_component_id(),
                        inertial_mass_properties(2.0e10).unwrap(),
                    )
                    .with_component(
                        gravitational_mass_component_id(),
                        linked_gravitational_mass_properties(),
                    ),
            )])
            .unwrap();
        let domain = Domain::centred_cube(4.0, 8).unwrap();
        plugin
            .create_solver(SolverContext {
                configuration: &Default::default(),
                domain: &domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0).unwrap(),
                },
                cancellation: Default::default(),
            })
            .unwrap()
    }

    #[test]
    fn publishes_acceleration_and_potential_channels() {
        let solver = solver();
        let geometry = SampleGeometry::probes(vec![ProbeId::new(0)], vec![DVec3::X]).unwrap();
        let acceleration = solver
            .sample(GRAVITATIONAL_ACCELERATION_HANDLE, &geometry)
            .unwrap();
        let potential = solver
            .sample(GRAVITATIONAL_POTENTIAL_HANDLE, &geometry)
            .unwrap();
        let FieldColumn::Vector(acceleration_values) = acceleration.values else {
            panic!("expected vector field");
        };
        let FieldColumn::Scalar(potential_values) = potential.values else {
            panic!("expected scalar field");
        };
        assert!(acceleration_values[0].x < 0.0);
        assert!(potential_values[0] < 0.0);
    }
}
