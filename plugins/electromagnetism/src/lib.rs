//! CPU `f64` reference solver for Maxwell equations.
//!
//! Electric and magnetic components live on the conventional staggered Yee
//! lattice. Time integration uses the synchronized kick-drift-kick form of the
//! Yee leapfrog, so snapshots expose `E` and `B` at the same simulation time.
//! This solver supports periodic boundaries, a prescribed plane-wave validation
//! condition, a constrained electrostatic initialization for stationary
//! authored charges, and charge-conserving field/particle coupling.

mod coupling;

pub use coupling::{
    CoupledAdvance, continuity_residual, deposit_charge_conserving_current,
    deposit_particle_charge, deposit_source_charge, interpolate_particle_fields,
    periodic_charge_initial_state, relativistic_boris_velocity,
};

use std::sync::Arc;

use fieldcad_core::{
    BoundaryCondition, ChannelId, ChannelSchema, ComponentSchema, DiagnosticSeverity, Dimension,
    Domain, FieldColumn, FieldValueKind, InterpolationMethod, PluginId, PluginVersion, Precision,
    PropertyBag, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, SampleGeometry,
    SampleValidity, SolverDiagnostic, StepContext, TimeStep, UndefinedReason, WorldRevision,
    WorldSnapshot,
};
use fieldcad_electromagnetic_sources::{
    ChargeDistribution, ChargeSource, charge_component_id, charge_component_schema,
    collect_charge_sources, electric_field_channel_schema, magnetic_field_channel_schema,
};
use fieldcad_mass_sources::mass_component_schemas;
use fieldcad_particles::particle_component_schema;
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginConfigurationSchema,
    PluginError, PluginMetadata, ResolvedFieldBrushStroke, SampledColumn, SolverCancellation,
    SolverContext, SolverKind, SolverStepOutcome,
};
use glam::{DVec3, IVec3, UVec3};

use coupling::{ParticleCoupling, collect_coupled_particles, coupling_is_requested};

pub const PLUGIN_ID: &str = "fieldcad.electromagnetism";
pub const ENERGY_DENSITY_CHANNEL: &str = "energy-density";
pub const ELECTRIC_DIVERGENCE_CHANNEL: &str = "electric-divergence-residual";
pub const MAGNETIC_DIVERGENCE_CHANNEL: &str = "magnetic-divergence-residual";

const AMPLITUDE_PROPERTY: &str = "plane-wave-amplitude";
const MODE_PROPERTY: &str = "plane-wave-mode";
const INITIAL_CONDITION_PROPERTY: &str = "initial-condition";
const STATIC_CHARGES_OPTION: &str = "Static charges";
const PLANE_WAVE_OPTION: &str = "Prescribed plane wave";

pub const ELECTRIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const MAGNETIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(1);
pub const ENERGY_DENSITY_HANDLE: ChannelHandle = ChannelHandle::new(2);
pub const ELECTRIC_DIVERGENCE_HANDLE: ChannelHandle = ChannelHandle::new(3);
pub const MAGNETIC_DIVERGENCE_HANDLE: ChannelHandle = ChannelHandle::new(4);

/// Re-exported so this plugin and the dynamics integrator cannot disagree.
pub use fieldcad_core::SPEED_OF_LIGHT;
/// Vacuum permeability used by the reference energy diagnostic.
pub const VACUUM_PERMEABILITY: f64 = 1.256_637_062_12e-6;
/// Kept algebraically consistent with `c` and `mu0` for the discrete update.
pub const VACUUM_PERMITTIVITY: f64 = 1.0 / (VACUUM_PERMEABILITY * SPEED_OF_LIGHT * SPEED_OF_LIGHT);
/// Derived from the same vacuum constants used by the Maxwell update.
pub const COULOMB_CONSTANT: f64 = 1.0 / (4.0 * std::f64::consts::PI * VACUUM_PERMITTIVITY);

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

fn channel_id(name: &str) -> ChannelId {
    ChannelId::new(plugin_id(), name).expect("static channel ID is valid")
}

/// `E` and `B` are the scene's fields, which this system is one model of. The
/// energy density and divergence residuals below stay in this plugin's own
/// namespace: they are diagnostics of a Yee discretization, not quantities the
/// world has independently of how it was discretized.
pub use fieldcad_electromagnetic_sources::{electric_field_channel_id, magnetic_field_channel_id};

pub fn energy_density_channel_id() -> ChannelId {
    channel_id(ENERGY_DENSITY_CHANNEL)
}

pub fn electric_divergence_channel_id() -> ChannelId {
    channel_id(ELECTRIC_DIVERGENCE_CHANNEL)
}

pub fn magnetic_divergence_channel_id() -> ChannelId {
    channel_id(MAGNETIC_DIVERGENCE_CHANNEL)
}

fn amplitude_property_id() -> PropertyId {
    PropertyId::new(AMPLITUDE_PROPERTY).expect("static property ID is valid")
}

fn mode_property_id() -> PropertyId {
    PropertyId::new(MODE_PROPERTY).expect("static property ID is valid")
}

fn initial_condition_property_id() -> PropertyId {
    PropertyId::new(INITIAL_CONDITION_PROPERTY).expect("static property ID is valid")
}

/// How a Maxwell solver obtains its initial constrained field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MaxwellInitialCondition {
    /// Build a curl-free electric field from authored stationary charges and
    /// rebuild it when those sources are edited. Magnetic field starts at zero.
    StaticCharges,
    /// Source-free validation case retained for convergence and backend parity.
    PrescribedPlaneWave { amplitude: f64, mode: u32 },
}

impl MaxwellInitialCondition {
    pub const fn label(self) -> &'static str {
        match self {
            Self::StaticCharges => STATIC_CHARGES_OPTION,
            Self::PrescribedPlaneWave { .. } => PLANE_WAVE_OPTION,
        }
    }

    /// Whether this initial state is genuinely periodic on the lattice.
    pub const fn periodicity(self) -> LatticePeriodicity {
        match self {
            Self::StaticCharges => LatticePeriodicity::SeamOnOuterLayer,
            Self::PrescribedPlaneWave { .. } => LatticePeriodicity::Periodic,
        }
    }
}

/// Whether every stored lattice value is meaningful, or the outermost layer
/// differences two opposite faces of the domain.
///
/// A periodic lattice can only carry a field whose potential is periodic. The
/// prescribed plane wave is; an isolated charge's Coulomb potential is not, so
/// the constrained static state has a seam. Values that would be read across
/// that seam are reported as undefined rather than published as measurements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatticePeriodicity {
    Periodic,
    SeamOnOuterLayer,
}

/// Cell indices on one axis that a channel cannot read without crossing a seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeamIndices {
    None,
    Last(u32),
    FirstAndLast(u32),
}

impl SeamIndices {
    const fn contains(self, index: u32) -> bool {
        match self {
            Self::None => false,
            Self::Last(last) => index == last,
            Self::FirstAndLast(last) => index == 0 || index == last,
        }
    }
}

/// Backend-neutral inputs for one Maxwell solver instance.
#[derive(Clone, Debug)]
pub struct MaxwellSolverSetup {
    pub domain: Domain,
    pub initial_condition: MaxwellInitialCondition,
    pub initial_state: YeeFieldState,
    /// The authored charges `initial_state` was constrained by, empty unless
    /// the initial condition reads them. Retained so a later world edit can
    /// tell whether the constraint actually changed.
    pub initial_sources: Vec<ChargeSource>,
    /// Generic particle state used when field/particle coupling is active.
    pub initial_particles: Vec<fieldcad_particles::Particle>,
    pub particle_coupling: bool,
    pub world_revision: WorldRevision,
    pub initial_step: StepContext,
    pub cancellation: SolverCancellation,
}

/// Host-injected implementation of Maxwell storage and advancement.
///
/// The plugin owns the equations and channel contract. The application host can
/// inject a backend that uses its existing `wgpu` device without introducing
/// graphics types into this headless crate or exposing solver buffers to the
/// renderer.
pub trait MaxwellSolverBackend: Send + Sync {
    fn precision(&self) -> Precision;

    fn create_solver(
        &self,
        setup: MaxwellSolverSetup,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError>;
}

#[derive(Clone, Copy, Debug, Default)]
struct CpuMaxwellBackend;

impl MaxwellSolverBackend for CpuMaxwellBackend {
    fn precision(&self) -> Precision {
        Precision::F64
    }

    fn create_solver(
        &self,
        setup: MaxwellSolverSetup,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        Ok(Box::new(MaxwellSolver::new(setup)))
    }
}

/// Maxwell equation system with an injectable CPU or host-owned GPU
/// backend. Every backend declares the same fields, configuration, and plugin
/// identity.
#[derive(Clone)]
pub struct ElectromagnetismPlugin {
    backend: Arc<dyn MaxwellSolverBackend>,
}

impl Default for ElectromagnetismPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ElectromagnetismPlugin {
    pub fn new() -> Self {
        Self {
            backend: Arc::new(CpuMaxwellBackend),
        }
    }

