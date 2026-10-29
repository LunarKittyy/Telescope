//! The colour pipeline (telescope.colour.ColourPipeline): every colour change plugins ask for, put one after the other into a
//! single LUT per stream and run on the graphics card as one shader step in the Colour stage.
//!
//! A plugin hands over each change already baked (ColourStage: a 3D lattice, and 256 exact entries per channel when each
//! channel only depends on itself). A pipeline of only such stages becomes exact tables, anything else a 3D LUT of 33 or 65
//! points, both with every stage's output clipped to 0..1 before the next one, as colour.bake does. The LUT goes up as two
//! RGBA pictures holding each value as 16 bits (high byte, low byte), read with textureLoad, so the lattice isn't rounded
//! to whole levels before it's interpolated.

use crate::frame::{Frame, PixelFormat, ShaderStep};
use crate::host::ColourStage;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// The 3D LUT sizes the pipeline bakes at: points per side.
pub const SIZES: [u32; 2] = [33, 65];
/// Entries of an exact per-channel table.
pub const LEVELS: usize = 256;
/// The line between the two halves of a compare (Python's _SPLIT_LINE), as grey.
pub const SPLIT_LINE: u8 = 235;

/// Every point of an n-point 3D LUT as RGB triples 0..1, blue changing slowest and red fastest.
pub fn lattice(n: usize) -> Vec<f32> {
    let step = |i: usize| if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 };
    let mut out = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                out.extend([step(r), step(g), step(b)]);
            }
        }
    }
    out
}

/// cube (n points a side, [b][g][r], RGB out) at rgb (each channel clamped to 0..1), interpolated trilinearly.
pub fn sample_cube(cube: &[f32], n: usize, rgb: [f32; 3]) -> [f32; 3] {
    let n1 = n.saturating_sub(1) as f32;
    let pos = rgb.map(|v| v.clamp(0.0, 1.0) * n1);
    let lo = pos.map(|p| (p as usize).min(n.saturating_sub(2)));
    let t = [pos[0] - lo[0] as f32, pos[1] - lo[1] as f32, pos[2] - lo[2] as f32];
    let mut out = [0.0f32; 3];
    for db in 0..2 {
        for dg in 0..2 {
            for dr in 0..2 {
                let w = (if db == 1 { t[2] } else { 1.0 - t[2] }) * (if dg == 1 { t[1] } else { 1.0 - t[1] })
                    * (if dr == 1 { t[0] } else { 1.0 - t[0] });
                let at = (((lo[2] + db).min(n - 1) * n + (lo[1] + dg).min(n - 1)) * n + (lo[0] + dr).min(n - 1)) * 3;
                for c in 0..3 {
                    out[c] += cube[at + c] * w;
                }
            }
        }
    }
    out
}

/// A per-channel table (256 RGB triples) at rgb, each channel interpolated between its two nearest levels.
fn sample_table(table: &[f32], rgb: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let pos = rgb[c].clamp(0.0, 1.0) * (LEVELS - 1) as f32;
        let lo = (pos as usize).min(LEVELS - 2);
        let t = pos - lo as f32;
        out[c] = table[lo * 3 + c] * (1.0 - t) + table[(lo + 1) * 3 + c] * t;
    }
    out
}

fn sound(stage: &ColourStage) -> bool {
    let n = stage.size as usize;
    n >= 2 && stage.rgb.len() == n * n * n * 3 && stage.table.as_ref().is_none_or(|t| t.len() == LEVELS * 3)
}

/// One stage on a list of RGB triples, clipped to 0..1 afterwards (NaN becomes 0).
fn run(stage: &ColourStage, rgb: &mut [f32]) {
    let n = stage.size as usize;
    for px in rgb.chunks_exact_mut(3) {
        let at = [px[0], px[1], px[2]];
        let out = match &stage.table {
            Some(table) => sample_table(table, at),
            None => sample_cube(&stage.rgb, n, at),
        };
        for c in 0..3 {
            px[c] = if out[c].is_nan() { 0.0 } else { out[c].clamp(0.0, 1.0) };
        }
    }
}

