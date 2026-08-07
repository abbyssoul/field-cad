//! The compute operations the harness measures, and the sweeps that show how
//! each one scales.
//!
//! Everything here goes through the published plugin and runtime Interfaces —
//! `EquationSystemPlugin::create_solver`, `EquationSystemSolver::{step, sample,
//! diagnostics, on_world_changed}`, and `SimulationRuntime` — rather than
//! through solver internals. Milestone 6 is rewriting how Maxwell advances
//! charge and current; a harness bolted to the current lattice code would have
//! to be rewritten with it, while these boundaries are the contract that is
//! meant to survive.
//!
//! Visualization is deliberately absent. It performs acceptably today and the
//! target is computation, so scene extraction and rendering are left out rather
//! than measured badly. The sampling benchmarks here are the *solver* side of
//! presentation: the interpolation a visualizer's density setting asks the
//! solver to do.

use fieldcad_core::{
    ObjectShape, ObjectSpec, PlaneId, PlaneLattice, ProbePosition, SampleGeometry, SessionId,
    StepContext, TimeStep, Transform, World, WorldCommand,
};
use fieldcad_electromagnetism::{
    ELECTRIC_FIELD_HANDLE as MAXWELL_ELECTRIC_HANDLE, ElectromagnetismPlugin, courant_limit,
    prescribed_plane_wave_configuration,
};
use fieldcad_electrostatics::{ELECTRIC_FIELD_HANDLE, ElectrostaticsPlugin};
use fieldcad_gravity::GRAVITATIONAL_ACCELERATION_HANDLE;
use fieldcad_mass_sources::{
    gravitational_mass_component_id, inertial_mass_component_id, inertial_mass_properties,
    linked_gravitational_mass_properties, mass_component_schemas,
};
use fieldcad_plugin_api::{
    DynamicBody, EquationSystemPlugin, EquationSystemSolver, SolverCancellation, SolverContext,
};
use fieldcad_simulation::{RuntimeConfig, SimulationRuntime};
use glam::{DVec3, UVec2};

use crate::{
    measure::{MeasureConfig, Timing, measure},
    scaling::Complexity,
    scene::{MaxwellMode, Scene},
};

/// What a sweep varies, and therefore what the reported exponent is in terms of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parameter {
    /// Yee lattice cells, `cells_per_axis³`.
    Cells,
    /// Authored charge sources.
    Charges,
    /// Samples one channel is asked for in one publication.
    Samples,
}

impl Parameter {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cells => "cells",
            Self::Charges => "charges",
            Self::Samples => "samples",
        }
    }

    pub fn value(self, scene: &Scene) -> f64 {
        match self {
            Self::Cells => scene.cells() as f64,
            Self::Charges => scene.charges as f64,
            Self::Samples => scene.samples_per_channel() as f64,
        }
    }
}

/// One measurable operation swept over a range of scene sizes.
pub struct Benchmark {
    /// Stable identifier. Baselines and agent tooling key on this, so it must
    /// not change casually.
    pub id: &'static str,
    pub group: &'static str,
    /// The operation being timed.
    pub what: &'static str,
    /// Why it is worth watching — what makes it a hot path.
    pub why: &'static str,
    pub parameter: Parameter,
    pub declared: Complexity,
    pub scenes: Vec<Scene>,
    pub runner: fn(&Scene, &MeasureConfig) -> Timing,
}

fn time_step_for(scene: &Scene) -> TimeStep {
    TimeStep::from_seconds(courant_limit(&scene.domain()) * 0.8)
        .expect("a Courant-limited step is positive and finite")
}

fn initial_step(scene: &Scene) -> StepContext {
    StepContext {
        tick: 0,
        time_seconds: 0.0,
        time_step: time_step_for(scene),
    }
}

fn maxwell_solver(scene: &Scene, world: &World) -> Box<dyn EquationSystemSolver> {
    let plugin = ElectromagnetismPlugin::new();
    let configuration = match scene.maxwell {
        MaxwellMode::StaticCharges => plugin.default_configuration(),
        MaxwellMode::PrescribedWave => prescribed_plane_wave_configuration(1.0, 1)
            .expect("a unit-amplitude first mode is a valid wave configuration"),
    };
    plugin
        .create_solver(SolverContext {
            configuration: &configuration,
            domain: &scene.domain(),
            world: &world.snapshot(),
            initial_step: initial_step(scene),
            cancellation: SolverCancellation::default(),
        })
        .expect("benchmark scenes are valid Maxwell configurations")
}