    pub fn with_backend(backend: Arc<dyn MaxwellSolverBackend>) -> Self {
        Self { backend }
    }
}

impl EquationSystemPlugin for ElectromagnetismPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 4, 0),
            display_name: "Electromagnetism".to_owned(),
            description: "Periodic Yee-lattice Maxwell solver with generic field/particle coupling"
                .to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        vec![
            electric_field_channel_schema(),
            magnetic_field_channel_schema(),
            ChannelSchema {
                id: energy_density_channel_id(),
                display_name: "Electromagnetic energy density".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::ENERGY_DENSITY),
            },
            ChannelSchema {
                id: electric_divergence_channel_id(),
                display_name: "Electric divergence div(E)".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::ELECTRIC_FIELD_DIVERGENCE),
            },
            ChannelSchema {
                id: magnetic_divergence_channel_id(),
                display_name: "div(B) residual".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::MAGNETIC_FIELD_DIVERGENCE),
            },
        ]
    }

    fn component_schemas(&self) -> Vec<ComponentSchema> {
        // Mass is declared here because this system integrates inertia, not
        // because it is electromagnetic. A gravity plugin will declare the same
        // shared schema, and the runtime registers one identical definition
        // once — which is what lets one object carry both without either plugin
        // depending on the other.
        [charge_component_schema(), particle_component_schema()]
            .into_iter()
            .chain(mass_component_schemas())
            .collect()
    }

    fn configuration_schema(&self) -> PluginConfigurationSchema {
        PluginConfigurationSchema {
            properties: vec![
                PropertySchema {
                    id: initial_condition_property_id(),
                    display_name: "Initial condition".to_owned(),
                    kind: PropertyKind::Choice(vec![
                        STATIC_CHARGES_OPTION.to_owned(),
                        PLANE_WAVE_OPTION.to_owned(),
                    ]),
                    required: true,
                    default_value: None,
                    relevant_when: None,
                },
                PropertySchema {
                    id: amplitude_property_id(),
                    display_name: "Initial plane-wave amplitude".to_owned(),
                    kind: PropertyKind::Scalar(Dimension::ELECTRIC_FIELD),
                    required: true,
                    default_value: None,
                    relevant_when: None,
                },
                PropertySchema {
                    id: mode_property_id(),
                    display_name: "Initial plane-wave mode".to_owned(),
                    kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
                    required: true,
                    default_value: None,
                    relevant_when: None,
                },
            ],
        }
    }

    fn default_configuration(&self) -> PropertyBag {
        [
            (
                initial_condition_property_id(),
                PropertyValue::Choice(STATIC_CHARGES_OPTION.to_owned()),
            ),
            (
                amplitude_property_id(),
                PropertyValue::Scalar(
                    Quantity::new(1.0, Dimension::ELECTRIC_FIELD)
                        .expect("static quantity is valid"),
                ),
            ),
            (
                mode_property_id(),
                PropertyValue::Scalar(
                    Quantity::new(1.0, Dimension::DIMENSIONLESS).expect("static quantity is valid"),
                ),
            ),
        ]
        .into_iter()
        .collect()
    }

    fn create_solver(
        &self,
        context: SolverContext<'_>,
    ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
        self.configuration_schema()
            .validate(context.configuration)?;
        validate_domain(context.domain, self.backend.precision())?;

        let amplitude = context
            .configuration
            .scalar(&amplitude_property_id())
            .ok_or_else(|| {
                PluginError::InvalidConfiguration(
                    "plane-wave amplitude must be an electric-field scalar".to_owned(),
                )
            })?;
        let mode_value = context
            .configuration
            .scalar(&mode_property_id())
            .ok_or_else(|| {
                PluginError::InvalidConfiguration(
                    "plane-wave mode must be a dimensionless scalar".to_owned(),
                )
            })?;
        if mode_value < 1.0 || mode_value.fract() != 0.0 || mode_value > u32::MAX as f64 {
            return Err(PluginError::InvalidConfiguration(
                "plane-wave mode must be a positive integer".to_owned(),
            ));
        }

        let initial_condition = match context.configuration.get(&initial_condition_property_id()) {
            Some(PropertyValue::Choice(choice)) if choice == STATIC_CHARGES_OPTION => {
                MaxwellInitialCondition::StaticCharges
            }
            Some(PropertyValue::Choice(choice)) if choice == PLANE_WAVE_OPTION => {
                MaxwellInitialCondition::PrescribedPlaneWave {
                    amplitude,
                    mode: mode_value as u32,
                }
            }
            _ => {
                return Err(PluginError::InvalidConfiguration(
                    "initial condition must be 'Static charges' or 'Prescribed plane wave'"
                        .to_owned(),
                ));
            }
        };
        validate_initial_condition_world(initial_condition, context.world)?;
        let initial_sources = match initial_condition {
            MaxwellInitialCondition::StaticCharges => collect_sources(context.world)?,
            MaxwellInitialCondition::PrescribedPlaneWave { .. } => Vec::new(),
        };
        let particle_coupling = initial_condition == MaxwellInitialCondition::StaticCharges
            && coupling_is_requested(context.world)?;
        let initial_particles = if particle_coupling {
            collect_coupled_particles(*context.domain, context.world)?
        } else {
            Vec::new()
        };
        if particle_coupling {
            validate_coupled_sources(&initial_sources, &initial_particles)?;
        }
        let initial_state = match initial_condition {
            MaxwellInitialCondition::StaticCharges if particle_coupling => {
                periodic_charge_initial_state(*context.domain, &initial_sources)?
            }
            MaxwellInitialCondition::StaticCharges => {
                static_charge_state_from_sources(*context.domain, &initial_sources)
            }
            MaxwellInitialCondition::PrescribedPlaneWave { amplitude, mode } => {
                plane_wave_initial_state(*context.domain, amplitude, mode, context.initial_step)
            }
        };

        self.backend.create_solver(MaxwellSolverSetup {
            domain: *context.domain,
            initial_condition,
            initial_state,
            initial_sources,
            initial_particles,
            particle_coupling,
            world_revision: context.world.revision(),
            initial_step: context.initial_step,
            cancellation: context.cancellation,
        })
    }
}

/// Explicit configuration for the source-free convergence/parity scenario.
pub fn prescribed_plane_wave_configuration(
    amplitude: f64,
    mode: u32,
) -> Result<PropertyBag, PluginError> {
    if mode == 0 {
        return Err(PluginError::InvalidConfiguration(
            "plane-wave mode must be a positive integer".to_owned(),
        ));
    }
    let mut configuration = ElectromagnetismPlugin::new().default_configuration();
    configuration.insert(
        initial_condition_property_id(),
        PropertyValue::Choice(PLANE_WAVE_OPTION.to_owned()),
    );
    configuration.insert(
        amplitude_property_id(),
        PropertyValue::Scalar(
            Quantity::new(amplitude, Dimension::ELECTRIC_FIELD)
                .map_err(|error| PluginError::InvalidConfiguration(error.to_string()))?,
        ),
    );
    configuration.insert(
        mode_property_id(),
        PropertyValue::Scalar(
            Quantity::new(f64::from(mode), Dimension::DIMENSIONLESS)
                .map_err(|error| PluginError::InvalidConfiguration(error.to_string()))?,
        ),
    );
    Ok(configuration)
}

fn validate_domain(domain: &Domain, backend_precision: Precision) -> Result<(), PluginError> {
    if domain.precision() != backend_precision {
        return Err(PluginError::InvalidConfiguration(format!(
            "the Maxwell backend produces {}, but the domain declares {}",
            backend_precision.label(),
            domain.precision().label()
        )));
    }
    let boundaries = domain.boundaries();
    if [boundaries.x, boundaries.y, boundaries.z]
        .into_iter()
        .any(|condition| condition != BoundaryCondition::Periodic)
    {
        return Err(PluginError::InvalidConfiguration(
            "the first Maxwell reference slice supports periodic boundaries only".to_owned(),
        ));
    }
    if domain.resolution().cells().min_element() < 2 {
        return Err(PluginError::InvalidConfiguration(
            "the Yee lattice requires at least two cells on every axis".to_owned(),
        ));
    }
    Ok(())
}

fn collect_sources(world: &WorldSnapshot) -> Result<Vec<ChargeSource>, PluginError> {
    collect_charge_sources(world).map_err(|error| PluginError::UnsupportedWorld(error.to_string()))
}

fn validate_coupled_sources(
    sources: &[ChargeSource],
    particles: &[fieldcad_particles::Particle],
) -> Result<(), PluginError> {
    for source in sources {
        if matches!(
            source.distribution,
            ChargeDistribution::UniformSphere { .. }
        ) {
            return Err(PluginError::UnsupportedWorld(
                "particle-coupled Maxwell currently supports point charge deposition only"
                    .to_owned(),
            ));
        }
        if source.velocity != fieldcad_core::Velocity::default()
            && !particles
                .iter()
                .any(|particle| particle.object == source.object)
        {
            return Err(PluginError::UnsupportedWorld(format!(
                "moving charge {:?} must carry the generic particle component",
                source.object
            )));
        }
    }
    Ok(())
}

/// Courant limit for a three-dimensional rectangular Yee lattice.
pub fn courant_limit(domain: &Domain) -> f64 {
    let spacing = domain.cell_size();
    1.0 / (SPEED_OF_LIGHT
        * (spacing.x.recip().powi(2) + spacing.y.recip().powi(2) + spacing.z.recip().powi(2))
            .sqrt())
}

/// Host-readable Yee storage used to initialize a backend and reconstruct
/// ordinary snapshot samples after GPU readback.
#[derive(Clone, Debug, PartialEq)]
pub struct YeeFieldState {
    pub electric: Vec<DVec3>,
    pub magnetic: Vec<DVec3>,
}

