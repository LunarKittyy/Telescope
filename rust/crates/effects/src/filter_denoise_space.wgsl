// Denoise in space: each pixel becomes the average of itself and 12 around it (the ring at 1 pixel and the four straight out at 2), each counting less the
// further away it is and the more its colour differs, so noise is smoothed and an edge is kept. A bigger `range` forgives more
// difference. filters_cpu.rs::denoise_space takes the same taps.

struct Params {
    range: f32,
}

// The colour (times its weight) and the weight of the pixel at an offset from here
fn tap(uv: vec2<f32>, c: vec3<f32>, dx: f32, dy: f32, px: vec2<f32>) -> vec4<f32> {
    let q = sample(uv + vec2<f32>(dx, dy) * px).rgb;
    let d = q - c;
    let w = exp(-(dx * dx + dy * dy) / 4.5) * exp(-dot(d, d) / (2.0 * params.range * params.range));
    return vec4<f32>(q * w, w);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    let px = 1.0 / frame.size;
    var sum = vec4<f32>(c, 1.0);
    sum += tap(uv, c, 1.0, 0.0, px);
    sum += tap(uv, c, -1.0, 0.0, px);
    sum += tap(uv, c, 0.0, 1.0, px);
    sum += tap(uv, c, 0.0, -1.0, px);
    sum += tap(uv, c, 1.0, 1.0, px);
    sum += tap(uv, c, -1.0, -1.0, px);
    sum += tap(uv, c, 1.0, -1.0, px);
    sum += tap(uv, c, -1.0, 1.0, px);
    sum += tap(uv, c, 2.0, 0.0, px);
    sum += tap(uv, c, -2.0, 0.0, px);
    sum += tap(uv, c, 0.0, 2.0, px);
    sum += tap(uv, c, 0.0, -2.0, px);
    return vec4<f32>(sum.rgb / sum.w, 1.0);
}
