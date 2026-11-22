//! What a ShaderStep's WGSL may expect and what the GPU executor has to give it, so plugin-host and media agree without knowing each other.
//!
//! A plugin writes `fn effect(uv: vec2<f32>) -> vec4<f32>` and, if it has settings, `struct Params { ... }` with scalar and vector fields.
//! assemble() wraps that in PRELUDE and the entry points; the executor compiles the result, binds group 0 (shared) and GROUP_STEP (the step's own)
//! and draws one full-frame triangle. A step that doesn't compile is logged once and passes frames through, like a failing CPU step.
//! Colours are RGBA 0..1 in the camera's own (sRGB-encoded) values, uv (0, 0) is the top left. The alpha effect() returns is dropped.
//!
//! A shader that calls person_mask(uv) or has_person_mask() gets the person mask of the picture as it reaches that step: how sure the
//! vision service is that a person is at uv, 0..1, or 0 everywhere while it can't say (has_person_mask() tells the two apart). Only then is a
//! mask worked out and bound, and it's uploaded only when it changes. Saying nothing about it costs a shader nothing. A mask user that isn't
//! first on the card sends the picture down once for the vision model; see uses_person_mask.
//!
//! A shader that calls blurred(uv) or blurred_weighted(uv) gets the picture shrunk by the step's uniform `blur_shrink` and Gaussian-blurred with `blur_sigma` there (blurred_weighted leaves out what the step texture `blur_weights` marks 0); a shader that defines a function of either name itself keeps its own and gets no helper; see uses_blurred.
//! A shader that calls previous(uv) or has_previous() gets what this same step drew for the stream's last frame, which is how a step averages
//! over time. has_previous() is false for the first frame, after the picture's size changed and when the last frame is too old to still be
//! the same scene (HISTORY_MAX_AGE_NS), and previous() is 0 then. The card only keeps a copy for a shader that asks; see uses_history.
//!
//! A shader that needs pictures worked out first (a shrunk copy, a running sum) defines `fn pass_0(at: vec2<i32>) -> vec4<f32>` (or `-> f32`), then pass_1 and so on, up to MAX_PASSES.
//! Each pass makes a picture of its own before effect() runs, at the size the step's uniform PASS_SIZES gives it (the picture's own when it gives none), `at` being the pixel it makes.
//! A vec4 pass is kept in 8 bits a channel like the picture, an f32 pass as one exact float. Later passes and effect() read pass k with `passed_k(at)`, which repeats the edge
//! pixel past the edges, and its size with `passed_size_k()`; see passes.

use crate::frame::ShaderStep;
use serde_json::Value;

/// The function every plugin shader defines: the colour at uv in the changed picture.
pub const ENTRY: &str = "effect";
/// The struct a shader declares for its uniforms (ShaderStep::uniforms, by field name).
pub const PARAMS: &str = "Params";
/// Entry points assemble() adds, for the executor's pipeline.
pub const VERTEX_ENTRY: &str = "telescope_vs";
pub const FRAGMENT_ENTRY: &str = "telescope_fs";

/// Group 0, the same for every step: the picture so far, its sampler (linear, clamped) and FrameInfo.
pub const BINDING_INPUT: u32 = 0;
pub const BINDING_SAMPLER: u32 = 1;
pub const BINDING_FRAME: u32 = 2;
/// The person mask: one channel, 1x1 and 0 when a step doesn't ask for it or there's none.
pub const BINDING_MASK: u32 = 3;
/// What the step drew for the last frame: the picture's size and RGBA, or 1x1 for a step that doesn't ask for it.
pub const BINDING_HISTORY: u32 = 4;
/// A last frame older than this (by the frames' own times) isn't used: the stream paused, or the step ran somewhere else for a while.
pub const HISTORY_MAX_AGE_NS: u64 = 250_000_000;
/// Group 1, the step's own: Params, then ShaderStep::textures in order.
pub const GROUP_STEP: u32 = 1;
pub const BINDING_PARAMS: u32 = 0;
pub const BINDING_FIRST_TEXTURE: u32 = 1;

/// Declared before the plugin's code. `frame` and `sample()` are what a shader uses; telescope_* names are the host's.
pub const PRELUDE: &str = "\
struct FrameInfo {
    size: vec2<f32>,
    time: f32,
    number: u32,
    has_mask: u32,
    has_history: u32,
}

