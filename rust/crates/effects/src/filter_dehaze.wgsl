// Dehaze, after the dark channel prior: in a clear picture some colour of every small patch is close to black, so how bright the
// darkest colour of a patch is says how much haze lies over it. The air light is taken as nearly white, which is what a lamp, a window
// or a dirty lens does. The patch is a 5 x 5 grid of taps `gap` whole pixels apart (filters_cpu.rs::dehaze looks at the same pixels).
// Haze adds light, and light adds up in linear values, so the camera's own (gamma encoded) values go to linear light, the haze comes
// out there and the result goes back.

struct Params {
    strength: f32,
}

const AIR: f32 = 0.92;
const MOST: f32 = 0.6;
const FLOOR: f32 = 0.25;
const GAMMA: f32 = 2.2;

fn lowest(uv: vec2<f32>) -> f32 {
    let c = sample(uv).rgb;
    return min(c.r, min(c.g, c.b)) / AIR;
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv).rgb;
    let px = 1.0 / frame.size;
    let gap = max(1.0, floor(5.0 * frame.size.y / 1080.0 + 0.5));
    var dark = 1.0e9;
    for (var j = -2; j <= 2; j++) {
        for (var i = -2; i <= 2; i++) {
            dark = min(dark, lowest(uv + vec2<f32>(f32(i), f32(j)) * gap * px));
        }
    }
    let t = max(1.0 - params.strength * MOST * pow(dark, GAMMA), FLOOR);
    let air = pow(AIR, GAMMA);
    let j = clamp((pow(c, vec3<f32>(GAMMA)) - air) / t + air, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(pow(j, vec3<f32>(1.0 / GAMMA)), 1.0);
}
