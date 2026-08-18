//! One-off Phase 6a measurement: decompose the inverse-square `sample`
//! path's cost into its cache-hit (assembly) and cold-publication
//! (evaluate + assemble) regimes.
//!
//! Uses the same `measure`/`Timing` infrastructure as `fieldcad-bench`
//! itself, so the reported min/mean/median/max distribution and total
//! iteration count sit on the same footing as every other benchmark in this
//! crate — a single steady/cold number here would be exactly the kind of
//! one-run claim the harness exists to avoid making.
//!
//! Usage: cargo run --release -p fieldcad-bench --example sample_path

use fieldcad_bench::measure::{MeasureConfig, Timing, measure};
use fieldcad_bench::report::format_ns;
use fieldcad_core::quantities::ChargeCoulombs;
use fieldcad_core::{
    Domain, ObjectId, ObjectShape, ObjectSpec, PlaneId, PlaneLattice, SampleGeometry, StepContext,
    TimeStep, Transform, World, WorldCommand,
};
use fieldcad_electrostatics::ElectrostaticsPlugin;
use fieldcad_plugin_api::{
    EquationSystemPlugin, EquationSystemSolver, SolverCancellation, SolverContext,
};
use glam::{DVec3, UVec2};

fn world_with_charge() -> World {
    use fieldcad_core::quantities::coulomb;
    use fieldcad_electromagnetic_sources::{charge_component_id, charge_properties};
    let mut world = World::new();
    world
        .commit([
            WorldCommand::RegisterComponentSchema(
                ElectrostaticsPlugin::new().component_schemas().remove(0),
            ),
            WorldCommand::CreateObject(
                ObjectSpec::new("charge")
                    .with_transform(Transform::at_finite(DVec3::new(0.5, 0.1, 0.0)))
                    .with_shape(ObjectShape::point(0.01).unwrap())
                    .with_component(
                        charge_component_id(),
                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-8)).unwrap(),
                    ),
            ),
        ])
        .unwrap();
    world
}

fn solver(world: &World) -> Box<dyn EquationSystemSolver> {
    let domain = Domain::centred_cube(4.0, 8).unwrap();
    ElectrostaticsPlugin::new()
        .create_solver(SolverContext {
            configuration: &Default::default(),
            domain: &domain,
            world: &world.snapshot(),
            initial_step: StepContext {
                tick: 0,
                time_seconds: 0.0,
                time_step: TimeStep::from_seconds(0.1).unwrap(),
            },
            cancellation: SolverCancellation::default(),
        })
        .unwrap()
}

fn plane(count: u32) -> SampleGeometry {
    let span = 3.2_f64;
    let step = span / f64::from(count - 1);
    SampleGeometry::Plane {
        plane: PlaneId::new(0),
        lattice: PlaneLattice::new(
            DVec3::new(-span * 0.5, -span * 0.5, 0.25),
            DVec3::new(step, 0.0, 0.0),
            DVec3::new(0.0, step, 0.0),
            UVec2::splat(count),
        ),
    }
}

/// Regime 1: steady-state reads (cache hits) — pure assembly, no
/// evaluation. `setup` warms the cache once so every timed call is a hit.
fn steady_state(world: &World, geometry: &SampleGeometry, config: &MeasureConfig) -> Timing {
    measure(
        config,
        || {
            let solver = solver(world);
            solver
                .sample(fieldcad_electrostatics::ELECTRIC_FIELD_HANDLE, geometry)
                .unwrap();
            solver
        },
        |solver, _| {
            solver
                .sample(fieldcad_electrostatics::ELECTRIC_FIELD_HANDLE, geometry)
                .unwrap()
        },
    )
}

struct DragState {
    solver: Box<dyn EquationSystemSolver>,
    base: World,
    charge_object: ObjectId,
}

/// Regime 2: drag publications — invalidate (force recompute), then sample
/// both channels, the runtime's per-publish read pattern.
fn drag_publish(world: &World, geometry: &SampleGeometry, config: &MeasureConfig) -> Timing {
    let charge_object = world.snapshot().objects().keys().next().copied().unwrap();
    measure(
        config,
        || DragState {
            solver: solver(world),
            base: world.clone(),
            charge_object,
        },
        |state, iteration| {
            let mut moved = state.base.clone();
            moved
                .commit([WorldCommand::SetTransform {
                    object: state.charge_object,
                    transform: Transform::at_finite(DVec3::new(
                        0.5 + iteration as f64 * 1.0e-3,
                        0.1,
                        0.0,
                    )),
                }])
                .unwrap();
            state.solver.on_world_changed(&moved.snapshot()).unwrap();
            state
                .solver
                .sample(fieldcad_electrostatics::ELECTRIC_FIELD_HANDLE, geometry)
                .unwrap();
            state
                .solver
                .sample(fieldcad_electrostatics::ELECTRIC_POTENTIAL_HANDLE, geometry)
                .unwrap()
        },
    )
}

fn print_row(label: &str, samples: u64, timing: &Timing) {
    println!(
        "{label:6} {samples:>8} | min {:>10} | mean {:>10} | median {:>10} | max {:>10} | {:>9} iters",
        format_ns(timing.min_ns),
        format_ns(timing.mean_ns),
        format_ns(timing.median_ns),
        format_ns(timing.max_ns),
        timing.total_iterations(),
    );
}

fn main() {
    let world = world_with_charge();
    let config = MeasureConfig::default();

    for count in [65u32, 129, 257] {
        let geometry = plane(count);
        let samples = u64::from(count) * u64::from(count);

        print_row("steady", samples, &steady_state(&world, &geometry, &config));
        print_row("drag", samples, &drag_publish(&world, &geometry, &config));
        println!();
    }
}
