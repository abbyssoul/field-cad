//! Reference field/particle coupling shared by CPU and host-owned GPU backends.
//!
//! The routines here deliberately favour a small, inspectable `f64` oracle.
//! Charge and current use the same periodic cloud-in-cell shape. Current is
//! accumulated along all six axis-order paths between the old and new particle
//! positions; every path satisfies the discrete continuity equation exactly,
//! and their average removes a preferred coordinate ordering.

use fieldcad_core::quantities::SiScalar;
use fieldcad_core::{
    Domain, ObjectId, ObjectShape, Transform, Velocity, WorldSnapshot, lorentz_factor,
    relativistic_kinetic_energy,
};
use fieldcad_electromagnetic_sources::ChargeSource;
use fieldcad_particles::{Particle, collect_particles};
use fieldcad_plugin_api::{ObjectKinematicsUpdate, PluginError, SolverStepOutcome};
use glam::{DVec3, UVec3};

use super::{
    SPEED_OF_LIGHT, VACUUM_PERMITTIVITY, YeeFieldState, axis_weight, centred_fields,
    interpolation_cell, linear_index, wrap_next, wrap_position, wrap_previous, wrapped_cell,
};

const AXIS_ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

#[derive(Clone, Debug)]
pub(crate) struct ParticleCoupling {
    particles: Vec<Particle>,
    kinematic_objects: Vec<ObjectId>,
    total_charge_coulombs: f64,
    neutralizing_background_coulombs: f64,
    particle_energy_joules: f64,
    reference_total_energy_joules: f64,
    continuity_residual: f64,
    intervention_count: u64,
    /// Whether any body is carried along at an authored velocity rather than
    /// integrated from the fields. Such a body exchanges work with whatever is
    /// holding it, which the energy budget cannot see.
    authored_motion: bool,
}

#[derive(Clone, Debug)]
pub struct CoupledAdvance {
    pub current_density: Vec<DVec3>,
    pub outcome: SolverStepOutcome,
}

impl ParticleCoupling {
    pub(crate) fn new(
        particles: Vec<Particle>,
        sources: &[ChargeSource],
        initial_field_energy_joules: f64,
    ) -> Result<Self, PluginError> {
        let kinematic_objects = particles
            .iter()
            .filter(|particle| particle.needs_kinematic_authority())
            .map(|particle| particle.object)
            .collect();
        let particle_energy_joules = particle_kinetic_energy(&particles);
        let total_charge_coulombs = sources
            .iter()
            .map(|source| source.coupling_value.into_si())
            .sum();
        Ok(Self {
            authored_motion: particles
                .iter()
                .any(|particle| particle.pinned && particle.velocity != DVec3::ZERO),
            particles,
            kinematic_objects,
            total_charge_coulombs,
            // A periodic Poisson problem cannot represent net charge. Removing
            // the mean density is equivalent to an explicit uniform background.
            neutralizing_background_coulombs: -total_charge_coulombs,
            particle_energy_joules,
            reference_total_energy_joules: initial_field_energy_joules + particle_energy_joules,
            continuity_residual: 0.0,
            intervention_count: 0,
        })
    }

    pub(crate) fn kinematic_objects(&self) -> &[ObjectId] {
        &self.kinematic_objects
    }

    pub(crate) fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// Adopt an authored physical change. Solver-produced revisions already
    /// match the state predicted in `advance` and never call this method.
    pub(crate) fn adopt_intervention(
        &mut self,
        particles: Vec<Particle>,
        sources: &[ChargeSource],
    ) {
        self.particles = particles;
        self.kinematic_objects = self
            .particles
            .iter()
            .filter(|particle| particle.needs_kinematic_authority())
            .map(|particle| particle.object)
            .collect();
        self.total_charge_coulombs = sources
            .iter()
            .map(|source| source.coupling_value.into_si())
            .sum();
        self.neutralizing_background_coulombs = -self.total_charge_coulombs;
        self.particle_energy_joules = particle_kinetic_energy(&self.particles);
        self.authored_motion = self
            .particles
            .iter()
            .any(|particle| particle.pinned && particle.velocity != DVec3::ZERO);
        self.intervention_count += 1;
    }