@group(0) @binding(0) var telescope_input: texture_2d<f32>;
@group(0) @binding(1) var telescope_sampler: sampler;
@group(0) @binding(2) var<uniform> frame: FrameInfo;
@group(0) @binding(3) var telescope_mask: texture_2d<f32>;
@group(0) @binding(4) var telescope_history: texture_2d<f32>;

fn sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(telescope_input, telescope_sampler, uv, 0.0);
}

fn sample_texture(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(t, telescope_sampler, uv, 0.0);
}

fn person_mask(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(telescope_mask, telescope_sampler, uv, 0.0).r;
}

fn has_person_mask() -> bool {
    return frame.has_mask != 0u;
}

fn previous(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(telescope_history, telescope_sampler, uv, 0.0);
}

fn has_previous() -> bool {
    return frame.has_history != 0u;
}
";

/// Declared after PRELUDE for a shader that calls blurred() without defining it: the blur of the picture at its small size, read with linear filtering.
pub const BLURRED_HELPER: &str = "\
fn blurred(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(telescope_blurred, telescope_sampler, uv, 0.0);
}
";

/// The same for blurred_weighted().
pub const BLURRED_WEIGHTED_HELPER: &str = "\
fn blurred_weighted(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(telescope_blurred_weighted, telescope_sampler, uv, 0.0);
}
";

/// The step's uniform that says how many times smaller than the picture the blur is worked out, and the sigma it blurs with at that size.
pub const BLUR_SHRINK: &str = "blur_shrink";
pub const BLUR_SIGMA: &str = "blur_sigma";
/// The step's uniform that, set to 1, makes the card shrink an I420 picture's planes before it works out colours and lay the blur over the picture plane by plane, as the CPU way does: what the cutout (the step texture BLUR_CUTOUT, one channel, red) leaves is the picture's own samples.
pub const BLUR_PLANES: &str = "blur_planes";
pub const BLUR_CUTOUT: &str = "blur_cutout";
/// The step texture (RGBA, at the picture's own size) that takes the blur's place as what the cutout is laid over, for a step that sets BLUR_PLANES but has no blur of its own (Background's picture): the picture's I420 planes are laid over it plane by plane as over the blur.
pub const ROOM_PICTURE: &str = "room_picture";
/// The step texture (one channel, red, at the blurred size) that weights blurred_weighted: 255 where the picture counts, 0 where it's left out.
pub const BLUR_WEIGHTS: &str = "blur_weights";

/// Most passes a shader can define before effect().
pub const MAX_PASSES: usize = 8;
/// The step's uniform with each pass's size, [[width, height], ...]: a pass left out works at the picture's size.
pub const PASS_SIZES: &str = "pass_sizes";

/// What a pass gives: four channels kept in 8 bits as the picture is, or one exact number (32-bit float).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    Colour,
    Number,
}

const ENTRY_POINTS: &str = "
struct TelescopeVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn telescope_vs(@builtin(vertex_index) i: u32) -> TelescopeVertex {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: TelescopeVertex;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn telescope_fs(v: TelescopeVertex) -> @location(0) vec4<f32> {
    return vec4<f32>(effect(v.uv).rgb, 1.0);
}
";

/// A field of a shader's Params, where it sits in the uniform buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamField {
    pub name: String,
    pub kind: ParamKind,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    F32,
    I32,
    U32,
    Vec2,
    Vec3,
    Vec4,
    Mat4,
}

impl ParamKind {
    fn parse(ty: &str) -> Option<ParamKind> {
        Some(match ty.replace(' ', "").as_str() {
            "f32" => ParamKind::F32,
            "i32" => ParamKind::I32,
            "u32" => ParamKind::U32,
            "vec2<f32>" | "vec2f" => ParamKind::Vec2,
            "vec3<f32>" | "vec3f" => ParamKind::Vec3,
            "vec4<f32>" | "vec4f" => ParamKind::Vec4,
            "mat4x4<f32>" | "mat4x4f" => ParamKind::Mat4,
            _ => return None,
        })
    }