fn electrostatics_solver(scene: &Scene, world: &World) -> Box<dyn EquationSystemSolver> {
    ElectrostaticsPlugin::new()
        .create_solver(SolverContext {
            configuration: &Default::default(),
            domain: &scene.domain(),
            world: &world.snapshot(),
            initial_step: initial_step(scene),
            cancellation: SolverCancellation::default(),
        })
        .expect("benchmark scenes are valid electrostatic configurations")
}

/// A world populated with massive bodies at the scene's charge positions.
///
/// Each body carries linked inertial and gravitational mass so every source
/// gravitates. The mass value grows with source count so the total field
/// strength stays physical at any scale, avoiding overflow in the sampling
/// kernel's superposition arithmetic.
fn gravity_world(scene: &Scene) -> World {
    let mut world = World::new();
    let mut commands: Vec<WorldCommand> = mass_component_schemas()
        .into_iter()
        .map(WorldCommand::RegisterComponentSchema)
        .collect();
    for (index, position) in scene.charge_positions().into_iter().enumerate() {
        // Mass grows with the source count so per-source strength is constant,
        // giving a clean linear-in-sources superposition test.
        let mass_kg = 1.0e10 / (scene.charges as f64).sqrt().max(1.0);
        commands.push(WorldCommand::CreateObject(
            ObjectSpec::new(format!("mass {index}"))
                .with_transform(Transform::at(position).expect("mass position is finite"))
                .with_shape(ObjectShape::point(0.15).expect("source radius is positive"))
                .with_component(
                    inertial_mass_component_id(),
                    inertial_mass_properties(mass_kg).expect("mass is a valid quantity"),
                )
                .with_component(
                    gravitational_mass_component_id(),
                    linked_gravitational_mass_properties(),
                ),
        ));
    }
    world.commit(commands).expect("gravity world is valid");
    world
}

fn gravity_solver(scene: &Scene, world: &World) -> Box<dyn EquationSystemSolver> {
    use fieldcad_gravity::NewtonianGravityPlugin;
    NewtonianGravityPlugin
        .create_solver(SolverContext {
            configuration: &Default::default(),
            domain: &scene.domain(),
            world: &world.snapshot(),
            initial_step: initial_step(scene),
            cancellation: SolverCancellation::default(),
        })
        .expect("benchmark scenes are valid Newtonian gravity configurations")
}

/// A plane lattice carrying the scene's requested presentation density.
///
/// Sampling cost is driven by how many values a visualizer asks for, which is
/// the density setting, not the solver resolution. Keeping those separate here
/// mirrors the invariant that display density never changes the physics.
fn plane_geometry(scene: &Scene) -> SampleGeometry {
    let counts = scene.plane_samples_per_axis.max(1);
    let span = scene.half_extent_metres * 1.6;
    let step = span / f64::from(counts.max(2) - 1);
    SampleGeometry::Plane {
        plane: PlaneId::new(0),
        lattice: PlaneLattice::new(
            DVec3::new(-span * 0.5, -span * 0.5, 0.25),
            DVec3::new(step, 0.0, 0.0),
            DVec3::new(0.0, step, 0.0),
            UVec2::splat(counts),
        ),
    }
}

fn runtime_for(scene: &Scene) -> SimulationRuntime {
    let plugin = ElectromagnetismPlugin::new();
    let configuration = match scene.maxwell {
        MaxwellMode::StaticCharges => plugin.default_configuration(),
        MaxwellMode::PrescribedWave => prescribed_plane_wave_configuration(1.0, 1)
            .expect("a unit-amplitude first mode is a valid wave configuration"),
    };
    let mut runtime = SimulationRuntime::new(
        RuntimeConfig::new(
            scene.domain(),
            time_step_for(scene),
            SessionId::from_u128(1),
        )
        .with_subscription(scene.subscription())
        // Composed but inactive: these scenes measure the Maxwell model of the
        // electric field, and two active models of one field is exactly what the
        // runtime refuses. The electrostatic workloads below drive that solver
        // directly rather than through a runtime.
        .with_plugin_registration(
            fieldcad_simulation::PluginRegistration::with_default_configuration(Box::new(
                ElectrostaticsPlugin::new(),
            ))
            .with_enabled(false),
        )
        .with_plugin_registration(fieldcad_simulation::PluginRegistration {
            plugin: Box::new(plugin),
            configuration,
            enabled: true,
            realtime: true,
        }),
    )
    .expect("benchmark scenes compose a valid runtime");
    // Author into the runtime rather than handing it a populated world: the
    // runtime registers plugin component schemas itself, so a world that
    // already carries them is rejected. This is the desktop's own sequence.
    runtime
        .commit_world_commands(scene.authoring_commands())
        .expect("scene authoring is valid");
    runtime
}

