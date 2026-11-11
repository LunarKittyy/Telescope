// CRT (fun_effects.Crt): the picture bulged like a curved screen, with a soft glow, scanlines, a phosphor mask and dark corners.

const GLOW_TAPS: i32 = 2;

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
    // A 9 x 9 box around it as 25 taps of two pixels each, a quarter of it added back
    var glow = vec3<f32>(0.0);
    for (var j = -GLOW_TAPS; j <= GLOW_TAPS; j++) {
        for (var i = -GLOW_TAPS; i <= GLOW_TAPS; i++) {
            glow += sample((at + vec2<f32>(f32(i), f32(j)) * 2.0) / size).rgb;
        }
    }
    let lit = min(bent + glow * (0.25 / 25.0), vec3<f32>(1.0));
    // The scanline pitch, the dark corners and the colour of this column's phosphor
    let pitch = max(2u, u32(size.y) / 270u);
    var s = clamp(1.15 - 0.45 * r2, 0.0, 1.0);
    if u32(pix.y) % pitch == pitch - 1u { s *= 0.55; }
    var triad = vec3<f32>(0.82, 0.82, 0.82);
    let column = u32(pix.x) % 3u;
    if column == 0u { triad.b = 1.0; } else if column == 1u { triad.g = 1.0; } else { triad.r = 1.0; }
    return vec4<f32>(lit * floor(s * triad * 255.0) / 255.0, 1.0);
}