/// The stages one after the other as one transform (colour.Lut).
#[derive(Clone, Debug, PartialEq)]
pub enum Baked {
    /// 256 RGB triples: the output for each input level of every channel.
    Table(Vec<f32>),
    /// size³ RGB triples, [b][g][r].
    Cube { size: usize, rgb: Vec<f32> },
}

impl Baked {
    /// "1D" or "3D 33", as the diagnostics name it.
    pub fn kind(&self) -> String {
        match self {
            Baked::Table(_) => "1D".into(),
            Baked::Cube { size, .. } => format!("3D {size}"),
        }
    }

    /// One pixel of 8-bit RGB through it, rounded back to 8 bits: what the shader does, for tests and checks.
    pub fn apply(&self, px: [u8; 3]) -> [u8; 3] {
        let x = px.map(|v| v as f32 / 255.0);
        let out = match self {
            Baked::Table(t) => {
                let mut o = [0.0f32; 3];
                for c in 0..3 {
                    o[c] = t[px[c] as usize * 3 + c];
                }
                o
            }
            Baked::Cube { size, rgb } => sample_cube(rgb, *size, x),
        };
        out.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    }
}

/// The stages in order as one LUT (None for none): exact tables when every stage is per channel, else a 3D LUT with the most
/// points any stage asks for (or `size`). A stage whose data doesn't fit its size is left out and logged.
pub fn bake(stages: &[&ColourStage], size: Option<u32>) -> Option<Baked> {
    if stages.is_empty() {
        return None;
    }
    let flat = stages.iter().all(|s| s.table.is_some()) && size.is_none();
    let mut rgb: Vec<f32>;
    let n;
    if flat {
        n = 0;
        rgb = (0..LEVELS).flat_map(|i| [i as f32 / (LEVELS - 1) as f32; 3]).collect();
    } else {
        let asked = size.unwrap_or_else(|| stages.iter().map(|s| s.size).max().unwrap_or(SIZES[0]));
        n = if asked > SIZES[0] { SIZES[1] } else { SIZES[0] } as usize;
        rgb = lattice(n);
    }
    for stage in stages {
        if !sound(stage) {
            log::error!("A colour stage failed; leaving it out (its data doesn't fit its size)");
            continue;
        }
        run(stage, &mut rgb);
    }
    Some(if flat { Baked::Table(rgb) } else { Baked::Cube { size: n, rgb } })
}

const COMMON: &str = "
fn decode(hi: vec3<f32>, lo: vec3<f32>) -> vec3<f32> {
    return (hi * 65280.0 + lo * 255.0) / 65535.0;
}
";

