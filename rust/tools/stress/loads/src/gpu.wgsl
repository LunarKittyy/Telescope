// A full-screen pass that does a heavy game's fragment work: many texture reads across a big array, so the VRAM stays in use.

struct Params { time: f32, steps: u32, layers: u32, pad: u32 }

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var noise: texture_2d_array<f32>;
@group(0) @binding(2) var linear: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var uv = pos.xy / 512.0 + vec2<f32>(params.time * 0.11, params.time * 0.07);
    var sum = vec4<f32>(0.0);
    for (var i = 0u; i < params.steps; i++) {
        let layer = (i * 7u + u32(params.time * 60.0)) % params.layers;
        let s = textureSampleLevel(noise, linear, fract(uv), layer, 0.0);
        sum += s * s + vec4<f32>(sin(s.r * 6.28 + uv.x), cos(s.g * 6.28 + uv.y), 0.0, 0.0);
        uv = uv * 1.07 + s.rg * 0.1;
    }
    return sum / f32(params.steps);
}
