// NOTE: The `shader_filter_common.wgsl` source is prepended to this before compilation.

// The first pass of a blur made smaller: each tap averages a box of bilinear
// samples, each sample two texels.
struct Filter {
    step: vec2<f32>,
    texel: vec2<f32>,
    samples: vec2<f32>,
    m: f32,
    alpha: f32,
    full_size: f32,
    uv_bounds: vec4<f32>,
}

@group(0) @binding(0) var texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;
@group(0) @binding(2) var<uniform> filter_args: Filter;

@vertex
fn main_vertex(in: filter__VertexInput) -> filter__VertexOutput {
    return filter__main_vertex(in);
}

fn box_at(uv: vec2<f32>) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var y = 0.0; y < filter_args.samples.y; y += 1.0) {
        for (var x = 0.0; x < filter_args.samples.x; x += 1.0) {
            let at = uv + filter_args.texel * (vec2<f32>(x, y) * 2.0 - (filter_args.samples - 1.0));
            sum += textureSample(texture, texture_sampler, clamp(at, filter_args.uv_bounds.xy, filter_args.uv_bounds.zw));
        }
    }
    return sum / (filter_args.samples.x * filter_args.samples.y);
}

@fragment
fn main_fragment(in: filter__VertexOutput) -> @location(0) vec4<f32> {
    var total = vec4<f32>(0.0);
    for (var i = -filter_args.m; i <= filter_args.m; i += 1.0) {
        total += box_at(in.uv + filter_args.step * i);
    }
    let edge = filter_args.step * (filter_args.m + 1.0);
    total += (box_at(in.uv - edge) + box_at(in.uv + edge)) * filter_args.alpha;
    let result = total / filter_args.full_size;

    // This rounding imitates the fixed-point computations of FP, as `blur.wgsl` does.
    return floor(result * 255.0) / 255.0;
}