pub fn validate_initial_condition_world(
    initial_condition: MaxwellInitialCondition,
    world: &WorldSnapshot,
) -> Result<(), PluginError> {
    match initial_condition {
        MaxwellInitialCondition::StaticCharges => {
            collect_sources(world)?;
            if coupling_is_requested(world)? {
                return Ok(());
            }
            if let Some((object, _)) =
                world
                    .objects_with(&charge_component_id())
                    .find(|(object, _)| {
                        object.velocity.linear.length_squared() > 0.0
                            || object.velocity.angular.length_squared() > 0.0
                    })
            {
                return Err(PluginError::UnsupportedWorld(format!(
                    "moving charge '{}' must carry a mass component, so that its motion is either integrated from the fields or authored by pinning it",
                    object.name
                )));
            }
            Ok(())
        }
        MaxwellInitialCondition::PrescribedPlaneWave { .. } => {
            if coupling_is_requested(world)? {
                Err(PluginError::UnsupportedWorld(
                    "the prescribed plane-wave validation condition cannot be combined with moving particles"
                        .to_owned(),
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Curl-free periodic discrete gradient of the authored electrostatic
/// potential. This satisfies the Yee curl constraint exactly in `f64`, so a
/// stationary charge produces a stationary Maxwell field rather than launching
/// an unrelated wave. The interior tracks Coulomb's field closely — measured at
/// a 0.3% median over the desktop's default plane — and refines with the grid.
pub fn static_charge_initial_state(
    domain: Domain,
    world: &WorldSnapshot,
) -> Result<YeeFieldState, PluginError> {
    Ok(static_charge_state_from_sources(
        domain,
        &collect_sources(world)?,
    ))
}

/// The same construction against an already-collected source list.
///
/// The outermost lattice layer on each axis differences two opposite faces of
/// the box, because a Coulomb potential is not periodic. Those values are
/// fabricated; [`LatticePeriodicity::SeamOnOuterLayer`] is what stops them being
/// published as measurements.
pub fn static_charge_state_from_sources(domain: Domain, sources: &[ChargeSource]) -> YeeFieldState {
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    // `ChargeDistribution::Point { exclusion_radius: 0.0 }` is a valid, reachable
    // input (`ObjectShape::point(0.0)`), but a zero-radius interior region can
    // never contain a sample, so a lattice node sitting exactly on the charge
    // divides by zero. Floor it at half a cell: below the grid's own resolution,
    // "point" and "half a cell wide" are indistinguishable anyway.
    let minimum_radius = 0.5 * spacing.min_element();
    let mut potential = vec![0.0; domain.resolution().cell_count() as usize];
    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let position = domain.bounds().min()
                    + DVec3::new(
                        f64::from(x) * spacing.x,
                        f64::from(y) * spacing.y,
                        f64::from(z) * spacing.z,
                    );
                potential[linear_index(counts, x, y, z)] =
                    regularized_potential(sources, position, minimum_radius);
            }
        }
    }

    let mut electric = vec![DVec3::ZERO; potential.len()];
    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let index = linear_index(counts, x, y, z);
                let phi = potential[index];
                electric[index] = -DVec3::new(
                    (potential[linear_index(counts, wrap_next(x, counts.x), y, z)] - phi)
                        / spacing.x,
                    (potential[linear_index(counts, x, wrap_next(y, counts.y), z)] - phi)
                        / spacing.y,
                    (potential[linear_index(counts, x, y, wrap_next(z, counts.z))] - phi)
                        / spacing.z,
                );
            }
        }
    }
    let magnetic = vec![DVec3::ZERO; electric.len()];
    YeeFieldState { electric, magnetic }
}

fn regularized_potential(sources: &[ChargeSource], position: DVec3, minimum_radius: f64) -> f64 {
    sources
        .iter()
        .filter(|source| source.charge_coulombs != 0.0)
        .map(|source| {
            let distance_squared = (position - source.position).length_squared();
            let declared_radius = match source.distribution {
                ChargeDistribution::Point { exclusion_radius } => exclusion_radius,
                ChargeDistribution::UniformSphere { radius } => radius,
            };
            // Only the degenerate (zero-radius) case is floored — a legitimately
            // small but positive radius keeps its own exact interior/exterior split.
            let radius = if declared_radius > 0.0 {
                declared_radius
            } else {
                minimum_radius
            };
            if distance_squared < radius * radius {
                COULOMB_CONSTANT * source.charge_coulombs / (2.0 * radius)
                    * (3.0 - distance_squared / (radius * radius))
            } else {
                COULOMB_CONSTANT * source.charge_coulombs / distance_squared.sqrt()
            }
        })
        .sum()
}

pub fn plane_wave_initial_state(
    domain: Domain,
    amplitude: f64,
    mode: u32,
    initial_step: StepContext,
) -> YeeFieldState {
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    let mut electric = vec![DVec3::ZERO; domain.resolution().cell_count() as usize];
    let mut magnetic = vec![DVec3::ZERO; electric.len()];
    let wave_number = f64::from(mode) * std::f64::consts::TAU / domain.bounds().size().x;
    let phase_time = SPEED_OF_LIGHT * wave_number * initial_step.time_seconds;

    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let index = linear_index(counts, x, y, z);
                // Ey is at x_i; Bz is half a cell farther along x. This is
                // the spatial staggering of a +x travelling plane wave.
                let electric_x = domain.bounds().min().x + f64::from(x) * spacing.x;
                let magnetic_x = electric_x + 0.5 * spacing.x;
                electric[index].y = amplitude * (wave_number * electric_x - phase_time).sin();
                magnetic[index].z =
                    amplitude / SPEED_OF_LIGHT * (wave_number * magnetic_x - phase_time).sin();
            }
        }
    }

    YeeFieldState { electric, magnetic }
}

/// The parts of a Maxwell solver that do not depend on where the field is
/// stored.
///
/// The equations belong to the plugin, not to a backend. A CPU and a GPU
/// implementation must agree on the Courant limit, tick sequencing, world
/// validation, when a constrained static state has to be rebuilt, and what the
/// diagnostics report. Both backends delegate here so those cannot drift apart
/// — ADR 0015 requires identical behaviour but previously enforced it only by
/// convention, and each backend had its own copy.
pub struct MaxwellCore {
    domain: Domain,
    initial_condition: MaxwellInitialCondition,
    /// The charge configuration the resident constrained state was built from.
    sources: Vec<ChargeSource>,
    particle_coupling: Option<ParticleCoupling>,
    backend_label: &'static str,
    tick: u64,
    world_revision: WorldRevision,
}

impl MaxwellCore {
    pub fn new(setup: &MaxwellSolverSetup, backend_label: &'static str) -> Self {
        let periodicity = if setup.particle_coupling {
            LatticePeriodicity::Periodic
        } else {
            setup.initial_condition.periodicity()
        };
        let initial_field_energy = yee_conservation(
            setup.domain,
            &setup.initial_state.electric,
            &setup.initial_state.magnetic,
            periodicity,
        )
        .map_or(0.0, |conservation| conservation.energy_joules);
        let particle_coupling = setup.particle_coupling.then(|| {
            ParticleCoupling::new(
                setup.initial_particles.clone(),
                &setup.initial_sources,
                initial_field_energy,
            )
            .expect("solver setup contains validated particles")
        });
        Self {
            domain: setup.domain,
            initial_condition: setup.initial_condition,
            sources: setup.initial_sources.clone(),
            particle_coupling,
            backend_label,
            tick: setup.initial_step.tick,
            world_revision: setup.world_revision,
        }
    }

    pub const fn domain(&self) -> Domain {
        self.domain
    }

    pub const fn periodicity(&self) -> LatticePeriodicity {
        if self.particle_coupling.is_some() {
            LatticePeriodicity::Periodic
        } else {
            self.initial_condition.periodicity()
        }
    }

    pub fn validate_time_step(&self, time_step: TimeStep) -> Result<(), PluginError> {
        let limit = courant_limit(&self.domain);
        if time_step.seconds() > limit {
            return Err(PluginError::InvalidConfiguration(format!(
                "time step {:.6e} s exceeds the Yee Courant limit {:.6e} s",
                time_step.seconds(),
                limit
            )));
        }
        Ok(())
    }

