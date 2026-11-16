/// Common WGSL shared among many Ruffle shaders.
/// This is prepended to a lot of shaders at runtime before compiling them.
/// The `common__` identifier prefix serves as pseudo-namespacing.

/// Global uniforms that are constant throughout a frame.
struct common__Globals {
    // The view matrix determined by the viewport and stage.
    global_matrix: mat4x4<f32>,
};

/// Transform uniforms that are changed per object.
struct common__Transforms {
    /// The world matrix that transforms this object into stage space.
    world_matrix: mat4x4<f32>,

    /// The multiplicative color transform of this object.
    mult_color: vec4<f32>,

    /// The additive color transform of this object.
    add_color: vec4<f32>,
};

/// The vertex format shared among most shaders.
struct common__VertexInput {
    /// The position of the vertex in object space.
    @location(0) position: vec2<f32>,
};

struct common__VertexInputUv {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) stroke: u32,
};

/// Common uniform layout shared by all shaders.
@group(0) @binding(0) var<uniform> common__globals: common__Globals;

/// Places a vertex in the target, moving stroke vertices out along their
/// normal so that strokes are at least a pixel wide.
fn common__place_vertex(position: vec2<f32>, stroke: u32, world_matrix: mat4x4<f32>) -> vec4<f32> {
    let normal = vec2<f32>(f32(bitcast<i32>(stroke << 24u) >> 24u), f32(bitcast<i32>(stroke << 16u) >> 24u)) / 32.0;
    let half_width = unpack2x16float(stroke).y;
    let pixels = length((world_matrix * vec4<f32>(normal, 0.0, 0.0)).xy);
    let min_half_width = 0.5 * length(normal) / max(pixels, 1e-6);
    let widened = position + normal * max(0.0, min_half_width - half_width);
    return common__globals.global_matrix * (world_matrix * vec4<f32>(widened, 0.0, 1.0));
}

/// Converts a color from linear to sRGB color space.
fn common__linear_to_srgb(linear_: vec4<f32>) -> vec4<f32> {
    var rgb: vec3<f32> = linear_.rgb;
    if (linear_.a > 0.0) {
        rgb = rgb / linear_.a;
    }
    let a = 12.92 * rgb;
    let b = 1.055 * pow(rgb, vec3<f32>(1.0 / 2.4)) - 0.055;
    let c = step(vec3<f32>(0.0031308), rgb);
    return vec4<f32>(mix(a, b, c) * linear_.a, linear_.a);
}

/// Converts a color from sRGB to linear color space.
fn common__srgb_to_linear(srgb: vec4<f32>) -> vec4<f32> {
    var rgb: vec3<f32> = srgb.rgb;
    if (srgb.a > 0.0) {
        rgb = rgb / srgb.a;
    }
    let a = rgb / 12.92;
    let b = pow((rgb + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let c = step(vec3<f32>(0.04045), rgb);
    return vec4<f32>(mix(a, b, c) * srgb.a, srgb.a);
}
