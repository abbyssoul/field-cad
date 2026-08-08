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

// Per-frame state the flow-line ribbon pass needs beyond the shared camera.
struct FlowLineUniform {
    time_seconds: f32,
    _padding: f32,
    viewport_size: vec2<f32>,
};

@group(1) @binding(0)
var<uniform> flow: FlowLineUniform;

struct FlowLineVertexInput {
    @location(0) position: vec3<f32>,
    // The segment's other endpoint. Together with `side`, this is what lets
    // the vertex shader expand a zero-width traced line into a ribbon of
    // constant screen-space width, without a geometry shader or precomputed
    // billboard geometry that would need rebuilding whenever the camera moves.
    @location(1) neighbor: vec3<f32>,
    // Which edge of the ribbon this vertex is, and which direction to expand
    // it. Baked in at authoring time so this shader needs no notion of
    // "which end of the segment am I" (see `build_flow_ribbon` in
    // `scene/flow_lines.rs`): the sign is already chosen so that the two ends
    // of a segment expand into a consistent, non-twisted quad.
    @location(2) side: f32,
    // Cumulative world-space distance from the streamline's seed, converted
    // to render-space units. Drives the animated scroll.
    @location(3) arclength: f32,
    @location(4) thickness_px: f32,
    // Zero for a static line; nonzero enables the scrolling brightness wave
    // in `fs_flow_line`. Baked in per-vertex (not read from `flow` alone) so
    // streamlines from differently configured layers share one draw call.
    @location(5) speed: f32,
    @location(6) color: vec4<f32>,
};

struct FlowLineVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) arclength: f32,
    @location(2) speed: f32,
};

@vertex
fn vs_flow_line(input: FlowLineVertexInput) -> FlowLineVertexOutput {
    var clip = camera.view_projection * vec4<f32>(input.position, 1.0);
    let clip_neighbor = camera.view_projection * vec4<f32>(input.neighbor, 1.0);

    // Work in a pixel-space direction (NDC scaled by half the viewport, in
    // pixels per axis) rather than raw NDC, so the perpendicular offset below
    // has equal length in actual screen pixels regardless of the viewport's
    // aspect ratio.
    let half_viewport = flow.viewport_size * 0.5;
    let ndc = clip.xy / clip.w;
    let ndc_neighbor = clip_neighbor.xy / clip_neighbor.w;
    var direction_px = (ndc_neighbor - ndc) * half_viewport;
    if length(direction_px) < 1.0e-5 {
        // Degenerate (coincident or behind-camera) segment: any direction
        // keeps the vertex shader well-defined, and a zero-length segment is
        // invisible anyway.
        direction_px = vec2<f32>(1.0, 0.0);
    }
    let tangent_px = normalize(direction_px);
    let perp_px = vec2<f32>(-tangent_px.y, tangent_px.x);
    let offset_px = perp_px * (input.side * input.thickness_px * 0.5);
    let offset_ndc = offset_px / half_viewport;

    // Offset the already-projected clip position rather than the world
    // position: shifting post-projection is what keeps the ribbon's width
    // constant in screen pixels at any distance from the camera.
    clip.x += offset_ndc.x * clip.w;
    clip.y += offset_ndc.y * clip.w;

    var output: FlowLineVertexOutput;
    output.clip_position = clip;
    output.color = input.color;
    output.arclength = input.arclength;
    output.speed = input.speed;
    return output;
}

@fragment
fn fs_flow_line(input: FlowLineVertexOutput) -> @location(0) vec4<f32> {
    // `speed == 0` (the static "continuous line" mode) takes the `select`'s
    // false branch unconditionally, so the ribbon reads as a solid line with
    // no dash pattern baked in. Only a nonzero speed (the animated mode)
    // brings in the traveling brightness wave, scrolling via `-time*speed`.
    let frequency = 2.0;
    let phase = input.arclength * frequency - flow.time_seconds * input.speed;
    let pulse = 0.5 + 0.5 * cos(phase * 6.283185307179586);
    let brightness = select(1.0, 0.4 + 0.6 * pulse, input.speed > 0.0);
    return vec4<f32>(input.color.rgb * brightness, input.color.a);
}