/// The first charged object in the scene, for edit benchmarks.
fn first_charge(world: &World) -> fieldcad_core::ObjectId {
    fieldcad_electromagnetic_sources::collect_charge_sources(&world.snapshot())
        .expect("scene charges are valid")
        .first()
        .expect("edit benchmarks need at least one charge")
        .object
}

// --- runners ---------------------------------------------------------------

fn maxwell_step(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = scene.world();
    let step = time_step_for(scene);
    measure(
        config,
        || (maxwell_solver(scene, &world), 0u64),
        |(solver, tick), _| {
            *tick += 1;
            solver
                .step(StepContext {
                    tick: *tick,
                    time_seconds: *tick as f64 * step.seconds(),
                    time_step: step,
                })
                .expect("a Courant-limited step is accepted")
        },
    )
}

fn maxwell_solver_init(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = scene.world();
    measure(config, || (), |(), _| maxwell_solver(scene, &world))
}

fn maxwell_sample_plane(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = scene.world();
    let geometry = plane_geometry(scene);
    measure(
        config,
        || maxwell_solver(scene, &world),
        |solver, _| {
            solver
                .sample(MAXWELL_ELECTRIC_HANDLE, &geometry)
                .expect("the electric channel is published")
        },
    )
}

fn maxwell_diagnostics(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = scene.world();
    measure(
        config,
        || maxwell_solver(scene, &world),
        |solver, _| solver.diagnostics(),
    )
}

/// Rebuild cost when an edit genuinely changes the charge configuration.
fn maxwell_edit_charge(scene: &Scene, config: &MeasureConfig) -> Timing {
    let base = scene.world();
    let charge = first_charge(&base);
    // Two worlds whose charge positions differ, so alternating between them
    // always forces the constrained state to be rebuilt.
    let snapshots: Vec<_> = [0.4, -0.4]
        .into_iter()
        .map(|offset| {
            let mut world = base.clone();
            world
                .commit([WorldCommand::SetTransform {
                    object: charge,
                    transform: Transform::at(DVec3::new(offset, 0.2, 0.35))
                        .expect("edit position is finite"),
                }])
                .expect("moving a charge is a valid edit");
            world.snapshot()
        })
        .collect();

    measure(
        config,
        || maxwell_solver(scene, &base),
        |solver, iteration| {
            solver
                .on_world_changed(&snapshots[iteration as usize % snapshots.len()])
                .expect("a moved charge is representable")
        },
    )
}

/// Cost when an edit cannot change the constrained state.
///
/// Moving a probe or a slice plane is the most frequent interaction in the app,
/// and the runtime calls `on_world_changed` for every accepted commit. This must
/// stay flat in cells: if it starts tracking lattice size, the source-equality
/// skip has regressed and every drag is rebuilding the grid again.
fn maxwell_edit_probe(scene: &Scene, config: &MeasureConfig) -> Timing {
    let base = scene.world();
    let mut moved = base.clone();
    let probe = *moved
        .snapshot()
        .probes()
        .keys()
        .next()
        .expect("probe edit benchmarks need a probe");
    moved
        .commit([WorldCommand::SetProbePosition {
            probe,
            position: ProbePosition::World(DVec3::new(1.5, 0.25, 0.5)),
        }])
        .expect("moving a probe is a valid edit");
    let snapshot = moved.snapshot();

    measure(
        config,
        || maxwell_solver(scene, &base),
        |solver, _| {
            solver
                .on_world_changed(&snapshot)
                .expect("a moved probe is representable")
        },
    )
}

