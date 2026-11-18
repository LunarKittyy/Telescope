// The blurred copy of the picture a shader's blurred() reads: shrunk by area, blurred along each axis at the small size, and mixed with the weighted blur.

@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(3) var c: texture_2d<f32>;
@group(0) @binding(4) var d: texture_2d<f32>;
@group(0) @binding(5) var e: texture_2d<f32>;
@group(0) @binding(6) var smp: sampler;
// source size, target size, the direction of the blur, the size of an I420 picture's chroma planes, its taps, 1 when the first texture is weighted by the second, its sigma, the plane a composite makes
struct Pass {
    src: vec2<u32>,
    dst: vec2<u32>,
    dir: vec2<u32>,
    chroma: vec2<u32>,
    taps: u32,
    weighted: u32,
    sigma: f32,
    plane: u32,
}
@group(0) @binding(2) var<uniform> p: Pass;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Every source pixel an output pixel covers, by how much of it: the area average cv2.resize(INTER_AREA) takes
@fragment
fn fs_shrink(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<f32>(floor(pos.xy));
    let scale = vec2<f32>(p.src) / vec2<f32>(p.dst);
    let lo = o * scale;
    let hi = lo + scale;
    let first = vec2<u32>(floor(lo));
    let last = min(vec2<u32>(ceil(hi)), p.src);
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1u), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let w = (min(f32(x + 1u), hi.x) - max(f32(x), lo.x)) * wy;
            sum += textureLoad(a, vec2<i32>(i32(x), i32(y)), 0).rgb * w;
            total += w;
        }
    }
    return vec4<f32>(sum / max(total, 1e-6), 1.0);
}

// One plane's area average at an output pixel o of the target, as fs_shrink makes it for colours
fn area_plane(t: texture_2d<f32>, src: vec2<u32>, o: vec2<f32>) -> f32 {
    let scale = vec2<f32>(src) / vec2<f32>(p.dst);
    let lo = o * scale;
    let hi = lo + scale;
    let first = vec2<u32>(floor(lo));
    let last = min(vec2<u32>(ceil(hi)), src);
    var sum = 0.0;
    var total = 0.0;
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1u), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let w = (min(f32(x + 1u), hi.x) - max(f32(x), lo.x)) * wy;
            sum += textureLoad(t, vec2<i32>(i32(x), i32(y)), 0).r * w;
            total += w;
        }
    }
    return sum / max(total, 1e-6);
}

// BT.601 limited, as convert.wgsl and the CPU read YUV
fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let c = 1.164383 * (y * 255.0 - 16.0);
    let du = u * 255.0 - 128.0;
    let dv = v * 255.0 - 128.0;
    return clamp(vec3<f32>(c + 1.596027 * dv, c - 0.391762 * du - 0.812968 * dv, c + 2.017232 * du) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

// An I420 picture shrunk plane by plane (luma from a, chroma from b and c) and only then turned into colours, as the CPU does
@fragment
fn fs_shrink_yuv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<f32>(floor(pos.xy));
    return vec4<f32>(yuv_to_rgb(area_plane(a, p.src, o), area_plane(b, p.chroma, o), area_plane(c, p.chroma, o)), 1.0);
}

// OpenCV's default border: the edge pixel isn't repeated
fn reflected(i: i32, n: i32) -> i32 {
    if n == 1 {
        return 0;
    }
    var k = i;
    loop {
        if k < 0 {
            k = -k;
        } else if k >= n {
            k = 2 * (n - 1) - k;
        } else {
            break;
        }
    }
    return k;
}

// One axis of the Gaussian; for a weighted first pass the colour is multiplied by the weight and the weight rides along in alpha
@fragment
fn fs_blur(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(floor(pos.xy));
    let size = vec2<i32>(p.dst);
    let dir = vec2<i32>(p.dir);
    let reach = i32(p.taps / 2u);
    let k = -0.5 / (p.sigma * p.sigma);
    var sum = vec4<f32>(0.0);
    var norm = 0.0;
    for (var j = -reach; j <= reach; j++) {
        let weight = exp(f32(j * j) * k);
        let at = o + dir * j;
        let c = vec2<i32>(reflected(at.x, size.x), reflected(at.y, size.y));
        var v = textureLoad(a, c, 0);
        if p.weighted == 1u {
            let w = textureLoad(b, c, 0).r;
            v = vec4<f32>(v.rgb * w, w);
        }
        sum += v * weight;
        norm += weight;
    }
    return sum / norm;
}

// What blurred_with does: the weighted blur where there's background to take from, the plain one where there isn't
@fragment
fn fs_combine(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(floor(pos.xy));
    let plain = textureLoad(a, o, 0);
    let t = textureLoad(b, o, 0);
    let weight = clamp(t.a * 4.0, 0.0, 1.0);
    return vec4<f32>(t.rgb / max(t.a, 1e-3) * weight + plain.rgb * (1.0 - weight), 1.0);
}

// One plane (p.plane: Y, U or V) of the result in YUV as the CPU lays it: the picture's own sample (a, b, c) over the blur (d, turned into that plane at the plane's own pixel) by the cutout (e, red), rounded as lay_over does
@fragment
fn fs_composite(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(floor(pos.xy));
    let uv = pos.xy / vec2<f32>(p.dst);
    var own = textureLoad(a, o, 0).r;
    if p.plane == 1u {
        own = textureLoad(b, o, 0).r;
    } else if p.plane == 2u {
        own = textureLoad(c, o, 0).r;
    }
    let rgb = textureSampleLevel(d, smp, uv, 0.0).rgb * 255.0;
    var room = 16.0 + 0.256788 * rgb.r + 0.504129 * rgb.g + 0.097906 * rgb.b;
    if p.plane == 1u {
        room = 128.0 - 0.148223 * rgb.r - 0.290993 * rgb.g + 0.439216 * rgb.b;
    } else if p.plane == 2u {
        room = 128.0 + 0.439216 * rgb.r - 0.367788 * rgb.g - 0.071427 * rgb.b;
    }
    let cut = u32(round(clamp(textureSampleLevel(e, smp, uv, 0.0).r, 0.0, 1.0) * 255.0));
    let top = u32(round(own * 255.0));
    let under = u32(round(clamp(room, 0.0, 255.0)));
    let x = top * cut + under * (255u - cut) + 128u;
    return vec4<f32>(f32((x + (x >> 8u)) >> 8u) / 255.0, 0.0, 0.0, 1.0);
}
