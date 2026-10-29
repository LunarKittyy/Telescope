// Game Boy (fun_effects.GameBoy): the picture shrunk to 144 lines by averaging what each small pixel covers, turned to grey, dithered
// with a 4 x 4 Bayer pattern into four shades of green, and scaled back up with nearest-neighbour.

const LINES: f32 = 144.0;
const BAYER = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);

fn shade(level: i32) -> vec3<f32> {
    if level == 0 { return vec3<f32>(15.0, 56.0, 15.0); }
    if level == 1 { return vec3<f32>(48.0, 98.0, 48.0); }
    if level == 2 { return vec3<f32>(139.0, 172.0, 15.0); }
    return vec3<f32>(155.0, 188.0, 15.0);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let size = frame.size;
    let small = vec2<f32>(max(1.0, round(size.x * LINES / size.y)), LINES);
    // Whole numbers, as nearest-neighbour has them: a float 0.8 times 5 would land just under 4
    let cell = vec2<i32>((vec2<u32>(floor(uv * size)) * vec2<u32>(small)) / vec2<u32>(size));
    let scale = size / small;
    let lo = vec2<f32>(cell) * scale;
    let hi = lo + scale;
    let first = vec2<i32>(floor(lo));
    let last = vec2<i32>(ceil(hi));
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var y = first.y; y < last.y; y++) {
        let wy = min(f32(y + 1), hi.y) - max(f32(y), lo.y);
        for (var x = first.x; x < last.x; x++) {
            let wx = min(f32(x + 1), hi.x) - max(f32(x), lo.x);
            sum += sample((vec2<f32>(f32(x), f32(y)) + 0.5) / size).rgb * (wx * wy);
            total += wx * wy;
        }
    }
    let rgb = round(sum / total * 255.0);
    let grey = floor(0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b + 0.5);
    let threshold = BAYER[(cell.y % 4) * 4 + cell.x % 4] / 16.0 - 0.5;
    let level = i32(floor(clamp(grey / 255.0 * 3.0 + threshold + 0.5, 0.0, 3.0)));
    return vec4<f32>(shade(level) / 255.0, 1.0);
}
