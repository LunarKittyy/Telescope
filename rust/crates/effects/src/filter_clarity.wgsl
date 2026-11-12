// Clarity: contrast in the mid-sized detail. The base is an edge-aware blur of the brightness (a grid of 9 x 9 taps, each counting less
// the further away it is and the more its brightness differs, so an edge isn't blurred across and gets no glowing outline), and what's
// left over is pushed up (or down, which softens) most in the mid-tones. The taps are `gap` whole pixels apart, a quarter of the
// radius; filters_cpu.rs::clarity does it on a picture shrunk by that much.

struct Params {
    amount: f32,
}

const RANGE: f32 = 0.1;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    let y = luma(c);
    let px = 1.0 / frame.size;
    let gap = max(1.0, floor(24.0 * frame.size.y / 1080.0 / 4.0 + 0.5));
    var sum = 0.0;
    var total = 0.0;
    for (var j = -4; j <= 4; j++) {
        for (var i = -4; i <= 4; i++) {
            let q = luma(sample(uv + vec2<f32>(f32(i), f32(j)) * gap * px).rgb);
            let d = q - y;
            let w = exp(-f32(i * i + j * j) / 8.0) * exp(-d * d / (2.0 * RANGE * RANGE));
            sum += w * q;
            total += w;
        }
    }
    let tones = 0.3 + 0.7 * 4.0 * y * (1.0 - y);
    let change = clamp(params.amount * (y - sum / total) * tones, -1.0, 1.0);
    return vec4<f32>(clamp(c + change, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
