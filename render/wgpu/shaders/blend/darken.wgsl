// NOTE: The `common.wgsl` source is prepended to this before compilation.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) parent_uv: vec2<f32>,
};

@group(2) @binding(0) var parent_texture: texture_2d<f32>;
@group(2) @binding(1) var current_texture: texture_2d<f32>;
@group(2) @binding(2) var texture_sampler: sampler;
@group(2) @binding(3) var<uniform> region: vec4<f32>;

@vertex
fn main_vertex(in: common__VertexInput) -> VertexOutput {
    let parent_uv = region.xy + in.position.xy * region.zw;
    let pos = vec4<f32>((parent_uv.x * 2.0) - 1.0, -(parent_uv.y * 2.0) + 1.0, 0.0, 1.0);
    return VertexOutput(pos, in.position.xy, parent_uv);
}

fn blend_func(src: vec3<f32>, dst: vec3<f32>) -> vec3<f32> {
    return min(src, dst);
}

@fragment
fn main_fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // dst is the parent pixel we're blending onto
    var dst: vec4<f32> = textureSample(parent_texture, texture_sampler, in.parent_uv);
    // src is the pixel that we want to apply
    var src: vec4<f32> = textureSample(current_texture, texture_sampler, in.uv);

    if (src.a > 0.0) {
        if (dst.a > 0.0) {
            return vec4<f32>(src.rgb * (1.0 - dst.a) + dst.rgb * (1.0 - src.a) + src.a * dst.a * blend_func(src.rgb / src.a, dst.rgb / dst.a), src.a + dst.a * (1.0 - src.a));
        } else {
            return src;
        }
    } else {
        if (true) {
            // This needs to be in a branch because... reasons. Bug in naga.
            // https://github.com/gfx-rs/naga/issues/2168
            discard;
        }
        return dst;
    }
}
