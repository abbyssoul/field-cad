//! Named, fully specified scenes.
//!
//! Compute cost in this application is a function of the scene, so a timing
//! that does not say which scene it measured is not a result. Every scene here
//! prints its own domain, source count, and published sample count, and every
//! reported number carries that description.
//!
//! Scenes are built deterministically from a seed-free rule so a sweep is
//! reproducible and two runs on the same machine compare directly.

use fieldcad_core::quantities::{ChargeCoulombs, coulomb};
use fieldcad_core::{
    BoundaryCondition, BoundaryConditions, Domain, DomainBounds, ObjectShape, ObjectSpec,
    Precision, ProbeSpec, Resolution, SlicePlaneSpec, Transform, World, WorldCommand,
};
use fieldcad_electromagnetic_sources::{
    charge_component_id, charge_component_schema, charge_properties,
};

use fieldcad_electromagnetism::{
    electric_field_channel_id as maxwell_electric_channel_id, magnetic_field_channel_id,
};
use fieldcad_electrostatics::{electric_field_channel_id, electric_potential_channel_id};
use fieldcad_simulation::Subscription;
use glam::{DVec2, DVec3, UVec2};
use serde::Serialize;

/// Which Maxwell initial condition a scene composes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum MaxwellMode {
    /// The desktop default: `E` constrained by authored stationary charges.
    StaticCharges,
    /// The source-free convergence and parity fixture.
    PrescribedWave,
}

/// A complete, reproducible description of what is being measured.
#[derive(Clone, Debug, Serialize)]
pub struct Scene {
    pub name: String,
    /// One line a reader can check the physics against.
    pub summary: String,
    pub cells_per_axis: u32,
    pub half_extent_metres: f64,
    pub charges: usize,
    pub probes: usize,
    pub planes: usize,
    /// Presentation samples per axis on each slice plane.
    pub plane_samples_per_axis: u32,
    /// Whole-domain lattice decimation, when a sparse 3D view is subscribed.
    pub domain_stride: Option<u32>,
    pub maxwell: MaxwellMode,
    pub precision: Precision,
    /// Nodes in an expression-graph benchmark; zero for solver scenes.
    pub expression_nodes: usize,
    /// Live property bindings in an expression benchmark; zero for solver scenes.
    pub live_bindings: usize,
}

