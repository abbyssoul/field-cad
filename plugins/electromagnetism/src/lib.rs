//! CPU `f64` reference solver for vacuum Maxwell equations.
//!
//! Electric and magnetic components live on the conventional staggered Yee
//! lattice. Time integration uses the synchronized kick-drift-kick form of the
//! Yee leapfrog, so snapshots expose `E` and `B` at the same simulation time.
//! This first slice deliberately supports periodic boundaries and a prescribed
//! plane-wave initial condition; charge/current deposition is a later coupling
//! step, not something hidden inside the visualizer.

use fieldcad_core::{
    BoundaryCondition, ChannelId, ChannelSchema, DiagnosticSeverity, Dimension, Domain,
    FieldColumn, FieldValueKind, InterpolationMethod, PluginId, PluginVersion, Precision,
    PropertyBag, PropertyId, PropertyKind, PropertySchema, PropertyValue, Quantity, SampleGeometry,
    SampleValidity, SolverDiagnostic, StepContext, TimeStep, UndefinedReason, WorldRevision,
    WorldSnapshot,
};
use fieldcad_plugin_api::{
    ChannelHandle, EquationSystemPlugin, EquationSystemSolver, PluginConfigurationSchema,
    PluginError, PluginMetadata, SampledColumn, SolverContext, SolverKind,
};
use glam::{DVec3, UVec3};

pub const PLUGIN_ID: &str = "fieldcad.electromagnetism";
pub const ELECTRIC_FIELD_CHANNEL: &str = "electric-field";
pub const MAGNETIC_FIELD_CHANNEL: &str = "magnetic-flux-density";
pub const ENERGY_DENSITY_CHANNEL: &str = "energy-density";
pub const ELECTRIC_DIVERGENCE_CHANNEL: &str = "electric-divergence-residual";
pub const MAGNETIC_DIVERGENCE_CHANNEL: &str = "magnetic-divergence-residual";

const AMPLITUDE_PROPERTY: &str = "plane-wave-amplitude";
const MODE_PROPERTY: &str = "plane-wave-mode";

pub const ELECTRIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(0);
pub const MAGNETIC_FIELD_HANDLE: ChannelHandle = ChannelHandle::new(1);
pub const ENERGY_DENSITY_HANDLE: ChannelHandle = ChannelHandle::new(2);
pub const ELECTRIC_DIVERGENCE_HANDLE: ChannelHandle = ChannelHandle::new(3);
pub const MAGNETIC_DIVERGENCE_HANDLE: ChannelHandle = ChannelHandle::new(4);

/// Exact SI definition, in metres per second.
pub const SPEED_OF_LIGHT: f64 = 299_792_458.0;
/// Vacuum permeability used by the reference energy diagnostic.
pub const VACUUM_PERMEABILITY: f64 = 1.256_637_062_12e-6;
/// Kept algebraically consistent with `c` and `mu0` for the discrete update.
pub const VACUUM_PERMITTIVITY: f64 = 1.0 / (VACUUM_PERMEABILITY * SPEED_OF_LIGHT * SPEED_OF_LIGHT);

pub fn plugin_id() -> PluginId {
    PluginId::new(PLUGIN_ID).expect("static plugin ID is valid")
}

fn channel_id(name: &str) -> ChannelId {
    ChannelId::new(plugin_id(), name).expect("static channel ID is valid")
}

pub fn electric_field_channel_id() -> ChannelId {
    channel_id(ELECTRIC_FIELD_CHANNEL)
}

pub fn magnetic_field_channel_id() -> ChannelId {
    channel_id(MAGNETIC_FIELD_CHANNEL)
}

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

/// A small reference plugin, intentionally independent of rendering and `wgpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ElectromagnetismPlugin;