    /// (alignment, size) in the uniform address space.
    fn layout(self) -> (usize, usize) {
        match self {
            ParamKind::F32 | ParamKind::I32 | ParamKind::U32 => (4, 4),
            ParamKind::Vec2 => (8, 8),
            ParamKind::Vec3 => (16, 12),
            ParamKind::Vec4 => (16, 16),
            ParamKind::Mat4 => (16, 64),
        }
    }

    fn components(self) -> usize {
        match self {
            ParamKind::F32 | ParamKind::I32 | ParamKind::U32 => 1,
            ParamKind::Vec2 => 2,
            ParamKind::Vec3 => 3,
            ParamKind::Vec4 => 4,
            ParamKind::Mat4 => 16,
        }
    }
}

/// The source without // and /* */ comments, so a commented-out struct or function doesn't count.
fn strip_comments(wgsl: &str) -> String {
    let mut out = String::with_capacity(wgsl.len());
    let mut rest = wgsl;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.find('\n').map_or("", |i| &r[i..]);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.find("*/").map_or("", |i| &r[i + 2..]);
            out.push(' ');
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Where `word` stands as a whole word in code, followed (after spaces) by `then`.
fn find_word(code: &str, word: &str, then: char) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = code[from..].find(word).map(|i| i + from) {
        let before = code[..i].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        let after = code[i + word.len()..].trim_start();
        if before && after.starts_with(then) {
            return Some(i);
        }
        from = i + word.len();
    }
    None
}

/// The fields of the shader's `struct Params`, laid out as WGSL lays out a uniform buffer; empty when it has none.
pub fn params_layout(wgsl: &str) -> Result<Vec<ParamField>, String> {
    let code = strip_comments(wgsl);
    let Some(at) = find_word(&code, &format!("struct {PARAMS}"), '{') else { return Ok(Vec::new()) };
    let body_start = at + code[at..].find('{').unwrap() + 1;
    let body_end = body_start + code[body_start..].find('}').ok_or("struct Params has no closing }")?;
    let mut fields = Vec::new();
    let mut offset = 0usize;
    for part in code[body_start..body_end].split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, ty) = part.split_once(':').ok_or_else(|| format!("Params field {part:?} has no type"))?;
        let name = name.trim();
        if !is_ident(name) {
            return Err(format!("Params field {name:?} isn't a plain name"));
        }
        let kind = ParamKind::parse(ty.trim())
            .ok_or_else(|| format!("Params field {name} is {}; only f32, i32, u32, vec2/3/4<f32> and mat4x4<f32> are allowed", ty.trim()))?;
        let (align, size) = kind.layout();
        offset = offset.next_multiple_of(align);
        fields.push(ParamField { name: name.to_string(), kind, offset });
        offset += size;
    }
    Ok(fields)
}

/// The uniform buffer for Params from uniforms by field name: a missing value is 0, a bool is 0 or 1, a vector is an array, a mat4x4 16 numbers column by column.
pub fn pack_uniforms(wgsl: &str, uniforms: &Value) -> Result<Vec<u8>, String> {
    let fields = params_layout(wgsl)?;
    let Some(last) = fields.last() else { return Ok(Vec::new()) };
    let size = (last.offset + last.kind.layout().1).next_multiple_of(16);
    let mut buf = vec![0u8; size];
    for field in &fields {
        let Some(value) = uniforms.get(&field.name) else { continue };
        let values: Vec<&Value> = match value {
            Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        if values.len() != field.kind.components() {
            return Err(format!("uniform {} needs {} number(s)", field.name, field.kind.components()));
        }
        for (i, v) in values.into_iter().enumerate() {
            let n = match v {
                Value::Bool(b) => *b as u8 as f64,
                Value::Number(n) => n.as_f64().unwrap_or(0.0),
                _ => return Err(format!("uniform {} has to be a number", field.name)),
            };
            let bytes = match field.kind {
                ParamKind::I32 => (n as i32).to_le_bytes(),
                ParamKind::U32 => (n.max(0.0) as u32).to_le_bytes(),
                _ => (n as f32).to_le_bytes(),
            };
            let at = field.offset + 4 * i;
            buf[at..at + 4].copy_from_slice(&bytes);
        }
    }
    Ok(buf)
}

/// Whether a shader reads the person mask, so the executor only fetches one for those that do.
pub fn uses_person_mask(wgsl: &str) -> bool {
    let code = strip_comments(wgsl);
    find_word(&code, "person_mask", '(').is_some() || find_word(&code, "has_person_mask", '(').is_some()
}

/// Whether a shader reads what it drew for the last frame, so the executor only keeps a copy for those that do.
pub fn uses_history(wgsl: &str) -> bool {
    let code = strip_comments(wgsl);
    find_word(&code, "previous", '(').is_some() || find_word(&code, "has_previous", '(').is_some()
}

/// Whether the code defines `fn name(`, as a plugin's own helper of that name does.
fn defines_fn(code: &str, name: &str) -> bool {
    let mut from = 0;
    while let Some(at) = find_word(&code[from..], name, '(').map(|i| i + from) {
        let before = code[..at].trim_end();
        if before.ends_with("fn") && before[..before.len() - 2].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_')) {
            return true;
        }
        from = at + name.len();
    }
    false
}