    pub(crate) fn reset_energy_reference(&mut self, field_energy_joules: f64) {
        self.reference_total_energy_joules = field_energy_joules + self.particle_energy_joules;
        self.continuity_residual = 0.0;
    }

    pub(crate) fn advance(
        &mut self,
        domain: Domain,
        fields: &YeeFieldState,
        seconds: f64,
    ) -> Result<CoupledAdvance, PluginError> {
        let mut current_density = zero_vector_grid(domain);
        let mut updates = Vec::with_capacity(self.kinematic_objects.len());
        let old_charge = deposit_particle_charge(domain, &self.particles);

        for particle in &mut self.particles {
            // A pinned, stationary body cannot move and deposits no current, so
            // it is skipped rather than pushed through the deposition path to
            // produce zeros.
            if !particle.needs_kinematic_authority() {
                continue;
            }
            let old_position = particle.position;
            let new_velocity = if particle.pinned {
                // The user owns this body's motion: carry it at the authored
                // velocity without integrating any force on it.
                particle.velocity
            } else {
                let (electric, magnetic) =
                    interpolate_particle_fields(domain, fields, particle.position)?;
                relativistic_boris_velocity(
                    particle.velocity,
                    particle.charge_coulombs.into_si(),
                    particle.mass_kg.into_si(),
                    electric,
                    magnetic,
                    seconds,
                )?
            };
            let unwrapped_position = old_position + new_velocity * seconds;
            let new_position = wrap_position(domain, unwrapped_position);
            deposit_charge_conserving_current(
                domain,
                particle.charge_coulombs.into_si(),
                old_position,
                unwrapped_position,
                seconds,
                &mut current_density,
            )?;
            particle.position = new_position;
            particle.velocity = new_velocity;
            updates.push(ObjectKinematicsUpdate {
                object: particle.object,
                transform: Transform::at(new_position)
                    .map_err(|error| PluginError::Solver(error.to_string()))?,
                velocity: Velocity::new(new_velocity, DVec3::ZERO)
                    .map_err(|error| PluginError::Solver(error.to_string()))?,
            });
        }

        let new_charge = deposit_particle_charge(domain, &self.particles);
        self.continuity_residual =
            continuity_residual(domain, &old_charge, &new_charge, &current_density, seconds);
        self.particle_energy_joules = particle_kinetic_energy(&self.particles);

        Ok(CoupledAdvance {
            current_density,
            outcome: SolverStepOutcome {
                object_kinematics: updates,
            },
        })
    }

    pub(crate) fn diagnostic_summary(&self, field_energy_joules: f64) -> String {
        let total_energy = field_energy_joules + self.particle_energy_joules;
        let scale = self
            .reference_total_energy_joules
            .abs()
            .max(f64::MIN_POSITIVE);
        let drift = (total_energy - self.reference_total_energy_joules) / scale;
        format!(
            "periodic CIC + relativistic Boris; charge {:.6e} C; neutralizing background {:.6e} C; particle kinetic energy {:.6e} J; combined energy drift {:+.6e}; max continuity residual {:.6e} C m^-3 s^-1; interventions {}; periodic wrap/pass-through particles; no collision model{}",
            self.total_charge_coulombs,
            self.neutralizing_background_coulombs,
            self.particle_energy_joules,
            drift,
            self.continuity_residual,
            self.intervention_count,
            if self.authored_motion {
                "; authored (pinned) motion can exchange untracked external work"
            } else {
                ""
            }
        )
    }
}

pub(crate) fn coupling_is_requested(world: &WorldSnapshot) -> Result<bool, PluginError> {
    let particles = collect_particles(world)
        .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))?;
    Ok(particles.iter().any(Particle::needs_kinematic_authority))
}

