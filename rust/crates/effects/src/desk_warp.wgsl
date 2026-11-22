// Desk mode's straightening: each pixel of the sheet's rectangle looks up where it came from through the perspective warp.

// r0, r1, r2: the inverse warp's rows (x, y, w) from the rectangle's own pixel numbers; rect: (x, y, width, height) in the frame; backdrop: the grey around it.
// For the cleanup: k0 to k3 the shading blur's 13 weights out of 256, small the size the shading is worked out at, block the black and white window, untouched 1 to leave the picture alone.
struct Params {
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
    rect: vec4<f32>,
    k0: vec4<f32>,
    k1: vec4<f32>,
    k2: vec4<f32>,
    k3: vec4<f32>,
    small: vec2<f32>,
    backdrop: f32,
    block: f32,
    untouched: f32,
}

// Known, small difference from the CPU: positions here are f32 and the CPU's are f64. At 1080p about 0.4% of the 1/32 pixel positions land one step off (0.7% at 4K), which moves a pixel's blend by 1/32 of its neighbour's difference: at most 8 levels on a hard edge, nothing on a smooth area. Measured against the CPU: mean 0.02 to 0.07 levels, 0.005% of values off by more than 3.
// The colour of the sheet's pixel `local`, counted from its rectangle's corner.
fn warp_at(local: vec2<f32>) -> vec3<f32> {
    let size = frame.size;
    let at = vec3<f32>(local, 1.0);
    let w = dot(params.r2.xyz, at);
    // Positions are looked up in 1/32 pixels like the CPU does, a pixel's number being its middle; w of 0 means the corner
    var s = vec2<f32>(0.0);
    if w != 0.0 {
        s = vec2<f32>(dot(params.r0.xyz, at), dot(params.r1.xyz, at)) * (32.0 / w);
    }
    s = round(clamp(s, vec2<f32>(-1.0e6), vec2<f32>(1.0e6)));
    let whole = floor(s / 32.0);
    let f = (s - whole * 32.0) / 32.0;
    let top = vec2<f32>(0.0);
    let last = size - vec2<f32>(1.0);
    let a = clamp(whole, top, last);
    let b = clamp(whole + vec2<f32>(1.0), top, last);
    // Four whole pixels read at their middles, so the blend is the CPU's and not the card's
    let c00 = sample((a + 0.5) / size).rgb;
    let c10 = sample((vec2<f32>(b.x, a.y) + 0.5) / size).rgb;
    let c01 = sample((vec2<f32>(a.x, b.y) + 0.5) / size).rgb;
    let c11 = sample((b + 0.5) / size).rgb;
    return mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y);
}

// Where the frame's pixel at uv sits in the sheet's rectangle, and whether it's inside it.
fn sheet_pixel(uv: vec2<f32>) -> vec3<f32> {
    let local = floor(uv * frame.size) - params.rect.xy;
    let inside = local.x >= 0.0 && local.y >= 0.0 && local.x < params.rect.z && local.y < params.rect.w;
    return vec3<f32>(local, select(0.0, 1.0, inside));
}
