struct Camera {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

// Per-instance object transform, supplied as four rows because WGSL vertex
// attributes are limited to four components each.
struct InstanceInput {
    @location(2) model_0: vec4<f32>,
    @location(3) model_1: vec4<f32>,
    @location(4) model_2: vec4<f32>,
    @location(5) model_3: vec4<f32>,
    @location(6) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// Unlit geometry drawn directly in world space: the reference grid and axes.
@vertex
fn vs_world(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    return output;
}

// Instanced object proxies. The mesh is a unit cube; per-object scale, rotation,
// and translation arrive in the instance transform.
@vertex
fn vs_instanced(input: VertexInput, instance: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(
        instance.model_0,
        instance.model_1,
        instance.model_2,
        instance.model_3,
    );
    let world_position = model * vec4<f32>(input.position, 1.0);

    var output: VertexOutput;
    output.clip_position = camera.view_projection * world_position;
    output.color = input.color * instance.tint;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