pub(crate) fn collect_coupled_particles(
    domain: Domain,
    world: &WorldSnapshot,
) -> Result<Vec<Particle>, PluginError> {
    let particles = collect_particles(world)
        .map_err(|error| PluginError::UnsupportedWorld(error.to_string()))?;
    for particle in &particles {
        if !domain.bounds().contains(particle.position) {
            return Err(PluginError::UnsupportedWorld(format!(
                "particle {:?} lies outside the periodic Maxwell domain",
                particle.object
            )));
        }
        if particle.velocity.length() >= SPEED_OF_LIGHT {
            return Err(PluginError::UnsupportedWorld(format!(
                "particle {:?} has speed at or above c",
                particle.object
            )));
        }
        let object = world
            .object(particle.object)
            .expect("collected particle belongs to this world");
        if matches!(
            object.shape,
            Some(ObjectShape::Sphere { .. } | ObjectShape::Box { .. })
        ) {
            return Err(PluginError::UnsupportedWorld(format!(
                "coupled particle '{}' must use a point authoring shape",
                object.name
            )));
        }
    }
    Ok(particles)
}

/// Periodic, Gauss-consistent electric initialization for PIC coupling.
pub fn periodic_charge_initial_state(
    domain: Domain,
    sources: &[ChargeSource],
) -> Result<YeeFieldState, PluginError> {
    let mut rho = deposit_source_charge(domain, sources);
    let mean = rho.iter().sum::<f64>() / rho.len() as f64;
    for value in &mut rho {
        *value = (*value - mean) / VACUUM_PERMITTIVITY;
    }
    let potential = solve_periodic_poisson(domain, &rho)?;
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    let mut electric = zero_vector_grid(domain);
    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let index = linear_index(counts, x, y, z);
                electric[index] = -DVec3::new(
                    (potential[linear_index(counts, wrap_next(x, counts.x), y, z)]
                        - potential[index])
                        / spacing.x,
                    (potential[linear_index(counts, x, wrap_next(y, counts.y), z)]
                        - potential[index])
                        / spacing.y,
                    (potential[linear_index(counts, x, y, wrap_next(z, counts.z))]
                        - potential[index])
                        / spacing.z,
                );
            }
        }
    }
    Ok(YeeFieldState {
        magnetic: zero_vector_grid(domain),
        electric,
    })
}

pub fn deposit_source_charge(domain: Domain, sources: &[ChargeSource]) -> Vec<f64> {
    let mut rho = zero_scalar_grid(domain);
    for source in sources {
        deposit_cic_scalar(
            domain,
            source.position,
            source.coupling_value.into_si(),
            &mut rho,
        );
    }
    rho
}

pub fn deposit_particle_charge(domain: Domain, particles: &[Particle]) -> Vec<f64> {
    let mut rho = zero_scalar_grid(domain);
    for particle in particles {
        deposit_cic_scalar(
            domain,
            particle.position,
            particle.charge_coulombs.into_si(),
            &mut rho,
        );
    }
    rho
}

fn deposit_cic_scalar(domain: Domain, position: DVec3, charge: f64, rho: &mut [f64]) {
    let counts = domain.resolution().cells();
    let volume = cell_volume(domain);
    let shape = cic_shape(domain, position);
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let cell = UVec3::new(
                    shape.indices[0][x],
                    shape.indices[1][y],
                    shape.indices[2][z],
                );
                rho[linear_index(counts, cell.x, cell.y, cell.z)] +=
                    charge * shape.weights[0][x] * shape.weights[1][y] * shape.weights[2][z]
                        / volume;
            }
        }
    }
}

