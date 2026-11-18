// Cinema's zebras, focus peaking and false colour as effects' cinema_tools::tint does them on the CPU: masks judged on "work" (at most 1280 wide), then painted false colour, zebras, peaking.

@group(0) @binding(0) var pic: texture_2d<f32>;
@group(0) @binding(1) var work: texture_2d<f32>;
@group(0) @binding(2) var marks: texture_2d<f32>;
@group(0) @binding(3) var lut: texture_2d<f32>;
@group(0) @binding(4) var smp: sampler;
@group(0) @binding(5) var<uniform> p: Params;
// How a mask is grown from work to pic's size, as the CPU's cv2.resize(INTER_LINEAR) does it: x taps on row 0, y taps on row 1, each (first source, second source, weight of the first, weight of the second) out of 2048
@group(0) @binding(6) var taps: texture_2d<u32>;
// For each red (x) and green (y), the CPU's level for the one blue that puts that colour exactly half way between two levels (none: 0)
@group(0) @binding(7) var halves: texture_2d<u32>;

struct Params {
    // flags (1 zebras, 2 peaking, 4 false colour, 8 work is a smaller copy of pic), zebras' brightness, peaking's threshold, stripe phase
    a: vec4<u32>,
    // peaking's colour
    b: vec4<u32>,
    // pic's width and height, work's width and height
    c: vec4<u32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let q = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(q * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(q.x, 1.0 - q.y);
    return out;
}

fn bytes(c: vec4<f32>) -> vec3<u32> {
    return vec3<u32>(round(c.rgb * 255.0));
}

// BT.709 brightness as scopes::luma_of gets it (float steps, a multiply fused into each add): worked out here in whole numbers, 2126 r + 7152 g + 722 b over 10000, which
// agrees with it everywhere but on a colour exactly half way between two levels, where its float rounding goes either way. Those few colours are looked up in a table made
// on the CPU from its own steps, so no card's own fma or mad can differ from it (DX12 turns fma into a plain mad).
fn luma(rgb: vec3<u32>) -> u32 {
    let n = rgb.r * 2126u + rgb.g * 7152u + rgb.b * 722u;
    let q = n / 10000u;
    let rest = n % 10000u;
    if rest == 5000u {
        return textureLoad(halves, vec2<i32>(i32(rgb.r), i32(rgb.g)), 0).r;
    }
    return min(q + select(0u, 1u, rest > 5000u), 255u);
}

// BT.601 grey in 15 bits, as OpenCV's BGR2GRAY
fn grey(rgb: vec3<u32>) -> i32 {
    return i32((rgb.r * 9798u + rgb.g * 19235u + rgb.b * 3735u + 16384u) >> 15u);
}

fn work_size() -> vec2<i32> {
    return vec2<i32>(i32(p.c.z), i32(p.c.w));
}

// BORDER_REFLECT_101
fn reflect101(i: i32, n: i32) -> i32 {
    if n == 1 {
        return 0;
    }
    if i < 0 {
        return -i;
    }
    if i >= n {
        return 2 * (n - 1) - i;
    }
    return i;
}

fn grey_at(x: i32, y: i32) -> i32 {
    let size = work_size();
    return grey(bytes(textureLoad(work, vec2<i32>(reflect101(x, size.x), reflect101(y, size.y)), 0)));
}

fn is_bright(at: vec2<i32>) -> bool {
    return luma(bytes(textureLoad(work, at, 0))) >= p.a.y;
}

// Both Sobel gradients held to 255, then their mean past the threshold
fn is_edge(at: vec2<i32>) -> bool {
    let x = at.x;
    let y = at.y;
    let gx = (grey_at(x + 1, y - 1) - grey_at(x - 1, y - 1)) + 2 * (grey_at(x + 1, y) - grey_at(x - 1, y)) + (grey_at(x + 1, y + 1) - grey_at(x - 1, y + 1));
    let gy = (grey_at(x - 1, y + 1) - grey_at(x - 1, y - 1)) + 2 * (grey_at(x, y + 1) - grey_at(x, y - 1)) + (grey_at(x + 1, y + 1) - grey_at(x + 1, y - 1));
    let m = round(f32(min(abs(gx), 255)) * 0.5 + f32(min(abs(gy), 255)) * 0.5);
    return i32(m) > i32(p.a.z);
}

fn false_at(at: vec2<i32>) -> vec3<f32> {
    return textureLoad(lut, vec2<i32>(i32(luma(bytes(textureLoad(work, at, 0)))), 0), 0).rgb;
}

// pic shrunk to work by area: each output pixel averages the part of pic it covers, edge pixels by how much of them it covers
@fragment
fn fs_shrink(in: VsOut) -> @location(0) vec4<f32> {
    let at = vec2<i32>(in.pos.xy);
    let src = vec2<f32>(f32(p.c.x), f32(p.c.y));
    let scale = src / vec2<f32>(f32(p.c.z), f32(p.c.w));
    let lo = vec2<f32>(at) * scale;
    let hi = lo + scale;
    let first = vec2<i32>(floor(lo));
    let last = min(vec2<i32>(ceil(hi)) - 1, vec2<i32>(i32(p.c.x), i32(p.c.y)) - 1);
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y <= last.y; y++) {
        let wy = min(f32(y + 1), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x <= last.x; x++) {
            let w = (min(f32(x + 1), hi.x) - max(f32(x), lo.x)) * wy;
            sum += textureLoad(pic, vec2<i32>(x, y), 0).rgb * w;
            total += w;
        }
    }
    return vec4<f32>(sum / total, 1.0);
}

