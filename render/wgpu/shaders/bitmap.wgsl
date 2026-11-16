/// Shader used for drawing bitmap fills.
/// NOTE: The `common.wgsl` source is prepended to this before compilation.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) mult_color: vec4<f32>,
    @location(2) add_color: vec4<f32>,
};

@group(1) @binding(0) var<uniform> transforms: array<common__Transforms, max_transforms>;
@group(2) @binding(0) var texture: texture_2d<f32>;
@group(2) @binding(1) var texture_sampler: sampler;
override late_saturate: bool = false;

@vertex
fn main_vertex(in: common__VertexInputUv, @builtin(instance_index) instanceIndex: u32) -> VertexOutput {
    let pos = common__place_vertex(in.position, in.stroke, transforms[instanceIndex].world_matrix);
    return VertexOutput(pos, in.uv, transforms[instanceIndex].mult_color, transforms[instanceIndex].add_color);
}

@fragment
fn main_fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var color: vec4<f32> = textureSample(texture, texture_sampler, in.uv);
    // Texture is premultiplied by alpha.
    // Unmultiply alpha, apply color transform, remultiply alpha.
    if (color.a > 0.0) {
        color = vec4<f32>(color.rgb / color.a, color.a);
        color = color * in.mult_color + in.add_color;
        if (!late_saturate) {
            color = saturate(color);
        }
        color = vec4<f32>(color.rgb * color.a, color.a);
        if (late_saturate) {
            color = saturate(color);
        }
    }
    return color;
}