fn electrostatics_sample_plane(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = scene.world();
    let geometry = plane_geometry(scene);
    measure(
        config,
        || electrostatics_solver(scene, &world),
        |solver, _| {
            solver
                .sample(ELECTRIC_FIELD_HANDLE, &geometry)
                .expect("the electric channel is published")
        },
    )
}

fn gravity_sample_plane(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = gravity_world(scene);
    let geometry = plane_geometry(scene);
    measure(
        config,
        || gravity_solver(scene, &world),
        |solver, _| {
            solver
                .sample(GRAVITATIONAL_ACCELERATION_HANDLE, &geometry)
                .expect("the gravitational acceleration channel is published")
        },
    )
}

fn gravity_forces(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = gravity_world(scene);
    let snapshot = world.snapshot();
    let sources = fieldcad_mass_sources::collect_gravity_sources(&snapshot)
        .expect("gravity world has valid mass sources");
    let body = DynamicBody {
        object: sources[0].object,
        inertial_mass_kg: sources[0].inertial_mass_kg,
        position: sources[0].position,
        velocity: Default::default(),
    };
    measure(
        config,
        || gravity_solver(scene, &world),
        |solver, _| {
            solver
                .forces(&[body])
                .expect("force calculation from a valid scene is defined")
        },
    )
}

fn gravity_solver_init(scene: &Scene, config: &MeasureConfig) -> Timing {
    let world = gravity_world(scene);
    measure(config, || (), |(), _| gravity_solver(scene, &world))
}

/// One end-to-end fixed tick: every active solver advances, then the runtime
/// publishes an immutable snapshot over the scene's subscription.
fn runtime_tick(scene: &Scene, config: &MeasureConfig) -> Timing {
    measure(
        config,
        || runtime_for(scene),
        |runtime, _| runtime.step_once().expect("a paused runtime can step"),
    )
}

fn runtime_commit_charge_edit(scene: &Scene, config: &MeasureConfig) -> Timing {
    let charge = first_charge(&scene.world());
    measure(
        config,
        || runtime_for(scene),
        |runtime, iteration| {
            // A fresh position every iteration, so the world revision really
            // advances and no commit is skipped as a no-op.
            let offset = 0.3 + (iteration % 32) as f64 * 0.01;
            runtime
                .commit_world_commands(vec![WorldCommand::SetTransform {
                    object: charge,
                    transform: Transform::at(DVec3::new(offset, 0.2, 0.35))
                        .expect("edit position is finite"),
                }])
                .expect("moving a charge is a valid edit")
        },
    )
}

fn runtime_commit_probe_edit(scene: &Scene, config: &MeasureConfig) -> Timing {
    let probe = *scene
        .world()
        .snapshot()
        .probes()
        .keys()
        .next()
        .expect("probe edit benchmarks need a probe");
    measure(
        config,
        || runtime_for(scene),
        |runtime, iteration| {
            let offset = 1.0 + (iteration % 32) as f64 * 0.01;
            runtime
                .commit_world_commands(vec![WorldCommand::SetProbePosition {
                    probe,
                    position: ProbePosition::World(DVec3::new(offset, 0.25, 0.5)),
                }])
                .expect("moving a probe is a valid edit")
        },
    )
}

// --- sweeps ----------------------------------------------------------------

/// Lattice sizes. Spans a 64x range in cells, which is enough separation for a
/// log-log fit to distinguish linear from quadratic.
fn cell_sweep(base: Scene, quick: bool) -> Vec<Scene> {
    let axes: &[u32] = if quick {
        &[16, 24, 32]
    } else {
        &[16, 24, 32, 48, 64]
    };
    axes.iter()
        .map(|&axis| {
            base.clone()
                .with_cells_per_axis(axis)
                .with_name(format!("{}-{axis}cubed", base.name))
        })
        .collect()
}

fn charge_sweep(base: Scene, quick: bool) -> Vec<Scene> {
    let counts: &[usize] = if quick {
        &[1, 4, 16]
    } else {
        &[1, 2, 4, 8, 16, 32]
    };
    counts
        .iter()
        .map(|&charges| {
            base.clone()
                .with_charges(charges)
                .with_name(format!("{}-{charges}q", base.name))
        })
        .collect()
}

