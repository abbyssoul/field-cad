// Batched f32 pairwise inverse-square-law evaluator, shared by electrostatics
// (Coulomb's law) and Newtonian gravity — the same functional form with a
// different coupling constant and, for gravity, an opposite sign. The
// coupling constant is a uniform, not a shader constant, so one compiled
// kernel serves both. The CPU f64 `fieldcad-superposition` kernel is the
// correctness oracle for this shader.

const VALID_EXACT: u32 = 0u;
const INVALID_INSIDE_SOURCE: u32 = 1u;
const INVALID_OUTSIDE_DOMAIN: u32 = 2u;
const INVALID_OVERFLOW: u32 = 3u;

struct Params {
    // x = source count, y = sample count
    counts: vec4<u32>,
    // x = coupling constant (Coulomb's constant, or -G for gravity)
    coupling: vec4<f32>,
};

struct Source {
    // xyz = world position, w = coupling value (charge or mass)
    position_value: vec4<f32>,
    // x = 0 point / 1 uniform sphere, y = radius
    distribution_radius: vec4<f32>,
};

struct SamplePosition {
    position: vec4<f32>,
};

struct SampleOutput {
    // xyz = field (electric field or gravitational acceleration), w = potential
    field_potential: vec4<f32>,
    // x = validity code; remaining lanes preserve a 32-byte stride
    validity: vec4<u32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> sources: array<Source>;
@group(0) @binding(2) var<storage, read> positions: array<SamplePosition>;
@group(0) @binding(3) var<storage, read_write> outputs: array<SampleOutput>;

fn write_undefined(index: u32, reason: u32) {
    outputs[index].field_potential = vec4<f32>(0.0);
    outputs[index].validity = vec4<u32>(reason, 0u, 0u, 0u);
}

@compute @workgroup_size(64)
fn evaluate(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if index >= params.counts.y {
        return;
    }

    let coupling_constant = params.coupling.x;
    let position = positions[index].position.xyz;
    var field = vec3<f32>(0.0);
    var potential = 0.0;
    for (var source_index = 0u; source_index < params.counts.x; source_index += 1u) {
        let source = sources[source_index];
        let value = source.position_value.w;
        if value == 0.0 {
            continue;
        }

        let displacement = position - source.position_value.xyz;
        let distance_squared = dot(displacement, displacement);
        let distance = sqrt(distance_squared);
        let radius = source.distribution_radius.y;
        var field_contribution = vec3<f32>(0.0);
        var potential_contribution = 0.0;

        if source.distribution_radius.x < 0.5 {
            if distance <= radius {
                write_undefined(index, INVALID_INSIDE_SOURCE);
                return;
            }
            let inverse_distance = 1.0 / distance;
            let inverse_distance_cubed =
                inverse_distance * inverse_distance * inverse_distance;
            field_contribution = coupling_constant * value * displacement
                * inverse_distance_cubed;
            potential_contribution = coupling_constant * value * inverse_distance;
        } else if distance < radius {
            let radius_squared = radius * radius;
            let radius_cubed = radius_squared * radius;
            field_contribution = coupling_constant * value * displacement / radius_cubed;
            potential_contribution = coupling_constant * value / (2.0 * radius)
                * (3.0 - distance_squared / radius_squared);
        } else {
            let inverse_distance = 1.0 / distance;
            let inverse_distance_cubed =
                inverse_distance * inverse_distance * inverse_distance;
            field_contribution = coupling_constant * value * displacement
                * inverse_distance_cubed;
            potential_contribution = coupling_constant * value * inverse_distance;
        }

        field += field_contribution;
        potential += potential_contribution;
        // WGSL has no isFinite/isNan built-ins. NaN is the only float unequal
        // to itself; infinities exceed the largest finite f32 magnitude.
        let field_is_nan = any(field != field);
        let field_is_infinite = any(abs(field) > vec3<f32>(3.402823e38));
        let potential_is_nan = potential != potential;
        let potential_is_infinite = abs(potential) > 3.402823e38;
        if field_is_nan || field_is_infinite
            || potential_is_nan || potential_is_infinite {
            write_undefined(index, INVALID_OVERFLOW);
            return;
        }
    }

    outputs[index].field_potential = vec4<f32>(field, potential);
    outputs[index].validity = vec4<u32>(VALID_EXACT, 0u, 0u, 0u);
}