/// Deposit current for one particle move while preserving
/// `(rho_new-rho_old)/dt + div(J) = 0` on the periodic grid.
pub fn deposit_charge_conserving_current(
    domain: Domain,
    charge: f64,
    old_position: DVec3,
    new_unwrapped_position: DVec3,
    seconds: f64,
    current_density: &mut [DVec3],
) -> Result<(), PluginError> {
    if seconds <= 0.0 || !seconds.is_finite() {
        return Err(PluginError::Solver(
            "current deposition requires a finite positive dt".to_owned(),
        ));
    }
    if current_density.len() != domain.resolution().cell_count() as usize {
        return Err(PluginError::Solver(
            "current grid size does not match the Maxwell domain".to_owned(),
        ));
    }
    let displacement = new_unwrapped_position - old_position;
    let spacing = domain.cell_size();
    if (displacement.abs() / spacing).max_element() > 1.0 + 1.0e-12 {
        return Err(PluginError::Solver(
            "a particle crossed more than one cell in one Maxwell step".to_owned(),
        ));
    }

    let counts = domain.resolution().cells();
    let axis_count = counts.max_element() as usize;
    let mut delta = vec![0.0; axis_count];
    let mut flux = vec![0.0; axis_count];

    for order in AXIS_ORDERS {
        let mut start = old_position;
        for axis in order {
            let mut end = start;
            end[axis] = new_unwrapped_position[axis];
            deposit_axis_segment(
                domain,
                charge / 6.0,
                start,
                end,
                axis,
                seconds,
                current_density,
                &mut delta,
                &mut flux,
            );
            start = end;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn deposit_axis_segment(
    domain: Domain,
    charge: f64,
    start: DVec3,
    end: DVec3,
    axis: usize,
    seconds: f64,
    current: &mut [DVec3],
    delta: &mut [f64],
    flux: &mut [f64],
) {
    if start[axis] == end[axis] || charge == 0.0 {
        return;
    }
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    let old_shape = cic_shape(domain, start);
    let new_shape = cic_shape(domain, end);
    delta.fill(0.0);
    for corner in 0..2 {
        delta[old_shape.indices[axis][corner] as usize] -= old_shape.weights[axis][corner];
        delta[new_shape.indices[axis][corner] as usize] += new_shape.weights[axis][corner];
    }
    one_dimensional_flux(
        delta,
        -charge * spacing[axis] / (cell_volume(domain) * seconds),
        flux,
    );
    let transverse = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    for first_corner in 0..2 {
        for second_corner in 0..2 {
            let weight = old_shape.weights[transverse[0]][first_corner]
                * old_shape.weights[transverse[1]][second_corner];
            for (coordinate, value) in flux.iter().enumerate() {
                if *value == 0.0 {
                    continue;
                }
                let mut cell = UVec3::ZERO;
                cell[axis] = coordinate as u32;
                cell[transverse[0]] = old_shape.indices[transverse[0]][first_corner];
                cell[transverse[1]] = old_shape.indices[transverse[1]][second_corner];
                current[linear_index(counts, cell.x, cell.y, cell.z)][axis] += value * weight;
            }
        }
    }
}

fn one_dimensional_flux(delta: &[f64], scale: f64, flux: &mut [f64]) {
    let count = delta.len();
    flux.fill(0.0);
    let cut = (0..count)
        .find(|&index| delta[index].abs() <= f64::EPSILON)
        .unwrap_or(0);
    let mut accumulated = 0.0;
    for offset in 0..count {
        let index = (cut + offset) % count;
        accumulated += scale * delta[index];
        flux[index] = accumulated;
    }
    // Roundoff in weights can leave a tiny constant on the closing edge.
    if accumulated.abs() <= 64.0 * f64::EPSILON * scale.abs().max(1.0) {
        flux[(cut + count - 1) % count] = 0.0;
    }
}

pub fn continuity_residual(
    domain: Domain,
    old_charge: &[f64],
    new_charge: &[f64],
    current: &[DVec3],
    seconds: f64,
) -> f64 {
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    let mut maximum: f64 = 0.0;
    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let index = linear_index(counts, x, y, z);
                let here = current[index];
                let xp = current[linear_index(counts, wrap_previous(x, counts.x), y, z)];
                let yp = current[linear_index(counts, x, wrap_previous(y, counts.y), z)];
                let zp = current[linear_index(counts, x, y, wrap_previous(z, counts.z))];
                let divergence = (here.x - xp.x) / spacing.x
                    + (here.y - yp.y) / spacing.y
                    + (here.z - zp.z) / spacing.z;
                maximum = maximum
                    .max(((new_charge[index] - old_charge[index]) / seconds + divergence).abs());
            }
        }
    }
    maximum
}