impl EquationSystemPlugin for ElectromagnetismPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: plugin_id(),
            version: PluginVersion::new(0, 1, 0),
            display_name: "Vacuum electromagnetism (CPU reference)".to_owned(),
            description: "Periodic Yee-lattice Maxwell solver with a prescribed plane wave"
                .to_owned(),
        }
    }

    fn channels(&self) -> Vec<ChannelSchema> {
        vec![
            ChannelSchema {
                id: electric_field_channel_id(),
                display_name: "Electric field E".to_owned(),
                value_kind: FieldValueKind::Vector(Dimension::ELECTRIC_FIELD),
            },
            ChannelSchema {
                id: magnetic_field_channel_id(),
                display_name: "Magnetic flux density B".to_owned(),
                value_kind: FieldValueKind::Vector(Dimension::MAGNETIC_FLUX_DENSITY),
            },
            ChannelSchema {
                id: energy_density_channel_id(),
                display_name: "Electromagnetic energy density".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::ENERGY_DENSITY),
            },
            ChannelSchema {
                id: electric_divergence_channel_id(),
                display_name: "Vacuum div(E) residual".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::ELECTRIC_FIELD_DIVERGENCE),
            },
            ChannelSchema {
                id: magnetic_divergence_channel_id(),
                display_name: "div(B) residual".to_owned(),
                value_kind: FieldValueKind::Scalar(Dimension::MAGNETIC_FIELD_DIVERGENCE),
            },
        ]
    }

    fn configuration_schema(&self) -> PluginConfigurationSchema {
        PluginConfigurationSchema {
            properties: vec![
                PropertySchema {
                    id: amplitude_property_id(),
                    display_name: "Initial plane-wave amplitude".to_owned(),
                    kind: PropertyKind::Scalar(Dimension::ELECTRIC_FIELD),
                    required: true,
                },
                PropertySchema {
                    id: mode_property_id(),
                    display_name: "Initial plane-wave mode".to_owned(),
                    kind: PropertyKind::Scalar(Dimension::DIMENSIONLESS),
                    required: true,
                },
            ],
        }
    }

    fn default_configuration(&self) -> PropertyBag {
        [
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
        validate_domain(context.domain)?;

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

        Ok(Box::new(MaxwellSolver::new(
            *context.domain,
            amplitude,
            mode_value as u32,
            context.world.revision(),
        )))
    }
}

