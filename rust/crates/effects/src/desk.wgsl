// Desk mode's plain straightening in one pass (desk_warp.wgsl comes first).

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let p = sheet_pixel(uv);
    if p.z == 0.0 {
        return vec4<f32>(vec3<f32>(params.backdrop), 1.0);
    }
    return vec4<f32>(warp_at(p.xy), 1.0);
}
