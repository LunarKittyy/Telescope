//! Cinema's zebras, focus peaking and false colour on the card (shaders/tint.wgsl), to match effects' cinema_tools::tint.

use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use telescope_core::preview::TintSpec;

const TINT: &str = include_str!("shaders/tint.wgsl");
/// px, the widest the tools judge a picture at (cinema_tools::WORK_WIDTH); a wider one is judged on a copy shrunk to it.
const WORK_WIDTH: u32 = 1280;

const ZEBRAS: u32 = 1;
const PEAKING: u32 = 2;
const FALSE_COLOUR: u32 = 4;
const SMALLER: u32 = 8;

/// How long a scope's picture may take to come back from the card before it counts as a miss.
const SCOPE_PATIENCE: Duration = Duration::from_secs(1);
/// How many misses in a row (a picture that took too long, or a card that refused one) end the card's part in the scopes.
const MISSES: u32 = 3;
/// How many paints pass between looks at whether the card was removed.
const REMOVED_CHECK_EVERY: u32 = 30;

/// One weight pair of OpenCV's INTER_LINEAR: first source, second source, their weights out of 2048 (core's image.rs has the same).
type Tap = (usize, usize, i32, i32);

fn taps_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Uint, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

/// scopes::luma_of's float steps (as cv2.transform does them) for a colour.
fn float_luma(r: u8, g: u8, b: u8) -> u8 {
    let acc = 0.7152f32 * g as f32;
    let acc = 0.0722f32.mul_add(b as f32, acc);
    let acc = 0.2126f32.mul_add(r as f32, acc);
    acc.round_ties_even().clamp(0.0, 255.0) as u8
}

/// For each red and green, row by row, the float level of the one blue (if any) that makes 2126 r + 7152 g + 722 b exactly half way between two levels, else 0.
/// A blue is the one when 361 b is 2500 - 1063 r - 3576 g over 5000, and fits in a byte.
fn half_way_levels() -> Vec<u8> {
    let inverse = (1..5000u32).find(|k| 361 * k % 5000 == 1).unwrap();
    let mut levels = vec![0u8; 256 * 256];
    for r in 0..256u32 {
        for g in 0..256u32 {
            let need = (5000 + 10000 - (2126 * r + 7152 * g) % 10000) % 10000;
            let b = (need / 2 % 5000) * inverse % 5000;
            if b < 256 {
                levels[(g * 256 + r) as usize] = float_luma(r as u8, g as u8, b as u8);
            }
        }
    }
    levels
}

/// The taps of cv2.resize(INTER_LINEAR) along one axis, from n pixels to m, worked out as image::linear_taps does for the CPU so the card's weights are the CPU's.
fn linear_taps(n: usize, m: usize, vertical: bool) -> Vec<Tap> {
    let scale = n as f64 / m as f64;
    let last = n as i64 - 1;
    (0..m).map(|d| {
        let mut f = ((d as f64 + 0.5) * scale - 0.5) as f32;
        let mut s = f.floor() as i64;
        f -= s as f32;
        if !vertical && s < 0 {
            f = 0.0;
            s = 0;
        }
        if !vertical && s >= last {
            f = 0.0;
            s = last;
        }
        let (p, q) = ((1.0f32 - f) * 2048.0f32, f * 2048.0f32);
        (s.clamp(0, last) as usize, (s + 1).clamp(0, last) as usize, (p as f64).round_ties_even() as i32, (q as f64).round_ties_even() as i32)
    }).collect()
}

/// The tint shader's pipelines, built the first time a picture is tinted.
pub(super) struct Kit {
    layout: wgpu::BindGroupLayout,
    shrink: wgpu::RenderPipeline,
    marks: wgpu::RenderPipeline,
    tint: wgpu::RenderPipeline,
    lut: Mutex<Lut>,
    /// The weights a mask is grown from the judged copy to the picture's size with, for the size it was last made for.
    taps: Mutex<Option<Taps>>,
    /// Stands in for the weights where no mask is grown.
    blank: wgpu::Texture,
    /// half_way_levels() on the card, 256 x 256.
    halves: wgpu::Texture,
}

