// One plane of an I420 picture fitted into the virtual camera's size: scaled into a box with OpenCV's fixed-point two-tap blend,
// the sums the CPU's core::image::resize_linear_into makes, and the bar's level everywhere outside the box.

@group(0) @binding(0) var plane: texture_2d<f32>;
// Each output column's and row's taps (first source, second source, their weights out of 2048): x on row fit.rows.x, y on fit.rows.y
@group(0) @binding(1) var taps: texture_2d<u32>;
struct Fit {
    // The box: x, y, width, height in the output plane
    box: vec4<u32>,
    // The bar's level, then the taps' rows for x and y
    rows: vec4<u32>,
}
@group(0) @binding(2) var<uniform> fit: Fit;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn byte(x: u32, y: u32) -> i32 {
    return i32(round(textureLoad(plane, vec2<u32>(x, y), 0).r * 255.0));
}

// A source row blended across, kept in 15 bits as the CPU keeps it
fn across(t: vec4<u32>, y: u32) -> i32 {
    return (byte(t.x, y) * i32(t.z) + byte(t.y, y) * i32(t.w)) >> 4u;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let at = vec2<u32>(pos.xy);
    if at.x < fit.box.x || at.y < fit.box.y || at.x >= fit.box.x + fit.box.z || at.y >= fit.box.y + fit.box.w {
        return vec4<f32>(f32(fit.rows.x) / 255.0, 0.0, 0.0, 1.0);
    }
    let tx = textureLoad(taps, vec2<u32>(at.x - fit.box.x, fit.rows.y), 0);
    let ty = textureLoad(taps, vec2<u32>(at.y - fit.box.y, fit.rows.z), 0);
    let top = across(tx, ty.x);
    let bottom = across(tx, ty.y);
    let level = (((top * i32(ty.z)) >> 16u) + ((bottom * i32(ty.w)) >> 16u) + 2) >> 2u;
    return vec4<f32>(f32(level) / 255.0, 0.0, 0.0, 1.0);
}
