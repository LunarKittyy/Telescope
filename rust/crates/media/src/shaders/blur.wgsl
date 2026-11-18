// The blurred copy of the picture a shader's blurred() reads: shrunk by area, blurred along each axis at the small size, and mixed with the weighted blur.

@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
// source size, target size, the direction of the blur, its taps, 1 when the first texture is weighted by the second, its sigma
struct Pass {
    src: vec2<u32>,
    dst: vec2<u32>,
    dir: vec2<u32>,
    taps: u32,
    weighted: u32,
    sigma: f32,
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