    pub fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        validate_initial_condition_world(self.initial_condition, world)?;
        if coupling_is_requested(world)? {
            let particles = collect_coupled_particles(self.domain, world)?;
            validate_coupled_sources(&collect_sources(world)?, &particles)?;
        }
        Ok(())
    }

    pub fn kinematic_objects(&self) -> &[fieldcad_core::ObjectId] {
        self.particle_coupling
            .as_ref()
            .map_or(&[], ParticleCoupling::kinematic_objects)
    }

    pub const fn has_particle_coupling(&self) -> bool {
        self.particle_coupling.is_some()
    }

    pub fn advance_particles(
        &mut self,
        fields: &YeeFieldState,
        seconds: f64,
    ) -> Result<Option<CoupledAdvance>, PluginError> {
        let Some(coupling) = &mut self.particle_coupling else {
            return Ok(None);
        };
        let advance = coupling.advance(self.domain, fields, seconds)?;
        for source in &mut self.sources {
            if let Some(particle) = coupling
                .particles()
                .iter()
                .find(|particle| particle.object == source.object)
            {
                source.position = particle.position;
                source.velocity = fieldcad_core::Velocity::new(particle.velocity, DVec3::ZERO)
                    .map_err(|error| PluginError::Solver(error.to_string()))?;
            }
        }
        Ok(Some(advance))
    }

    /// The constrained state a world edit requires, or `None` when nothing the
    /// solver depends on changed.
    ///
    /// The runtime calls `on_world_changed` for every accepted commit, so this
    /// runs on probe and slice-plane drags too. Those cannot alter a charge
    /// configuration, and rebuilding the whole grid for them costs a full
    /// `cells × sources` pass plus, on GPU, a complete grid upload.
    pub fn constrained_state_for(
        &mut self,
        world: &WorldSnapshot,
    ) -> Result<Option<YeeFieldState>, PluginError> {
        self.world_revision = world.revision();
        if self.initial_condition != MaxwellInitialCondition::StaticCharges {
            return Ok(None);
        }
        let sources = collect_sources(world)?;
        let coupling_requested = coupling_is_requested(world)?;
        if coupling_requested {
            let particles = collect_coupled_particles(self.domain, world)?;
            validate_coupled_sources(&sources, &particles)?;
            let unchanged = sources == self.sources
                && self
                    .particle_coupling
                    .as_ref()
                    .is_some_and(|coupling| coupling.particles() == particles);
            if unchanged {
                return Ok(None);
            }
            let state = periodic_charge_initial_state(self.domain, &sources)?;
            let field_energy = yee_conservation(
                self.domain,
                &state.electric,
                &state.magnetic,
                LatticePeriodicity::Periodic,
            )?
            .energy_joules;
            if let Some(coupling) = &mut self.particle_coupling {
                coupling.adopt_intervention(particles, &sources);
                coupling.reset_energy_reference(field_energy);
            } else {
                self.particle_coupling =
                    Some(ParticleCoupling::new(particles, &sources, field_energy)?);
            }
            self.sources = sources;
            return Ok(Some(state));
        }

        let coupling_was_active = self.particle_coupling.take().is_some();
        if !coupling_was_active && sources == self.sources {
            return Ok(None);
        }
        let state = static_charge_state_from_sources(self.domain, &sources);
        self.sources = sources;
        Ok(Some(state))
    }

    /// Validate and adopt one tick. Backends call this before advancing state.
    pub fn accept_tick(&mut self, context: StepContext) -> Result<(), PluginError> {
        self.validate_time_step(context.time_step)?;
        if context.tick != self.tick + 1 {
            return Err(PluginError::Solver(format!(
                "expected Maxwell tick {}, received {}",
                self.tick + 1,
                context.tick
            )));
        }
        self.tick = context.tick;
        Ok(())
    }

    pub fn diagnostics(&self, conservation: MaxwellConservation) -> Vec<SolverDiagnostic> {
        let seam_note = if self.periodicity() == LatticePeriodicity::SeamOnOuterLayer {
            "; outer lattice layer undefined across the periodic seam"
        } else {
            ""
        };
        let mut diagnostics = vec![
            SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "yee-courant-limit".to_owned(),
                message: format!(
                    "{} {} Yee lattice; {}; periodic boundaries; Courant dt <= {:.6e} s{seam_note}",
                    self.backend_label,
                    self.domain.precision().label(),
                    self.initial_condition.label(),
                    courant_limit(&self.domain)
                ),
            },
            SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "maxwell-conservation".to_owned(),
                message: format!(
                    "energy {:.6e} J; max |div E| {:.6e}{}; max |div B| {:.6e}; world revision {}",
                    conservation.energy_joules,
                    conservation.max_divergence_e,
                    if self.initial_condition == MaxwellInitialCondition::StaticCharges {
                        " (includes charge source)"
                    } else {
                        ""
                    },
                    conservation.max_divergence_b,
                    self.world_revision
                ),
            },
        ];
        if let Some(coupling) = &self.particle_coupling {
            diagnostics.push(SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "particle-coupling-conservation".to_owned(),
                message: coupling.diagnostic_summary(conservation.energy_joules),
            });
        }
        diagnostics
    }
}

struct MaxwellSolver {
    core: MaxwellCore,
    counts: UVec3,
    spacing: DVec3,
    electric: Vec<DVec3>,
    magnetic: Vec<DVec3>,
}

impl MaxwellSolver {
    fn new(setup: MaxwellSolverSetup) -> Self {
        let core = MaxwellCore::new(&setup, "CPU");
        Self {
            counts: setup.domain.resolution().cells(),
            spacing: setup.domain.cell_size(),
            electric: setup.initial_state.electric,
            magnetic: setup.initial_state.magnetic,
            core,
        }
    }

    fn advance_magnetic(&mut self, seconds: f64) {
        for z in 0..self.counts.z {
            for y in 0..self.counts.y {
                for x in 0..self.counts.x {
                    let index = linear_index(self.counts, x, y, z);
                    let curl = self.curl_e_forward(x, y, z);
                    self.magnetic[index] -= seconds * curl;
                }
            }
        }
    }

    fn advance_electric(&mut self, seconds: f64, current_density: Option<&[DVec3]>) {
        let scale = SPEED_OF_LIGHT * SPEED_OF_LIGHT * seconds;
        for z in 0..self.counts.z {
            for y in 0..self.counts.y {
                for x in 0..self.counts.x {
                    let index = linear_index(self.counts, x, y, z);
                    let curl = self.curl_b_backward(x, y, z);
                    self.electric[index] += scale * curl;
                    if let Some(current_density) = current_density {
                        self.electric[index] -=
                            seconds / VACUUM_PERMITTIVITY * current_density[index];
                    }
                }
            }
        }
    }

    fn curl_e_forward(&self, x: u32, y: u32, z: u32) -> DVec3 {
        let here = self.electric_at(x, y, z);
        let x_next = self.electric_at(wrap_next(x, self.counts.x), y, z);
        let y_next = self.electric_at(x, wrap_next(y, self.counts.y), z);
        let z_next = self.electric_at(x, y, wrap_next(z, self.counts.z));
        DVec3::new(
            (y_next.z - here.z) / self.spacing.y - (z_next.y - here.y) / self.spacing.z,
            (z_next.x - here.x) / self.spacing.z - (x_next.z - here.z) / self.spacing.x,
            (x_next.y - here.y) / self.spacing.x - (y_next.x - here.x) / self.spacing.y,
        )
    }

    fn curl_b_backward(&self, x: u32, y: u32, z: u32) -> DVec3 {
        let here = self.magnetic_at(x, y, z);
        let x_previous = self.magnetic_at(wrap_previous(x, self.counts.x), y, z);
        let y_previous = self.magnetic_at(x, wrap_previous(y, self.counts.y), z);
        let z_previous = self.magnetic_at(x, y, wrap_previous(z, self.counts.z));
        DVec3::new(
            (here.z - y_previous.z) / self.spacing.y - (here.y - z_previous.y) / self.spacing.z,
            (here.x - z_previous.x) / self.spacing.z - (here.z - x_previous.z) / self.spacing.x,
            (here.y - x_previous.y) / self.spacing.x - (here.x - y_previous.x) / self.spacing.y,
        )
    }

    fn electric_at(&self, x: u32, y: u32, z: u32) -> DVec3 {
        self.electric[linear_index(self.counts, x, y, z)]
    }

    fn magnetic_at(&self, x: u32, y: u32, z: u32) -> DVec3 {
        self.magnetic[linear_index(self.counts, x, y, z)]
    }
}

struct YeeFieldView<'a> {
    domain: Domain,
    counts: UVec3,
    spacing: DVec3,
    electric: &'a [DVec3],
    magnetic: &'a [DVec3],
    periodicity: LatticePeriodicity,
    centred_electric: Vec<DVec3>,
    centred_magnetic: Vec<DVec3>,
}

impl<'a> YeeFieldView<'a> {
    fn new(
        domain: Domain,
        electric: &'a [DVec3],
        magnetic: &'a [DVec3],
        periodicity: LatticePeriodicity,
    ) -> Result<Self, PluginError> {
        let expected = domain.resolution().cell_count() as usize;
        if electric.len() != expected || magnetic.len() != expected {
            return Err(PluginError::Solver(format!(
                "Yee storage has E={} and B={} cells, expected {expected}",
                electric.len(),
                magnetic.len()
            )));
        }
        let counts = domain.resolution().cells();
        let mut centred_electric = Vec::with_capacity(expected);
        let mut centred_magnetic = Vec::with_capacity(expected);
        for z in 0..counts.z {
            for y in 0..counts.y {
                for x in 0..counts.x {
                    let (e, m) = centred_fields(counts, electric, magnetic, x, y, z);
                    centred_electric.push(e);
                    centred_magnetic.push(m);
                }
            }
        }
        Ok(Self {
            domain,
            counts,
            spacing: domain.cell_size(),
            electric,
            magnetic,
            periodicity,
            centred_electric,
            centred_magnetic,
        })
    }