fn sample_sweep(base: Scene, quick: bool) -> Vec<Scene> {
    let densities: &[u32] = if quick {
        &[17, 33, 65]
    } else {
        &[17, 33, 65, 129, 257]
    };
    densities
        .iter()
        .map(|&density| {
            base.clone()
                .with_planes(1)
                .with_domain_stride(None)
                .with_plane_samples_per_axis(density)
                .with_name(format!("{}-{density}sq", base.name))
        })
        .collect()
}

/// Every benchmark the harness knows how to run.
///
/// Adding a hot path here is the intended way to extend the harness: declare
/// what scales it, what complexity it should have, and how to time it.
pub fn benchmarks(quick: bool) -> Vec<Benchmark> {
    let default = Scene::desktop_default();

    vec![
        Benchmark {
            id: "maxwell/step",
            group: "maxwell",
            what: "one Yee leapfrog tick (B half, E full, B half)",
            why: "the inner loop of every running simulation; Milestone 6 changes it",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("step"), quick),
            runner: maxwell_step,
        },
        Benchmark {
            id: "maxwell/solver-init",
            group: "maxwell",
            what: "create_solver, including the constrained static-charge state",
            why: "runs on activation and on every charge edit",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("init"), quick),
            runner: maxwell_solver_init,
        },
        Benchmark {
            id: "maxwell/solver-init-by-charges",
            group: "maxwell",
            what: "create_solver as the authored source count grows",
            why: "the constrained state sums every source at every node, so this \
                  is the term that turns a scene edit expensive",
            parameter: Parameter::Charges,
            declared: Complexity::Linear,
            scenes: charge_sweep(default.clone().with_name("init"), quick),
            runner: maxwell_solver_init,
        },
        Benchmark {
            id: "maxwell/sample-plane",
            group: "maxwell",
            what: "trilinear reconstruction of E over a slice plane",
            why: "runs once per published channel per snapshot",
            parameter: Parameter::Samples,
            declared: Complexity::Linear,
            scenes: sample_sweep(default.clone().with_name("sample"), quick),
            runner: maxwell_sample_plane,
        },
        Benchmark {
            id: "maxwell/diagnostics",
            group: "maxwell",
            what: "energy and divergence residuals over the whole lattice",
            why: "a full-grid sweep the runtime performs on every publication, \
                  independent of how little the visualizer subscribed to",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("diagnostics"), quick),
            runner: maxwell_diagnostics,
        },
        Benchmark {
            id: "maxwell/edit-charge",
            group: "maxwell",
            what: "on_world_changed after a charge actually moved",
            why: "the unavoidable rebuild; sets the cost of dragging a source",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("edit-charge"), quick),
            runner: maxwell_edit_charge,
        },
        Benchmark {
            id: "maxwell/edit-probe",
            group: "maxwell",
            what: "on_world_changed after an edit that cannot change the charges",
            why: "must stay flat in cells; if it does not, probe and plane drags \
                  are rebuilding the lattice again",
            parameter: Parameter::Cells,
            declared: Complexity::Constant,
            scenes: cell_sweep(default.clone().with_name("edit-probe"), quick),
            runner: maxwell_edit_probe,
        },
        Benchmark {
            id: "electrostatics/sample-plane",
            group: "electrostatics",
            what: "analytic Coulomb evaluation over a slice plane",
            why: "the analytic oracle's cost per presentation sample",
            parameter: Parameter::Samples,
            declared: Complexity::Linear,
            scenes: sample_sweep(default.clone().with_name("sample"), quick),
            runner: electrostatics_sample_plane,
        },
        Benchmark {
            id: "electrostatics/sample-by-charges",
            group: "electrostatics",
            what: "analytic Coulomb evaluation as the source count grows",
            why: "superposition is linear in sources at fixed density; a steeper \
                  slope means the batch path regressed",
            parameter: Parameter::Charges,
            declared: Complexity::Linear,
            scenes: charge_sweep(
                default
                    .clone()
                    .with_name("sample")
                    .with_plane_samples_per_axis(65)
                    .with_domain_stride(None),
                quick,
            ),
            runner: electrostatics_sample_plane,
        },
        Benchmark {
            id: "gravity/sample-plane",
            group: "gravity",
            what: "analytic Newtonian superposition over a slice plane",
            why: "the newest solver's sampling cost; same inverse-square kernel as \
                  electrostatics but with signed G constant and an opposite sign",
            parameter: Parameter::Samples,
            declared: Complexity::Linear,
            scenes: sample_sweep(default.clone().with_name("g-sample"), quick),
            runner: gravity_sample_plane,
        },
        Benchmark {
            id: "gravity/sample-by-charges",
            group: "gravity",
            what: "analytic Newtonian evaluation as the source count grows",
            why: "superposition is linear in sources at fixed density; this is the \
                  gravity analogue of electrostatics/sample-by-charges and the two \
                  should agree on exponent",
            parameter: Parameter::Charges,
            declared: Complexity::Linear,
            scenes: charge_sweep(
                default
                    .clone()
                    .with_name("g-sample")
                    .with_plane_samples_per_axis(65)
                    .with_domain_stride(None),
                quick,
            ),
            runner: gravity_sample_plane,
        },
        Benchmark {
            id: "gravity/forces",
            group: "gravity",
            what: "force on one body from every other source",
            why: "O(n) in sources at fixed cell count; tracks the per-tick cost \
                  the dynamics system accumulates",
            parameter: Parameter::Charges,
            declared: Complexity::Linear,
            scenes: charge_sweep(default.clone().with_name("g-forces"), quick),
            runner: gravity_forces,
        },
        Benchmark {
            id: "gravity/solver-init",
            group: "gravity",
            what: "create_solver for the analytic Newtonian backend",
            why: "runs on activation; the analytic solver is trivially \
                  constructed so this is a baseline for the Maxwell init cost",
            parameter: Parameter::Cells,
            declared: Complexity::Constant,
            scenes: cell_sweep(default.clone().with_name("g-init"), quick),
            runner: gravity_solver_init,
        },
        Benchmark {
            id: "runtime/tick",
            group: "runtime",
            what: "step_once: advance every solver, then publish a snapshot",
            why: "end-to-end tick cost; the difference from maxwell/step is what \
                  publication and sampling cost",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("tick"), quick),
            runner: runtime_tick,
        },
        Benchmark {
            id: "runtime/commit-charge-edit",
            group: "runtime",
            what: "commit a charge move: validate, adopt, rebuild, republish",
            why: "the full cost of dragging a source in the viewport",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.clone().with_name("charge-edit"), quick),
            runner: runtime_commit_charge_edit,
        },
        Benchmark {
            id: "runtime/commit-probe-edit",
            group: "runtime",
            what: "commit a probe move: validate, adopt, republish",
            why: "compare against commit-charge-edit to separate solver rebuild \
                  from publication",
            parameter: Parameter::Cells,
            declared: Complexity::Linear,
            scenes: cell_sweep(default.with_name("probe-edit"), quick),
            runner: runtime_commit_probe_edit,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_identifiers_are_unique_and_namespaced() {
        let benchmarks = benchmarks(true);
        let mut ids: Vec<_> = benchmarks.iter().map(|bench| bench.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();

        assert_eq!(ids.len(), count, "benchmark IDs must be unique");
        for bench in &benchmarks {
            assert!(
                bench.id.starts_with(bench.group),
                "{} is not namespaced by its group {}",
                bench.id,
                bench.group
            );
        }
    }

    #[test]
    fn every_sweep_varies_its_declared_parameter() {
        // A sweep whose parameter never changes cannot produce a slope, so the
        // complexity claim would be untestable.
        for bench in benchmarks(false) {
            let values: Vec<_> = bench
                .scenes
                .iter()
                .map(|scene| bench.parameter.value(scene))
                .collect();
            let first = values[0];

            assert!(bench.scenes.len() >= 3, "{} sweeps too few sizes", bench.id);
            assert!(
                values.iter().any(|value| *value != first),
                "{} does not vary {}",
                bench.id,
                bench.parameter.label()
            );
        }
    }

    #[test]
    fn every_benchmark_runs_and_reports_a_positive_cost() {
        // Smoke test at the smallest configuration: proves each runner's setup,
        // world, and solver composition actually work.
        let config = MeasureConfig {
            reps: 1,
            warmup_reps: 0,
            min_rep_time: std::time::Duration::from_micros(1),
            max_iterations: 2,
        };
        for bench in benchmarks(true) {
            let scene = &bench.scenes[0];
            let timing = (bench.runner)(scene, &config);

            assert!(
                timing.median_ns > 0.0,
                "{} reported no cost for {}",
                bench.id,
                scene.name
            );
        }
    }
}
