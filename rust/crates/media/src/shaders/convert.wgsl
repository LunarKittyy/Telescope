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
    let mode = settings.x;
    if mode <= 2u {
        let taps = settings.y;
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
    // Back to YUV from the RGBA picture in p0. The chroma targets are half size, so their uv lands between four pixels
    // and the linear sampler averages them.
    let c = rgb_at(in.uv) * 255.0;
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
