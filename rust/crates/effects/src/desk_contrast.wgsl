// Desk mode's Contrast cleanup as sheet.rs's whiten does it, in passes (desk_warp.wgsl comes first): the sheet straightened, shrunk to
// an eighth by area, the ink taken out with a 5 x 5 ellipse's max, the shading blurred, then scaled back up and divided out.

fn bytes_of(c: vec4<f32>) -> vec3<f32> {
    return round(c.rgb * 255.0);
}

// The sheet at its own size, as the CPU's warp gives it.
fn pass_0(at: vec2<i32>) -> vec4<f32> {
    return vec4<f32>(warp_at(vec2<f32>(at)), 1.0);
}

// How much source pixel s counts for pixel d of an area shrink from n to n / scale (OpenCV's area table, which the CPU copies).
fn area_weight(s: i32, d: i32, n: i32, scale: f32) -> f32 {
    let f1 = f32(d) * scale;
    let f2 = f1 + scale;
    let cell = min(scale, f32(n) - f1);
    let s2 = min(i32(floor(f2)), n - 1);
    let s1 = min(i32(ceil(f1)), s2);
    var w = 0.0;
    if s == s1 - 1 && f32(s1) - f1 > 1e-3 {
        w = (f32(s1) - f1) / cell;
    }
    if s >= s1 && s < s2 {
        w = 1.0 / cell;
    }
    if s == s2 && f2 - f32(s2) > 1e-3 {
        w = min(min(f2 - f32(s2), 1.0), cell) / cell;
    }
    return w;
}

// cv2.resize(INTER_AREA) of the sheet to the small size.
fn pass_1(at: vec2<i32>) -> vec4<f32> {
    let n = vec2<i32>(params.rect.zw);
    let m = vec2<i32>(params.small);
    if n.x == m.x * 8 && n.y == m.y * 8 {
        // Whole blocks: an exact sum, rounded once
        var sum = vec3<f32>(0.0);
        for (var y = 0; y < 8; y++) {
            for (var x = 0; x < 8; x++) {
                sum += bytes_of(passed_0(at * 8 + vec2<i32>(x, y)));
            }
        }
        return vec4<f32>(round(sum * (1.0 / 64.0)) / 255.0, 1.0);
    }
    let scale = vec2<f32>(n) / vec2<f32>(m);
    let first = vec2<i32>(floor(vec2<f32>(at) * scale));
    let last = min(vec2<i32>(floor(vec2<f32>(at) * scale + scale)), n - vec2<i32>(1));
    var out = vec3<f32>(0.0);
    for (var y = first.y; y <= last.y; y++) {
        let beta = area_weight(y, at.y, n.y, scale.y);
        if beta == 0.0 {
            continue;
        }
        var across = vec3<f32>(0.0);
        for (var x = first.x; x <= last.x; x++) {
            across += bytes_of(passed_0(vec2<i32>(x, y))) * area_weight(x, at.x, n.x, scale.x);
        }
        out += across * beta;
    }
    return vec4<f32>(clamp(round(out), vec3<f32>(0.0), vec3<f32>(255.0)) / 255.0, 1.0);
}

// cv2.dilate with the 5 x 5 ellipse: three full middle rows and one pixel above and below, nothing past the edges.
fn pass_2(at: vec2<i32>) -> vec4<f32> {
    let m = vec2<i32>(params.small);
    var most = vec3<f32>(0.0);
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            let p = at + vec2<i32>(dx, dy);
            if (abs(dy) == 2 && dx != 0) || p.x < 0 || p.y < 0 || p.x >= m.x || p.y >= m.y {
                continue;
            }
            most = max(most, passed_1(p).rgb);
        }
    }
    return vec4<f32>(most, 1.0);
}

fn reflect_101(i: i32, n: i32) -> i32 {
    if n == 1 {
        return 0;
    }
    var j = i;
    while j < 0 || j >= n {
        j = select(2 * (n - 1) - j, -j, j < 0);
    }
    return j;
}

fn weight(j: i32) -> u32 {
    let k = array<vec4<f32>, 4>(params.k0, params.k1, params.k2, params.k3);
    return u32(k[j / 4][j % 4]);
}

// cv2.GaussianBlur at sigma 2 on 8 bits: OpenCV's fixed-point weights, the sums kept whole and rounded once, the edges mirrored.
fn pass_3(at: vec2<i32>) -> vec4<f32> {
    let m = vec2<i32>(params.small);
    var sum = vec3<u32>(0u);
    for (var i = 0; i < 13; i++) {
        let y = reflect_101(at.y + i - 6, m.y);
        var across = vec3<u32>(0u);
        for (var j = 0; j < 13; j++) {
            across += weight(j) * vec3<u32>(bytes_of(passed_2(vec2<i32>(reflect_101(at.x + j - 6, m.x), y))));
        }
        sum += weight(i) * across;
    }
    return vec4<f32>(vec3<f32>(min((sum + vec3<u32>(0x8000u)) >> vec3<u32>(16u), vec3<u32>(255u))) / 255.0, 1.0);
}

// One axis of OpenCV's 8-bit INTER_LINEAR from n to m: the two sources and their weights out of 2048; across, a tap past an edge takes the edge pixel whole.
fn linear_tap(d: i32, n: i32, m: i32, across: bool) -> vec4<i32> {
    var f = (f32(d) + 0.5) * (f32(n) / f32(m)) - 0.5;
    var s = i32(floor(f));
    f -= f32(s);
    if across && s < 0 {
        f = 0.0;
        s = 0;
    }
    if across && s >= n - 1 {
        f = 0.0;
        s = n - 1;
    }
    return vec4<i32>(clamp(s, 0, n - 1), clamp(s + 1, 0, n - 1), i32(round((1.0 - f) * 2048.0)), i32(round(f * 2048.0)));
}

fn shade_across(x: vec4<i32>, y: i32) -> vec3<i32> {
    let a = vec3<i32>(bytes_of(passed_3(vec2<i32>(x.x, y))));
    let b = vec3<i32>(bytes_of(passed_3(vec2<i32>(x.y, y))));
    return (a * x.z + b * x.w) >> vec3<u32>(4u);
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
    let n = vec2<i32>(params.small);
    let m = vec2<i32>(params.rect.zw);
    let tx = linear_tap(local.x, n.x, m.x, true);
    let ty = linear_tap(local.y, n.y, m.y, false);
    let top = (shade_across(tx, ty.x) * ty.z) >> vec3<u32>(16u);
    let bottom = (shade_across(tx, ty.y) * ty.w) >> vec3<u32>(16u);
    let paper = vec3<f32>((top + bottom + vec3<i32>(2)) >> vec3<u32>(2u));
    let sheet = bytes_of(passed_0(local));
    // cv2.divide in floats, scaled so the ink stays dark; a zero below gives zero
    let v = clamp(round(sheet * 234.6 / max(paper, vec3<f32>(1.0))), vec3<f32>(0.0), vec3<f32>(255.0));
    return vec4<f32>(select(v, vec3<f32>(0.0), paper == vec3<f32>(0.0)) / 255.0, 1.0);
}