    /// Which cell indices on one axis hold a value this channel cannot trust.
    ///
    /// `E` is built as a forward difference, so its seam is the last index.
    /// `div E` is a backward difference of `E`, so it reads the seam from the
    /// last index and, through the wrap, from index zero as well. Energy
    /// density is quadratic in `E` and inherits `E`'s seam. `B` is zero for a
    /// constrained static state and periodic otherwise, so it has none.
    fn seam_indices(&self, channel: ChannelHandle, count: u32) -> SeamIndices {
        if self.periodicity == LatticePeriodicity::Periodic {
            return SeamIndices::None;
        }
        match channel {
            ELECTRIC_FIELD_HANDLE | ENERGY_DENSITY_HANDLE => SeamIndices::Last(count - 1),
            ELECTRIC_DIVERGENCE_HANDLE => SeamIndices::FirstAndLast(count - 1),
            _ => SeamIndices::None,
        }
    }

    fn cell_is_on_seam(&self, channel: ChannelHandle, cell: UVec3) -> bool {
        let counts = self.counts.to_array();
        let cell = cell.to_array();
        (0..3).any(|axis| {
            self.seam_indices(channel, counts[axis])
                .contains(cell[axis])
        })
    }

    /// Whether the trilinear stencil around `position` reads any seam value.
    fn stencil_crosses_seam(&self, channel: ChannelHandle, position: DVec3) -> bool {
        if self.periodicity == LatticePeriodicity::Periodic {
            return false;
        }
        let (base, _) = self.interpolation_cell(position);
        let counts = self.counts.to_array();
        let base = base.to_array();
        (0..3).any(|axis| {
            let seam = self.seam_indices(channel, counts[axis]);
            let wrap = |index: i32| index.rem_euclid(counts[axis] as i32) as u32;
            seam.contains(wrap(base[axis])) || seam.contains(wrap(base[axis] + 1))
        })
    }

    fn electric_at(&self, x: u32, y: u32, z: u32) -> DVec3 {
        self.electric[linear_index(self.counts, x, y, z)]
    }

    fn magnetic_at(&self, x: u32, y: u32, z: u32) -> DVec3 {
        self.magnetic[linear_index(self.counts, x, y, z)]
    }

    fn electric_divergence(&self, x: u32, y: u32, z: u32) -> f64 {
        let here = self.electric_at(x, y, z);
        let xp = self.electric_at(wrap_previous(x, self.counts.x), y, z);
        let yp = self.electric_at(x, wrap_previous(y, self.counts.y), z);
        let zp = self.electric_at(x, y, wrap_previous(z, self.counts.z));
        (here.x - xp.x) / self.spacing.x
            + (here.y - yp.y) / self.spacing.y
            + (here.z - zp.z) / self.spacing.z
    }

    fn magnetic_divergence(&self, x: u32, y: u32, z: u32) -> f64 {
        let here = self.magnetic_at(x, y, z);
        let xn = self.magnetic_at(wrap_next(x, self.counts.x), y, z);
        let yn = self.magnetic_at(x, wrap_next(y, self.counts.y), z);
        let zn = self.magnetic_at(x, y, wrap_next(z, self.counts.z));
        (xn.x - here.x) / self.spacing.x
            + (yn.y - here.y) / self.spacing.y
            + (zn.z - here.z) / self.spacing.z
    }

    fn interpolate_vector(
        &self,
        position: DVec3,
        select: impl Fn((DVec3, DVec3)) -> DVec3,
    ) -> DVec3 {
        let (base, fraction) = self.interpolation_cell(position);
        let mut result = DVec3::ZERO;
        for dz in 0..=1 {
            for dy in 0..=1 {
                for dx in 0..=1 {
                    let weight = axis_weight(fraction.x, dx)
                        * axis_weight(fraction.y, dy)
                        * axis_weight(fraction.z, dz);
                    let cell = self.wrapped_cell(base.x + dx, base.y + dy, base.z + dz);
                    let index = linear_index(self.counts, cell.x, cell.y, cell.z);
                    result += weight * select((self.centred_electric[index], self.centred_magnetic[index]));
                }
            }
        }
        result
    }

    fn interpolate_scalar(
        &self,
        position: DVec3,
        select: impl Fn(&Self, u32, u32, u32) -> f64,
    ) -> f64 {
        let (base, fraction) = self.interpolation_cell(position);
        let mut result = 0.0;
        for dz in 0..=1 {
            for dy in 0..=1 {
                for dx in 0..=1 {
                    let weight = axis_weight(fraction.x, dx)
                        * axis_weight(fraction.y, dy)
                        * axis_weight(fraction.z, dz);
                    let cell = self.wrapped_cell(base.x + dx, base.y + dy, base.z + dz);
                    result += weight * select(self, cell.x, cell.y, cell.z);
                }
            }
        }
        result
    }

    fn interpolation_cell(&self, position: DVec3) -> (IVec3, DVec3) {
        interpolation_cell(self.domain, position)
    }

    fn wrapped_cell(&self, x: i32, y: i32, z: i32) -> UVec3 {
        wrapped_cell(self.counts, x, y, z)
    }

    fn energy_at_cell(&self, x: u32, y: u32, z: u32) -> f64 {
        let index = linear_index(self.counts, x, y, z);
        0.5 * (VACUUM_PERMITTIVITY * self.centred_electric[index].length_squared()
            + self.centred_magnetic[index].length_squared() / VACUUM_PERMEABILITY)
    }
}

/// Sample a Yee state through the same reconstruction used by the CPU oracle.
/// GPU backends call this after readback so all consumers still receive ordinary
/// typed snapshot columns.
pub fn sample_yee_fields(
    domain: Domain,
    electric: &[DVec3],
    magnetic: &[DVec3],
    periodicity: LatticePeriodicity,
    channel: ChannelHandle,
    geometry: &SampleGeometry,
) -> Result<SampledColumn, PluginError> {
    let field = YeeFieldView::new(domain, electric, magnetic, periodicity)?;
    let mut validity = Vec::with_capacity(geometry.len());
    let mark = |position: DVec3, validity: &mut Vec<SampleValidity>| {
        if !domain.bounds().contains(position) {
            validity.push(SampleValidity::Undefined(UndefinedReason::OutsideDomain));
            false
        } else if field.stencil_crosses_seam(channel, position) {
            validity.push(SampleValidity::Undefined(
                UndefinedReason::AcrossPeriodicSeam,
            ));
            false
        } else {
            validity.push(SampleValidity::Interpolated(InterpolationMethod::Trilinear));
            true
        }
    };

    match channel {
        ELECTRIC_FIELD_HANDLE | MAGNETIC_FIELD_HANDLE => {
            let mut values = Vec::with_capacity(geometry.len());
            for position in geometry.positions() {
                let inside = mark(position, &mut validity);
                let value = if !inside {
                    DVec3::ZERO
                } else if channel == ELECTRIC_FIELD_HANDLE {
                    field.interpolate_vector(position, |(electric, _)| electric)
                } else {
                    field.interpolate_vector(position, |(_, magnetic)| magnetic)
                };
                values.push(value);
            }
            Ok(SampledColumn::new(FieldColumn::vectors(values), validity))
        }
        ENERGY_DENSITY_HANDLE | ELECTRIC_DIVERGENCE_HANDLE | MAGNETIC_DIVERGENCE_HANDLE => {
            let mut values = Vec::with_capacity(geometry.len());
            for position in geometry.positions() {
                let inside = mark(position, &mut validity);
                let value = if !inside {
                    0.0
                } else if channel == ENERGY_DENSITY_HANDLE {
                    field.interpolate_scalar(position, YeeFieldView::energy_at_cell)
                } else if channel == ELECTRIC_DIVERGENCE_HANDLE {
                    field.interpolate_scalar(position, YeeFieldView::electric_divergence)
                } else {
                    field.interpolate_scalar(position, YeeFieldView::magnetic_divergence)
                };
                values.push(value);
            }
            Ok(SampledColumn::new(FieldColumn::scalars(values), validity))
        }
        other => Err(PluginError::UnknownChannel(other.index())),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaxwellConservation {
    pub energy_joules: f64,
    pub max_divergence_e: f64,
    pub max_divergence_b: f64,
}

/// Integrated energy and peak divergence residuals over the cells whose values
/// the lattice can actually defend.
///
/// Seam cells are excluded per channel rather than integrated. A fabricated
/// outer layer would otherwise dominate the reported energy — measured at up to
/// 30% of the total once an authored charge is dragged toward a boundary — and
/// present it as a conservation result.
pub fn yee_conservation(
    domain: Domain,
    electric: &[DVec3],
    magnetic: &[DVec3],
    periodicity: LatticePeriodicity,
) -> Result<MaxwellConservation, PluginError> {
    let field = YeeFieldView::new(domain, electric, magnetic, periodicity)?;
    let cell_volume = field.spacing.x * field.spacing.y * field.spacing.z;
    let mut result = MaxwellConservation::default();
    for z in 0..field.counts.z {
        for y in 0..field.counts.y {
            for x in 0..field.counts.x {
                let cell = UVec3::new(x, y, z);
                if !field.cell_is_on_seam(ENERGY_DENSITY_HANDLE, cell) {
                    result.energy_joules += field.energy_at_cell(x, y, z) * cell_volume;
                }
                if !field.cell_is_on_seam(ELECTRIC_DIVERGENCE_HANDLE, cell) {
                    result.max_divergence_e = result
                        .max_divergence_e
                        .max(field.electric_divergence(x, y, z).abs());
                }
                result.max_divergence_b = result
                    .max_divergence_b
                    .max(field.magnetic_divergence(x, y, z).abs());
            }
        }
    }
    Ok(result)
}

impl EquationSystemSolver for MaxwellSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::TimeStepped
    }

    fn validate_time_step(&self, time_step: TimeStep) -> Result<(), PluginError> {
        self.core.validate_time_step(time_step)
    }

    fn time_step_limit(&self) -> Option<TimeStep> {
        TimeStep::from_seconds(courant_limit(&self.core.domain())).ok()
    }

    fn validate_world(&self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.core.validate_world(world)
    }

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        if let Some(state) = self.core.constrained_state_for(world)? {
            self.electric = state.electric;
            self.magnetic = state.magnetic;
        }
        Ok(())
    }

    fn kinematic_objects(&self) -> &[fieldcad_core::ObjectId] {
        self.core.kinematic_objects()
    }

    fn mutable_vector_channels(&self) -> &[ChannelHandle] {
        &[ELECTRIC_FIELD_HANDLE, MAGNETIC_FIELD_HANDLE]
    }

    fn apply_field_brush_stroke(
        &mut self,
        stroke: &ResolvedFieldBrushStroke,
    ) -> Result<(), PluginError> {
        let target = match stroke.stroke.channel {
            ref channel if channel == &electric_field_channel_id() => &mut self.electric,
            ref channel if channel == &magnetic_field_channel_id() => &mut self.magnetic,
            _ => {
                return Err(PluginError::Solver(
                    "Maxwell cannot paint this field channel".to_owned(),
                ));
            }
        };
        let radius_squared = stroke.stroke.radius_metres * stroke.stroke.radius_metres;
        let amount = stroke.direction * stroke.stroke.strength.si_value();
        let bounds = self.core.domain().bounds();
        for z in 0..self.counts.z {
            for y in 0..self.counts.y {
                for x in 0..self.counts.x {
                    let position = bounds.min()
                        + DVec3::new(
                            f64::from(x) * self.spacing.x,
                            f64::from(y) * self.spacing.y,
                            f64::from(z) * self.spacing.z,
                        );
                    let weight = stroke.centres.iter().fold(0.0_f64, |weight, centre| {
                        let normalized = position.distance_squared(*centre) / radius_squared;
                        if normalized < 1.0 {
                            weight.max((1.0 - normalized).powi(2))
                        } else {
                            weight
                        }
                    });
                    target[linear_index(self.counts, x, y, z)] += amount * weight;
                }
            }
        }
        Ok(())
    }

    fn step(&mut self, context: StepContext) -> Result<SolverStepOutcome, PluginError> {
        self.core.accept_tick(context)?;
        let coupled = if self.core.has_particle_coupling() {
            self.core.advance_particles(
                &YeeFieldState {
                    electric: self.electric.clone(),
                    magnetic: self.magnetic.clone(),
                },
                context.time_step.seconds(),
            )?
        } else {
            None
        };
        let half_step = 0.5 * context.time_step.seconds();
        self.advance_magnetic(half_step);
        self.advance_electric(
            context.time_step.seconds(),
            coupled
                .as_ref()
                .map(|advance| advance.current_density.as_slice()),
        );
        self.advance_magnetic(half_step);
        Ok(coupled.map_or_else(SolverStepOutcome::default, |advance| advance.outcome))
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        sample_yee_fields(
            self.core.domain(),
            &self.electric,
            &self.magnetic,
            self.core.periodicity(),
            channel,
            geometry,
        )
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        let conservation = yee_conservation(
            self.core.domain(),
            &self.electric,
            &self.magnetic,
            self.core.periodicity(),
        )
        .expect("the CPU solver owns complete Yee storage");
        self.core.diagnostics(conservation)
    }
}