const TABLE_WGSL: &str = "
fn level(i: i32) -> vec3<f32> {
    return decode(textureLoad(lut_hi, vec2<i32>(i, 0), 0).rgb, textureLoad(lut_lo, vec2<i32>(i, 0), 0).rgb);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = clamp(sample(uv).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let i = vec3<i32>(round(c * 255.0));
    return vec4<f32>(level(i.r).r, level(i.g).g, level(i.b).b, 1.0);
}
";

const CUBE_WGSL: &str = "
struct Params {
    size: f32,
}

fn at(r: i32, g: i32, b: i32) -> vec3<f32> {
    let n = i32(params.size);
    let p = vec2<i32>(b * n + r, g);
    return decode(textureLoad(lut_hi, p, 0).rgb, textureLoad(lut_lo, p, 0).rgb);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = clamp(sample(uv).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let n = params.size;
    let pos = c * (n - 1.0);
    let lo = min(floor(pos), vec3<f32>(n - 2.0));
    let t = pos - lo;
    let i = vec3<i32>(lo);
    let r0 = mix(at(i.x, i.y, i.z), at(i.x + 1, i.y, i.z), t.x);
    let r1 = mix(at(i.x, i.y + 1, i.z), at(i.x + 1, i.y + 1, i.z), t.x);
    let r2 = mix(at(i.x, i.y, i.z + 1), at(i.x + 1, i.y, i.z + 1), t.x);
    let r3 = mix(at(i.x, i.y + 1, i.z + 1), at(i.x + 1, i.y + 1, i.z + 1), t.x);
    return vec4<f32>(mix(mix(r0, r1, t.y), mix(r2, r3, t.y), t.z), 1.0);
}
";

/// An RGBA picture w x h with each value of `values` (RGB triples, 0..1) as 16 bits: the high bytes and the low bytes.
fn pictures(values: &[f32], w: u32, h: u32) -> (Frame, Frame) {
    let (mut hi, mut lo) = (Vec::with_capacity(values.len() / 3 * 4), Vec::with_capacity(values.len() / 3 * 4));
    for px in values.chunks_exact(3) {
        for v in px {
            let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u32;
            hi.push((q >> 8) as u8);
            lo.push((q & 255) as u8);
        }
        hi.push(255);
        lo.push(255);
    }
    (Frame::new(w, h, PixelFormat::Rgba, hi), Frame::new(w, h, PixelFormat::Rgba, lo))
}

impl Baked {
    /// The shader step that applies it. A cube is laid out as one picture with y the green and x the blue * n + red.
    pub fn shader(&self) -> ShaderStep {
        match self {
            Baked::Table(t) => {
                let (hi, lo) = pictures(t, LEVELS as u32, 1);
                ShaderStep { wgsl: format!("{COMMON}{TABLE_WGSL}"), uniforms: json!({}), textures: vec![("lut_hi".into(), hi), ("lut_lo".into(), lo)] }
            }
            Baked::Cube { size, rgb } => {
                let n = *size;
                let mut tile = Vec::with_capacity(rgb.len());
                for g in 0..n {
                    for b in 0..n {
                        for r in 0..n {
                            let at = ((b * n + g) * n + r) * 3;
                            tile.extend_from_slice(&rgb[at..at + 3]);
                        }
                    }
                }
                let (hi, lo) = pictures(&tile, (n * n) as u32, n as u32);
                ShaderStep { wgsl: format!("{COMMON}{CUBE_WGSL}"), uniforms: json!({"size": n}), textures: vec![("lut_hi".into(), hi), ("lut_lo".into(), lo)] }
            }
        }
    }
}

#[derive(Clone)]
struct Entry {
    order: i32,
    owner: String,
    stage: ColourStage,
}

/// What a stage is told apart by when baking again would give the same LUT: the data it holds. The Arcs are kept, so a freed
/// stage's address can't be taken by another one and pass for it (colour.py keys on the stage objects themselves).
#[derive(Clone)]
struct Identity(Vec<(Arc<Vec<f32>>, Option<Arc<Vec<f32>>>)>);

impl Identity {
    fn of(stages: &[&ColourStage]) -> Identity {
        Identity(stages.iter().map(|s| (s.rgb.clone(), s.table.clone())).collect())
    }

    fn same(&self, other: &Identity) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(&other.0).all(|((a, ta), (b, tb))| {
                Arc::ptr_eq(a, b)
                    && match (ta, tb) {
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        (None, None) => true,
                        _ => false,
                    }
            })
    }
}

const KEEP_BAKED: usize = 8;

/// The window's colour step (HostColour): the stages of the stream the panels show and of single streams, the compare
/// line, and the LUT baked from them.
#[derive(Default)]
pub struct ColourPipeline {
    shown: Vec<Entry>,
    by_source: HashMap<String, Vec<Entry>>,
    splits: Vec<(String, f32)>,
    baked: Vec<(Identity, Option<Arc<Baked>>)>,
}

fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| (a.order, &a.owner).cmp(&(b.order, &b.owner)));
    entries
}

impl ColourPipeline {
    pub fn new() -> ColourPipeline {
        ColourPipeline::default()
    }

