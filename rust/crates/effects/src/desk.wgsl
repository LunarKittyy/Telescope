// Desk mode's straightening: the sheet's perspective warp in one pass, each pixel of the sheet's rectangle looking up where it came from.

// r0, r1, r2: the inverse warp's rows (x, y, w) from the rectangle's own pixel numbers; rect: (x, y, width, height) in the frame; backdrop: the grey around it.
struct Params {
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
    rect: vec4<f32>,
    backdrop: f32,
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let size = frame.size;
    let local = floor(uv * size) - params.rect.xy;
    if local.x < 0.0 || local.y < 0.0 || local.x >= params.rect.z || local.y >= params.rect.w {
        return vec4<f32>(vec3<f32>(params.backdrop), 1.0);
    }
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
    return vec4<f32>(mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y), 1.0);
}
