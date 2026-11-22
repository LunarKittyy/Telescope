// Background's picture: the frame over the chosen picture (scaled to cover the frame on the CPU), where the cutout (the mask feathered on the CPU) says; no cutout picture is no cutout.

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let room = sample_texture(room_picture, uv).rgb;
    let cut = sample_texture(blur_cutout, uv).r;
    return vec4<f32>(mix(room, sample(uv).rgb, cut), 1.0);
}
