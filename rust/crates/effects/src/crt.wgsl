// CRT (fun_effects.Crt): the picture bulged like a curved screen, with a soft glow, scanlines, a phosphor mask and dark corners.

// The glow follows the CPU: the picture averaged in blocks a third of a side, a 3 x 3 blur of the blocks, bent on that small grid,
// then widened smoothly. Per axis that comes out as 12 pixels with block weights 1-f, 1, 1, f (over 9), read as 6 linear taps.
struct Taps {
    pos: array<f32, 6>,
    wt: array<f32, 6>,
}

fn block_weight(b: u32, f: f32) -> f32 {
    if b == 0u { return 1.0 - f; }
    if b == 3u { return f; }
    return 1.0;
}

// m is the place on the small grid, bw how many pixels a block spans; positions are in pixels
fn axis_taps(m: f32, bw: f32) -> Taps {
    let k0 = floor(m);
    let f = m - k0;
    // whole pixels, so that each pair of taps lands between two of them
    let start = round((k0 - 1.0) * bw);
    var t: Taps;
    for (var i = 0u; i < 6u; i++) {
        let a = i * 2u;
        let wa = block_weight(a / 3u, f);
        let wb = block_weight((a + 1u) / 3u, f);
        let sum = wa + wb;
        t.wt[i] = sum / 9.0;
        t.pos[i] = start + f32(a) + 0.5 + select(0.0, wb / sum, sum > 0.0);
    }
    return t;
}

// Where the screen's place p (-1 to 1) looks on a picture of n pixels, from the bulge
fn small_axis(i: f32, n: f32) -> f32 {
    return -1.0 + i * 2.0 / max(n - 1.0, 1.0);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let size = frame.size;
    let pix = floor(uv * size);
    // The pixel's place on a screen that runs from -1 to 1
    let p = -1.0 + pix * 2.0 / max(size - vec2<f32>(1.0), vec2<f32>(1.0));
    let r2 = dot(p, p);
    let q = p * (1.0 + 0.06 * r2);
    // beyond the bulge the screen is black
    if abs(q.x) > 1.0 || abs(q.y) > 1.0 { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
    let at = (q + 1.0) * 0.5 * (size - vec2<f32>(1.0)) + vec2<f32>(0.5);
    let bent = sample(at / size).rgb;
    // The glow: where this pixel sits on the grid of blocks, bent there as on the CPU, then the blurred blocks around it
    let small = max(floor(size / 3.0), vec2<f32>(1.0));
    let u = clamp((pix + 0.5) * small / size - 0.5, vec2<f32>(0.0), small - 1.0);
    let sp = vec2<f32>(small_axis(u.x, small.x), small_axis(u.y, small.y));
    let sb = 1.0 + 0.06 * dot(sp, sp);
    let m = clamp((sp * sb + 1.0) * 0.5 * (small - 1.0), vec2<f32>(0.0), small - 1.0);
    let tx = axis_taps(m.x, size.x / small.x);
    let ty = axis_taps(m.y, size.y / small.y);
    var glow = vec3<f32>(0.0);
    for (var j = 0u; j < 6u; j++) {
        for (var i = 0u; i < 6u; i++) {
            glow += sample(vec2<f32>(tx.pos[i], ty.pos[j]) / size).rgb * (tx.wt[i] * ty.wt[j]);
        }
    }
    let lit = min(bent + glow * 0.25, vec3<f32>(1.0));
    // The scanline pitch, the dark corners and the colour of this column's phosphor
    let pitch = max(2u, u32(size.y) / 270u);
    var s = clamp(1.15 - 0.45 * r2, 0.0, 1.0);
    if u32(pix.y) % pitch == pitch - 1u { s *= 0.55; }
    var triad = vec3<f32>(0.82, 0.82, 0.82);
    let column = u32(pix.x) % 3u;
    if column == 0u { triad.b = 1.0; } else if column == 1u { triad.g = 1.0; } else { triad.r = 1.0; }
    return vec4<f32>(lit * floor(s * triad * 255.0) / 255.0, 1.0);
}