/// Whether the shader calls the helper `name` and leaves it for the executor to provide: one that defines its own keeps it, as before the helpers existed.
fn wants_helper(code: &str, name: &str) -> bool {
    find_word(code, name, '(').is_some() && !defines_fn(code, name)
}

/// Whether a shader reads the blurred picture through blurred() or blurred_weighted() (not its own function of that name), so the executor only works one out for those that do.
pub fn uses_blurred(wgsl: &str) -> bool {
    let code = strip_comments(wgsl);
    wants_helper(&code, "blurred") || wants_helper(&code, "blurred_weighted")
}

/// The bindings (group GROUP_STEP) of the blurred picture and the weighted one after the step's own textures; None for a shader that doesn't ask.
pub fn blur_bindings(step: &ShaderStep) -> Option<(u32, u32)> {
    let first = BINDING_FIRST_TEXTURE + step.textures.len() as u32;
    uses_blurred(&step.wgsl).then_some((first, first + 1))
}

/// The passes a shader defines, fn pass_0 onwards with no gap, and what each gives (by its return type, f32 or vec4<f32>).
pub fn passes(wgsl: &str) -> Vec<PassKind> {
    let code = strip_comments(wgsl);
    let mut out = Vec::new();
    while out.len() < MAX_PASSES {
        let name = format!("pass_{}", out.len());
        if !defines_fn(&code, &name) {
            break;
        }
        let at = find_word(&code, &format!("fn {name}"), '(').or_else(|| code.find(&format!("{name}("))).unwrap_or(0);
        let ret = code[at..].split_once(')').and_then(|(_, r)| r.split_once('{')).map(|(r, _)| r.trim().trim_start_matches("->").trim().replace(' ', ""));
        out.push(if ret.as_deref() == Some("f32") { PassKind::Number } else { PassKind::Colour });
    }
    out
}

/// The binding (group GROUP_STEP) of the first pass's picture, after the step's own textures and the blurred copies.
pub fn pass_binding(step: &ShaderStep) -> u32 {
    BINDING_FIRST_TEXTURE + step.textures.len() as u32 + if blur_bindings(step).is_some() { 2 } else { 0 }
}

/// The fragment entry point that runs pass k.
pub fn pass_entry(k: usize) -> String {
    format!("telescope_pass_{k}")
}

/// The size of pass k for a picture of w x h: what PASS_SIZES says, or the picture's own, and at least 1 x 1.
pub fn pass_size(uniforms: &Value, k: usize, w: u32, h: u32) -> (u32, u32) {
    let given = uniforms.get(PASS_SIZES).and_then(|s| s.get(k)).and_then(|s| Some((s.get(0)?.as_f64()?, s.get(1)?.as_f64()?)));
    given.map_or((w.max(1), h.max(1)), |(pw, ph)| ((pw.max(1.0) as u32), (ph.max(1.0) as u32)))
}

/// The size the blur is worked out at for a picture of w x h shrunk by `shrink`.
pub fn blur_size(w: u32, h: u32, shrink: u32) -> (u32, u32) {
    let shrink = shrink.max(1);
    ((w / shrink).max(1), (h / shrink).max(1))
}

/// How many taps the blur's kernel has for a sigma: 3 sigma either way, rounded to an odd number.
pub fn blur_taps(sigma: f64) -> u32 {
    ((sigma * 3.0 * 2.0 + 1.0).round_ties_even() as u32) | 1
}

