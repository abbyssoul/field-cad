struct Params {
    counts: vec4<u32>,
    spacing_dt: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> electric_in: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> magnetic_in: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> field_out: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> current_density: array<vec4<f32>>;

const LIGHT_SPEED_SQUARED: f32 = 8.9875518e16;
const INVERSE_VACUUM_PERMITTIVITY: f32 = 1.1294091e11;

fn index_of(cell: vec3<u32>) -> u32 {
    return cell.x + params.counts.x * (cell.y + params.counts.y * cell.z);
}

fn cell_of(index: u32) -> vec3<u32> {
    let xy = params.counts.x * params.counts.y;
    let z = index / xy;
    let remainder = index - z * xy;
    let y = remainder / params.counts.x;
    return vec3<u32>(remainder - y * params.counts.x, y, z);
}

fn next_cell(cell: vec3<u32>, axis: u32) -> vec3<u32> {
    var result = cell;
    if axis == 0u {
        result.x = (cell.x + 1u) % params.counts.x;
    } else if axis == 1u {
        result.y = (cell.y + 1u) % params.counts.y;
    } else {
        result.z = (cell.z + 1u) % params.counts.z;
    }
    return result;
}

fn previous_cell(cell: vec3<u32>, axis: u32) -> vec3<u32> {
    var result = cell;
    if axis == 0u {
        result.x = (cell.x + params.counts.x - 1u) % params.counts.x;
    } else if axis == 1u {
        result.y = (cell.y + params.counts.y - 1u) % params.counts.y;
    } else {
        result.z = (cell.z + params.counts.z - 1u) % params.counts.z;
    }
    return result;
}

fn electric_at(cell: vec3<u32>) -> vec3<f32> {
    return electric_in[index_of(cell)].xyz;
}

fn magnetic_at(cell: vec3<u32>) -> vec3<f32> {
    return magnetic_in[index_of(cell)].xyz;
}

fn curl_e_forward(cell: vec3<u32>) -> vec3<f32> {
    let here = electric_at(cell);
    let x_next = electric_at(next_cell(cell, 0u));
    let y_next = electric_at(next_cell(cell, 1u));
    let z_next = electric_at(next_cell(cell, 2u));
    return vec3<f32>(
        (y_next.z - here.z) / params.spacing_dt.y -
            (z_next.y - here.y) / params.spacing_dt.z,
        (z_next.x - here.x) / params.spacing_dt.z -
            (x_next.z - here.z) / params.spacing_dt.x,
        (x_next.y - here.y) / params.spacing_dt.x -
            (y_next.x - here.x) / params.spacing_dt.y,
    );
}

fn curl_b_backward(cell: vec3<u32>) -> vec3<f32> {
    let here = magnetic_at(cell);
    let x_previous = magnetic_at(previous_cell(cell, 0u));
    let y_previous = magnetic_at(previous_cell(cell, 1u));
    let z_previous = magnetic_at(previous_cell(cell, 2u));
    return vec3<f32>(
        (here.z - y_previous.z) / params.spacing_dt.y -
            (here.y - z_previous.y) / params.spacing_dt.z,
        (here.x - z_previous.x) / params.spacing_dt.z -
            (here.z - x_previous.z) / params.spacing_dt.x,
        (here.y - x_previous.y) / params.spacing_dt.x -
            (here.x - y_previous.x) / params.spacing_dt.y,
    );
}

@compute @workgroup_size(64)
fn advance_magnetic(@builtin(global_invocation_id) global: vec3<u32>) {
    let index = global.x;
    let count = params.counts.x * params.counts.y * params.counts.z;
    if index >= count {
        return;
    }
    let cell = cell_of(index);
    field_out[index] = vec4<f32>(
        magnetic_at(cell) - params.spacing_dt.w * curl_e_forward(cell),
        0.0,
    );
}

@compute @workgroup_size(64)
fn advance_electric(@builtin(global_invocation_id) global: vec3<u32>) {
    let index = global.x;
    let count = params.counts.x * params.counts.y * params.counts.z;
    if index >= count {
        return;
    }
    let cell = cell_of(index);
    field_out[index] = vec4<f32>(
        electric_at(cell) +
            LIGHT_SPEED_SQUARED * params.spacing_dt.w * curl_b_backward(cell) -
            INVERSE_VACUUM_PERMITTIVITY * params.spacing_dt.w * current_density[index].xyz,
        0.0,
    );
}
