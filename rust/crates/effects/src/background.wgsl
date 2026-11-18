// Background's blur: the frame over its own blur, where the cutout (a quarter-size mask, feathered on the CPU) says.
// blurred_weighted is the room shrunk to an eighth and blurred without the people in blur_weights, as the CPU way makes it; no soft picture is no cutout.

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let room = blurred_weighted(uv).rgb;
    let cut = sample_texture(soft, uv).r;
    return vec4<f32>(mix(room, sample(uv).rgb, cut), 1.0);
}