/// Trilinear interpolation of the reconstructed cell-centred Yee fields.
pub fn interpolate_particle_fields(
    domain: Domain,
    fields: &YeeFieldState,
    position: DVec3,
) -> Result<(DVec3, DVec3), PluginError> {
    let expected = domain.resolution().cell_count() as usize;
    if fields.electric.len() != expected || fields.magnetic.len() != expected {
        return Err(PluginError::Solver(
            "particle interpolation received incomplete Yee storage".to_owned(),
        ));
    }
    let counts = domain.resolution().cells();
    let (base, fraction) = interpolation_cell(domain, position);
    let mut electric = DVec3::ZERO;
    let mut magnetic = DVec3::ZERO;
    for dz in 0..=1 {
        for dy in 0..=1 {
            for dx in 0..=1 {
                let weight = axis_weight(fraction.x, dx)
                    * axis_weight(fraction.y, dy)
                    * axis_weight(fraction.z, dz);
                let cell = wrapped_cell(counts, base.x + dx, base.y + dy, base.z + dz);
                let (cell_e, cell_b) = centred_fields(
                    counts,
                    &fields.electric,
                    &fields.magnetic,
                    cell.x,
                    cell.y,
                    cell.z,
                );
                electric += weight * cell_e;
                magnetic += weight * cell_b;
            }
        }
    }
    Ok((electric, magnetic))
}

/// Relativistic Boris momentum rotation. This bounds finite particle speeds
/// below `c` while retaining the ordinary non-relativistic Boris orbit at low
/// velocity.
pub fn relativistic_boris_velocity(
    velocity: DVec3,
    charge_coulombs: f64,
    mass_kg: f64,
    electric: DVec3,
    magnetic: DVec3,
    seconds: f64,
) -> Result<DVec3, PluginError> {
    if mass_kg <= 0.0 || !mass_kg.is_finite() || seconds <= 0.0 || !seconds.is_finite() {
        return Err(PluginError::Solver(
            "the Boris pusher requires finite positive mass and dt".to_owned(),
        ));
    }
    if velocity.length() >= SPEED_OF_LIGHT {
        return Err(PluginError::Solver(
            "the Boris pusher requires an initial speed below c".to_owned(),
        ));
    }
    let gamma = lorentz_factor(velocity);
    let momentum_per_mass = gamma * velocity;
    let electric_kick = charge_coulombs * seconds / (2.0 * mass_kg) * electric;
    let minus = momentum_per_mass + electric_kick;
    let gamma_minus = (1.0 + minus.length_squared() / SPEED_OF_LIGHT.powi(2)).sqrt();
    let t = charge_coulombs * seconds / (2.0 * mass_kg * gamma_minus) * magnetic;
    let s = 2.0 * t / (1.0 + t.length_squared());
    let prime = minus + minus.cross(t);
    let plus = minus + prime.cross(s);
    let final_momentum_per_mass = plus + electric_kick;
    let final_gamma =
        (1.0 + final_momentum_per_mass.length_squared() / SPEED_OF_LIGHT.powi(2)).sqrt();
    let result = final_momentum_per_mass / final_gamma;
    if !result.is_finite() || result.length() >= SPEED_OF_LIGHT {
        return Err(PluginError::Solver(
            "the Boris pusher produced invalid relativistic kinematics".to_owned(),
        ));
    }
    Ok(result)
}