fn linear_index(counts: UVec3, x: u32, y: u32, z: u32) -> usize {
    (x + counts.x * (y + counts.y * z)) as usize
}

fn wrap_next(value: u32, count: u32) -> u32 {
    if value + 1 == count { 0 } else { value + 1 }
}

fn wrap_previous(value: u32, count: u32) -> u32 {
    if value == 0 { count - 1 } else { value - 1 }
}

fn axis_weight(fraction: f64, corner: i32) -> f64 {
    if corner == 0 {
        1.0 - fraction
    } else {
        fraction
    }
}

/// De-staggers the Yee lattice's `E`/`B` storage onto one shared cell-centred
/// point. The single implementation both `YeeFieldView::centred_fields`
/// (sampling for display) and `coupling::interpolate_particle_fields`
/// (the particle pusher) build their trilinear reconstruction from — this
/// used to be a character-for-character duplicate of itself across the two
/// modules (PH-17), with no mechanism to keep them agreeing after a change
/// to one.
fn centred_fields(
    counts: UVec3,
    electric: &[DVec3],
    magnetic: &[DVec3],
    x: u32,
    y: u32,
    z: u32,
) -> (DVec3, DVec3) {
    let xn = wrap_next(x, counts.x);
    let yn = wrap_next(y, counts.y);
    let zn = wrap_next(z, counts.z);
    let at = |values: &[DVec3], x, y, z| values[linear_index(counts, x, y, z)];
    let e000 = at(electric, x, y, z);
    let electric = DVec3::new(
        0.25 * (e000.x
            + at(electric, x, yn, z).x
            + at(electric, x, y, zn).x
            + at(electric, x, yn, zn).x),
        0.25 * (e000.y
            + at(electric, xn, y, z).y
            + at(electric, x, y, zn).y
            + at(electric, xn, y, zn).y),
        0.25 * (e000.z
            + at(electric, xn, y, z).z
            + at(electric, x, yn, z).z
            + at(electric, xn, yn, z).z),
    );
    let b000 = at(magnetic, x, y, z);
    let magnetic = DVec3::new(
        0.5 * (b000.x + at(magnetic, xn, y, z).x),
        0.5 * (b000.y + at(magnetic, x, yn, z).y),
        0.5 * (b000.z + at(magnetic, x, y, zn).z),
    );
    (electric, magnetic)
}

/// A trilinear stencil corner's raw offset, wrapped to a valid lattice index.
fn wrapped_cell(counts: UVec3, x: i32, y: i32, z: i32) -> UVec3 {
    UVec3::new(
        x.rem_euclid(counts.x as i32) as u32,
        y.rem_euclid(counts.y as i32) as u32,
        z.rem_euclid(counts.z as i32) as u32,
    )
}

/// `position` folded into `[domain.bounds().min(), domain.bounds().min() +
/// domain.bounds().size())` along every axis. A particle's own tracked
/// position is never itself reset at a periodic crossing — only read
/// through this wrap — so callers that source positions from moving bodies
/// (unlike a fixed sample geometry, which is always authored inside the
/// domain) need it before turning a position into lattice coordinates.
fn wrap_position(domain: Domain, position: DVec3) -> DVec3 {
    let min = domain.bounds().min();
    let size = domain.bounds().size();
    min + (position - min).rem_euclid(size)
}