/// The weights of both axes, x taps on the first row and y taps on the second, as (first, second, p, q) in 16 bits each.
struct Taps {
    /// The picture's and the judged copy's size.
    key: (u32, u32, u32, u32),
    texture: wgpu::Texture,
}

/// The false colour table as it sits on the card, 256 x 1.
struct Lut {
    /// None until a table was put there, and again after a refused call that may have left it half written.
    colours: Option<[[u8; 3]; 256]>,
    texture: wgpu::Texture,
}

impl Kit {
    fn new(gpu: &Gpu) -> Kit {
        let layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tint"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), tex_entry(3), sampler_entry(4), uniform_entry(5), taps_entry(6), taps_entry(7)],
        });
        let pipeline_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("tint"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("tint"), source: wgpu::ShaderSource::Wgsl(TINT.into()) });
        let pipeline = |label: &str, fragment: &str| gpu.render_pipeline(label, &pipeline_layout, &module, ("vs_main", fragment), WORK);
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("false colour"),
            size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let blank = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no taps"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let halves = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("half way levels"),
            size: wgpu::Extent3d { width: 256, height: 256, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.write_plane(&halves, &half_way_levels(), 256);
        Kit {
            shrink: pipeline("tint shrink", "fs_shrink"),
            marks: pipeline("tint marks", "fs_marks"),
            tint: pipeline("tint", "fs_tint"),
            lut: Mutex::new(Lut { colours: None, texture }),
            taps: Mutex::new(None),
            blank,
            halves,
            layout,
        }
    }
}

/// A w x h picture shrunk to at most `width` wide, the shape kept, as scopes::shrunk_size has it.
fn shrunk_to(w: u32, h: u32, width: u32) -> (u32, u32) {
    if w <= width {
        return (w, h);
    }
    (width, (((h as u64 * width as u64) as f64 / w as f64).round_ties_even() as u32).max(1))
}

/// The size the tools judge a w x h picture at: at most WORK_WIDTH wide.
fn work_size(w: u32, h: u32) -> (u32, u32) {
    shrunk_to(w, h, WORK_WIDTH)
}

impl Gpu {
    fn tint_kit(&self) -> Arc<Kit> {
        self.tint_kit.lock().unwrap().get_or_insert_with(|| Arc::new(Kit::new(self))).clone()
    }