fn solve_periodic_poisson(domain: Domain, rhs: &[f64]) -> Result<Vec<f64>, PluginError> {
    let expected = domain.resolution().cell_count() as usize;
    if rhs.len() != expected {
        return Err(PluginError::Solver(
            "Poisson right-hand side does not match the domain".to_owned(),
        ));
    }
    let mut solution = vec![0.0; expected];
    let mut residual = rhs.to_vec();
    let mut direction = residual.clone();
    let initial_norm = dot(&residual, &residual);
    if initial_norm == 0.0 {
        return Ok(solution);
    }
    let target = initial_norm * 1.0e-24;
    let max_iterations = 8 * domain
        .resolution()
        .cells()
        .to_array()
        .into_iter()
        .map(|value| value as usize)
        .sum::<usize>();
    let mut residual_norm = initial_norm;
    for _ in 0..max_iterations {
        let applied = negative_laplacian(domain, &direction);
        let denominator = dot(&direction, &applied);
        if denominator <= 0.0 || !denominator.is_finite() {
            return Err(PluginError::Solver(
                "periodic Poisson solve lost positive definiteness".to_owned(),
            ));
        }
        let alpha = residual_norm / denominator;
        axpy(&mut solution, alpha, &direction);
        axpy(&mut residual, -alpha, &applied);
        let next_norm = dot(&residual, &residual);
        if next_norm <= target {
            return Ok(solution);
        }
        let beta = next_norm / residual_norm;
        for (direction, residual) in direction.iter_mut().zip(&residual) {
            *direction = *residual + beta * *direction;
        }
        residual_norm = next_norm;
    }
    Err(PluginError::Solver(format!(
        "periodic Poisson solve did not converge; relative residual {:.3e}",
        (residual_norm / initial_norm).sqrt()
    )))
}

fn negative_laplacian(domain: Domain, values: &[f64]) -> Vec<f64> {
    let counts = domain.resolution().cells();
    let spacing = domain.cell_size();
    let inverse_squared = DVec3::new(
        spacing.x.recip().powi(2),
        spacing.y.recip().powi(2),
        spacing.z.recip().powi(2),
    );
    let mut result = vec![0.0; values.len()];
    for z in 0..counts.z {
        for y in 0..counts.y {
            for x in 0..counts.x {
                let index = linear_index(counts, x, y, z);
                let here = values[index];
                result[index] = (2.0 * here
                    - values[linear_index(counts, wrap_previous(x, counts.x), y, z)]
                    - values[linear_index(counts, wrap_next(x, counts.x), y, z)])
                    * inverse_squared.x
                    + (2.0 * here
                        - values[linear_index(counts, x, wrap_previous(y, counts.y), z)]
                        - values[linear_index(counts, x, wrap_next(y, counts.y), z)])
                        * inverse_squared.y
                    + (2.0 * here
                        - values[linear_index(counts, x, y, wrap_previous(z, counts.z))]
                        - values[linear_index(counts, x, y, wrap_next(z, counts.z))])
                        * inverse_squared.z;
            }
        }
    }
    result
}

fn particle_kinetic_energy(particles: &[Particle]) -> f64 {
    particles
        .iter()
        .map(|particle| relativistic_kinetic_energy(particle.velocity, particle.mass_kg.into_si()))
        .sum()
}

#[derive(Clone, Copy)]
struct CicShape {
    indices: [[u32; 2]; 3],
    weights: [[f64; 2]; 3],
}

fn cic_shape(domain: Domain, position: DVec3) -> CicShape {
    let counts = domain.resolution().cells();
    let grid = (wrap_position(domain, position) - domain.bounds().min()) / domain.cell_size();
    let floor = grid.floor();
    let fraction = grid - floor;
    let base = floor.as_ivec3();
    let counts_array = counts.to_array();
    let base_array = base.to_array();
    let fraction_array = fraction.to_array();
    let mut indices = [[0; 2]; 3];
    let mut weights = [[0.0; 2]; 3];
    for axis in 0..3 {
        indices[axis] = [
            base_array[axis].rem_euclid(counts_array[axis] as i32) as u32,
            (base_array[axis] + 1).rem_euclid(counts_array[axis] as i32) as u32,
        ];
        weights[axis] = [1.0 - fraction_array[axis], fraction_array[axis]];
    }
    CicShape { indices, weights }
}

