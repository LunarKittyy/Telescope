// Pictures onto and off the card: YUV to RGBA (scaling as it goes, for the preview) and RGBA back to YUV planes.
// BT.601 with video range, the same maths as OpenCV's I420 conversions and swscale's default the Python app used.

@group(0) @binding(0) var p0: texture_2d<f32>;
@group(0) @binding(1) var p1: texture_2d<f32>;
@group(0) @binding(2) var p2: texture_2d<f32>;
@group(0) @binding(3) var smp: sampler;
// mode, taps a side per output pixel, source width, source height
@group(0) @binding(4) var<uniform> settings: vec4<u32>;

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
    let c = 1.164383 * (y * 255.0 - 16.0);
    let d = u * 255.0 - 128.0;
    let e = v * 255.0 - 128.0;
    return clamp(vec3<f32>(c + 1.596027 * e, c - 0.391762 * d - 0.812968 * e, c + 2.017232 * d) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The same by index, as the CPU reads it: pixel (x, y) takes the chroma sample (x / 2, y / 2), whatever the size
fn source_by_block(uv: vec2<f32>, mode: u32) -> vec3<f32> {
    let size = vec2<i32>(i32(settings.z), i32(settings.w));
    let at = clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2<i32>(0), size - 1);
    let y = textureLoad(p0, at, 0).r;
    if mode == 0u {
        return yuv_to_rgb(y, textureLoad(p1, at / 2, 0).r, textureLoad(p2, at / 2, 0).r);
    }
    let c = textureLoad(p1, at / 2, 0).rg;
    return yuv_to_rgb(y, c.r, c.g);
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

fn rgb_at(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(p0, smp, uv, 0.0).rgb;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let mode = settings.x & 7u;
    if mode <= 2u {
        let taps = settings.y;
        if (settings.x & 8u) != 0u && mode <= 1u && taps <= 1u {
            return vec4<f32>(source_by_block(in.uv, mode), 1.0);
        }
        if taps <= 1u {
            return vec4<f32>(source(in.uv, mode), 1.0);
        }
        // One output pixel covers this much of the picture; average a taps x taps grid inside it
        let step = vec2<f32>(dpdxFine(in.uv.x), dpdyFine(in.uv.y)) / f32(taps);
        var sum = vec3<f32>(0.0);
        for (var j = 0u; j < taps; j++) {
            for (var i = 0u; i < taps; i++) {
                let off = (vec2<f32>(f32(i), f32(j)) + 0.5 - f32(taps) * 0.5) * step;
                sum += source(in.uv + off, mode);
            }
        }
        return vec4<f32>(sum / f32(taps * taps), 1.0);
    }
    // Back to YUV from the RGBA picture in p0. A chroma target pixel is the average of its own 2 x 2 source pixels, found by index so odd
    // sizes line up with the CPU: the last column and row of an odd picture average only the pixels there are.
    var c = rgb_at(in.uv) * 255.0;
    if mode >= 4u {
        let at = vec2<i32>(in.pos.xy) * 2;
        let last = vec2<i32>(i32(settings.z) - 1, i32(settings.w) - 1);
        var sum = vec3<f32>(0.0);
        var n = 0.0;
        for (var j = 0; j < 2; j++) {
            for (var i = 0; i < 2; i++) {
                let q = at + vec2<i32>(i, j);
                if q.x <= last.x && q.y <= last.y {
                    sum += textureLoad(p0, q, 0).rgb;
                    n += 1.0;
                }
            }
        }
        c = sum * 255.0 / max(n, 1.0);
    }
    let y = (16.0 + 0.256788 * c.r + 0.504129 * c.g + 0.097906 * c.b) / 255.0;
    let u = (128.0 - 0.148223 * c.r - 0.290993 * c.g + 0.439216 * c.b) / 255.0;
    let v = (128.0 + 0.439216 * c.r - 0.367788 * c.g - 0.071427 * c.b) / 255.0;
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