    /// The weights that grow a mask from the judged copy (`work`) to the picture (`pic`), as a view the shader reads them from.
    fn taps_view(&self, kit: &Kit, pic: (u32, u32), work: (u32, u32), wanted: bool) -> wgpu::TextureView {
        if !wanted {
            return kit.blank.create_view(&Default::default());
        }
        let key = (pic.0, pic.1, work.0, work.1);
        let mut kept = kit.taps.lock().unwrap();
        if kept.as_ref().is_none_or(|t| t.key != key) {
            let rows = [linear_taps(work.0 as usize, pic.0 as usize, false), linear_taps(work.1 as usize, pic.1 as usize, true)];
            let width = pic.0.max(pic.1);
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("taps"),
                size: wgpu::Extent3d { width, height: 2, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let mut bytes = vec![0u8; width as usize * 2 * 8];
            for (y, row) in rows.iter().enumerate() {
                for (x, t) in row.iter().enumerate() {
                    let at = (y * width as usize + x) * 8;
                    for (i, v) in [t.0 as u16, t.1 as u16, t.2 as u16, t.3 as u16].into_iter().enumerate() {
                        bytes[at + i * 2..at + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            self.write_plane(&texture, &bytes, width * 8);
            *kept = Some(Taps { key, texture });
        }
        kept.as_ref().unwrap().texture.create_view(&Default::default())
    }

    /// The tint shader's bindings: the picture, the judged copy, the masks judged on it, the false colour table, the weights, the settings and the half way levels.
    fn tint_binds(&self, kit: &Kit, views: [&wgpu::TextureView; 4], taps: &wgpu::TextureView, settings: &wgpu::Buffer) -> wgpu::BindGroup {
        let halves = kit.halves.create_view(&Default::default());
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tint"),
            layout: &kit.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(views[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(views[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(views[2]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(views[3]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 5, resource: settings.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(taps) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&halves) },
            ],
        })
    }

    /// Draws `src` (as paint() leaves it) into `dst`, the same size, with the tools of `spec` on it; a `src` wider than 1280 is judged shrunk, as on the CPU.
    /// Everything the card refuses on the way (an allocation, a write, the submission) comes out as an error, never as `dst` left as it was.
    pub fn tint(&self, src: &wgpu::Texture, dst: &wgpu::Texture, spec: &TintSpec) -> Result<(), String> {
        self.card_ok()?;
        let (w, h) = (src.width(), src.height());
        if (dst.width(), dst.height()) != (w, h) || src.format() != WORK || dst.format() != WORK || w == 0 || h == 0 {
            return Err(format!("can't tint {w}x{h} {:?} into {}x{} {:?}", src.format(), dst.width(), dst.height(), dst.format()));
        }
        match self.scoped(|| self.tint_pass(src, dst, spec)).and_then(|done| done) {
            Ok(used) => {
                self.give_back(used);
                Ok(())
            }
            Err(e) => {
                // What a refused call left half made (the pipelines, the table, the weights) is made again
                self.tint_kit.lock().unwrap().take();
                Err(e)
            }
        }
    }

    /// The commands of `tint`, submitted; the textures they used, to give back once nothing refused them.
    fn tint_pass(&self, src: &wgpu::Texture, dst: &wgpu::Texture, spec: &TintSpec) -> Result<Vec<wgpu::Texture>, String> {
        let (w, h) = (src.width(), src.height());
        let kit = self.tint_kit();
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let (ww, wh) = work_size(w, h);
        let smaller = (ww, wh) != (w, h);
        let colours = spec.false_colour.map(|lut| {
            let mut kept = kit.lut.lock().unwrap();
            if kept.colours.as_ref() != Some(lut) {
                // Not trusted until the write is through
                kept.colours = None;
                let rgba: Vec<u8> = lut.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect();
                self.write_plane(&kept.texture, &rgba, 256 * 4);
                kept.colours = Some(*lut);
            }
            kept.texture.create_view(&Default::default())
        });
        let colours = colours.unwrap_or_else(|| kit.lut.lock().unwrap().texture.create_view(&Default::default()));
        let flags = spec.zebra.map_or(0, |_| ZEBRAS) | spec.peaking.map_or(0, |_| PEAKING) | spec.false_colour.map_or(0, |_| FALSE_COLOUR) | if smaller { SMALLER } else { 0 };
        let peak = spec.peaking.map_or([0; 3], |(_, rgb)| rgb);
        let settings: [u32; 12] = [
            flags, spec.zebra.unwrap_or(0) as u32, spec.peaking.map_or(0, |(t, _)| t as u32), spec.stripe_phase as u32,
            peak[0] as u32, peak[1] as u32, peak[2] as u32, 0,
            w, h, ww, wh,
        ];
        let buf = self.uniform(bytemuck_u32(&settings));
        let picture = src.create_view(&Default::default());
        let taps = self.taps_view(&kit, (w, h), (ww, wh), smaller && flags & (ZEBRAS | PEAKING) != 0);
        let bind = |work: &wgpu::TextureView, marks: &wgpu::TextureView| self.tint_binds(&kit, [&picture, work, marks, &colours], &taps, &buf);
        let (mut work, mut marks) = (picture.clone(), picture.clone());
        if smaller {
            let shrunk = self.texture(ww, wh, WORK);
            let shrunk_view = shrunk.create_view(&Default::default());
            draw(&mut enc, &kit.shrink, &[&bind(&picture, &picture)], &shrunk_view);
            if flags & (ZEBRAS | PEAKING) != 0 {
                let judged = self.texture(ww, wh, WORK);
                let judged_view = judged.create_view(&Default::default());
                draw(&mut enc, &kit.marks, &[&bind(&shrunk_view, &picture)], &judged_view);
                marks = judged_view;
                used.push(judged);
            }
            work = shrunk_view;
            used.push(shrunk);
        }
        draw(&mut enc, &kit.tint, &[&bind(&work, &marks)], &dst.create_view(&Default::default()));
        self.queue.submit([enc.finish()]);
        Ok(used)
    }

    /// Starts bringing `src` (as paint() leaves it) back shrunk to at most `width` wide by area, as RGBA: the small picture the scopes work from,
    /// about 150 KB at 256 wide, in place of the whole preview coming back. Never waits: `scope_poll` says when it has arrived.
    pub fn scope_start(&self, src: &wgpu::Texture, width: u32) -> Result<ScopeRead, String> {
        self.card_ok()?;
        let (w, h) = (src.width(), src.height());
        if src.format() != WORK || w == 0 || h == 0 || width == 0 {
            return Err(format!("can't shrink {w}x{h} {:?} to {width} wide", src.format()));
        }
        let (ow, oh) = shrunk_to(w, h, width);
        transfers::preview_down();
        let row = aligned_row(ow * 4);
        let (buffer, done, small) = self.scoped(|| {
            let mut enc = self.device.create_command_encoder(&Default::default());
            let small = ((ow, oh) != (w, h)).then(|| {
                let kit = self.tint_kit();
                let settings: [u32; 12] = [0, 0, 0, 0, 0, 0, 0, 0, w, h, ow, oh];
                let buf = self.uniform(bytemuck_u32(&settings));
                let picture = src.create_view(&Default::default());
                let colours = kit.lut.lock().unwrap().texture.create_view(&Default::default());
                let taps = self.taps_view(&kit, (w, h), (ow, oh), false);
                let bind = self.tint_binds(&kit, [&picture, &picture, &picture, &colours], &taps, &buf);
                let small = self.texture(ow, oh, WORK);
                draw(&mut enc, &kit.shrink, &[&bind], &small.create_view(&Default::default()));
                small
            });
            let from = small.as_ref().unwrap_or(src);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope"),
                size: row as u64 * oh as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(from.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            }, from.size());
            self.queue.submit([enc.finish()]);
            let (tx, done) = mpsc::channel();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r.map_err(|e| e.to_string()));
            });
            (buffer, done, small)
        }).inspect_err(|_| {
            // What a refused call left half made is made again, as on the tint path
            self.tint_kit.lock().unwrap().take();
        })?;
        // Handed back only once the card took all of it: the next use of it is queued behind the copy
        self.give_back(small);
        Ok(ScopeRead { buffer, size: (ow, oh), row, done, began: Instant::now() })
    }

    /// Looks, without waiting, whether a scope's picture has come back.
    pub fn scope_poll(&self, read: &ScopeRead) -> Scope {
        let arrived = self.guarded("waiting for the scope's picture", || {
            self.device.poll(wgpu::PollType::Poll).map_err(|e| e.to_string())?;
            Ok(read.done.try_recv())
        });
        match arrived {
            Err(e) | Ok(Ok(Err(e))) => Scope::Failed(e),
            Ok(Err(mpsc::TryRecvError::Empty)) => Scope::Waiting,
            Ok(Err(mpsc::TryRecvError::Disconnected)) => Scope::Failed("the card dropped the readback".into()),
            Ok(Ok(Ok(()))) => match self.scope_frame(read) {
                Ok(frame) => Scope::Ready(frame),
                Err(e) => Scope::Failed(e),
            },
        }
    }

    /// Marks the card lost, and says why, when the device was removed (a driver reset); never on other backends.
    pub fn check_removed(&self) -> Option<String> {
        let why = device_removed(&self.device)?;
        self.lost.store(true, Ordering::SeqCst);
        log::error!("The graphics card was removed ({why})");
        Some(why)
    }

    /// The rows of a mapped scope buffer without their padding.
    fn scope_frame(&self, read: &ScopeRead) -> Result<Frame, String> {
        // wgpu 30 on DX12 counts a removed device's fence as done without saying the device was lost, which leaves the buffer as it was
        if let Some(why) = self.check_removed() {
            return Err(format!("the graphics card was removed ({why})"));
        }
        let (w, h) = read.size;
        let mapped = read.buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
        let (row, stride) = (w as usize * 4, read.row as usize);
        let mut out = spare_bytes(row * h as usize);
        for y in 0..h as usize {
            out[y * row..(y + 1) * row].copy_from_slice(&mapped[y * stride..y * stride + row]);
        }
        drop(mapped);
        read.buffer.unmap();
        Ok(Frame::new(w, h, PixelFormat::Rgba, out))
    }

    /// `scope_start` and waiting for it, for a thread that may wait (tests, and anything off the window's thread).
    pub fn scope_picture(&self, src: &wgpu::Texture, width: u32) -> Result<Frame, String> {
        let read = self.scope_start(src, width)?;
        let until = Instant::now() + self.wait_limit;
        loop {
            match self.scope_poll(&read) {
                Scope::Ready(frame) => return Ok(frame),
                Scope::Failed(e) => return Err(e),
                Scope::Waiting if Instant::now() > until => return Err("the scope's picture didn't come back in time".into()),
                Scope::Waiting => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// A frame painted at its own size, tinted and brought back as RGBA, to compare with the CPU's.
    pub fn tint_rgba(&self, frame: &Frame, spec: &TintSpec) -> Result<Vec<u8>, String> {
        self.card_ok()?;
        let (w, h) = (frame.width, frame.height);
        let (plain, tinted) = (self.texture(w, h, WORK), self.texture(w, h, WORK));
        let out = self.paint(frame, &plain).and_then(|()| self.tint(&plain, &tinted, spec)).and_then(|()| {
            let enc = self.device.create_command_encoder(&Default::default());
            self.download(enc, &[&tinted], &[4])
        });
        self.give_back([plain, tinted]);
        out
    }
}

/// A scope's small picture on its way back from the card.
pub struct ScopeRead {
    buffer: wgpu::Buffer,
    size: (u32, u32),
    /// Bytes a row of the buffer takes, padding included.
    row: u32,
    done: mpsc::Receiver<Result<(), String>>,
    began: Instant,
}

/// Where a `ScopeRead` is.
pub enum Scope {
    Waiting,
    Ready(Frame),
    Failed(String),
}

/// Misses in a row, until there have been too many.
#[derive(Default)]
struct Strikes(u32);

impl Strikes {
    fn miss(&mut self) {
        self.0 += 1;
    }

    fn clear(&mut self) {
        self.0 = 0;
    }

    fn out(&self) -> bool {
        self.0 >= MISSES
    }
}

/// The scopes' pictures read back from the card with one in flight at a time, so the window never waits for one: asked for again only once the last has
/// come (or has been given up on, after SCOPE_PATIENCE), and after MISSES misses in a row it asks no more (the scopes are the CPU's then).
#[derive(Default)]
pub struct ScopeSamples {
    flight: Option<ScopeRead>,
    misses: Strikes,
}

impl ScopeSamples {
    /// Starts a read of `src` unless one is on its way already or the card has been given up on. Never waits.
    pub fn start(&mut self, gpu: &Gpu, src: &wgpu::Texture, width: u32) {
        if self.flight.is_some() || self.misses.out() {
            return;
        }
        match gpu.scope_start(src, width) {
            Ok(read) => self.flight = Some(read),
            Err(e) => {
                log::warn!("Couldn't start reading back the preview for the scopes: {e}");
                self.misses.miss();
            }
        }
    }

    /// The picture, when the read in flight has finished by `now`. Never waits: a read that is late stays in flight, and one that has been
    /// later than the patience is given up on and counts as a miss.
    pub fn poll(&mut self, gpu: &Gpu, now: Instant) -> Option<Frame> {
        let read = self.flight.as_ref()?;
        let began = read.began;
        match gpu.scope_poll(read) {
            Scope::Ready(frame) => {
                self.flight = None;
                self.misses.clear();
                Some(frame)
            }
            Scope::Failed(e) => {
                log::warn!("Couldn't read back the preview for the scopes: {e}");
                self.flight = None;
                self.misses.miss();
                None
            }
            Scope::Waiting => {
                if now.saturating_duration_since(began) > SCOPE_PATIENCE {
                    log::warn!("The graphics card is too busy to bring the scopes' picture back in time");
                    self.flight = None;
                    self.misses.miss();
                }
                None
            }
        }
    }

    /// Too many misses in a row: the scopes' pictures are made another way from now on.
    pub fn gave_up(&self) -> bool {
        self.misses.out()
    }

    /// A read is on its way.
    pub fn in_flight(&self) -> bool {
        self.flight.is_some()
    }

    /// A different stream: the read on its way was of the last one's picture.
    pub fn clear(&mut self) {
        self.flight = None;
    }
}

/// The preview's frames painted on the window's own device, two textures taking turns so the one Slint shows isn't drawn over. A tinted frame is
/// painted into `plain` first and tinted into the one Slint shows; the scopes' small copy is read back without the window waiting for it.
pub struct CardPreview {
    gpu: Arc<Gpu>,
    pictures: [Option<wgpu::Texture>; 2],
    plain: Option<wgpu::Texture>,
    next: usize,
    /// The widest a scope's picture is.
    scope_width: u32,
    scope: bool,
    samples: ScopeSamples,
    /// Paints the card refused, in a row; once out it stays out until `restart`.
    refused: Strikes,
    /// Paints since the card was last asked whether it was removed.
    since_check: u32,
}

impl CardPreview {
    pub fn new(gpu: Arc<Gpu>, scope_width: u32) -> CardPreview {
        CardPreview { gpu, pictures: [None, None], plain: None, next: 0, scope_width, scope: false, samples: ScopeSamples::default(), refused: Strikes::default(), since_check: 0 }
    }

    /// The frame as a texture of this size, with the tools of `tint` on it when there is one.
    pub fn paint(&mut self, frame: &Frame, width: u32, height: u32, tint: Option<&TintSpec>) -> Option<wgpu::Texture> {
        self.paint_with(width, height, tint, |gpu, target| gpu.paint(frame, target))
    }

    /// Draw a pipeline texture on the same device, landing only for a different device.
    pub fn paint_picture(&mut self, frame: &telescope_core::media::PipelineFrame, width: u32, height: u32, tint: Option<&TintSpec>) -> Option<wgpu::Texture> {
        self.paint_with(width, height, tint, |gpu, target| {
            if let Some(card) = frame.picture().and_then(|p| p.as_any().downcast_ref::<CardFrame>()) {
                if let Some(result) = card.paint(gpu, target) {
                    return result;
                }
            }
            gpu.paint(&frame.land(), target)
        })
    }

    fn paint_with(&mut self, width: u32, height: u32, tint: Option<&TintSpec>, paint: impl FnOnce(&Gpu, &wgpu::Texture) -> Result<(), String>) -> Option<wgpu::Texture> {
        let gpu = self.gpu.clone();
        // Without a scope nothing is read back, so a removed card is looked for here every so often
        self.since_check += 1;
        if self.since_check >= REMOVED_CHECK_EVERY {
            self.since_check = 0;
            gpu.check_removed();
        }
        let of_size = |slot: &mut Option<wgpu::Texture>| {
            if slot.as_ref().is_none_or(|t| (t.width(), t.height()) != (width, height)) {
                *slot = Some(gpu.picture(width, height));
            }
            slot.clone()
        };
        let shown = of_size(&mut self.pictures[self.next])?;
        self.next = 1 - self.next;
        let plain = if tint.is_some() { of_size(&mut self.plain)? } else { shown.clone() };
        let painted = paint(&gpu, &plain).and_then(|()| {
            // The scopes judge the picture before any tint, at their own small size
            if self.scope {
                self.samples.start(&gpu, &plain, self.scope_width);
            }
            match tint {
                Some(spec) => gpu.tint(&plain, &shown, spec),
                None => Ok(()),
            }
        });
        match painted {
            Ok(()) => {
                // Good paints between refusals break the run, but a card already given up on stays out
                if !self.refused.out() {
                    self.refused.clear();
                }
                Some(shown)
            }
            Err(e) => {
                log::warn!("Couldn't paint the preview: {e}");
                self.refused.miss();
                None
            }
        }
    }

    /// The card was lost: nothing can be painted on it any more.
    pub fn is_lost(&self) -> bool {
        self.gpu.is_lost()
    }

    /// Whether the tints and the scopes' pictures are still the card's: not after it was lost, refused paints in a row, or was too busy to bring back scope pictures.
    pub fn on_card(&self) -> bool {
        !self.gpu.is_lost() && !self.refused.out() && !self.samples.gave_up()
    }

    /// Whether the next paints also start a read of the picture for the scopes.
    pub fn want_scope(&mut self, wanted: bool) {
        self.scope = wanted;
    }

    /// The scopes' picture, if the read that was started has come back by now. Never waits.
    pub fn take_scope(&mut self) -> Option<Frame> {
        self.samples.poll(&self.gpu, Instant::now())
    }

    /// A different stream: no read of the last one's picture is wanted.
    pub fn restart(&mut self) {
        self.scope = false;
        self.samples.clear();
        self.samples.misses.clear();
        self.refused.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32, value: u8) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, vec![value; (w * h * 4) as usize])
    }

    /// A read the card never finishes: its answer is held back by the sender it comes with.
    fn stalled(gpu: &Gpu) -> (ScopeRead, mpsc::Sender<Result<(), String>>) {
        let (tx, done) = mpsc::channel();
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 256, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        (ScopeRead { buffer, size: (4, 1), row: 256, done, began: Instant::now() }, tx)
    }

    #[test]
    fn a_scope_picture_that_never_comes_back_holds_nothing_up_and_the_card_is_given_up_on_after_three() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let source = gpu.picture(640, 360);
        let mut samples = ScopeSamples::default();
        let began = Instant::now();
        for round in 1..=MISSES {
            let (read, _held_back) = stalled(&gpu);
            let at = read.began;
            samples.flight = Some(read);
            // One read is in flight: asking for another starts nothing, and looking doesn't wait
            samples.start(&gpu, &source, 256);
            assert!(samples.in_flight());
            assert!(samples.poll(&gpu, at).is_none() && samples.in_flight(), "round {round}: still on its way");
            assert!(!samples.gave_up() || round == MISSES);
            // Past the patience it is let go of, and counts
            assert!(samples.poll(&gpu, at + SCOPE_PATIENCE + Duration::from_millis(1)).is_none());
            assert!(!samples.in_flight(), "round {round}: given up on");
            assert_eq!(samples.gave_up(), round == MISSES, "round {round}");
        }
        assert!(began.elapsed() < SCOPE_PATIENCE, "nothing waited out a patience");
        // Given up on: no read is started again
        samples.start(&gpu, &source, 256);
        assert!(!samples.in_flight() && samples.gave_up());
    }

    #[test]
    fn successful_preview_paints_clear_consecutive_failures() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut card = CardPreview::new(gpu, 256);
        let frame = rgba(64, 64, 10);
        for _ in 0..MISSES + 1 {
            card.refused.miss();
            assert!(card.on_card());
            assert!(card.paint(&frame, 64, 64, None).is_some());
            assert_eq!(card.refused.0, 0);
            assert!(card.on_card());
        }
        for _ in 0..MISSES {
            card.refused.miss();
        }
        assert!(!card.on_card());
        assert!(card.paint(&frame, 64, 64, None).is_some());
        assert!(!card.on_card(), "a good untinted paint doesn't bring the card route back");
        card.restart();
        assert!(card.on_card());
    }

    #[test]
    fn a_scope_picture_that_comes_back_clears_the_misses() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let source = gpu.picture(640, 360);
        gpu.paint(&rgba(640, 360, 90), &source).unwrap();
        let mut samples = ScopeSamples::default();
        let (read, _held_back) = stalled(&gpu);
        let at = read.began;
        samples.flight = Some(read);
        samples.poll(&gpu, at + SCOPE_PATIENCE * 2);
        assert_eq!(samples.misses.0, 1);
        samples.start(&gpu, &source, 256);
        assert!(samples.in_flight());
        let until = Instant::now() + Duration::from_secs(20);
        let small = loop {
            if let Some(small) = samples.poll(&gpu, Instant::now()) {
                break small;
            }
            assert!(Instant::now() < until, "the picture never came back");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!((small.width, small.height), (256, 144));
        assert!(small.data.as_chunks::<4>().0.iter().all(|p| p[..3].iter().all(|&c| c.abs_diff(90) <= 1)));
        assert_eq!(samples.misses.0, 0);
        assert!(!samples.gave_up());
    }

    #[test]
    fn a_tint_the_card_refuses_is_an_error_and_not_the_old_pixels_passed_off_as_new() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let src = gpu.picture(64, 64);
        gpu.paint(&rgba(64, 64, 200), &src).unwrap();
        let spec = TintSpec { zebra: Some(100), ..TintSpec::default() };
        // A target that can't be drawn into: the card refuses the pass, which used to go unnoticed
        let no_target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no target"),
            size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        assert!(gpu.tint(&src, &no_target, &spec).is_err());
        // And the card is no worse for it
        let good = gpu.picture(64, 64);
        gpu.tint(&src, &good, &spec).unwrap();
        assert!(!gpu.is_lost());
    }

