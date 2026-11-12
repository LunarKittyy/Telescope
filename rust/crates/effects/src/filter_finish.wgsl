// The finish, drawn over the graded picture so the grade doesn't change it: glow, then the vignette, then film grain. Each is skipped
// (and costs nothing) while its amount is 0.
//
// Glow is the bright part of the picture blurred wide (a grid of 9 x 9 taps `gap` whole pixels apart, each with two samples moved by a different
// amount inside its square for every pixel, so the gaps between taps fill in as a soft noise instead of a copy of the highlight at every tap)
// and screened back on. The
// vignette darkens or lightens towards the corners. The grain is brightness only, bunched towards 0 like film, strongest in the
// mid-tones, one cell of it `cell` whole pixels wide, and different every frame; filters_cpu.rs::finish has the same hash.

struct Params {
    vignette: f32,
    inner: f32,
    round: f32,
    glow: f32,
    glow_radius: f32,
    grain: f32,
    grain_size: f32,
}

const GLOW_FROM: f32 = 0.55;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// Where inside its square a tap goes, -0.5 to 0.5 each way, the same for a pixel on every frame
fn jitter(at: vec2<u32>, n: u32) -> vec2<f32> {
    let h = pcg(at.x + pcg(at.y + pcg(n + 7919u)));
    return vec2<f32>(f32(h & 0xffffu), f32(h >> 16u)) / 65536.0 - 0.5;
}

fn bright(uv: vec2<f32>) -> vec3<f32> {
    let c = sample(uv).rgb;
    return c * smoothstep(GLOW_FROM, 1.0, luma(c));
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    var c = sample(uv).rgb;
    let scale = frame.size.y / 1080.0;
    let px = 1.0 / frame.size;
    if params.glow > 0.0 {
        let gap = max(1.0, floor(params.glow_radius * scale / 4.0 + 0.5));
        let here = vec2<u32>(uv * frame.size);
        var sum = vec3<f32>(0.0);
        var total = 0.0;
        for (var j = -4; j <= 4; j++) {
            for (var i = -4; i <= 4; i++) {
                let w = exp(-f32(i * i + j * j) / 8.0);
                let n = u32((j + 4) * 9 + i + 4);
                // two samples in each tap's square, so a small highlight is hit by more of them
                let a = vec2<f32>(f32(i), f32(j)) + vec2<f32>(-0.25) + jitter(here, n * 2u) * 0.5;
                let b = vec2<f32>(f32(i), f32(j)) + vec2<f32>(0.25) + jitter(here, n * 2u + 1u) * 0.5;
                sum += 0.5 * w * (bright(uv + a * gap * px) + bright(uv + b * gap * px));
                total += w;
            }
        }
        c = 1.0 - (1.0 - c) * (1.0 - min(sum / total * params.glow, vec3<f32>(1.0)));
    }
    if params.vignette != 0.0 {
        let aspect = frame.size.x / frame.size.y;
        let q = vec2<f32>((uv.x * 2.0 - 1.0) * (1.0 + (aspect - 1.0) * params.round), uv.y * 2.0 - 1.0);
        let corner = sqrt(2.0 + (aspect * aspect + 1.0 - 2.0) * params.round);
        let t = clamp((length(q) / corner - params.inner) / (1.0 - params.inner + 1e-4), 0.0, 1.0);
        let v = t * t * (3.0 - 2.0 * t);
        if params.vignette > 0.0 {
            c = c * (1.0 - params.vignette * v);
        } else {
            c = c + (1.0 - c) * (-params.vignette * v * 0.6);
        }
    }
    if params.grain > 0.0 {
        let cell = max(floor(params.grain_size * scale + 0.5), 1.0);
        let at = vec2<u32>(uv * frame.size / cell);
        let h = pcg(at.x + pcg(at.y + pcg(frame.number)));
        let a = f32(h) / 4294967296.0;
        let b = f32(pcg(h ^ 0x9E3779B9u)) / 4294967296.0;
        let y = clamp(luma(c), 0.0, 1.0);
        let tones = 0.25 + 0.75 * 4.0 * y * (1.0 - y);
        c = c + (a + b - 1.0) * params.grain * 0.12 * tones;
    }
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