    /// An owner's stage, or none; it replaces the one it had. With a source id the stage is for that stream alone, next to
    /// the shown ones. Stages run by order, then owner.
    pub fn set_stage(&mut self, owner: &str, stage: Option<ColourStage>, order: i32, source_id: Option<&str>) {
        let changed = |list: &[Entry]| -> Vec<Entry> {
            let mut kept: Vec<Entry> = list.iter().filter(|e| e.owner != owner).cloned().collect();
            if let Some(stage) = &stage {
                kept.push(Entry { order, owner: owner.to_string(), stage: stage.clone() });
            }
            sorted(kept)
        };
        match source_id {
            None => self.shown = changed(&self.shown),
            Some(id) => {
                let list = changed(self.by_source.get(id).map_or(&[], Vec::as_slice));
                if list.is_empty() {
                    self.by_source.remove(id);
                } else {
                    self.by_source.insert(id.to_string(), list);
                }
            }
        }
    }

    /// The stages source_id's stream gets, in order: the shown ones and its own.
    pub fn stages(&self, source_id: Option<&str>) -> Vec<&ColourStage> {
        let mut all: Vec<&Entry> = self.shown.iter().collect();
        all.extend(source_id.and_then(|id| self.by_source.get(id)).into_iter().flatten());
        all.sort_by(|a, b| (a.order, &a.owner).cmp(&(b.order, &b.owner)));
        all.into_iter().map(|e| &e.stage).collect()
    }

    /// Where the preview shows the shown stream without its colour (0..1); None takes an owner's compare away.
    pub fn set_split(&mut self, owner: &str, position: Option<f32>) {
        self.splits.retain(|(o, _)| o != owner);
        if let Some(p) = position {
            self.splits.push((owner.to_string(), p.clamp(0.0, 1.0)));
        }
    }

    /// The compare line that counts: the newest one asked for.
    pub fn split(&self) -> Option<f32> {
        self.splits.last().map(|(_, p)| *p)
    }

    /// The LUT of source_id's stages, baked once for as long as the same stages come back (a few are kept).
    pub fn lut(&mut self, source_id: Option<&str>) -> Option<Arc<Baked>> {
        let stages = self.stages(source_id);
        if stages.is_empty() {
            return None;
        }
        let key = Identity::of(&stages);
        if let Some(i) = self.baked.iter().position(|(k, _)| k.same(&key)) {
            let hit = self.baked.remove(i);
            self.baked.push(hit);
            return self.baked.last().and_then(|(_, b)| b.clone());
        }
        let lut = bake(&stages, None).map(Arc::new);
        while self.baked.len() >= KEEP_BAKED {
            self.baked.remove(0);
        }
        self.baked.push((key, lut.clone()));
        lut
    }

    /// The step the stream's frames go through in the Colour stage; None without stages (frames go through untouched).
    pub fn step(&mut self, source_id: Option<&str>) -> Option<ShaderStep> {
        self.lut(source_id).map(|b| b.shader())
    }
}