/// FrameInfo's buffer: the frame's size in pixels, the stream's time in seconds, the frame's number and whether a mask and a last frame are bound.
pub fn frame_uniform(width: u32, height: u32, time_s: f32, number: u64, has_mask: bool, has_history: bool) -> [u8; 24] {
    let mut buf = [0u8; 24];
    buf[0..4].copy_from_slice(&(width as f32).to_le_bytes());
    buf[4..8].copy_from_slice(&(height as f32).to_le_bytes());
    buf[8..12].copy_from_slice(&time_s.to_le_bytes());
    buf[12..16].copy_from_slice(&(number as u32).to_le_bytes());
    buf[16..20].copy_from_slice(&(has_mask as u32).to_le_bytes());
    buf[20..24].copy_from_slice(&(has_history as u32).to_le_bytes());
    buf
}

/// The whole module to compile: PRELUDE, Params' binding, a binding per extra texture, the plugin's code and the entry points.
pub fn assemble(step: &ShaderStep) -> Result<String, String> {
    let mut out = assemble_body(step)? + ENTRY_POINTS;
    for (k, kind) in passes(&step.wgsl).into_iter().enumerate() {
        let value = match kind {
            PassKind::Colour => format!("pass_{k}(at)"),
            PassKind::Number => format!("vec4<f32>(pass_{k}(at), 0.0, 0.0, 1.0)"),
        };
        out += &format!("\n@fragment\nfn {}(v: TelescopeVertex) -> @location(0) vec4<f32> {{\n    let at = vec2<i32>(floor(v.position.xy));\n    return {value};\n}}\n", pass_entry(k));
    }
    Ok(out)
}