// The zebras' and peaking's masks on work (red and green), for a picture that is bigger: they're smoothed up to its size from here
@fragment
fn fs_marks(in: VsOut) -> @location(0) vec4<f32> {
    let at = vec2<i32>(in.pos.xy);
    var m = vec2<f32>(0.0);
    if (p.a.x & 1u) != 0u && is_bright(at) {
        m.x = 1.0;
    }
    if (p.a.x & 2u) != 0u && is_edge(at) {
        m.y = 1.0;
    }
    return vec4<f32>(m, 0.0, 1.0);
}

// False colour of pic's position scaled up from work, blended as cv2.resize(INTER_LINEAR) does
fn false_scaled(at: vec2<i32>) -> vec3<f32> {
    let size = work_size();
    let scale = vec2<f32>(size) / vec2<f32>(f32(p.c.x), f32(p.c.y));
    let s = max((vec2<f32>(at) + 0.5) * scale - 0.5, vec2<f32>(0.0));
    let f = floor(s);
    var t = s - f;
    var a = vec2<i32>(f);
    if a.x >= size.x - 1 {
        a.x = size.x - 1;
        t.x = 0.0;
    }
    if a.y >= size.y - 1 {
        a.y = size.y - 1;
        t.y = 0.0;
    }
    let b = min(a + 1, size - 1);
    let top = mix(false_at(a), false_at(vec2<i32>(b.x, a.y)), t.x);
    let bottom = mix(false_at(vec2<i32>(a.x, b.y)), false_at(b), t.x);
    return mix(top, bottom, t.y);
}

fn mark_bytes(at: vec2<i32>) -> vec2<i32> {
    return vec2<i32>(round(textureLoad(marks, at, 0).rg * 255.0));
}

// The two masks grown from work to pic's size in whole numbers as the CPU does it: the weights in 11 bits from the taps, the sums kept in 15 bits across and rounded down
fn grown_marks(at: vec2<i32>) -> vec2<i32> {
    let tx = textureLoad(taps, vec2<i32>(at.x, 0), 0);
    let ty = textureLoad(taps, vec2<i32>(at.y, 1), 0);
    let a = mark_bytes(vec2<i32>(i32(tx.x), i32(ty.x)));
    let b = mark_bytes(vec2<i32>(i32(tx.y), i32(ty.x)));
    let c = mark_bytes(vec2<i32>(i32(tx.x), i32(ty.y)));
    let d = mark_bytes(vec2<i32>(i32(tx.y), i32(ty.y)));
    let top = (a * i32(tx.z) + b * i32(tx.w)) >> vec2<u32>(4u);
    let bottom = (c * i32(tx.z) + d * i32(tx.w)) >> vec2<u32>(4u);
    return (((top * i32(ty.z)) >> vec2<u32>(16u)) + ((bottom * i32(ty.w)) >> vec2<u32>(16u)) + vec2<i32>(2)) >> vec2<u32>(2u);
}

@fragment
fn fs_tint(in: VsOut) -> @location(0) vec4<f32> {
    let at = vec2<i32>(in.pos.xy);
    let flags = p.a.x;
    let smaller = (flags & 8u) != 0u;
    var bright = false;
    var edge = false;
    if smaller {
        // The masks scaled up and cut half way
        if (flags & 3u) != 0u {
            let m = grown_marks(at);
            bright = (flags & 1u) != 0u && m.x > 127;
            edge = (flags & 2u) != 0u && m.y > 127;
        }
    } else {
        bright = (flags & 1u) != 0u && is_bright(at);
        edge = (flags & 2u) != 0u && is_edge(at);
    }
    var out = textureLoad(pic, at, 0).rgb;
    if (flags & 4u) != 0u {
        if smaller {
            out = false_scaled(at);
        } else {
            out = false_at(at);
        }
    }
    // Stripes at the picture's own size, so they stay crisp
    let stripe = ((u32(at.x) + p.a.w % 8u + u32(at.y)) / 4u) % 2u == 0u;
    if bright && stripe {
        out = vec3<f32>(0.0);
    }
    if edge {
        out = vec3<f32>(p.b.rgb) / 255.0;
    }
    return vec4<f32>(out, 1.0);
}
