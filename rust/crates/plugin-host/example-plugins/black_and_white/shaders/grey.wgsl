// Takes the colour out of the picture; strength 0 leaves it as it is, 1 makes it fully grey.

struct Params {
    strength: f32,
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let grey = dot(c.rgb, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(mix(c.rgb, vec3<f32>(grey), params.strength), c.a);
}