impl Scene {
    /// The scene the desktop actually ships: one off-centre 1 nC charge, one
    /// probe, one XY plane, and a sparse whole-domain view.
    ///
    /// Anchoring the harness to the shipped configuration means the headline
    /// numbers describe what a user experiences, not a benchmark-only scene.
    pub fn desktop_default() -> Self {
        Self {
            name: "desktop-default".to_owned(),
            summary: "shipped scene: 1 off-centre point charge, 1 probe, 1 XY plane, sparse 3D"
                .to_owned(),
            cells_per_axis: 32,
            half_extent_metres: 5.0,
            charges: 1,
            probes: 1,
            planes: 1,
            plane_samples_per_axis: 33,
            domain_stride: Some(8),
            maxwell: MaxwellMode::StaticCharges,
            precision: Precision::F64,
            expression_nodes: 0,
            live_bindings: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn with_cells_per_axis(mut self, cells_per_axis: u32) -> Self {
        self.cells_per_axis = cells_per_axis;
        self
    }

    pub fn with_charges(mut self, charges: usize) -> Self {
        self.charges = charges;
        self
    }

    pub fn with_expression_nodes(mut self, expression_nodes: usize) -> Self {
        self.expression_nodes = expression_nodes;
        self
    }

    pub fn with_live_bindings(mut self, live_bindings: usize) -> Self {
        self.live_bindings = live_bindings;
        self
    }

    pub fn with_probes(mut self, probes: usize) -> Self {
        self.probes = probes;
        self
    }

    pub fn with_planes(mut self, planes: usize) -> Self {
        self.planes = planes;
        self
    }

    pub fn with_plane_samples_per_axis(mut self, samples: u32) -> Self {
        self.plane_samples_per_axis = samples;
        self
    }

    pub fn with_domain_stride(mut self, stride: Option<u32>) -> Self {
        self.domain_stride = stride;
        self
    }

    pub fn with_maxwell(mut self, maxwell: MaxwellMode) -> Self {
        self.maxwell = maxwell;
        self
    }

    pub fn with_precision(mut self, precision: Precision) -> Self {
        self.precision = precision;
        self
    }

    /// Yee lattice cells. The scaling parameter for field advance and
    /// diagnostics.
    pub fn cells(&self) -> u64 {
        u64::from(self.cells_per_axis).pow(3)
    }

    /// Samples one publication asks every active channel for. The scaling
    /// parameter for the sampling path.
    pub fn samples_per_channel(&self) -> u64 {
        let planes = self.planes as u64 * u64::from(self.plane_samples_per_axis).pow(2);
        let grid = self.domain_stride.map_or(0, |stride| {
            let per_axis = u64::from(self.cells_per_axis.div_ceil(stride.max(1)));
            per_axis.pow(3)
        });
        self.probes as u64 + planes + grid
    }

    pub fn domain(&self) -> Domain {
        Domain::new(
            DomainBounds::centred_cube(self.half_extent_metres)
                .expect("scene half extent is positive"),
            Resolution::uniform(self.cells_per_axis)
                .expect("scene resolution is at least one cell"),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            self.precision,
        )
    }

    pub fn subscription(&self) -> Subscription {
        let mut subscription = Subscription::PROBES_ONLY;
        if self.planes > 0 {
            subscription = subscription.with_planes(UVec2::splat(self.plane_samples_per_axis));
        }
        if let Some(stride) = self.domain_stride {
            subscription = subscription.with_domain_stride(stride);
        }
        subscription
    }

    /// Charge positions, spread deterministically through the interior.
    ///
    /// Deliberately never centred and never on a lattice node. A charge at the
    /// origin makes the periodic seam symmetric and a node-aligned charge hits
    /// an interpolation special case; either would measure an unrepresentatively
    /// tidy scene.
    pub fn charge_positions(&self) -> Vec<DVec3> {
        let reach = self.half_extent_metres * 0.55;
        (0..self.charges)
            .map(|index| {
                // An irrational-turn spiral so no two sources share an axis
                // plane however many are requested.
                let turn = index as f64 * 2.399_963_229_728_653;
                // Half-offset so the first source is never at radius zero. A
                // charge exactly at the origin is the symmetric special case
                // that hid the periodic seam defect in Milestone 5.
                let radial = ((index as f64 + 0.5) / self.charges.max(1) as f64).sqrt();
                let radius = reach * radial;
                DVec3::new(
                    radius * turn.cos(),
                    radius * turn.sin(),
                    reach * 0.6 * ((index as f64 * 0.7).sin()),
                )
            })
            .collect()
    }

    /// The authoring edits that populate this scene, without schema
    /// registration.
    ///
    /// Kept separate because `SimulationRuntime::new` registers every plugin's
    /// component schemas itself, and registering the charge schema twice is a
    /// rejected command. The runtime path therefore starts from an empty world
    /// and commits these, exactly as the desktop's composition root does.
    pub fn authoring_commands(&self) -> Vec<WorldCommand> {
        let mut commands = Vec::new();

        for (index, position) in self.charge_positions().into_iter().enumerate() {
            commands.push(WorldCommand::CreateObject(
                ObjectSpec::new(format!("charge {index}"))
                    .with_transform(Transform::at_finite(position))
                    .with_shape(ObjectShape::point(0.15).expect("source radius is positive"))
                    .with_component(
                        charge_component_id(),
                        charge_properties(ChargeCoulombs::new::<coulomb>(1.0e-9))
                            .expect("charge is a valid quantity"),
                    ),
            ));
        }

        let recorded = vec![
            electric_field_channel_id(),
            electric_potential_channel_id(),
            maxwell_electric_channel_id(),
            magnetic_field_channel_id(),
        ];
        for index in 0..self.probes {
            let offset = 1.0 + index as f64 * 0.25;
            commands.push(WorldCommand::CreateProbe(ProbeSpec::at(
                format!("probe {index}"),
                DVec3::new(offset, 0.0, 0.35),
                recorded.clone(),
            )));
        }

        for index in 0..self.planes {
            let normal = match index % 3 {
                0 => DVec3::Z,
                1 => DVec3::Y,
                _ => DVec3::X,
            };
            commands.push(WorldCommand::CreatePlane(
                SlicePlaneSpec::new(format!("plane {index}"), DVec3::ZERO, normal)
                    .and_then(|plane| {
                        plane.with_half_extent(DVec2::splat(self.half_extent_metres * 0.8))
                    })
                    .expect("slice plane is valid"),
            ));
        }

        commands
    }

    /// The authored world for this scene, with the charge schema registered.
    ///
    /// For benchmarks that drive a solver directly, with no runtime to register
    /// schemas on their behalf.
    pub fn world(&self) -> World {
        let mut world = World::new();
        let mut commands = vec![WorldCommand::RegisterComponentSchema(
            charge_component_schema(),
        )];
        commands.extend(self.authoring_commands());
        world.commit(commands).expect("scene world is valid");
        world
    }

    /// A compact one-line size description for the report.
    pub fn size_label(&self) -> String {
        format!(
            "{c}³={cells} cells, {q} charge(s), {s} sample(s)/channel, {e} expression node(s), {l} live binding(s)",
            c = self.cells_per_axis,
            cells = self.cells(),
            q = self.charges,
            s = self.samples_per_channel(),
            e = self.expression_nodes,
            l = self.live_bindings,
        )
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_electromagnetic_sources::collect_charge_sources;

    use super::*;

    #[test]
    fn the_default_scene_matches_what_the_desktop_ships() {
        let scene = Scene::desktop_default();

        assert_eq!(scene.cells(), 32 * 32 * 32);
        assert_eq!(scene.domain().resolution().cell_count(), 32u64.pow(3));
        // 1 probe + 33² plane + (32/8)³ sparse grid.
        assert_eq!(scene.samples_per_channel(), 1 + 1089 + 64);
    }

    #[test]
    fn scene_worlds_carry_every_authored_charge() {
        for charges in [1, 4, 32] {
            let scene = Scene::desktop_default().with_charges(charges);
            let world = scene.world();

            let sources = collect_charge_sources(&world.snapshot()).unwrap();

            assert_eq!(sources.len(), charges);
        }
    }

    #[test]
    fn charges_avoid_the_centre_and_each_other() {
        // A centred charge makes the periodic seam symmetric and hides real
        // boundary cost and error; coincident charges would understate work.
        let scene = Scene::desktop_default().with_charges(16);
        let positions = scene.charge_positions();

        for (index, position) in positions.iter().enumerate() {
            assert!(
                position.length() > 0.0,
                "charge {index} sits exactly at the origin"
            );
            assert!(
                position.abs().max_element() < scene.half_extent_metres,
                "charge {index} escaped the domain"
            );
            for other in &positions[index + 1..] {
                assert!(position.distance(*other) > 1.0e-6, "charges coincide");
            }
        }
    }

    #[test]
    fn sample_counts_track_the_subscribed_presentation_density() {
        let sparse = Scene::desktop_default()
            .with_planes(0)
            .with_domain_stride(None);
        let dense = Scene::desktop_default()
            .with_planes(3)
            .with_plane_samples_per_axis(65);

        assert_eq!(sparse.samples_per_channel(), 1);
        assert_eq!(dense.samples_per_channel(), 1 + 3 * 65 * 65 + 64);
    }
}
