// Skin smoothing: inside the person mask, wherever the colour is skin, a blend towards an edge-aware blur (7 x 7 taps), left out wherever
// the pixel differs from its blur by more than a pore would, which is an eye, a brow, a lip or the edge of the hair. Without a mask (no
// vision pack, or it can't say) nothing changes.

struct Params {
    strength: f32,
}

const RANGE: f32 = 0.09;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

// How much a colour looks like skin: the Cb and Cr that skin of every tone sits inside, with soft edges
fn skin_tone(c: vec3<f32>) -> f32 {
    let cb = 128.0 + 255.0 * (-0.168736 * c.r - 0.331264 * c.g + 0.5 * c.b);
    let cr = 128.0 + 255.0 * (0.5 * c.r - 0.418688 * c.g - 0.081312 * c.b);
    return smoothstep(69.0, 77.0, cb) * (1.0 - smoothstep(127.0, 135.0, cb)) * smoothstep(125.0, 133.0, cr) * (1.0 - smoothstep(173.0, 181.0, cr));
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    if !has_person_mask() {
        return vec4<f32>(c, 1.0);
    }
    let m = person_mask(uv);
    if m < 0.02 {
        return vec4<f32>(c, 1.0);
    }
    let weight = params.strength * m * skin_tone(c);
    if weight < 0.004 {
        return vec4<f32>(c, 1.0);
    }
    let px = 1.0 / frame.size;
    let gap = max(1.0, floor(2.0 * frame.size.y / 1080.0 + 0.5));
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var j = -3; j <= 3; j++) {
        for (var i = -3; i <= 3; i++) {
            let q = sample(uv + vec2<f32>(f32(i), f32(j)) * gap * px).rgb;
            let d = q - c;
            let w = exp(-f32(i * i + j * j) / 8.0) * exp(-dot(d, d) / (2.0 * RANGE * RANGE));
            sum += w * q;
            total += w;
        }
    }
    let blurred = sum / total;
    let keep = smoothstep(0.05, 0.14, abs(luma(c) - luma(blurred)));
    let by = weight * (1.0 - keep);
    return vec4<f32>(clamp(c + (blurred - c) * by, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
