// Appended to a shape shader whose `main_fragment` was renamed to
// `shape_color`, to draw a single shape with a complex blend mode directly
// instead of through a layer, followed by an entry point that reads what's
// below: `complex_copy.wgsl` or `complex_fetch.wgsl`. The composite matches
// blend/*.wgsl.

// x: the blend mode, numbered as in `DirectBlend::complex_direct_mode`.
@group(3) @binding(1) var<uniform> blend: vec4<u32>;

fn blend_func(src: vec3<f32>, dst: vec3<f32>) -> vec3<f32> {
    switch blend.x {
        case 0u: { return src * dst; }                // multiply
        case 1u: { return max(src, dst); }            // lighten
        case 2u: { return min(src, dst); }            // darken
        case 3u: { return abs(dst - src); }           // difference
        case 4u: {                                    // overlay
            return select(1.0 - 2.0 * (1.0 - dst) * (1.0 - src), 2.0 * src * dst, dst <= vec3<f32>(0.5));
        }
        default: {                                    // hardlight
            return select(1.0 - 2.0 * (1.0 - dst) * (1.0 - src), 2.0 * src * dst, src <= vec3<f32>(0.5));
        }
    }
}

fn composite(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    if (dst.a <= 0.0) {
        return src;
    }
    return vec4<f32>(
        src.rgb * (1.0 - dst.a) + dst.rgb * (1.0 - src.a) + src.a * dst.a * blend_func(src.rgb / src.a, dst.rgb / dst.a),
        src.a + dst.a * (1.0 - src.a),
    );
}
