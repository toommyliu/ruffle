/// Shader used for drawing solid color fills.

// NOTE: The `common.wgsl` source is prepended to this before compilation.

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) stroke: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@group(1) @binding(0) var<uniform> transforms: array<common__Transforms, max_transforms>;

@vertex
fn main_vertex(in: VertexInput, @builtin(instance_index) instanceIndex: u32) -> VertexOutput {
    let pos = common__place_vertex(in.position, in.stroke, transforms[instanceIndex].world_matrix);
    let color = saturate(in.color * transforms[instanceIndex].mult_color + transforms[instanceIndex].add_color);
    return VertexOutput(pos, vec4<f32>(color.rgb * color.a, color.a));
}

@fragment
fn main_fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