    #[test]
    fn the_shaders_whole_number_brightness_and_its_half_way_table_are_the_cpus_float_one_for_every_colour() {
        let halves = half_way_levels();
        assert!(halves.iter().filter(|&&l| l != 0).count() > 3000, "the table has its colours");
        for r in 0..256u32 {
            for g in 0..256u32 {
                for b in 0..256u32 {
                    let n = 2126 * r + 7152 * g + 722 * b;
                    let (q, rest) = (n / 10000, n % 10000);
                    let level = if rest == 5000 { halves[(g * 256 + r) as usize] as u32 } else { q + u32::from(rest > 5000) };
                    if level != float_luma(r as u8, g as u8, b as u8) as u32 {
                        panic!("({r}, {g}, {b}): {level} in whole numbers, {} in floats", float_luma(r as u8, g as u8, b as u8));
                    }
                }
            }
        }
    }

    #[test]
    fn the_taps_are_the_cpus() {
        use telescope_core::image::{resample_linear, resize_linear_fixed};
        // A picture of noise grown by these taps is the CPU's own bilinear resize to the byte
        for ((n, m), (k, l)) in [((1280usize, 1531usize), (742usize, 887usize)), ((200, 641), (113, 360)), ((7, 9), (5, 8))] {
            let mut x = 12345u32;
            let src: Vec<u8> = (0..n * k).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 24) as u8 }).collect();
            let grown = resample_linear(&src, n, k, 1, &linear_taps(n, m, false), &linear_taps(k, l, true));
            assert_eq!(grown, resize_linear_fixed(&src, n, k, 1, m, l), "{n}x{k} to {m}x{l}");
        }
    }
}
