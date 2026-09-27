// NOTE: The `shader_filter_common.wgsl` source is prepended to this before compilation.

// Both passes of `blur.wgsl` (horizontal, then vertical) in one, for a
// single quality pass with a small kernel: every vertical tap recomputes the
// horizontal blur of its row, rounded as it would be when stored between
// passes. The one difference is the fractional last tap of the vertical pass,
// which blends two rows here instead of in the texture sampler.

// One direction's kernel, as `struct Filter` in `blur.wgsl` describes it.
struct Axis {
    full_size: f32,
    m2: f32,
    first_weight: f32,
    last_offset: f32,
    last_weight: f32,
    step: f32,
    _padding: vec2<f32>,
}

struct Filter {
    x: Axis,
    y: Axis,
}

@group(0) @binding(0) var texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

@group(0) @binding(2) var<uniform> filter_args: Filter;

@vertex
fn main_vertex(in: filter__VertexInput) -> filter__VertexOutput {
    return filter__main_vertex(in);
}

fn sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(texture, texture_sampler, uv, 0.0);
}

fn blur_row(uv: vec2<f32>) -> vec4<f32> {
    let a = filter_args.x;
    let step = vec2<f32>(a.step, 0.0);
    let origin = uv - step * (a.m2 * 0.5);
    var total = sample(origin - step) * a.first_weight;
    var center = vec4<f32>(0.0);
    for (var i = 0.5; i < a.m2; i += 2.0) {
        center += sample(origin + step * i);
    }
    total += center * 2.0;
    total += sample(origin + step * (a.m2 + a.last_offset)) * a.last_weight;
    return floor(total / a.full_size * 255.0) / 255.0;
}

@fragment
fn main_fragment(in: filter__VertexOutput) -> @location(0) vec4<f32> {
    let b = filter_args.y;
    let step = vec2<f32>(0.0, b.step);
    let origin = in.uv - step * (b.m2 * 0.5);
    var total = blur_row(origin - step) * b.first_weight;
    for (var k = 0.0; k < b.m2; k += 1.0) {
        total += blur_row(origin + step * k);
    }
    let last = mix(blur_row(origin + step * b.m2), blur_row(origin + step * (b.m2 + 1.0)), b.last_offset);
    total += last * b.last_weight;
    return floor(total / b.full_size * 255.0) / 255.0;
}