fn assemble_body(step: &ShaderStep) -> Result<String, String> {
    let code = strip_comments(&step.wgsl);
    if find_word(&code, &format!("fn {ENTRY}"), '(').is_none() {
        return Err(format!("the shader has no fn {ENTRY}(uv: vec2<f32>) -> vec4<f32>"));
    }
    if code.contains("@group") || code.contains("@binding") {
        return Err("the shader declares bindings of its own; use Params and textures instead".into());
    }
    let mut out = String::from(PRELUDE);
    if !params_layout(&step.wgsl)?.is_empty() {
        out += &format!("\n@group({GROUP_STEP}) @binding({BINDING_PARAMS}) var<uniform> params: {PARAMS};\n");
    }
    for (i, (name, _)) in step.textures.iter().enumerate() {
        if !is_ident(name) || name.starts_with("telescope_") {
            return Err(format!("texture name {name:?} can't be used"));
        }
        out += &format!("@group({GROUP_STEP}) @binding({}) var {name}: texture_2d<f32>;\n", BINDING_FIRST_TEXTURE + i as u32);
    }
    if let Some((plain, weighted)) = blur_bindings(step) {
        out += &format!("@group({GROUP_STEP}) @binding({plain}) var telescope_blurred: texture_2d<f32>;\n");
        out += &format!("@group({GROUP_STEP}) @binding({weighted}) var telescope_blurred_weighted: texture_2d<f32>;\n");
        // Each helper only where the plugin hasn't defined one of that name
        if wants_helper(&code, "blurred") {
            out += BLURRED_HELPER;
        }
        if wants_helper(&code, "blurred_weighted") {
            out += BLURRED_WEIGHTED_HELPER;
        }
    }
    let first = pass_binding(step);
    for (k, kind) in passes(&step.wgsl).into_iter().enumerate() {
        let (ty, pick) = match kind {
            PassKind::Colour => ("vec4<f32>", ""),
            PassKind::Number => ("f32", ".r"),
        };
        out += &format!("@group({GROUP_STEP}) @binding({}) var telescope_passed_{k}: texture_2d<f32>;\n", first + k as u32);
        out += &format!("fn passed_size_{k}() -> vec2<i32> {{\n    return vec2<i32>(textureDimensions(telescope_passed_{k}));\n}}\n");
        out += &format!("fn passed_{k}(at: vec2<i32>) -> {ty} {{\n    return textureLoad(telescope_passed_{k}, clamp(at, vec2<i32>(0), passed_size_{k}() - vec2<i32>(1)), 0){pick};\n}}\n");
    }
    out += "\n";
    out += &step.wgsl;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, PixelFormat};
    use serde_json::json;

    const GREY: &str = "struct Params {\n    strength: f32,\n    tint: vec3<f32>, // a colour\n    steps: u32,\n}\n\
        fn effect(uv: vec2<f32>) -> vec4<f32> {\n    let c = sample(uv);\n    return c;\n}\n";

    #[test]
    fn params_are_laid_out_like_a_uniform_buffer() {
        let fields = params_layout(GREY).unwrap();
        let at: Vec<(&str, usize)> = fields.iter().map(|f| (f.name.as_str(), f.offset)).collect();
        assert_eq!(at, [("strength", 0), ("tint", 16), ("steps", 28)]);
        let buf = pack_uniforms(GREY, &json!({"strength": 0.5, "tint": [1, 0, 0.25], "steps": 3})).unwrap();
        assert_eq!(buf.len(), 32);
        assert_eq!(f32::from_le_bytes(buf[0..4].try_into().unwrap()), 0.5);
        assert_eq!(f32::from_le_bytes(buf[24..28].try_into().unwrap()), 0.25);
        assert_eq!(u32::from_le_bytes(buf[28..32].try_into().unwrap()), 3);
        assert!(pack_uniforms(GREY, &json!({"tint": 1})).is_err());
    }

    #[test]
    fn a_shader_without_params_has_no_buffer() {
        let wgsl = "// struct Params { x: f32 }\nfn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";
        assert_eq!(pack_uniforms(wgsl, &json!({"x": 1})).unwrap(), Vec::<u8>::new());
        let step = ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: json!({}), textures: Vec::new() };
        let module = assemble(&step).unwrap();
        assert!(!module.contains("var<uniform> params") && module.contains("fn telescope_fs"));
    }

    #[test]
    fn only_a_shader_that_calls_for_the_mask_counts_as_using_it() {
        let plain = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";
        assert!(!uses_person_mask(plain));
        assert!(!uses_person_mask(&format!("// person_mask(uv)\n{plain}")));
        assert!(uses_person_mask("fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(person_mask(uv)); }"));
        assert!(uses_person_mask("fn effect(uv: vec2<f32>) -> vec4<f32> { if has_person_mask() { return sample(uv); } return vec4<f32>(0.0); }"));
        let buf = frame_uniform(2, 2, 0.0, 1, true, false);
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 1);
    }

    #[test]
    fn only_a_shader_that_asks_for_the_last_frame_gets_one_kept() {
        let plain = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";
        assert!(!uses_history(plain));
        assert!(!uses_history(&format!("// previous(uv)\n{plain}")));
        assert!(!uses_history("fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * previous_gain; }"));
        assert!(uses_history("fn effect(uv: vec2<f32>) -> vec4<f32> { return mix(sample(uv), previous(uv), 0.5); }"));
        assert!(uses_history("fn effect(uv: vec2<f32>) -> vec4<f32> { if has_previous() { return previous(uv); } return sample(uv); }"));
        let buf = frame_uniform(2, 2, 0.0, 1, false, true);
        assert_eq!((u32::from_le_bytes(buf[16..20].try_into().unwrap()), u32::from_le_bytes(buf[20..24].try_into().unwrap())), (0, 1));
    }

    #[test]
    fn only_a_shader_that_asks_for_the_blur_gets_one() {
        let plain = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";
        let step = |wgsl: &str| ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: json!({}), textures: vec![("soft".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0; 4]))] };
        assert!(!uses_blurred(plain) && !uses_blurred(&format!("// blurred(uv)\n{plain}")));
        assert!(uses_blurred("fn effect(uv: vec2<f32>) -> vec4<f32> { return blurred(uv); }"));
        assert!(uses_blurred("fn effect(uv: vec2<f32>) -> vec4<f32> { return blurred_weighted(uv); }"));
        assert!(!uses_blurred("fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * blurred_gain; }"));
        // A plugin that has a blurred() of its own keeps it, with no helper or blur made for it
        let own = "fn blurred(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }\nfn effect(uv: vec2<f32>) -> vec4<f32> { return blurred(uv); }";
        assert!(!uses_blurred(own) && !uses_blurred(&own.replace("fn blurred(", "fn  blurred (")) && blur_bindings(&step(own)).is_none());
        assert!(!assemble(&step(own)).unwrap().contains("telescope_blurred"));
        // and one that has only that one still gets the other
        let mixed = format!("{own}\nfn other(uv: vec2<f32>) -> vec4<f32> {{ return blurred_weighted(uv); }}");
        let module = assemble(&step(&mixed)).unwrap();
        assert!(uses_blurred(&mixed) && module.matches("fn blurred(").count() == 1 && module.matches("fn blurred_weighted(").count() == 1);
        assert_eq!(blur_bindings(&step(plain)), None);
        let asking = step("fn effect(uv: vec2<f32>) -> vec4<f32> { return mix(blurred_weighted(uv), sample(uv), 0.5); }");
        assert_eq!(blur_bindings(&asking), Some((2, 3)));
        let module = assemble(&asking).unwrap();
        assert!(module.contains("@binding(2) var telescope_blurred:") && module.contains("@binding(3) var telescope_blurred_weighted:") && module.contains("fn blurred_weighted"));
        assert!(!assemble(&step(plain)).unwrap().contains("telescope_blurred"));
    }

    #[test]
    fn the_blur_works_at_the_size_and_with_the_kernel_the_cpu_blur_does() {
        assert_eq!(blur_size(3840, 2160, 8), (480, 270));
        assert_eq!(blur_size(1280, 720, 8), (160, 90));
        assert_eq!(blur_size(5, 3, 8), (1, 1));
        assert_eq!(blur_taps(3.5), 23);
        assert_eq!(blur_taps(6.5), 41);
        assert_eq!(blur_taps(1.1), 9);
    }

    #[test]
    fn passes_are_counted_from_0_with_what_each_gives_and_get_their_pictures_after_the_rest() {
        let wgsl = "fn pass_0(at: vec2<i32>) -> vec4<f32> { return sample(vec2<f32>(at)); }\n\
            fn pass_1(at: vec2<i32>) -> f32 { return passed_0(at).r; }\n\
            // fn pass_2(at: vec2<i32>) -> f32 { return 0.0; }\n\
            fn pass_3(at: vec2<i32>) -> f32 { return 0.0; }\n\
            fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(passed_1(vec2<i32>(0))); }";
        // a commented-out pass_2 ends the list, so pass_3 is only a function
        assert_eq!(passes(wgsl), [PassKind::Colour, PassKind::Number]);
        assert!(passes("fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }").is_empty());
        let step = ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: json!({}), textures: vec![("logo".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0; 4]))] };
        assert_eq!(pass_binding(&step), 2);
        let module = assemble(&step).unwrap();
        assert!(module.contains("@binding(2) var telescope_passed_0:") && module.contains("@binding(3) var telescope_passed_1:"));
        assert!(module.contains("fn passed_1(at: vec2<i32>) -> f32") && module.contains("fn telescope_pass_1(") && !module.contains("telescope_pass_2"));
        assert_eq!(pass_size(&json!({"pass_sizes": [[40, 20]]}), 0, 1920, 1080), (40, 20));
        assert_eq!(pass_size(&json!({"pass_sizes": [[40, 20]]}), 1, 1920, 1080), (1920, 1080));
        assert_eq!(pass_size(&json!({"pass_sizes": [[0, 0]]}), 0, 1920, 1080), (1, 1));
    }

    #[test]
    fn assembling_checks_what_the_executor_relies_on() {
        let step = |wgsl: &str| ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: json!({}), textures: Vec::new() };
        assert!(assemble(&step("fn other() {}")).unwrap_err().contains("fn effect"));
        assert!(assemble(&step("@group(0) @binding(9) var x: f32;\nfn effect(uv: vec2<f32>) -> vec4<f32> { return vec4(0.0); }")).is_err());
        assert!(params_layout("struct Params { m: mat3x3<f32> }").unwrap_err().contains("mat3x3"));
        assert_eq!(pack_uniforms("struct Params { a: f32, m: mat4x4<f32> }", &json!({"m": vec![1; 16]})).unwrap().len(), 80);
        let module = assemble(&step(GREY)).unwrap();
        assert!(module.contains("@group(1) @binding(0) var<uniform> params: Params;"));
    }
}
