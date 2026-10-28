// What the host gives every plugin shader (SHADERS.md). The plugin adds fn effect(uv: vec2<f32>) -> vec4<f32>.

struct FrameInfo {
    // Width and height in pixels
    size: vec2<f32>,
    // Seconds since the stream started
    time: f32,
    number: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> frame: FrameInfo;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// One triangle over the whole picture, uv (0, 0) at the top left
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(clamp(effect(in.uv).rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
