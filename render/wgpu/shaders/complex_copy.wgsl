// What's below comes from a copy of the target, made right before the draw.

@group(3) @binding(0) var parent_texture: texture_2d<f32>;

@fragment
fn main_fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = shape_color(in);
    if (src.a <= 0.0) {
        discard;
    }
    return composite(src, textureLoad(parent_texture, vec2<i32>(in.position.xy), 0));
}
