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
//! A shader that calls previous(uv) or has_previous() gets what this same step drew for the stream's last frame, which is how a step averages
//! over time. has_previous() is false for the first frame, after the picture's size changed and when the last frame is too old to still be
//! the same scene (HISTORY_MAX_AGE_NS), and previous() is 0 then. The card only keeps a copy for a shader that asks; see uses_history.

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
    Ok(assemble_body(step)? + ENTRY_POINTS)
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
    out += "\n";
    out += &step.wgsl;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let step = ShaderStep { wgsl: wgsl.into(), uniforms: json!({}), textures: Vec::new() };
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
    fn assembling_checks_what_the_executor_relies_on() {
        let step = |wgsl: &str| ShaderStep { wgsl: wgsl.into(), uniforms: json!({}), textures: Vec::new() };
        assert!(assemble(&step("fn other() {}")).unwrap_err().contains("fn effect"));
        assert!(assemble(&step("@group(0) @binding(9) var x: f32;\nfn effect(uv: vec2<f32>) -> vec4<f32> { return vec4(0.0); }")).is_err());
        assert!(params_layout("struct Params { m: mat3x3<f32> }").unwrap_err().contains("mat3x3"));
        assert_eq!(pack_uniforms("struct Params { a: f32, m: mat4x4<f32> }", &json!({"m": vec![1; 16]})).unwrap().len(), 80);
        let module = assemble(&step(GREY)).unwrap();
        assert!(module.contains("@group(1) @binding(0) var<uniform> params: Params;"));
    }
}
