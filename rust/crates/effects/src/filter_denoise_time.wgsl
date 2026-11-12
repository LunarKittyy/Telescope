// Denoise in time: each pixel is mixed with what this step made of it last frame, as far as nothing moved there. How different the
// picture is from the last one is averaged over 3 x 3 pixels, so one noisy pixel doesn't count as movement, and once that is more than
// the tolerance the last picture is let go (completely at twice the tolerance), so nothing leaves a ghost behind. The result goes back
// to the card as the next frame's last picture. A card keeps 8 bits, and a little noise added before the rounding stops a slow change
// from being rounded away (a mix that moves a pixel by less than half a step would otherwise never get there).

struct Params {
    tolerance: f32,
    keep: f32,
}

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn change(uv: vec2<f32>) -> f32 {
    let d = abs(sample(uv).rgb - previous(uv).rgb);
    return (d.r + d.g + d.b) / 3.0;
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    if !has_previous() {
        return vec4<f32>(c, 1.0);
    }
    let px = 1.0 / frame.size;
    var sum = 0.0;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            sum += change(uv + vec2<f32>(f32(i), f32(j)) * px);
        }
    }
    // the mean lets a lone pixel that changed a lot pass for noise, so one that changed far more than noise does counts alone
    let alone = 1.0 - smoothstep(3.0 * params.tolerance, 6.0 * params.tolerance, change(uv));
    let k = params.keep * (1.0 - smoothstep(params.tolerance, 2.0 * params.tolerance, sum / 9.0)) * alone;
    let at = vec2<u32>(uv * frame.size);
    let dither = f32(pcg(at.x + pcg(at.y + pcg(frame.number))) >> 8u) / 16777216.0 - 0.5;
    return vec4<f32>(clamp(mix(c, previous(uv).rgb, k) + dither / 255.0, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