/// `after` (what the stream makes) with the picture before the colour step to the left of the compare line `position`
/// (0..1), and a thin grey line along it, as ColourPipeline.preview draws it. `before` and `after` must be the same size
/// and format (RGBA, BGRA or I420), else `after` comes back as it is.
pub fn with_split(before: &Frame, after: &Frame, position: f32) -> Frame {
    if (before.width, before.height, before.format) != (after.width, after.height, after.format) {
        return after.clone();
    }
    let (w, h) = (after.width as usize, after.height as usize);
    let x = (w as f32 * position.clamp(0.0, 1.0)).round() as usize;
    let line = (w as f32 / 480.0).round().max(1.0) as usize;
    let (from, to) = (x.saturating_sub(line), (x + line).min(w));
    let mut out = after.clone();
    let data = out.data_mut();
    match after.format {
        PixelFormat::Rgba | PixelFormat::Bgra => {
            for row in 0..h {
                let at = row * w * 4;
                data[at..at + x.min(w) * 4].copy_from_slice(&before.data[at..at + x.min(w) * 4]);
                for px in from..to {
                    data[at + px * 4..at + px * 4 + 3].fill(SPLIT_LINE);
                }
            }
        }
        PixelFormat::I420 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            // Grey 235 on the video range: luma 16 + 235 * 219 / 255, neutral chroma
            let grey = (16.0 + SPLIT_LINE as f32 * 219.0 / 255.0).round() as u8;
            for row in 0..h {
                let at = row * w;
                data[at..at + x.min(w)].copy_from_slice(&before.data[at..at + x.min(w)]);
                data[at + from..at + to].fill(grey);
            }
            for base in [w * h, w * h + cw * ch] {
                for row in 0..ch {
                    let at = base + row * cw;
                    let cx = (x / 2).min(cw);
                    data[at..at + cx].copy_from_slice(&before.data[at..at + cx]);
                    data[at + from / 2..at + to.div_ceil(2).min(cw)].fill(128);
                }
            }
        }
        _ => return after.clone(),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(f: impl Fn(f32) -> f32, per_channel: bool, size: u32) -> ColourStage {
        let n = size as usize;
        let mut rgb = lattice(n);
        rgb.iter_mut().for_each(|v| *v = f(*v));
        let table = per_channel.then(|| Arc::new((0..LEVELS).flat_map(|i| [f(i as f32 / 255.0); 3]).collect::<Vec<f32>>()));
        ColourStage { size, rgb: Arc::new(rgb), table }
    }

    fn pixels() -> Vec<[u8; 3]> {
        (0u32..200).map(|i| [(i * 37 % 256) as u8, (i * 91 % 256) as u8, (i * 53 % 256) as u8]).collect()
    }

    fn graded(f: impl Fn(f32) -> f32, px: [u8; 3]) -> [u8; 3] {
        px.map(|v| (f(v as f32 / 255.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    }

    #[test]
    fn an_identity_lut_leaves_pixels_alone() {
        for size in SIZES {
            let lut = bake(&[&stage(|v| v, false, size)], None).unwrap();
            assert_eq!(lut.kind(), format!("3D {size}"));
            for px in pixels() {
                assert_eq!(lut.apply(px), px);
            }
        }
    }

    #[test]
    fn per_channel_stages_become_exact_tables() {
        let (a, b) = (stage(|v| 1.0 - v, true, 33), stage(|v| v * v, true, 33));
        let lut = bake(&[&a, &b], None).unwrap();
        assert_eq!(lut.kind(), "1D");
        for px in pixels() {
            let want = graded(|v| (1.0 - v) * (1.0 - v), px);
            let got = lut.apply(px);
            assert!((0..3).all(|c| (got[c] as i32 - want[c] as i32).abs() <= 1), "{px:?} {got:?} {want:?}");
        }
    }

    #[test]
    fn a_3d_lut_is_interpolated_trilinearly() {
        let warm = |size: usize| {
            let mut rgb = lattice(size);
            for px in rgb.chunks_exact_mut(3) {
                let (r, g, b) = (px[0], px[1], px[2]);
                px.copy_from_slice(&[r.powf(0.8), g * 0.9 + b * 0.1, b.sqrt()]);
            }
            rgb
        };
        for size in SIZES {
            let cube = warm(size as usize);
            let lut = Baked::Cube { size: size as usize, rgb: cube.clone() };
            for px in pixels() {
                let x = px.map(|v| v as f32 / 255.0);
                let want = sample_cube(&cube, size as usize, x).map(|v| (v * 255.0 + 0.5) as u8);
                assert_eq!(lut.apply(px), want);
            }
        }
    }

    #[test]
    fn a_lattice_is_red_fastest_and_cubes_are_blue_major() {
        let pts = lattice(3);
        assert_eq!(&pts[3..6], &[0.5, 0.0, 0.0]);
        assert_eq!(&pts[9..12], &[0.0, 0.5, 0.0]);
        assert_eq!(&pts[27..30], &[0.0, 0.0, 0.5]);
        // [b][g][r]: full blue in stays blue
        let mut cube = vec![0.0f32; 27 * 3];
        cube[(2 * 9) * 3..(2 * 9) * 3 + 3].copy_from_slice(&[0.0, 0.0, 1.0]);
        assert_eq!(Baked::Cube { size: 3, rgb: cube }.apply([0, 0, 255]), [0, 0, 255]);
    }

    #[test]
    fn the_bake_size_is_what_the_stages_need() {
        let plain = stage(|v| v, false, 33);
        assert_eq!(bake(&[&plain], None).unwrap().kind(), "3D 33");
        assert_eq!(bake(&[&plain, &stage(|v| v, false, 65)], None).unwrap().kind(), "3D 65");
        assert_eq!(bake(&[&stage(|v| v, true, 33)], Some(65)).unwrap().kind(), "3D 65");
        assert!(bake(&[], None).is_none());
    }

    #[test]
    fn stages_run_in_order_and_values_are_clipped_between_them() {
        let (double, half) = (stage(|v| v * 2.0, false, 33), stage(|v| v * 0.5, false, 33));
        let a = bake(&[&double, &half], None).unwrap().apply([200, 200, 200])[0] as i32;
        let b = bake(&[&half, &double], None).unwrap().apply([200, 200, 200])[0] as i32;
        assert!((a - 128).abs() <= 1, "{a}"); // 200 doubled clips to 255, then halves
        assert!((b - 200).abs() <= 1, "{b}");
    }

    #[test]
    fn a_stage_that_doesnt_fit_its_size_is_left_out() {
        let broken = ColourStage { size: 33, rgb: Arc::new(vec![0.0; 10]), table: None };
        let lut = bake(&[&broken, &stage(|v| 1.0 - v, false, 33)], None).unwrap();
        assert_eq!(lut.apply([0, 0, 0]), [255, 255, 255]);
        let short = ColourStage { size: 33, rgb: Arc::new(lattice(33)), table: Some(Arc::new(vec![0.0; 4])) };
        assert_eq!(bake(&[&short], None).unwrap().apply([10, 20, 30]), [10, 20, 30]);
    }

    #[test]
    fn stages_sort_by_order_then_owner_and_an_owner_replaces_its_own() {
        let mut p = ColourPipeline::new();
        let (a, b, c, d) = (stage(|v| v, false, 33), stage(|v| v * 0.9, false, 33), stage(|v| v * 0.8, false, 33), stage(|v| v * 0.7, false, 33));
        p.set_stage("zeta", Some(a.clone()), 0, None);
        p.set_stage("alpha", Some(b.clone()), 0, None);
        p.set_stage("match", Some(c.clone()), -100, None);
        assert_eq!(p.stages(Some("phone")), [&c, &b, &a]);
        p.set_stage("zeta", Some(d.clone()), -200, None);
        assert_eq!(p.stages(Some("phone")), [&d, &c, &b]);
        p.set_stage("zeta", None, 0, None);
        assert_eq!(p.stages(Some("phone")), [&c, &b]);
    }

    #[test]
    fn the_composed_lut_follows_the_order() {
        let mut p = ColourPipeline::new();
        p.set_stage("gamma", Some(stage(|v| v * v, true, 33)), 0, None);
        p.set_stage("invert", Some(stage(|v| 1.0 - v, true, 33)), -1, None);
        let lut = p.lut(None).unwrap();
        for px in pixels() {
            let want = graded(|v| (1.0 - v) * (1.0 - v), px);
            assert!((0..3).all(|c| (lut.apply(px)[c] as i32 - want[c] as i32).abs() <= 1));
        }
    }

    #[test]
    fn a_stream_gets_its_own_stages_next_to_the_shown_ones() {
        let mut p = ColourPipeline::new();
        p.set_stage("grade", Some(stage(|v| v * 0.5, true, 33)), 0, None);
        p.set_stage("match", Some(stage(|v| 1.0 - v, true, 33)), -1, Some("b"));
        let at = |p: &mut ColourPipeline, id: &str| p.lut(Some(id)).unwrap().apply([200, 200, 200])[0] as i32;
        assert!((at(&mut p, "a") - 100).abs() <= 1);
        assert!((at(&mut p, "b") - 28).abs() <= 1); // 55 inverted, then halved
        p.set_stage("match", None, -1, Some("b"));
        assert_eq!(p.stages(Some("b")).len(), p.stages(Some("a")).len());
    }

    #[test]
    fn without_stages_frames_go_through_untouched() {
        let mut p = ColourPipeline::new();
        assert!(p.step(None).is_none() && p.lut(Some("phone")).is_none());
    }

    #[test]
    fn the_same_stages_are_baked_once() {
        let mut p = ColourPipeline::new();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, false, 33)), 0, None);
        let (a, b) = (p.lut(None).unwrap(), p.lut(None).unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        p.set_stage("grade", Some(stage(|v| v, false, 33)), 0, None);
        assert!(!Arc::ptr_eq(&a, &p.lut(None).unwrap()));
    }

    #[test]
    fn a_stage_that_took_a_freed_ones_place_is_baked_afresh() {
        let mut p = ColourPipeline::new();
        p.set_stage("grade", Some(stage(|v| v * 0.5, false, 33)), 0, None);
        assert!((p.lut(None).unwrap().apply([200, 200, 200])[0] as i32 - 100).abs() <= 1);
        p.set_stage("grade", None, 0, None);
        assert!(p.lut(None).is_none());
        // the first stage is gone and its memory free for the next one
        p.set_stage("grade", Some(stage(|v| 1.0 - v, false, 33)), 0, None);
        assert!((p.lut(None).unwrap().apply([200, 200, 200])[0] as i32 - 55).abs() <= 1);
    }

    #[test]
    fn the_shader_steps_carry_the_lut_as_two_pictures() {
        let step = bake(&[&stage(|v| 1.0 - v, true, 33)], None).unwrap().shader();
        assert_eq!(step.textures.len(), 2);
        assert_eq!((step.textures[0].1.width, step.textures[0].1.height), (256, 1));
        let step = bake(&[&stage(|v| v, false, 33)], None).unwrap().shader();
        assert_eq!((step.textures[0].1.width, step.textures[0].1.height), (33 * 33, 33));
        assert_eq!(step.uniforms, json!({"size": 33}));
    }

    #[test]
    fn the_compare_line_is_the_newest_ask_and_clamped() {
        let mut p = ColourPipeline::new();
        assert_eq!(p.split(), None);
        p.set_split("grade", Some(1.5));
        assert_eq!(p.split(), Some(1.0));
        p.set_split("other", Some(0.25));
        assert_eq!(p.split(), Some(0.25));
        p.set_split("other", None);
        assert_eq!(p.split(), Some(1.0));
        p.set_split("grade", None);
        assert_eq!(p.split(), None);
    }

    #[test]
    fn the_picture_before_the_colour_step_shows_left_of_the_split() {
        let rgba = |v: u8| Frame::new(80, 40, PixelFormat::Rgba, [v, v, v, 255].repeat(80 * 40));
        let (before, after) = (rgba(10), rgba(245));
        let shown = with_split(&before, &after, 0.5);
        let px = |row: usize, col: usize| shown.data[(row * 80 + col) * 4];
        assert!(px(3, 0) == 10 && px(3, 37) == 10 && px(3, 45) == 245);
        assert_eq!(px(3, 40), SPLIT_LINE);
        assert_eq!(after.data[0], 245, "drawn on a copy");
        let tall = Frame::new(40, 80, PixelFormat::Rgba, vec![0; 40 * 80 * 4]);
        assert_eq!(with_split(&tall, &after, 0.5).data, after.data);
        let yuv = |y: u8| Frame::new(80, 40, PixelFormat::I420, [vec![y; 80 * 40], vec![128; 2 * 40 * 20]].concat());
        let shown = with_split(&yuv(40), &yuv(200), 0.25);
        assert!(shown.data[0] == 40 && shown.data[60] == 200 && shown.data.len() == 80 * 40 * 3 / 2);
    }
}