fn validate_domain(domain: &Domain) -> Result<(), PluginError> {
    if domain.precision() != Precision::F64 {
        return Err(PluginError::InvalidConfiguration(
            "the CPU Maxwell reference requires an f64 domain".to_owned(),
        ));
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

/// Courant limit for a three-dimensional rectangular Yee lattice.
pub fn courant_limit(domain: &Domain) -> f64 {
    let spacing = domain.cell_size();
    1.0 / (SPEED_OF_LIGHT
        * (spacing.x.recip().powi(2) + spacing.y.recip().powi(2) + spacing.z.recip().powi(2))
            .sqrt())
}

struct MaxwellSolver {
    domain: Domain,
    counts: UVec3,
    spacing: DVec3,
    electric: Vec<DVec3>,
    magnetic: Vec<DVec3>,
    tick: u64,
    world_revision: WorldRevision,
}

impl MaxwellSolver {
    fn new(domain: Domain, amplitude: f64, mode: u32, world_revision: WorldRevision) -> Self {
        let counts = domain.resolution().cells();
        let spacing = domain.cell_size();
        let mut electric = vec![DVec3::ZERO; domain.resolution().cell_count() as usize];
        let mut magnetic = vec![DVec3::ZERO; electric.len()];
        let wave_number = f64::from(mode) * std::f64::consts::TAU / domain.bounds().size().x;

        for z in 0..counts.z {
            for y in 0..counts.y {
                for x in 0..counts.x {
                    let index = linear_index(counts, x, y, z);
                    // Ey is at x_i; Bz is half a cell farther along x. This is
                    // the spatial staggering of a +x travelling plane wave.
                    let electric_x = domain.bounds().min().x + f64::from(x) * spacing.x;
                    let magnetic_x = electric_x + 0.5 * spacing.x;
                    electric[index].y = amplitude * (wave_number * electric_x).sin();
                    magnetic[index].z =
                        amplitude / SPEED_OF_LIGHT * (wave_number * magnetic_x).sin();
                }
            }
        }

        Self {
            domain,
            counts,
            spacing,
            electric,
            magnetic,
            tick: 0,
            world_revision,
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

    fn advance_electric(&mut self, seconds: f64) {
        let scale = SPEED_OF_LIGHT * SPEED_OF_LIGHT * seconds;
        for z in 0..self.counts.z {
            for y in 0..self.counts.y {
                for x in 0..self.counts.x {
                    let index = linear_index(self.counts, x, y, z);
                    let curl = self.curl_b_backward(x, y, z);
                    self.electric[index] += scale * curl;
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

    /// Reconstruct staggered components at one cell centre.
    fn centred_fields(&self, x: u32, y: u32, z: u32) -> (DVec3, DVec3) {
        let xn = wrap_next(x, self.counts.x);
        let yn = wrap_next(y, self.counts.y);
        let zn = wrap_next(z, self.counts.z);
        let e000 = self.electric_at(x, y, z);
        let electric = DVec3::new(
            0.25 * (e000.x
                + self.electric_at(x, yn, z).x
                + self.electric_at(x, y, zn).x
                + self.electric_at(x, yn, zn).x),
            0.25 * (e000.y
                + self.electric_at(xn, y, z).y
                + self.electric_at(x, y, zn).y
                + self.electric_at(xn, y, zn).y),
            0.25 * (e000.z
                + self.electric_at(xn, y, z).z
                + self.electric_at(x, yn, z).z
                + self.electric_at(xn, yn, z).z),
        );

        let b000 = self.magnetic_at(x, y, z);
        let magnetic = DVec3::new(
            0.5 * (b000.x + self.magnetic_at(xn, y, z).x),
            0.5 * (b000.y + self.magnetic_at(x, yn, z).y),
            0.5 * (b000.z + self.magnetic_at(x, y, zn).z),
        );
        (electric, magnetic)
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
                    result += weight * select(self.centred_fields(cell.x, cell.y, cell.z));
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

    fn interpolation_cell(&self, position: DVec3) -> (glam::IVec3, DVec3) {
        let grid = (position - self.domain.bounds().min()) / self.spacing - DVec3::splat(0.5);
        let floor = grid.floor();
        (floor.as_ivec3(), grid - floor)
    }

    fn wrapped_cell(&self, x: i32, y: i32, z: i32) -> UVec3 {
        UVec3::new(
            x.rem_euclid(self.counts.x as i32) as u32,
            y.rem_euclid(self.counts.y as i32) as u32,
            z.rem_euclid(self.counts.z as i32) as u32,
        )
    }

    fn energy_at_cell(&self, x: u32, y: u32, z: u32) -> f64 {
        let (electric, magnetic) = self.centred_fields(x, y, z);
        0.5 * (VACUUM_PERMITTIVITY * electric.length_squared()
            + magnetic.length_squared() / VACUUM_PERMEABILITY)
    }
}

impl EquationSystemSolver for MaxwellSolver {
    fn kind(&self) -> SolverKind {
        SolverKind::TimeStepped
    }

    fn validate_time_step(&self, time_step: TimeStep) -> Result<(), PluginError> {
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

    fn on_world_changed(&mut self, world: &WorldSnapshot) -> Result<(), PluginError> {
        self.world_revision = world.revision();
        Ok(())
    }

    fn step(&mut self, context: StepContext) -> Result<(), PluginError> {
        self.validate_time_step(context.time_step)?;
        if context.tick != self.tick + 1 {
            return Err(PluginError::Solver(format!(
                "expected Maxwell tick {}, received {}",
                self.tick + 1,
                context.tick
            )));
        }
        let half_step = 0.5 * context.time_step.seconds();
        self.advance_magnetic(half_step);
        self.advance_electric(context.time_step.seconds());
        self.advance_magnetic(half_step);
        self.tick = context.tick;
        Ok(())
    }

    fn sample(
        &self,
        channel: ChannelHandle,
        geometry: &SampleGeometry,
    ) -> Result<SampledColumn, PluginError> {
        let mut validity = Vec::with_capacity(geometry.len());
        let mark = |position: DVec3, validity: &mut Vec<SampleValidity>| {
            let inside = self.domain.bounds().contains(position);
            validity.push(if inside {
                SampleValidity::Interpolated(InterpolationMethod::Trilinear)
            } else {
                SampleValidity::Undefined(UndefinedReason::OutsideDomain)
            });
            inside
        };

        match channel {
            ELECTRIC_FIELD_HANDLE | MAGNETIC_FIELD_HANDLE => {
                let mut values = Vec::with_capacity(geometry.len());
                for position in geometry.positions() {
                    let inside = mark(position, &mut validity);
                    let value = if !inside {
                        DVec3::ZERO
                    } else if channel == ELECTRIC_FIELD_HANDLE {
                        self.interpolate_vector(position, |(electric, _)| electric)
                    } else {
                        self.interpolate_vector(position, |(_, magnetic)| magnetic)
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
                        self.interpolate_scalar(position, Self::energy_at_cell)
                    } else if channel == ELECTRIC_DIVERGENCE_HANDLE {
                        self.interpolate_scalar(position, Self::electric_divergence)
                    } else {
                        self.interpolate_scalar(position, Self::magnetic_divergence)
                    };
                    values.push(value);
                }
                Ok(SampledColumn::new(FieldColumn::scalars(values), validity))
            }
            other => Err(PluginError::UnknownChannel(other.index())),
        }
    }

    fn diagnostics(&self) -> Vec<SolverDiagnostic> {
        let mut energy = 0.0_f64;
        let mut max_div_e = 0.0_f64;
        let mut max_div_b = 0.0_f64;
        for z in 0..self.counts.z {
            for y in 0..self.counts.y {
                for x in 0..self.counts.x {
                    energy += self.energy_at_cell(x, y, z)
                        * self.spacing.x
                        * self.spacing.y
                        * self.spacing.z;
                    max_div_e = max_div_e.max(self.electric_divergence(x, y, z).abs());
                    max_div_b = max_div_b.max(self.magnetic_divergence(x, y, z).abs());
                }
            }
        }
        vec![
            SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "yee-courant-limit".to_owned(),
                message: format!(
                    "CPU f64 Yee lattice; periodic boundaries; Courant dt <= {:.6e} s",
                    courant_limit(&self.domain)
                ),
            },
            SolverDiagnostic {
                plugin: plugin_id(),
                severity: DiagnosticSeverity::Info,
                code: "maxwell-conservation".to_owned(),
                message: format!(
                    "energy {:.6e} J; max |div E| {:.6e}; max |div B| {:.6e}; world revision {}",
                    energy, max_div_e, max_div_b, self.world_revision
                ),
            },
        ]
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

#[cfg(test)]
mod tests {
    use fieldcad_core::{
        BoundaryConditions, DomainBounds, FieldColumn, ProbeId, Resolution, World,
    };

    use super::*;

    fn periodic_domain(x_cells: u32) -> Domain {
        Domain::new(
            DomainBounds::new(DVec3::ZERO, DVec3::ONE).unwrap(),
            Resolution::new(x_cells, 2, 2).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        )
    }

    fn solver(domain: &Domain) -> Box<dyn EquationSystemSolver> {
        let plugin = ElectromagnetismPlugin;
        let world = World::new();
        plugin
            .create_solver(SolverContext {
                configuration: &plugin.default_configuration(),
                domain,
                world: &world.snapshot(),
            })
            .unwrap()
    }

    fn points(positions: Vec<DVec3>) -> SampleGeometry {
        let ids = (0..positions.len() as u64).map(ProbeId::new).collect();
        SampleGeometry::probes(ids, positions).unwrap()
    }

    #[test]
    fn plugin_declares_coupled_fields_and_residuals() {
        let channels = ElectromagnetismPlugin.channels();

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
    fn reference_solver_requires_its_documented_representation() {
        let plugin = ElectromagnetismPlugin;
        let world = World::new();
        let open = Domain::centred_cube(1.0, 8).unwrap();

        let result = plugin.create_solver(SolverContext {
            configuration: &plugin.default_configuration(),
            domain: &open,
            world: &world.snapshot(),
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
