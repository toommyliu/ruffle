// Appended to a shape shader whose `main_fragment` was renamed to
// `shape_color`, to draw it with Flash's multiply blend in one pass instead of
// through a layer (see blend/multiply.wgsl). With premultiplied colors:
//
//   out.rgb = src.rgb * (1 - dst.a) + dst.rgb * (src.rgb + 1 - src.a)
//   out.a   = src.a   * (1 - dst.a) + dst.a
//
// The blend state supplies (1 - dst.a) and takes dst's factor from `factor`
// through dual-source blending.

struct MultiplyOutput {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) factor: vec4<f32>,
};

@fragment
fn main_fragment(in: VertexOutput) -> MultiplyOutput {
    let src = shape_color(in);
    return MultiplyOutput(src, vec4<f32>(src.rgb + (1.0 - src.a), 1.0));
}
