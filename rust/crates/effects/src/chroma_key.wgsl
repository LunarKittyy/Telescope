// Chroma key (plugins/chroma_key.py's matte() and compose()), a pixel at a time. Python looks the colour up at half the size in
// three 256 x 256 tables (how much of the pixel stays, and its Cr and Cb with the screen's spill taken out), scales the answers
// back up and joins them to the full-size brightness. `keys` holds the same tables, red = alpha, green = Cr, blue = Cb, one
// texel for each Cr row and Cb column. The colour maths is OpenCV's own fixed-point, whole numbers all the way, because a table
// has a cliff in it and a level either way at the cliff is a different picture.
//
// backdrop.rgb is the colour behind the person, backdrop.w is 1 when a picture goes there instead, and picture is its size.

struct Params {
    backdrop: vec4<f32>,
    picture: vec2<f32>,
}

// cv2.cvtColor's BGR2YCrCb on 8-bit values, as (Y, Cr, Cb)
fn ycc(rgb: vec3<f32>) -> vec3<f32> {
    let c = vec3<i32>(round(rgb * 255.0));
    let y = (c.b * 1868 + c.g * 9617 + c.r * 4899 + 8192) >> 14u;
    let cr = ((c.r - y) * 11682 + 2105344) >> 14u;
    let cb = ((c.b - y) * 9241 + 2105344) >> 14u;
    return vec3<f32>(f32(y), f32(cr), f32(cb));
}

// The colour at half the size, which INTER_AREA makes of the pixels it covers (exactly four of them for an even size)
fn small_ycc(ij: vec2<i32>, small: vec2<f32>) -> vec3<f32> {
    let scale = frame.size / small;
    let lo = vec2<f32>(ij) * scale;
    let hi = lo + scale;
    let first = vec2<i32>(floor(lo));
    let last = vec2<i32>(ceil(hi));
    var sum = vec3<f32>(0.0);
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let wx = min(f32(x + 1), hi.x) - max(f32(x), lo.x);
            let at = (vec2<f32>(f32(x), f32(y)) + 0.5) / frame.size;
            sum += ycc(sample(at).rgb) * (wx * wy);
        }
    }
    return floor(sum / (scale.x * scale.y) + 0.5);
}

// What the three tables answer for the pixel at ij of the half-size picture: (alpha, Cr, Cb) as whole numbers
fn keyed(ij: vec2<i32>, small: vec2<f32>) -> vec3<f32> {
    let c = clamp(small_ycc(clamp(ij, vec2<i32>(0), vec2<i32>(small) - vec2<i32>(1)), small).yz, vec2<f32>(0.0), vec2<f32>(255.0));
    return round(sample_texture(keys, (vec2<f32>(c.y, c.x) + 0.5) / 256.0).rgb * 255.0);
}

// image.cover(): the picture scaled to fill the frame and cut evenly where it doesn't fit. Bigger than the frame it is averaged
// over what each pixel takes in, as INTER_AREA does; smaller it is scaled up linearly.
fn behind(px: vec2<f32>) -> vec3<f32> {
    if params.backdrop.w < 0.5 {
        return params.backdrop.rgb * 255.0;
    }
    let size = frame.size;
    let scale = max(size.x / params.picture.x, size.y / params.picture.y);
    let cover = max(size, round(params.picture * scale));
    let cut = floor((cover - size) / 2.0);
    if scale >= 1.0 {
        return round(sample_texture(picture, (px + cut) / cover).rgb * 255.0);
    }
    let ratio = params.picture / cover;
    let lo = (floor(px) + cut) * ratio;
    let hi = lo + ratio;
    let first = vec2<i32>(floor(lo));
    let last = min(vec2<i32>(ceil(hi)), first + vec2<i32>(12));
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let wx = min(f32(x + 1), hi.x) - max(f32(x), lo.x);
            sum += sample_texture(picture, (vec2<f32>(f32(x), f32(y)) + 0.5) / params.picture).rgb * (wx * wy);
            total += wx * wy;
        }
    }
    return round(sum / total * 255.0);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * frame.size;
    let small = max(floor(frame.size / 2.0), vec2<f32>(1.0));
    // cv2.resize's linear scaling back up from the half-size picture: pixel centres line up, the edge repeats
    let pos = px * small / frame.size - 0.5;
    let base = floor(pos);
    let f = pos - base;
    let i = vec2<i32>(base);
    let k = mix(mix(keyed(i, small), keyed(i + vec2<i32>(1, 0), small), f.x),
                mix(keyed(i + vec2<i32>(0, 1), small), keyed(i + vec2<i32>(1, 1), small), f.x), f.y);
    let alpha = round(k.x);
    let y = i32(ycc(sample(uv).rgb).x);
    let cr = i32(round(k.y)) - 128;
    let cb = i32(round(k.z)) - 128;
    // cv2's YCrCb2BGR
    let clean = vec3<f32>(clamp(vec3<i32>(y + ((cr * 22987 + 8192) >> 14u),
                                           y + ((cb * -5636 + cr * -11698 + 8192) >> 14u),
                                           y + ((cb * 29049 + 8192) >> 14u)), vec3<i32>(0), vec3<i32>(255)));
    // compose(): two multiplies by 1/255, each rounded, and an add
    let out = round(clean * (alpha / 255.0)) + round(behind(px) * ((255.0 - alpha) / 255.0));
    return vec4<f32>(min(out, vec3<f32>(255.0)) / 255.0, 1.0);
}
