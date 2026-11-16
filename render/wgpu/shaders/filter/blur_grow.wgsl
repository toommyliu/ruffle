// NOTE: The `shader_filter_common.wgsl` source is prepended to this before compilation.

// The last pass of a blur made smaller, which scales it back up bilinearly.
// The blurs of the two nearest texels share all but their outermost taps, so
// their mix takes as many samples as one blur.
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

fn sample_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(texture, texture_sampler, clamp(uv, filter_args.uv_bounds.xy, filter_args.uv_bounds.zw));
}

@fragment
fn main_fragment(in: filter__VertexOutput) -> @location(0) vec4<f32> {
    let step = filter_args.step;
    let m = filter_args.m;
    let alpha = filter_args.alpha;
    let t = fract(dot(in.uv, step) / dot(step, step) - 0.5);
    let below = in.uv - step * t;

    var center = vec4<f32>(0.0);
    for (var i = 1.5 - m; i < m; i += 2.0) {
        center += sample_at(below + step * i);
    }
    let low = 1.0 - t + alpha;
    let high = alpha + t;
    let left = sample_at(below + step * (-m - 1.0 + (1.0 - t + t * alpha) / low)) * low;
    let right = sample_at(below + step * (m + 1.0 + t * alpha / high)) * high;
    let result = (center * 2.0 + left + right) / filter_args.full_size;

    // This rounding imitates the fixed-point computations of FP, as `blur.wgsl` does.
    return floor(result * 255.0) / 255.0;
}