fn zero_scalar_grid(domain: Domain) -> Vec<f64> {
    vec![0.0; domain.resolution().cell_count() as usize]
}

fn zero_vector_grid(domain: Domain) -> Vec<DVec3> {
    vec![DVec3::ZERO; domain.resolution().cell_count() as usize]
}

fn cell_volume(domain: Domain) -> f64 {
    let spacing = domain.cell_size();
    spacing.x * spacing.y * spacing.z
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn axpy(target: &mut [f64], scale: f64, value: &[f64]) {
    for (target, value) in target.iter_mut().zip(value) {
        *target += scale * value;
    }
}

#[cfg(test)]
mod tests {
    use fieldcad_core::quantities::{ChargeCoulombs, coulomb};
    use fieldcad_core::{
        BoundaryCondition, BoundaryConditions, ChargeDistribution, DomainBounds, Precision,
        Resolution,
    };

    use super::*;

    fn domain() -> Domain {
        Domain::new(
            DomainBounds::new(DVec3::ZERO, DVec3::splat(2.0)).unwrap(),
            Resolution::uniform(8).unwrap(),
            BoundaryConditions::uniform(BoundaryCondition::Periodic),
            Precision::F64,
        )
    }

    #[test]
    fn cic_charge_integrates_to_the_authored_charge() {
        let domain = domain();
        let mut rho = zero_scalar_grid(domain);
        deposit_cic_scalar(domain, DVec3::new(0.33, 0.71, 1.82), 4.0, &mut rho);
        let integrated = rho.iter().sum::<f64>() * cell_volume(domain);
        assert!((integrated - 4.0).abs() < 1.0e-13);
    }

    #[test]
    fn current_deposition_satisfies_continuity_inside_a_cell() {
        assert_move_preserves_continuity(
            DVec3::new(0.31, 0.52, 0.77),
            DVec3::new(0.37, 0.49, 0.81),
        );
    }

    #[test]
    fn current_deposition_satisfies_continuity_across_periodic_seams() {
        assert_move_preserves_continuity(
            DVec3::new(1.98, 0.03, 1.96),
            DVec3::new(2.04, -0.02, 2.01),
        );
    }

    fn assert_move_preserves_continuity(old: DVec3, new: DVec3) {
        let domain = domain();
        let seconds = 0.2;
        let charge = 1.7;
        let mut old_rho = zero_scalar_grid(domain);
        let mut new_rho = zero_scalar_grid(domain);
        deposit_cic_scalar(domain, old, charge, &mut old_rho);
        deposit_cic_scalar(domain, wrap_position(domain, new), charge, &mut new_rho);
        let mut current = zero_vector_grid(domain);
        deposit_charge_conserving_current(domain, charge, old, new, seconds, &mut current).unwrap();
        let residual = continuity_residual(domain, &old_rho, &new_rho, &current, seconds);
        let scale = old_rho
            .iter()
            .chain(&new_rho)
            .fold(0.0_f64, |scale, value| scale.max(value.abs()))
            / seconds;
        assert!(
            residual <= 5.0e-13 * scale,
            "residual {residual:e}, scale {scale:e}"
        );
    }

    /// PH-17 regression: the fields a particle sees must not depend on
    /// which periodic image of its own position happens to be stored —
    /// `interpolate_particle_fields` shares its stencil/wrap logic with the
    /// display sampling path in `lib.rs` now, but this exercises the public
    /// function's own observable behaviour directly.
    #[test]
    fn interpolate_particle_fields_agrees_for_a_position_and_its_periodic_wrap() {
        let domain = domain();
        let counts = domain.resolution().cell_count() as usize;
        // Non-trivial and non-symmetric, so the interpolated value actually
        // depends on which stencil gets chosen.
        let electric: Vec<DVec3> = (0..counts)
            .map(|index| DVec3::splat(index as f64 * 0.01))
            .collect();
        let magnetic: Vec<DVec3> = (0..counts)
            .map(|index| DVec3::splat(-(index as f64) * 0.02))
            .collect();
        let fields = YeeFieldState { electric, magnetic };

        let inside = DVec3::new(0.83, 1.41, 0.27);
        let outside = inside + domain.bounds().size();

        let (e_inside, b_inside) = interpolate_particle_fields(domain, &fields, inside).unwrap();
        let (e_outside, b_outside) = interpolate_particle_fields(domain, &fields, outside).unwrap();

        assert!((e_inside - e_outside).length() < 1.0e-12);
        assert!((b_inside - b_outside).length() < 1.0e-12);
    }

    #[test]
    fn periodic_poisson_initialization_satisfies_discrete_gauss_law() {
        let domain = domain();
        let source = ChargeSource::new(
            ObjectId::new(7),
            DVec3::new(0.43, 0.71, 1.2),
            Velocity::default(),
            ChargeCoulombs::new::<coulomb>(2.0e-12),
            ChargeDistribution::Point {
                exclusion_radius: 0.01,
            },
        );
        let state = periodic_charge_initial_state(domain, &[source]).unwrap();
        let rho = deposit_source_charge(domain, &[source]);
        let mean = rho.iter().sum::<f64>() / rho.len() as f64;
        let counts = domain.resolution().cells();
        let spacing = domain.cell_size();
        let mut maximum: f64 = 0.0;
        let mut scale: f64 = 0.0;
        for z in 0..counts.z {
            for y in 0..counts.y {
                for x in 0..counts.x {
                    let index = linear_index(counts, x, y, z);
                    let here = state.electric[index];
                    let xp = state.electric[linear_index(counts, wrap_previous(x, counts.x), y, z)];
                    let yp = state.electric[linear_index(counts, x, wrap_previous(y, counts.y), z)];
                    let zp = state.electric[linear_index(counts, x, y, wrap_previous(z, counts.z))];
                    let divergence = (here.x - xp.x) / spacing.x
                        + (here.y - yp.y) / spacing.y
                        + (here.z - zp.z) / spacing.z;
                    let expected = (rho[index] - mean) / VACUUM_PERMITTIVITY;
                    maximum = maximum.max((divergence - expected).abs());
                    scale = scale.max(expected.abs());
                }
            }
        }
        assert!(
            maximum < 1.0e-9 * scale,
            "Gauss residual {maximum:e}, scale {scale:e}"
        );
    }

    #[test]
    fn magnetic_boris_orbit_preserves_speed_and_turns_the_expected_angle() {
        let velocity = DVec3::new(1000.0, 0.0, 0.0);
        let charge = 2.0;
        let mass = 4.0;
        let magnetic = DVec3::Z;
        let dt = 1.0e-3;
        let actual =
            relativistic_boris_velocity(velocity, charge, mass, DVec3::ZERO, magnetic, dt).unwrap();
        let expected_angle = -2.0 * (charge * magnetic.z * dt / (2.0 * mass)).atan();
        assert!((actual.length() - velocity.length()).abs() < 1.0e-9);
        assert!((actual.y.atan2(actual.x) - expected_angle).abs() < 1.0e-9);
    }

    #[test]
    fn electric_acceleration_remains_subluminal() {
        let actual = relativistic_boris_velocity(
            DVec3::ZERO,
            1.0,
            1.0,
            DVec3::splat(1.0e30),
            DVec3::ZERO,
            1.0,
        )
        .unwrap();
        assert!(actual.length() < SPEED_OF_LIGHT);
    }
}
