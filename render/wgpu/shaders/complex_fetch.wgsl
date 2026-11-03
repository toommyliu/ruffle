// What's below is read from the target itself (`Features::FRAMEBUFFER_FETCH`).

@fragment
fn main_fragment(in: VertexOutput, @color(0) dst: vec4<f32>) -> @location(0) vec4<f32> {
    let src = shape_color(in);
    if (src.a <= 0.0) {
        discard;
    }
    return composite(src, dst);
}
