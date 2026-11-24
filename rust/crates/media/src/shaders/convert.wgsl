// Pictures onto and off the card: YUV to RGBA (scaling as it goes, for the preview) and RGBA back to YUV planes.
// BT.601 with video range, the same maths as OpenCV's I420 conversions and swscale's default the Python app used.

@group(0) @binding(0) var p0: texture_2d<f32>;
@group(0) @binding(1) var p1: texture_2d<f32>;
@group(0) @binding(2) var p2: texture_2d<f32>;
@group(0) @binding(3) var smp: sampler;
struct Convert {
    // mode, taps a side per output pixel, source width, source height
    settings: vec4<u32>,
    // For a decoder's picture in another encoding, yuv.rs card_terms: luma gain, black, red's and blue's chroma terms, then green's two
    matrix: vec4<i32>,
    green: vec4<i32>,
}
@group(0) @binding(4) var<uniform> conv: Convert;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let full = (conv.settings.x & 32u) != 0u;
    let c = select(1.164383 * (y * 255.0 - 16.0), y * 255.0, full);
    let d = (u * 255.0 - 128.0) * select(1.0, 224.0 / 255.0, full);
    let e = (v * 255.0 - 128.0) * select(1.0, 224.0 / 255.0, full);
    if (conv.settings.x & 16u) != 0u {
        return clamp(vec3<f32>(c + 1.792741 * e, c - 0.213249 * d - 0.532909 * e, c + 2.112402 * d) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return clamp(vec3<f32>(c + 1.596027 * e, c - 0.391762 * d - 0.812968 * e, c + 2.017232 * d) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The CPU's vector kernel in 16 bit lanes with 6 fractional bits (core's yuv.rs), step for step: its rounding multiply and saturating sums
fn mulhrs(a: i32, b: i32) -> i32 {
    return (a * b + 16384) >> 15u;
}

// A pixel as the CPU reads it: the first whole 32 pixels of a row by the vector kernel, the rest by the float formula, luma under black counting as black
fn yuv_to_rgb_cpu(y: f32, u: f32, v: f32, vector: bool) -> vec3<f32> {
    let yi = i32(round(y * 255.0));
    let d = (i32(round(u * 255.0)) - 128) << 6u;
    let e = (i32(round(v * 255.0)) - 128) << 6u;
    if vector {
        let red = e + mulhrs(e, 19531) + 32;
        let green = mulhrs(d, -12837) + mulhrs(e, -26640) + 32;
        let blue = d + d + mulhrs(d, 565) + 32;
        let a = max(yi - 16, 0) << 6u;
        let c = a + mulhrs(a, 5387);
        let sum = vec3<i32>(c + red, c + green, c + blue);
        return vec3<f32>(clamp(clamp(sum, vec3<i32>(-32768), vec3<i32>(32767)) >> vec3<u32>(6u), vec3<i32>(0), vec3<i32>(255))) / 255.0;
    }
    let c = f32(max(yi - 16, 0)) * 1.164383;
    let df = f32(d >> 6u);
    let ef = f32(e >> 6u);
    return clamp(floor(vec3<f32>(c + 1.596027 * ef, c - 0.391762 * df - 0.812968 * ef, c + 2.017232 * df) + 0.5), vec3<f32>(0.0), vec3<f32>(255.0)) / 255.0;
}

// The same by index, as the CPU reads it: pixel (x, y) takes the chroma sample (x / 2, y / 2), whatever the size
fn source_by_block(uv: vec2<f32>, mode: u32) -> vec3<f32> {
    let size = vec2<i32>(i32(conv.settings.z), i32(conv.settings.w));
    let at = clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2<i32>(0), size - 1);
    let y = textureLoad(p0, at, 0).r;
    let vector = at.x < i32(conv.settings.z & 0xffffffe0u);
    if mode == 0u {
        return yuv_to_rgb_cpu(y, textureLoad(p1, at / 2, 0).r, textureLoad(p2, at / 2, 0).r, vector);
    }
    let c = textureLoad(p1, at / 2, 0).rg;
    return yuv_to_rgb_cpu(y, c.r, c.g, vector);
}

fn source(uv: vec2<f32>, mode: u32) -> vec3<f32> {
    if mode == 0u {
        return yuv_to_rgb(textureSampleLevel(p0, smp, uv, 0.0).r, textureSampleLevel(p1, smp, uv, 0.0).r, textureSampleLevel(p2, smp, uv, 0.0).r);
    }
    if mode == 1u {
        let c = textureSampleLevel(p1, smp, uv, 0.0).rg;
        return yuv_to_rgb(textureSampleLevel(p0, smp, uv, 0.0).r, c.r, c.g);
    }
    return textureSampleLevel(p0, smp, uv, 0.0).rgb;
}

// A sample of an 8 bit plane as the byte it holds
fn byte(x: f32) -> i32 {
    return i32(round(x * 255.0));
}

// x / 255 with the rounding range.rs uses
fn div255(x: i32) -> i32 {
    return (x + (x >> 8u) + 1) >> 8u;
}

// Full range BT.601 squeezed into the limited range, as range.rs does
fn squeeze_luma(p: i32) -> i32 {
    return 16 + div255(p * 219 + 127);
}

fn squeeze_chroma(p: i32) -> i32 {
    let d = p - 128;
    let q = div255(abs(d) * 224 + 127);
    return 128 + select(-q, q, d >= 0);
}

// A pixel's 8 bit RGB step for step as yuv.rs colour: every value fits its 16 bit lanes, so i32 maths gives the same bytes
fn cpu_colour(y: i32, u: i32, v: i32) -> vec3<i32> {
    let m = conv.matrix;
    let offsets = vec3<i32>((m.z * v) >> 11u, (-(conv.green.x * u + conv.green.y * v)) >> 11u, (m.w * u) >> 11u);
    let base = (((y - m.y) << 7u) * m.x) >> 16u;
    return clamp((vec3<i32>(base) + offsets) >> vec3<u32>(5u), vec3<i32>(0), vec3<i32>(255));
}

// Colours weighed back into BT.601 as OpenCV's fixed point does (yuv.rs weigh)
fn weigh(bias: i32, rgb: vec3<i32>, w: vec3<i32>) -> i32 {
    let parts = ((rgb << vec3<u32>(7u)) * w) >> vec3<u32>(16u);
    return (bias + parts.x + parts.y + parts.z + 32) >> 6u;
}

// A plane's value as a sample of an 8 bit target
fn level(v: i32) -> vec4<f32> {
    return vec4<f32>(f32(v) / 255.0, 0.0, 0.0, 1.0);
}

// A colour as whole bytes
fn bytes(c: vec3<f32>) -> vec3<i32> {
    return vec3<i32>(round(c * 255.0));
}

fn rgb_at(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(p0, smp, uv, 0.0).rgb;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let mode = conv.settings.x & 7u;
    if mode <= 2u {
        let taps = conv.settings.y;
        // A plane read stretched (SCALE_UV): the factors are in the first two terms of the matrix, as f32 bits
        let stretch = select(vec2<f32>(1.0), vec2<f32>(bitcast<f32>(conv.matrix.x), bitcast<f32>(conv.matrix.y)), (conv.settings.x & 128u) != 0u);
        if (conv.settings.x & 8u) != 0u && mode <= 1u && taps <= 1u {
            return vec4<f32>(source_by_block(in.uv, mode), 1.0);
        }
        if taps <= 1u {
            return vec4<f32>(source(in.uv * stretch, mode), 1.0);
        }
        // One output pixel covers this much of the picture; average a taps x taps grid inside it
        let step = vec2<f32>(dpdxFine(in.uv.x), dpdyFine(in.uv.y)) / f32(taps);
        var sum = vec3<f32>(0.0);
        for (var j = 0u; j < taps; j++) {
            for (var i = 0u; i < taps; i++) {
                let off = (vec2<f32>(f32(i), f32(j)) + 0.5 - f32(taps) * 0.5) * step;
                sum += source((in.uv + off) * stretch, mode);
            }
        }
        return vec4<f32>(sum / f32(taps * taps), 1.0);
    }
    // Back to YUV from the RGBA picture in p0, or directly from NV12 planes when bit 6 (64) is set.
    // A chroma target pixel is the average of its own 2 x 2 source pixels, found by index so odd
    // sizes line up with the CPU: the last column and row of an odd picture average only the pixels there are.
    // All of it in whole bytes with the CPU's integer maths (rgb_to_420), so the two sides round the same way.
    var c = vec3<i32>(0);
    if (conv.settings.x & 64u) != 0u {
        let at = vec2<i32>(in.pos.xy);
        if (conv.settings.x & 48u) == 0u {
            if mode == 3u {
                return vec4<f32>(textureLoad(p0, at, 0).r, 0.0, 0.0, 1.0);
            }
            let uv = textureLoad(p1, at, 0).rg;
            if mode == 4u {
                return vec4<f32>(uv.r, 0.0, 0.0, 1.0);
            }
            if mode == 5u {
                return vec4<f32>(uv.g, 0.0, 0.0, 1.0);
            }
            return vec4<f32>(uv.r, uv.g, 0.0, 1.0);
        }
        // The same bytes the CPU makes of it (yuv.rs standard), so a stream can move between the two ways without a change in colour
        if (conv.settings.x & 48u) == 32u {
            // BT.601 in the full range is only squeezed into the limited range
            if mode == 3u {
                return level(squeeze_luma(byte(textureLoad(p0, at, 0).r)));
            }
            let uv = textureLoad(p1, at, 0).rg;
            if mode == 6u {
                return vec4<f32>(f32(squeeze_chroma(byte(uv.r))) / 255.0, f32(squeeze_chroma(byte(uv.g))) / 255.0, 0.0, 1.0);
            }
            return level(squeeze_chroma(byte(select(uv.g, uv.r, mode == 4u))));
        }
        // Any other matrix: each chroma sample colours its 2 x 2 pixels, and the top left one's colour gives the sample back
        let luma_at = select(at * 2, at, mode == 3u);
        let uv = textureLoad(p1, select(at, at / 2, mode == 3u), 0).rg;
        let rgb = cpu_colour(byte(textureLoad(p0, luma_at, 0).r), byte(uv.r) - 128, byte(uv.g) - 128);
        if mode == 3u {
            return level(weigh(16 << 6u, rgb, vec3<i32>(8421, 16515, 3211)));
        }
        let u = weigh(128 << 6u, rgb, vec3<i32>(-4850, -9535, 14385));
        let v = weigh(128 << 6u, rgb, vec3<i32>(14385, -12059, -2327));
        if mode == 6u {
            return vec4<f32>(f32(u) / 255.0, f32(v) / 255.0, 0.0, 1.0);
        }
        return level(select(v, u, mode == 4u));
    } else if mode >= 4u {
        let at = vec2<i32>(in.pos.xy) * 2;
        let last = vec2<i32>(i32(conv.settings.z) - 1, i32(conv.settings.w) - 1);
        var sum = vec3<i32>(0);
        var n = 0;
        for (var j = 0; j < 2; j++) {
            for (var i = 0; i < 2; i++) {
                let q = at + vec2<i32>(i, j);
                if q.x <= last.x && q.y <= last.y {
                    sum += bytes(textureLoad(p0, q, 0).rgb);
                    n += 1;
                }
            }
        }
        c = (sum + vec3<i32>(n / 2)) / max(n, 1);
    } else {
        c = bytes(rgb_at(in.uv));
    }
    let y = f32(16 + ((66 * c.r + 129 * c.g + 25 * c.b + 128) >> 8u)) / 255.0;
    let u = f32(((-38 * c.r - 74 * c.g + 112 * c.b + 128) >> 8u) + 128) / 255.0;
    let v = f32(((112 * c.r - 94 * c.g - 18 * c.b + 128) >> 8u) + 128) / 255.0;
    if mode == 3u {
        return vec4<f32>(y, 0.0, 0.0, 1.0);
    }
    if mode == 4u {
        return vec4<f32>(u, 0.0, 0.0, 1.0);
    }
    if mode == 5u {
        return vec4<f32>(v, 0.0, 0.0, 1.0);
    }
    return vec4<f32>(u, v, 0.0, 1.0);
}
