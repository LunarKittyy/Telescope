//! The colour pipeline (telescope.colour.ColourPipeline): every colour change plugins ask for, put one after the other into a
//! single LUT per stream and run on the graphics card as one shader step in the Colour stage, or on the CPU while the card is busy
//! (colour_cpu.rs).
//!
//! A plugin hands over each change already baked (ColourStage: a 3D lattice, and 256 exact entries per channel when each
//! channel only depends on itself). A pipeline of only such stages becomes exact tables, anything else a 3D LUT of 33 or 65
//! points, both with every stage's output clipped to 0..1 before the next one, as colour.bake does. The LUT goes up as two
//! RGBA pictures holding each value as 16 bits (high byte, low byte), read with textureLoad, so the lattice isn't rounded
//! to whole levels before it's interpolated.

use crate::colour_cpu::Graded;
use crate::frame::{Frame, FrameStep, PixelFormat, ShaderStep};
use crate::host::ColourStage;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
    if let Some(change) = &stage.eval.0 {
        // the change itself at the colours the stages before it left
        change(rgb);
        for v in rgb.iter_mut() {
            *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        }
        return;
    }
    for px in rgb.as_chunks_mut::<3>().0 {
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
        let before = rgb.clone();
        if catch_unwind(AssertUnwindSafe(|| run(stage, &mut rgb))).is_err() {
            log::error!("A colour stage failed; leaving it out");
            rgb = before;
        }
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
    for px in values.as_chunks::<3>().0 {
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
    /// The step that applies it, on the card or off it.
    pub fn frame_step(&self) -> FrameStep {
        FrameStep::Either(self.shader(), Box::new(Graded::new(self)))
    }

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

/// A stage's baked 1D table, when it has one.
type Table = Option<Arc<Vec<f32>>>;

/// What a stage is told apart by when baking again would give the same LUT: the data it holds. The Arcs are kept, so a freed
/// stage's address can't be taken by another one and pass for it (colour.py keys on the stage objects themselves).
#[derive(Clone)]
struct Identity(Vec<(Arc<Vec<f32>>, Table)>);

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

/// Composed LUTs kept per pipeline, for streams with different stages and quick back and forth.
const KEEP_BAKED: usize = 8;

/// A stream's key in the baking: its source id, or None for a stream without one.
type Key = Option<String>;
/// Turns stages into a LUT; the baking thread runs it.
type BakeFn = dyn Fn(&[ColourStage]) -> Option<Baked> + Send + Sync;

/// What the stream steps and the baking thread share.
#[derive(Default)]
struct Bakes {
    /// The newest last.
    baked: Vec<(Identity, Option<Arc<Baked>>)>,
    /// The LUT each stream got last, until the one for new stages is baked.
    current: HashMap<Key, Option<Arc<Baked>>>,
    /// Stages waiting for the baking thread, one set per stream.
    wanted: Vec<(Key, Identity, Vec<ColourStage>)>,
    /// Streams that stopped, which mustn't pin a LUT in current again.
    gone: BTreeSet<String>,
    baking: bool,
}

impl Bakes {
    fn find(&self, key: &Identity) -> Option<usize> {
        self.baked.iter().position(|(k, _)| k.same(key))
    }

    /// Adds a LUT, dropping the oldest first but never one a stream is showing.
    fn store(&mut self, key: Identity, lut: Option<Arc<Baked>>, keep: usize) {
        let mut i = 0;
        while self.baked.len() >= keep && i < self.baked.len() {
            let in_use = self.baked[i].1.as_ref().is_some_and(|b| self.current.values().flatten().any(|c| Arc::ptr_eq(c, b)));
            if in_use {
                i += 1;
            } else {
                self.baked.remove(i);
            }
        }
        self.baked.push((key, lut));
    }
}

struct Bakery {
    bakes: Mutex<Bakes>,
    idle: Condvar,
    bake: Box<BakeFn>,
    keep: usize,
    on_baked: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Bakery {
    fn run(self: Arc<Self>) {
        loop {
            let (key, stages) = {
                let mut b = self.bakes.lock().unwrap();
                let Some((_, key, stages)) = (!b.wanted.is_empty()).then(|| b.wanted.remove(0)) else {
                    b.baking = false;
                    self.idle.notify_all();
                    return;
                };
                if b.find(&key).is_some() || b.wanted.iter().any(|(_, k, _)| k.same(&key)) {
                    continue;
                }
                (key, stages)
            };
            match catch_unwind(AssertUnwindSafe(|| (self.bake)(&stages))) {
                Ok(lut) => self.bakes.lock().unwrap().store(key, lut.map(Arc::new), self.keep),
                Err(_) => log::error!("Baking the colour stages failed"),
            }
            let told = self.on_baked.lock().unwrap().clone();
            if let Some(told) = told {
                told();
            }
        }
    }
}

/// The window's colour step (HostColour): the stages of the stream the panels show and of single streams, the compare
/// line, and the LUT baked from them. New stages are baked on a thread of their own (a 65-point bake can take a tenth of a
/// second), so a dragged slider never holds anything up, and a stream keeps the LUT it had until the new one is there.
pub struct ColourPipeline {
    shown: Vec<Entry>,
    by_source: HashMap<String, Vec<Entry>>,
    splits: Vec<(String, f32)>,
    bakery: Arc<Bakery>,
    /// Bake on the call that first needs a LUT instead of on a thread of its own.
    inline: bool,
    /// The kind of the last LUT handed out, for the diagnostics.
    kind: Option<String>,
}

impl Default for ColourPipeline {
    fn default() -> Self {
        ColourPipeline::with(false, KEEP_BAKED, Box::new(bake_owned))
    }
}

fn bake_owned(stages: &[ColourStage]) -> Option<Baked> {
    bake(&stages.iter().collect::<Vec<_>>(), None)
}

fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| (a.order, &a.owner).cmp(&(b.order, &b.owner)));
    entries
}

impl ColourPipeline {
    pub fn new() -> ColourPipeline {
        ColourPipeline::default()
    }

    /// One that bakes on the call that first needs a LUT, for tests and for checking one LUT at a time.
    pub fn inline() -> ColourPipeline {
        ColourPipeline::with(true, KEEP_BAKED, Box::new(bake_owned))
    }

    fn with(inline: bool, keep: usize, bake: Box<BakeFn>) -> ColourPipeline {
        let bakery = Bakery { bakes: Mutex::default(), idle: Condvar::new(), bake, keep, on_baked: Mutex::new(None) };
        ColourPipeline { shown: Vec::new(), by_source: HashMap::new(), splits: Vec::new(), bakery: Arc::new(bakery), inline, kind: None }
    }

    /// Called from the baking thread each time a LUT is ready, so the streams can be given it.
    pub fn set_on_baked(&mut self, told: Arc<dyn Fn() + Send + Sync>) {
        *self.bakery.on_baked.lock().unwrap() = Some(told);
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

    /// The LUT of source_id's stages, baked once for as long as the same stages come back (a few are kept). Stages not
    /// baked yet go to the baking thread and the stream gets the LUT it had meanwhile.
    pub fn lut(&mut self, source_id: Option<&str>) -> Option<Arc<Baked>> {
        let stages: Vec<ColourStage> = self.stages(source_id).into_iter().cloned().collect();
        let lut = self.lut_of(&stages, source_id);
        if let Some(lut) = &lut {
            self.kind = Some(lut.kind());
        }
        lut
    }

    /// The baked LUT of these stages for the stream called key.
    pub fn lut_of(&self, stages: &[ColourStage], key: Option<&str>) -> Option<Arc<Baked>> {
        if stages.is_empty() {
            return None;
        }
        let identity = Identity::of(&stages.iter().collect::<Vec<_>>());
        let key: Key = key.map(String::from);
        let mut b = self.bakery.bakes.lock().unwrap();
        if key.as_ref().is_some_and(|k| b.gone.contains(k)) {
            // A stopped stream's last frames: whatever's there, nothing kept
            return b.find(&identity).and_then(|i| b.baked[i].1.clone());
        }
        if let Some(i) = b.find(&identity) {
            let hit = b.baked.remove(i);
            let lut = hit.1.clone();
            b.baked.push(hit); // the newest last again
            b.current.insert(key, lut.clone());
            return lut;
        }
        if !self.inline {
            b.wanted.retain(|(k, ..)| *k != key);
            b.wanted.push((key.clone(), identity, stages.to_vec()));
            if !b.baking {
                b.baking = true;
                let bakery = self.bakery.clone();
                std::thread::Builder::new().name("colour-bake".into()).spawn(move || bakery.run()).expect("a thread to bake on");
            }
            return b.current.get(&key).cloned().flatten();
        }
        drop(b);
        let lut = (self.bakery.bake)(stages).map(Arc::new);
        let mut b = self.bakery.bakes.lock().unwrap();
        b.current.insert(key, lut.clone());
        b.store(identity, lut.clone(), self.bakery.keep);
        lut
    }

    /// Whether a stream's LUT is being held on to.
    pub fn holds(&self, key: &str) -> bool {
        self.bakery.bakes.lock().unwrap().current.contains_key(&Some(key.to_string()))
    }

    /// The streams running now, by key. One that stopped lets go of its LUT, and a frame of it still on its way doesn't hold
    /// on to it again until it streams again.
    pub fn set_streams<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        let keys: BTreeSet<&str> = keys.into_iter().collect();
        let mut b = self.bakery.bakes.lock().unwrap();
        let known = b.current.keys().chain(b.wanted.iter().map(|(k, ..)| k)).flatten().cloned().collect::<Vec<_>>();
        let gone: BTreeSet<String> = b.gone.iter().cloned().chain(known).filter(|k| !keys.contains(k.as_str())).collect();
        for key in &gone {
            b.current.remove(&Some(key.clone()));
            b.wanted.retain(|(k, ..)| k.as_ref() != Some(key));
        }
        b.gone = gone;
    }

    /// Waits until nothing waits to be baked; false if it still does after timeout.
    pub fn settle(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        let mut b = self.bakery.bakes.lock().unwrap();
        while b.baking || !b.wanted.is_empty() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            b = self.bakery.idle.wait_timeout(b, left).unwrap().0;
        }
        true
    }

    /// What Copy diagnostics says about it, once frames have had a LUT. The LUT runs as a shader or next to it on the CPU, so unlike
    /// Python's there's no time of its own to report.
    pub fn diagnostics(&self) -> Vec<(String, String)> {
        self.kind.iter().map(|kind| ("Colour pipeline".to_string(), format!("{kind}, as a shader step"))).collect()
    }

    /// The step the stream's frames go through in the Colour stage; None without stages (frames go through untouched).
    pub fn step(&mut self, source_id: Option<&str>) -> Option<FrameStep> {
        self.lut(source_id).map(|b| b.frame_step())
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
    // round_ties_even: Python's round()
    let x = (w as f32 * position.clamp(0.0, 1.0)).round_ties_even() as usize;
    let line = (w as f32 / 480.0).round_ties_even().max(1.0) as usize;
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
        ColourStage::new(size, Arc::new(rgb), table)
    }

    /// Noise the size of the tests' frames.
    fn pixels() -> Vec<[u8; 3]> {
        let mut x = 1u32;
        let mut next = || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        };
        (0..128 * 72).map(|_| [next(), next(), next()]).collect()
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
            assert_eq!(lut.apply(px), graded(|v| (1.0 - v) * (1.0 - v), px), "{px:?}");
        }
        for level in 0..=255 {
            assert_eq!(lut.apply([level; 3]), graded(|v| (1.0 - v) * (1.0 - v), [level; 3]));
        }
    }

    /// A cube read the long way round, one axis at a time, so the check doesn't lean on sample_cube.
    fn trilinear(cube: &[f32], n: usize, rgb: [f32; 3]) -> [u8; 3] {
        let cell = rgb.map(|v| {
            let at = v as f64 * (n - 1) as f64;
            let low = (at.floor() as usize).min(n - 2);
            (low, at - low as f64)
        });
        let (r, g, b) = (cell[0], cell[1], cell[2]);
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let mut out = [0u8; 3];
        for (c, o) in out.iter_mut().enumerate() {
            let point = |bi: usize, gi: usize, ri: usize| cube[(bi * n * n + gi * n + ri) * 3 + c] as f64;
            let line = |bi: usize, gi: usize| lerp(point(bi, gi, r.0), point(bi, gi, r.0 + 1), r.1);
            let plane = |bi: usize| lerp(line(bi, g.0), line(bi, g.0 + 1), g.1);
            *o = (lerp(plane(b.0), plane(b.0 + 1), b.1).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        out
    }

    #[test]
    fn a_3d_lut_is_interpolated_trilinearly() {
        let warm = |size: usize| {
            let mut rgb = lattice(size);
            for px in rgb.as_chunks_mut::<3>().0 {
                let (r, g, b) = (px[0], px[1], px[2]);
                px.copy_from_slice(&[r.powf(0.8), g * 0.9 + b * 0.1, b.sqrt()]);
            }
            rgb
        };
        for size in SIZES {
            let cube = warm(size as usize);
            let lut = Baked::Cube { size: size as usize, rgb: cube.clone() };
            for px in pixels() {
                let (got, want) = (lut.apply(px), trilinear(&cube, size as usize, px.map(|v| v as f32 / 255.0)));
                assert!((0..3).all(|c| got[c].abs_diff(want[c]) <= 1), "{px:?} {got:?} {want:?}");
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
        let broken = ColourStage::new(33, Arc::new(vec![0.0; 10]), None);
        let lut = bake(&[&broken, &stage(|v| 1.0 - v, false, 33)], None).unwrap();
        assert_eq!(lut.apply([0, 0, 0]), [255, 255, 255]);
        let short = ColourStage::new(33, Arc::new(lattice(33)), Some(Arc::new(vec![0.0; 4])));
        let lut = bake(&[&short], None).unwrap();
        for px in pixels() {
            assert!((0..3).all(|c| lut.apply(px)[c].abs_diff(px[c]) <= 1), "{px:?}");
        }
    }

    #[test]
    fn stages_sort_by_order_then_owner_and_an_owner_replaces_its_own() {
        let mut p = ColourPipeline::inline();
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
        let mut p = ColourPipeline::inline();
        p.set_stage("gamma", Some(stage(|v| v * v, true, 33)), 0, None);
        p.set_stage("invert", Some(stage(|v| 1.0 - v, true, 33)), -1, None);
        let lut = p.lut(None).unwrap();
        for px in pixels() {
            assert_eq!(lut.apply(px), graded(|v| (1.0 - v) * (1.0 - v), px), "{px:?}");
        }
    }

    #[test]
    fn a_stream_gets_its_own_stages_next_to_the_shown_ones() {
        let mut p = ColourPipeline::inline();
        p.set_stage("grade", Some(stage(|v| v * 0.5, true, 33)), 0, None);
        p.set_stage("match", Some(stage(|v| 1.0 - v, true, 33)), -1, Some("b"));
        let at = |p: &mut ColourPipeline, id: &str| p.lut(Some(id)).unwrap().apply([200, 200, 200])[0] as i32;
        assert!((at(&mut p, "a") - 100).abs() <= 1);
        assert!((at(&mut p, "b") - 28).abs() <= 1); // 55 inverted, then halved
        p.set_stage("match", None, -1, Some("b"));
        assert_eq!(p.stages(Some("b")), p.stages(Some("a")));
    }

    #[test]
    fn without_stages_frames_go_through_untouched() {
        let mut p = ColourPipeline::inline();
        assert!(p.step(None).is_none() && p.lut(Some("phone")).is_none());
    }

    #[test]
    fn the_same_stages_are_baked_once() {
        let mut p = ColourPipeline::inline();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, false, 33)), 0, None);
        let (a, b) = (p.lut(None).unwrap(), p.lut(None).unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        p.set_stage("grade", Some(stage(|v| v, false, 33)), 0, None);
        assert!(!Arc::ptr_eq(&a, &p.lut(None).unwrap()));
    }

    #[test]
    fn a_stage_is_worked_out_at_the_one_before_its_output_not_read_off_its_table() {
        // a steep curve: 0 until 0.5, then up to 1 by 0.51 (the tone curve [(0, 0), (0.5, 0), (0.51, 1), (1, 1)])
        let steep = |v: f32| ((v - 0.5) / 0.01).clamp(0.0, 1.0);
        let half = stage(|v| v * 0.5, true, 33);
        let curve = stage(steep, true, 33).with_eval(move |px: &mut [f32]| px.iter_mut().for_each(|v| *v = steep(*v)));
        // the first stage leaves white at exactly 0.5, which the curve takes to 0
        assert_eq!(bake(&[&half, &curve], None).unwrap().apply([255, 255, 255]), [0, 0, 0]);
        // read off its 256 levels instead, the curve would pick up the step between the two levels around 0.5
        let sampled = stage(steep, true, 33);
        assert_ne!(bake(&[&half, &sampled], None).unwrap().apply([255, 255, 255]), [0, 0, 0]);
        // a 3D bake works the same way
        let half3 = stage(|v| v * 0.5, false, 33);
        let curve3 = stage(steep, false, 33).with_eval(move |px: &mut [f32]| px.iter_mut().for_each(|v| *v = steep(*v)));
        assert_eq!(bake(&[&half3, &curve3], None).unwrap().apply([255, 255, 255]), [0, 0, 0]);
    }

    #[test]
    fn a_stage_that_took_a_freed_ones_place_is_baked_afresh() {
        let mut p = ColourPipeline::inline();
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
        let mut p = ColourPipeline::inline();
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

    fn lut_of(stages: &[&ColourStage]) -> Vec<ColourStage> {
        stages.iter().map(|s| (*s).clone()).collect()
    }

    const SETTLE: Duration = Duration::from_secs(5);

    /// Keeps what's logged, per thread as tests run side by side.
    struct Caught;

    static LOGGED: Mutex<Vec<(std::thread::ThreadId, String)>> = Mutex::new(Vec::new());

    impl log::Log for Caught {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            LOGGED.lock().unwrap().push((std::thread::current().id(), record.args().to_string()));
        }
        fn flush(&self) {}
    }

    #[test]
    fn a_failing_stage_is_left_out() {
        let _ = log::set_logger(&Caught);
        log::set_max_level(log::LevelFilter::Error);
        let broken = stage(|v| v, false, 33).with_eval(|_: &mut [f32]| panic!("no"));
        let lut = bake(&[&broken, &stage(|v| 1.0 - v, false, 33)], None).unwrap();
        assert_eq!(lut.apply([0, 0, 0]), [255, 255, 255]);
        let me = std::thread::current().id();
        assert!(LOGGED.lock().unwrap().iter().any(|(who, text)| *who == me && text.contains("colour stage failed")));
    }

    #[test]
    fn a_stream_behind_the_panels_keeps_the_stages_it_had() {
        let mut p = ColourPipeline::inline();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, true, 33)), 0, None);
        let Some(FrameStep::Either(frozen, _)) = p.step(None) else { panic!("no step") };
        p.set_stage("grade", None, 0, None); // the panels moved on to a phone without a grade
        assert!(p.step(None).is_none());
        let (_, lo) = (&frozen.textures[0], &frozen.textures[1]);
        assert_eq!((frozen.textures[0].1.data[0], lo.1.data[0]), (255, 255), "the step it runs still inverts");
    }

    #[test]
    fn new_stages_are_baked_off_the_stream_thread() {
        let mut p = ColourPipeline::new();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, true, 33)), 0, None);
        let first = p.lut(Some("phone"));
        assert!(p.settle(SETTLE));
        assert!(first.is_none_or(|l| l.apply([0, 0, 0]) == [255, 255, 255])); // either not baked yet, or the thread was quick
        assert_eq!(p.lut(Some("phone")).unwrap().apply([0, 0, 0]), [255, 255, 255]);
        p.set_stage("grade", Some(stage(|_| 0.5, true, 33)), 0, None);
        let meanwhile = p.lut(Some("phone")).unwrap().apply([0, 0, 0])[0];
        assert!(meanwhile == 255 || meanwhile == 128, "the old LUT until the new one is there, never no grade at all");
        assert!(p.settle(SETTLE));
        assert_eq!(p.lut(Some("phone")).unwrap().apply([0, 0, 0])[0], 128);
    }

    #[test]
    fn a_stream_keeps_its_lut_through_a_moment_without_stages() {
        let mut p = ColourPipeline::new();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, true, 33)), 0, None);
        p.lut(Some("phone"));
        assert!(p.settle(SETTLE));
        assert_eq!(p.lut(Some("phone")).unwrap().apply([0, 0, 0])[0], 255);
        p.set_stage("grade", None, 0, None); // the defaults, then the next phone's settings
        assert!(p.lut(Some("phone")).is_none());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let held = gate.clone();
        let slow = stage(|_| 0.5, true, 33).with_eval(move |px: &mut [f32]| {
            let mut open = held.0.lock().unwrap();
            while !*open {
                open = held.1.wait_timeout(open, SETTLE).unwrap().0;
            }
            px.fill(0.5);
        });
        p.set_stage("grade", Some(slow), 0, None);
        assert_eq!(p.lut(Some("phone")).unwrap().apply([0, 0, 0])[0], 255, "the old LUT while the new one bakes");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(p.settle(SETTLE));
        assert_eq!(p.lut(Some("phone")).unwrap().apply([0, 0, 0])[0], 128);
    }

    #[test]
    fn the_window_hears_when_a_lut_is_ready() {
        let mut p = ColourPipeline::new();
        let (told, heard) = (Arc::new(Mutex::new(0)), Arc::new(Condvar::new()));
        let (t, h) = (told.clone(), heard.clone());
        p.set_on_baked(Arc::new(move || {
            *t.lock().unwrap() += 1;
            h.notify_all();
        }));
        p.set_stage("grade", Some(stage(|v| 1.0 - v, true, 33)), 0, None);
        p.lut(None);
        let mut n = told.lock().unwrap();
        while *n == 0 {
            let (next, timeout) = heard.wait_timeout(n, SETTLE).unwrap();
            n = next;
            assert!(!timeout.timed_out(), "never told");
        }
    }

    fn grades(values: &[f32]) -> Vec<Vec<ColourStage>> {
        values.iter().map(|v| {
            let v = *v;
            lut_of(&[&stage(move |_| v, true, 33)])
        }).collect()
    }

    fn holds(p: &ColourPipeline, stages: &[ColourStage]) -> bool {
        p.bakery.bakes.lock().unwrap().find(&Identity::of(&stages.iter().collect::<Vec<_>>())).is_some()
    }

    #[test]
    fn baked_luts_in_use_stay_and_a_hit_keeps_one_longer() {
        let p = ColourPipeline::with(true, 4, Box::new(bake_owned));
        let shown = lut_of(&[&stage(|v| 1.0 - v, true, 33)]);
        p.lut_of(&shown, Some("a"));
        let o = grades(&[0.1, 0.2, 0.3, 0.4, 0.5]);
        for i in [0, 1, 0, 2, 3] {
            p.lut_of(&o[i], Some("b")); // o[0] used again before o[2]
        }
        assert!(holds(&p, &shown), "stream a still shows it, though it's the oldest");
        assert!(holds(&p, &o[0]) && !holds(&p, &o[1]));
        p.lut_of(&o[4], Some("b"));
        assert!(holds(&p, &shown) && holds(&p, &o[3]));
        assert_eq!(p.bakery.bakes.lock().unwrap().baked.len(), 4);
    }

    #[test]
    fn a_stopped_streams_lut_isnt_kept_from_the_cache_limit() {
        let p = ColourPipeline::with(true, 2, Box::new(bake_owned));
        let g = grades(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        for (sid, stages) in ["a", "b", "c", "d"].iter().zip(&g) {
            p.lut_of(stages, Some(sid));
        }
        p.set_streams(["d"]); // the others stopped
        p.lut_of(&g[0], Some("a")); // a last frame of one of them, still on its way
        p.lut_of(&g[4], Some("d"));
        p.lut_of(&g[5], Some("d"));
        assert!(holds(&p, &g[4]) && holds(&p, &g[5]));
        assert_eq!(p.bakery.bakes.lock().unwrap().baked.len(), 2);
        assert_eq!(p.bakery.bakes.lock().unwrap().current.keys().cloned().collect::<Vec<_>>(), [Some("d".to_string())]);
        p.set_streams(["a", "d"]); // streaming again
        let lut = p.lut_of(&g[0], Some("a")).unwrap();
        assert!(Arc::ptr_eq(p.bakery.bakes.lock().unwrap().current[&Some("a".to_string())].as_ref().unwrap(), &lut));
    }

    #[test]
    fn a_bake_that_fails_doesnt_leave_the_baking_thread_stuck() {
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        let mut p = ColourPipeline::with(false, KEEP_BAKED, Box::new(move |_| {
            *counted.lock().unwrap() += 1;
            panic!("no")
        }));
        p.set_stage("grade", Some(stage(|v| v, false, 33)), 0, None);
        assert!(p.lut(None).is_none());
        assert!(p.settle(SETTLE));
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(!p.bakery.bakes.lock().unwrap().baking);
    }

    #[test]
    fn diagnostics_say_which_lut_runs() {
        let mut p = ColourPipeline::inline();
        assert!(p.diagnostics().is_empty());
        p.set_stage("grade", Some(stage(|v| v, false, 33)), 0, None);
        p.lut(None);
        let said = p.diagnostics();
        assert_eq!(said[0].0, "Colour pipeline");
        assert!(said[0].1.starts_with("3D 33, "), "{}", said[0].1);
    }

    #[test]
    fn the_split_is_only_drawn_on_the_stream_it_came_from() {
        let before = Frame::new(80, 40, PixelFormat::Rgba, vec![0; 80 * 40 * 4]);
        let tall = Frame::new(40, 80, PixelFormat::Rgba, vec![7; 40 * 80 * 4]);
        assert_eq!(with_split(&before, &tall, 0.5).data, tall.data);
    }

    #[test]
    fn the_picture_before_the_colour_step_shows_left_of_the_split() {
        let mut p = ColourPipeline::inline();
        p.set_stage("grade", Some(stage(|v| 1.0 - v, true, 33)), 0, None);
        let rgba = |v: u8| Frame::new(80, 40, PixelFormat::Rgba, [v, v, v, 255].repeat(80 * 40));
        let before = rgba(10);
        let after = rgba(p.lut(None).unwrap().apply([10; 3])[0]);
        assert_eq!(p.split(), None, "no split asked for");
        p.set_split("grade", Some(0.5));
        let shown = with_split(&before, &after, p.split().unwrap());
        assert!(after.data.chunks(4).all(|px| px[..3] == [245; 3]), "drawn on a copy");
        for row in shown.data.chunks(80 * 4) {
            let grey: Vec<u8> = row.chunks(4).map(|px| px[0]).collect();
            assert!(grey[..39].iter().all(|v| *v == 10) && grey[41..].iter().all(|v| *v == 245), "{grey:?}");
            assert_eq!(grey[39..41], [SPLIT_LINE; 2], "the line between them");
        }
        p.set_split("grade", None);
        assert_eq!(p.split(), None);
        let tall = Frame::new(40, 80, PixelFormat::Rgba, vec![0; 40 * 80 * 4]);
        assert_eq!(with_split(&tall, &after, 0.5).data, after.data);
        let yuv = |y: u8| Frame::new(80, 40, PixelFormat::I420, [vec![y; 80 * 40], vec![128; 2 * 40 * 20]].concat());
        let shown = with_split(&yuv(40), &yuv(200), 0.25);
        assert!(shown.data[0] == 40 && shown.data[60] == 200 && shown.data.len() == 80 * 40 * 3 / 2);
    }

    // Python's round(): a tie goes to the even neighbour
    #[test]
    fn the_split_rounds_as_python_does() {
        let rgba = |w: u32, v: u8| Frame::new(w, 1, PixelFormat::Rgba, [v, v, v, 255].repeat(w as usize));
        let grey = |f: Frame| f.data.chunks(4).map(|px| px[0]).collect::<Vec<u8>>();
        // 1200 / 480 is 2.5: a line of 2 either side of 600
        let row = grey(with_split(&rgba(1200, 10), &rgba(1200, 245), 0.5));
        assert!(row[..598].iter().all(|v| *v == 10) && row[598..602].iter().all(|v| *v == SPLIT_LINE) && row[602..].iter().all(|v| *v == 245));
        // 5 * 0.5 is 2.5: the line sits at 2
        let row = grey(with_split(&rgba(5, 10), &rgba(5, 245), 0.5));
        assert_eq!(row, [10, SPLIT_LINE, SPLIT_LINE, 245, 245]);
    }
}
