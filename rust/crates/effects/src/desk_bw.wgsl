// Desk mode's Black and white cleanup as sheet.rs's black_and_white does it, in passes (desk_warp.wgsl comes first): the sheet straightened,
// then each pixel against the mean of the block around it (an adaptive threshold), with exact whole sums and the edges repeated.


// The sheet at its own size, as the CPU's warp gives it.
fn pass_0(at: vec2<i32>) -> vec4<f32> {
    return vec4<f32>(warp_at(vec2<f32>(at)), 1.0);
}

// cv2.cvtColor's grey with OpenCV's whole-number weights, worked out once a pixel.
fn pass_1(at: vec2<i32>) -> f32 {
    let c = vec3<u32>(round(passed_0(at).rgb * 255.0));
    return f32((c.r * 9798u + c.g * 19235u + c.b * 3735u + 16384u) >> 15u);
}

// The grey summed across the block's width, edges repeating the pixel they end on.
fn pass_2(at: vec2<i32>) -> f32 {
    let half = i32(params.block) / 2;
    var total = 0.0;
    for (var dx = -half; dx <= half; dx++) {
        total += passed_1(vec2<i32>(at.x + dx, at.y));
    }
    return total;
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    if params.untouched > 0.5 {
        return sample(uv);
    }
    let p = sheet_pixel(uv);
    // Paper around a sheet that keeps its shape
    if p.z == 0.0 {
        return vec4<f32>(1.0);
    }
    let local = vec2<i32>(p.xy);
    let block = u32(params.block);
    let half = i32(block) / 2;
    var total = 0u;
    for (var dy = -half; dy <= half; dy++) {
        total += u32(passed_2(vec2<i32>(local.x, local.y + dy)));
    }
    // v > the rounded mean - 12, with no division
    let area = block * block;
    let on = (u32(passed_1(local)) + 12u) * area > total + area / 2u;
    return vec4<f32>(vec3<f32>(select(0.0, 1.0, on)), 1.0);
}
