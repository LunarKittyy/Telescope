// Sharpen: the brightness detail (what's left of a pixel once a blur of it is taken away) added back, held in three ways. Detail
// smaller than the threshold (`gate`) is left alone so skin and walls stay as they are, and nothing ends up brighter than its brightest
// neighbour or darker than its darkest by more than `HALO`, so a strong edge gets steeper without a white outline. The blur is a grid of
// 7 x 7 taps, as wide as the radius says (filters_cpu.rs::sharpen does the same with a true blur). The change is added to all three
// colours so the hue stays.

struct Params {
    amount: f32,
    sigma: f32,
    gate: f32,
}

const HALO: f32 = 0.03;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    let y = luma(c);
    let px = 1.0 / frame.size;
    let sigma = max(params.sigma * frame.size.y / 1080.0, 0.4);
    let spacing = max(sigma * 0.8, 0.5);
    let k = spacing * spacing / (2.0 * sigma * sigma);
    var sum = 0.0;
    var total = 0.0;
    for (var j = -3; j <= 3; j++) {
        for (var i = -3; i <= 3; i++) {
            let w = exp(-f32(i * i + j * j) * k);
            sum += w * luma(sample(uv + vec2<f32>(f32(i), f32(j)) * spacing * px).rgb);
            total += w;
        }
    }
    var lo = y;
    var hi = y;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let t = luma(sample(uv + vec2<f32>(f32(i), f32(j)) * px).rgb);
            lo = min(lo, t);
            hi = max(hi, t);
        }
    }
    let d = y - sum / total;
    var gate = 1.0;
    if params.gate > 0.0 {
        gate = smoothstep(params.gate, 2.0 * params.gate, abs(d));
    }
    let to = clamp(y + params.amount * d * gate, lo - HALO, hi + HALO);
    return vec4<f32>(clamp(c + (to - y), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