/// The stencil base cell and fractional offset for trilinear interpolation
/// at `position` on the dual (cell-centred) Yee lattice, the single
/// implementation shared by the display sampling path and the particle
/// pusher (PH-17). Always wraps `position` into the domain first: a sample
/// geometry is already authored inside the domain, so this is a no-op
/// there, but a particle that has drifted past a periodic boundary without
/// its own position being reset is not, and would otherwise resolve to the
/// wrong stencil (or, before this was unified, silently *did* — the two
/// call sites disagreed on exactly this).
fn interpolation_cell(domain: Domain, position: DVec3) -> (IVec3, DVec3) {
    let spacing = domain.cell_size();
    let grid =
        (wrap_position(domain, position) - domain.bounds().min()) / spacing - DVec3::splat(0.5);
    let floor = grid.floor();
    (floor.as_ivec3(), grid - floor)
}

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        BoundaryConditions, DomainBounds, FieldColumn, ProbeId, Resolution, World,
    };

    use super::*;

    struct DeclaredF32Backend;

    impl MaxwellSolverBackend for DeclaredF32Backend {
        fn precision(&self) -> Precision {
            Precision::F32
        }

        fn create_solver(
            &self,
            setup: MaxwellSolverSetup,
        ) -> Result<Box<dyn EquationSystemSolver>, PluginError> {
            Ok(Box::new(MaxwellSolver::new(setup)))
        }
    }

    fn periodic_domain(x_cells: u32) -> Domain {
        Domain::new(
            DomainBounds::new(DVec3::ZERO, DVec3::ONE).unwrap(),
            Resolution::new(x_cells, 2, 2).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        )
    }

    fn solver(domain: &Domain) -> Box<dyn EquationSystemSolver> {
        let plugin = ElectromagnetismPlugin::new();
        let world = World::new();
        let configuration = prescribed_plane_wave_configuration(1.0, 1).unwrap();
        plugin
            .create_solver(SolverContext {
                configuration: &configuration,
                domain,
                world: &world.snapshot(),
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(1.0e-12).unwrap(),
                },
                cancellation: SolverCancellation::default(),
            })
            .unwrap()
    }

    fn points(positions: Vec<DVec3>) -> SampleGeometry {
        let ids = (0..positions.len() as u64).map(ProbeId::new).collect();
        SampleGeometry::probes(ids, positions).unwrap()
    }

    /// The desktop's default Maxwell domain: ±5 m, 32³ periodic cells.
    fn desktop_domain() -> Domain {
        Domain::new(
            DomainBounds::centred_cube(5.0).unwrap(),
            Resolution::uniform(32).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        )
    }

    /// A world holding one 1 nC point charge, and the object ID so a test can
    /// move it.
    fn charged_world(position: DVec3) -> (fieldcad_core::World, fieldcad_core::ObjectId) {
        use fieldcad_core::{ObjectShape, ObjectSpec, Transform, WorldCommand};
        use fieldcad_electromagnetic_sources::{
            charge_component_id, charge_component_schema, charge_properties,
        };

        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("static charge")
                        .with_transform(Transform::at(position).unwrap())
                        .with_shape(ObjectShape::point(0.15).unwrap())
                        .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
                ),
            ])
            .unwrap();
        let charge = report.created_objects[0];
        (world, charge)
    }

    fn static_charge_solver(
        domain: &Domain,
        world: &WorldSnapshot,
    ) -> Box<dyn EquationSystemSolver> {
        let plugin = ElectromagnetismPlugin::new();
        plugin
            .create_solver(SolverContext {
                configuration: &plugin.default_configuration(),
                domain,
                world,
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: TimeStep::from_seconds(courant_limit(domain) * 0.8).unwrap(),
                },
                cancellation: SolverCancellation::default(),
            })
            .unwrap()
    }

    fn vectors(column: SampledColumn) -> Vec<DVec3> {
        let FieldColumn::Vector(values) = column.values else {
            panic!("electric field must be a vector column");
        };
        values.to_vec()
    }

    #[test]
    fn plugin_declares_coupled_fields_and_residuals() {
        let channels = ElectromagnetismPlugin::new().channels();

        assert_eq!(
            channels[ELECTRIC_FIELD_HANDLE.index()].id,
            electric_field_channel_id()
        );
        assert_eq!(
            channels[MAGNETIC_FIELD_HANDLE.index()].id,
            magnetic_field_channel_id()
        );
        assert_eq!(
            channels[ENERGY_DENSITY_HANDLE.index()].dimension(),
            Dimension::ENERGY_DENSITY
        );
        assert_eq!(
            channels[MAGNETIC_DIVERGENCE_HANDLE.index()].dimension(),
            Dimension::MAGNETIC_FIELD_DIVERGENCE
        );
    }

    #[test]
    fn injected_backends_do_not_change_the_plugin_contract() {
        let reference = ElectromagnetismPlugin::new();
        let accelerated = ElectromagnetismPlugin::with_backend(Arc::new(DeclaredF32Backend));

        assert_eq!(reference.metadata(), accelerated.metadata());
        assert_eq!(reference.channels(), accelerated.channels());
        assert_eq!(
            reference.configuration_schema(),
            accelerated.configuration_schema()
        );
        assert_eq!(
            reference.default_configuration(),
            accelerated.default_configuration()
        );
    }

    #[test]
    fn reference_solver_requires_its_documented_representation() {
        let plugin = ElectromagnetismPlugin::new();
        let world = World::new();
        let open = Domain::centred_cube(1.0, 8).unwrap();

        let result = plugin.create_solver(SolverContext {
            configuration: &plugin.default_configuration(),
            domain: &open,
            world: &world.snapshot(),
            initial_step: StepContext {
                tick: 0,
                time_seconds: 0.0,
                time_step: TimeStep::from_seconds(1.0e-12).unwrap(),
            },
            cancellation: SolverCancellation::default(),
        });

        assert!(matches!(result, Err(PluginError::InvalidConfiguration(_))));
    }

    #[test]
    fn courant_limit_is_enforced_before_a_tick_is_adopted() {
        let domain = periodic_domain(16);
        let solver = solver(&domain);
        let limit = courant_limit(&domain);

        assert!(
            solver
                .validate_time_step(TimeStep::from_seconds(limit).unwrap())
                .is_ok()
        );
        assert!(matches!(
            solver.validate_time_step(TimeStep::from_seconds(limit * 1.001).unwrap()),
            Err(PluginError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn staggered_fields_are_interpolated_and_outside_samples_are_explicit() {
        let domain = periodic_domain(16);
        let solver = solver(&domain);
        let geometry = points(vec![DVec3::splat(0.5), DVec3::new(2.0, 0.5, 0.5)]);

        let electric = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();

        assert!(matches!(electric.values, FieldColumn::Vector(_)));
        assert_eq!(
            electric.validity[0],
            SampleValidity::Interpolated(InterpolationMethod::Trilinear)
        );
        assert_eq!(
            electric.validity[1],
            SampleValidity::Undefined(UndefinedReason::OutsideDomain)
        );
    }

    #[test]
    fn default_static_charge_field_matches_electrostatics_and_stays_stationary() {
        use fieldcad_core::{ObjectShape, ObjectSpec, Transform, WorldCommand};
        use fieldcad_electromagnetic_sources::{
            charge_component_id, charge_component_schema, charge_properties,
        };
        use fieldcad_electrostatics::{collect_sources, evaluate_sources};

        let domain = Domain::new(
            DomainBounds::centred_cube(5.0).unwrap(),
            Resolution::uniform(32).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        );
        let mut world = World::new();
        let report = world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("static charge")
                        .with_transform(Transform::at(DVec3::ZERO).unwrap())
                        .with_shape(ObjectShape::point(0.15).unwrap())
                        .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
                ),
            ])
            .unwrap();
        let charge = report.created_objects[0];
        let snapshot = world.snapshot();
        let plugin = ElectromagnetismPlugin::new();
        let step = TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap();
        let mut solver = plugin
            .create_solver(SolverContext {
                configuration: &plugin.default_configuration(),
                domain: &domain,
                world: &snapshot,
                initial_step: StepContext {
                    tick: 0,
                    time_seconds: 0.0,
                    time_step: step,
                },
                cancellation: SolverCancellation::default(),
            })
            .unwrap();
        let position = DVec3::new(1.0, 0.0, 0.0);
        let geometry = points(vec![position]);
        let expected =
            evaluate_sources(&collect_sources(&snapshot).unwrap(), position).electric_field;
        let initial = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();
        let FieldColumn::Vector(initial) = initial.values else {
            panic!("electric field must be a vector column");
        };
        let initial = initial[0];

        assert!(
            initial.normalize().dot(expected.normalize()) > 0.995,
            "Maxwell E {initial:?} points away from electrostatic E {expected:?}"
        );
        assert!(
            (initial.length() - expected.length()).abs() / expected.length() < 0.2,
            "Maxwell |E|={} differs from electrostatic |E|={}",
            initial.length(),
            expected.length()
        );

        for tick in 1..=8 {
            solver
                .step(StepContext {
                    tick,
                    time_seconds: tick as f64 * step.seconds(),
                    time_step: step,
                })
                .unwrap();
        }
        let evolved = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();
        let FieldColumn::Vector(evolved) = evolved.values else {
            panic!("electric field must be a vector column");
        };
        assert!((evolved[0] - initial).length() / initial.length() < 1.0e-9);

        world
            .commit([WorldCommand::SetTransform {
                object: charge,
                transform: Transform::at(DVec3::new(2.0, 0.0, 0.0)).unwrap(),
            }])
            .unwrap();
        let moved = world.snapshot();
        solver.validate_world(&moved).unwrap();
        solver.on_world_changed(&moved).unwrap();
        let rebuilt = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();
        let FieldColumn::Vector(rebuilt) = rebuilt.values else {
            panic!("electric field must be a vector column");
        };
        assert!(rebuilt[0].x < 0.0, "edited charge must rebuild Maxwell E");
    }

    /// The seam is the defect a centred charge cannot expose.
    ///
    /// `static_charge_state_from_sources` differences a Coulomb potential, which
    /// is not periodic, so the outermost lattice layer differences two opposite
    /// faces of the box. For a charge at the origin the sampled potential is
    /// symmetric across that seam and the fabricated value is accidentally
    /// correct — which is why the original static-charge test, which placed its
    /// charge at the origin, could not see this. Off centre the same layer was
    /// measured at 281% error in the desktop's shipped default scene and 493%
    /// with the charge at x = 2.
    #[test]
    fn an_off_centre_static_charge_reports_its_periodic_seam_as_undefined() {
        let domain = desktop_domain();
        // The position the desktop actually ships.
        let (world, _) = charged_world(DVec3::new(0.0, 0.0, 0.6));
        let solver = static_charge_solver(&domain, &world.snapshot());

        // Inside the domain but within the seam layer's interpolation stencil.
        let seam = points(vec![
            DVec3::new(4.7, 0.0, 0.0),
            DVec3::new(0.0, 4.9, 0.0),
            DVec3::new(0.0, 0.0, -4.95),
        ]);
        let column = solver.sample(ELECTRIC_FIELD_HANDLE, &seam).unwrap();

        for (index, validity) in column.validity.iter().enumerate() {
            assert_eq!(
                *validity,
                SampleValidity::Undefined(UndefinedReason::AcrossPeriodicSeam),
                "seam sample {index} was published as a measurement"
            );
        }
    }

    #[test]
    fn a_prescribed_wave_is_genuinely_periodic_and_has_no_seam() {
        // The wave state is periodic by construction, so the same outer layer is
        // a real value. Marking it undefined would discard good data.
        let domain = periodic_domain(16);
        let solver = solver(&domain);
        let dx = domain.cell_size().x;
        let geometry = points(vec![
            DVec3::new(1.0 - 0.5 * dx, 0.5, 0.5),
            DVec3::new(1.0 - 0.01, 0.5, 0.5),
        ]);

        let column = solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap();

        for validity in &column.validity {
            assert_eq!(
                *validity,
                SampleValidity::Interpolated(InterpolationMethod::Trilinear)
            );
        }
    }

    /// PH-17 regression: `interpolation_cell` is the single implementation
    /// both the display sampling path here and `coupling::interpolate_particle_fields`
    /// build their trilinear reconstruction from. It must resolve a position
    /// and that same position shifted by exactly one periodic domain width
    /// to the identical stencil — before the two call sites shared one
    /// function, only the particle-pusher's copy wrapped position first, so
    /// they silently disagreed on exactly this for any position outside
    /// `[bounds.min(), bounds.min() + bounds.size())`.
    #[test]
    fn interpolation_cell_agrees_for_a_position_and_its_periodic_wrap() {
        let domain = periodic_domain(8);
        let bounds = domain.bounds();
        let inside = bounds.min() + bounds.size() * 0.37;
        let outside = inside + bounds.size();

        let (inside_base, inside_fraction) = interpolation_cell(domain, inside);
        let (outside_base, outside_fraction) = interpolation_cell(domain, outside);

        assert_eq!(inside_base, outside_base);
        assert!((inside_fraction - outside_fraction).length() < 1.0e-9);
    }

    /// The interior is what the desktop's default plane shows, and it is the
    /// reason the seam is marked undefined rather than the whole construction
    /// being replaced by a periodic Poisson solve: a Poisson solution is
    /// self-consistent but reproduces the periodic *lattice* field, which was
    /// measured at a 22.6% median error over this plane against 0.3% here.
    #[test]
    fn static_charge_interior_tracks_the_electrostatic_oracle() {
        use fieldcad_electrostatics::{collect_sources, evaluate_sources};

        let domain = desktop_domain();
        let (world, _) = charged_world(DVec3::new(0.0, 0.0, 0.6));
        let snapshot = world.snapshot();
        let solver = static_charge_solver(&domain, &snapshot);
        let sources = collect_sources(&snapshot).unwrap();

        // The shipped default slice plane: z = 0, half-extent 4.
        let positions: Vec<_> = (0..9)
            .flat_map(|u| (0..9).map(move |v| (u, v)))
            .map(|(u, v)| DVec3::new(f64::from(u) - 4.0, f64::from(v) - 4.0, 0.0))
            .filter(|position| position.distance(DVec3::new(0.0, 0.0, 0.6)) > 0.5)
            .collect();
        let geometry = points(positions.clone());
        let actual = vectors(solver.sample(ELECTRIC_FIELD_HANDLE, &geometry).unwrap());

        let mut worst: f64 = 0.0;
        for (position, actual) in positions.iter().zip(actual.iter()) {
            let expected = evaluate_sources(&sources, *position).electric_field;
            assert!(
                actual.normalize().dot(expected.normalize()) > 0.99,
                "at {position:?} Maxwell E {actual:?} points away from electrostatic {expected:?}"
            );
            worst = worst.max((*actual - expected).length() / expected.length());
        }
        assert!(worst < 0.2, "worst interior relative error was {worst:e}");
    }

    /// `ObjectShape::point(0.0)` is valid and reachable via MCP. A zero-radius
    /// point charge sitting exactly on a lattice node used to divide by zero
    /// in `regularized_potential`'s exterior branch, poisoning the whole grid
    /// with `NaN` within a few curl evaluations.
    #[test]
    fn a_zero_radius_point_charge_on_a_lattice_node_does_not_poison_the_grid() {
        use fieldcad_core::{ObjectShape, ObjectSpec, Transform, WorldCommand};
        use fieldcad_electromagnetic_sources::charge_properties;

        let domain = desktop_domain();
        // 32 cells over [-5, 5] puts a lattice node exactly at the origin.
        let mut world = World::new();
        world
            .commit([
                WorldCommand::RegisterComponentSchema(charge_component_schema()),
                WorldCommand::CreateObject(
                    ObjectSpec::new("degenerate point charge")
                        .with_transform(Transform::at(DVec3::ZERO).unwrap())
                        .with_shape(ObjectShape::point(0.0).unwrap())
                        .with_component(charge_component_id(), charge_properties(1.0e-9).unwrap()),
                ),
            ])
            .unwrap();

        let state = static_charge_initial_state(domain, &world.snapshot()).unwrap();

        assert!(
            state.electric.iter().all(|value| value.is_finite()),
            "a zero-radius point charge on a lattice node produced a non-finite electric field"
        );
    }

    #[test]
    fn conservation_diagnostics_exclude_the_fabricated_seam() {
        // With the charge near a boundary the seam layer previously carried
        // ~30% of the reported total energy.
        let domain = desktop_domain();
        let (world, _) = charged_world(DVec3::new(0.0, 0.0, 4.0));
        let state = static_charge_initial_state(domain, &world.snapshot()).unwrap();

        let honest = yee_conservation(
            domain,
            &state.electric,
            &state.magnetic,
            LatticePeriodicity::SeamOnOuterLayer,
        )
        .unwrap();
        let with_seam = yee_conservation(
            domain,
            &state.electric,
            &state.magnetic,
            LatticePeriodicity::Periodic,
        )
        .unwrap();

        assert!(
            honest.energy_joules < 0.8 * with_seam.energy_joules,
            "seam energy {:e} was not excluded from {:e}",
            with_seam.energy_joules,
            honest.energy_joules
        );
        assert!(honest.energy_joules > 0.0);
    }

    /// The runtime calls `on_world_changed` for every accepted commit, including
    /// probe and slice-plane drags. Rebuilding the whole constrained grid for an
    /// edit that cannot change it costs a full `cells × sources` pass and, on
    /// GPU, a complete grid upload.
    #[test]
    fn edits_that_cannot_change_the_charges_do_not_rebuild_the_constrained_state() {
        use fieldcad_core::{ProbeSpec, Transform, WorldCommand};

        let domain = desktop_domain();
        let (mut world, charge) = charged_world(DVec3::new(0.0, 0.0, 0.6));
        let sources = fieldcad_electrostatics::collect_sources(&world.snapshot()).unwrap();
        let setup = MaxwellSolverSetup {
            domain,
            initial_condition: MaxwellInitialCondition::StaticCharges,
            initial_state: static_charge_state_from_sources(domain, &sources),
            initial_sources: sources,
            initial_particles: Vec::new(),
            particle_coupling: false,
            world_revision: world.snapshot().revision(),
            initial_step: StepContext {
                tick: 0,
                time_seconds: 0.0,
                time_step: TimeStep::from_seconds(courant_limit(&domain) * 0.8).unwrap(),
            },
            cancellation: SolverCancellation::default(),
        };
        let mut core = MaxwellCore::new(&setup, "test");

        world
            .commit([WorldCommand::CreateProbe(ProbeSpec::at(
                "probe",
                DVec3::new(1.0, 0.0, 0.0),
                vec![electric_field_channel_id()],
            ))])
            .unwrap();
        assert!(
            core.constrained_state_for(&world.snapshot())
                .unwrap()
                .is_none(),
            "adding a probe rebuilt the Maxwell field"
        );

        world
            .commit([WorldCommand::SetTransform {
                object: charge,
                transform: Transform::at(DVec3::new(2.0, 0.0, 0.0)).unwrap(),
            }])
            .unwrap();
        assert!(
            core.constrained_state_for(&world.snapshot())
                .unwrap()
                .is_some(),
            "moving the charge must rebuild the Maxwell field"
        );
    }

    fn one_period_error(x_cells: u32) -> f64 {
        let domain = periodic_domain(x_cells);
        let mut solver = solver(&domain);
        let limit = courant_limit(&domain);
        let step = TimeStep::from_seconds(limit * 0.8).unwrap();
        let period = domain.bounds().size().x / SPEED_OF_LIGHT;
        let steps = (period / step.seconds()).round() as u64;
        let actual_time = steps as f64 * step.seconds();
        for tick in 1..=steps {
            solver
                .step(StepContext {
                    tick,
                    time_seconds: tick as f64 * step.seconds(),
                    time_step: step,
                })
                .unwrap();
        }

        let dx = domain.cell_size().x;
        let positions: Vec<_> = (0..x_cells)
            .map(|x| DVec3::new((f64::from(x) + 0.5) * dx, 0.5, 0.5))
            .collect();
        let column = solver
            .sample(ELECTRIC_FIELD_HANDLE, &points(positions.clone()))
            .unwrap();
        let FieldColumn::Vector(values) = column.values else {
            panic!("electric channel must be a vector");
        };
        let wave_number = std::f64::consts::TAU / domain.bounds().size().x;
        let reconstructed_amplitude = (0.5 * wave_number * dx).cos();
        let squared_error: f64 = positions
            .iter()
            .zip(values.iter())
            .map(|(position, value)| {
                let expected = reconstructed_amplitude
                    * (wave_number * position.x - wave_number * SPEED_OF_LIGHT * actual_time).sin();
                (value.y - expected).powi(2)
            })
            .sum();
        (squared_error / f64::from(x_cells)).sqrt()
    }

    #[test]
    fn vacuum_wave_converges_toward_the_continuum_wave_speed() {
        let coarse = one_period_error(16);
        let fine = one_period_error(32);

        assert!(
            fine < coarse,
            "coarse error {coarse:e}, fine error {fine:e}"
        );
        assert!(fine < 0.03, "fine-grid one-period error was {fine:e}");
    }
}
