// Desk mode's Black and white cleanup as sheet.rs's black_and_white does it, in passes (desk_warp.wgsl comes first): the sheet straightened,
// then each pixel against the mean of the block around it (an adaptive threshold), with exact whole sums and the edges repeated.


// The sheet at its own size, as the CPU's warp gives it.
fn pass_0(at: vec2<i32>) -> vec4<f32> {
    return vec4<f32>(warp_at(vec2<f32>(at)), 1.0);
}

// A whole number below 65536 kept in two 8-bit channels, and one below 16777216 in three.
fn keep(v: u32) -> vec4<f32> {
    return vec4<f32>(f32(v & 255u), f32((v >> 8u) & 255u), f32(v >> 16u), 255.0) / 255.0;
}

fn kept(c: vec4<f32>) -> i32 {
    let b = vec3<i32>(round(c.rgb * 255.0));
    return b.x + (b.y << 8u) + (b.z << 16u);
}

// cv2.cvtColor's grey with OpenCV's whole-number weights, worked out once a pixel.
fn pass_1(at: vec2<i32>) -> vec4<f32> {
    let c = vec3<u32>(round(passed_0(at).rgb * 255.0));
    return keep((c.r * 9798u + c.g * 19235u + c.b * 3735u + 16384u) >> 15u);
}

fn grey_at(at: vec2<i32>) -> i32 {
    return i32(round(passed_1(at).r * 255.0));
}

// The grey summed over each run of 8 pixels along a row (the last run may be short), so a wide window needs a tap an 8th as often.
fn pass_2(at: vec2<i32>) -> vec4<f32> {
    let n = passed_size_1().x;
    var total = 0;
    for (var i = 0; i < 8; i++) {
        let x = at.x * 8 + i;
        if x < n {
            total += grey_at(vec2<i32>(x, at.y));
        }
    }
    return keep(u32(total));
}

// The grey summed across the block's width, edges repeating the pixel they end on: the runs of 8 inside, single pixels at the ends.
fn pass_3(at: vec2<i32>) -> vec4<f32> {
    let n = passed_size_1().x;
    let half = i32(params.block) / 2;
    var lo = at.x - half;
    var hi = at.x + half;
    var total = 0;
    if lo < 0 {
        total += -lo * grey_at(vec2<i32>(0, at.y));
        lo = 0;
    }
    if hi > n - 1 {
        total += (hi - (n - 1)) * grey_at(vec2<i32>(n - 1, at.y));
        hi = n - 1;
    }
    let first = (lo + 7) >> 3u;
    let last = ((hi + 1) >> 3u) - 1;
    var start = lo;
    var to = hi + 1;
    if first <= last {
        for (var k = first; k <= last; k++) {
            total += kept(passed_2(vec2<i32>(k, at.y)));
        }
        to = first * 8;
    }
    for (var x = start; x < to; x++) {
        total += grey_at(vec2<i32>(x, at.y));
    }
    if first <= last {
        for (var x = (last + 1) * 8; x <= hi; x++) {
            total += grey_at(vec2<i32>(x, at.y));
        }
    }
    return keep(u32(total));
}

// The row sums added over each run of 8 rows down a column.
fn pass_4(at: vec2<i32>) -> vec4<f32> {
    let n = passed_size_1().y;
    var total = 0;
    for (var i = 0; i < 8; i++) {
        let y = at.y * 8 + i;
        if y < n {
            total += kept(passed_3(vec2<i32>(at.x, y)));
        }
    }
    return keep(u32(total));
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
    let n = passed_size_1().y;
    let block = u32(params.block);
    let half = i32(block) / 2;
    var lo = local.y - half;
    var hi = local.y + half;
    var total = 0;
    if lo < 0 {
        total += -lo * kept(passed_3(vec2<i32>(local.x, 0)));
        lo = 0;
    }
    if hi > n - 1 {
        total += (hi - (n - 1)) * kept(passed_3(vec2<i32>(local.x, n - 1)));
        hi = n - 1;
    }
    let first = (lo + 7) >> 3u;
    let last = ((hi + 1) >> 3u) - 1;
    var to = hi + 1;
    if first <= last {
        for (var k = first; k <= last; k++) {
            total += kept(passed_4(vec2<i32>(local.x, k)));
        }
        to = first * 8;
    }
    for (var y = lo; y < to; y++) {
        total += kept(passed_3(vec2<i32>(local.x, y)));
    }
    if first <= last {
        for (var y = (last + 1) * 8; y <= hi; y++) {
            total += kept(passed_3(vec2<i32>(local.x, y)));
        }
    }
    // v > the rounded mean - 12, with no division
    let area = block * block;
    let on = (u32(grey_at(local)) + 12u) * area > u32(total) + area / 2u;
    return vec4<f32>(vec3<f32>(select(0.0, 1.0, on)), 1.0);
}
