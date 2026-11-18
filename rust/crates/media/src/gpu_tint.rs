//! Cinema's zebras, focus peaking and false colour on the card (shaders/tint.wgsl), to match effects' cinema_tools::tint.

use super::*;
use telescope_core::preview::TintSpec;

const TINT: &str = include_str!("shaders/tint.wgsl");
/// px, the widest the tools judge a picture at (cinema_tools::WORK_WIDTH); a wider one is judged on a copy shrunk to it.
const WORK_WIDTH: u32 = 1280;

const ZEBRAS: u32 = 1;
const PEAKING: u32 = 2;
const FALSE_COLOUR: u32 = 4;
const SMALLER: u32 = 8;

/// The tint shader's pipelines, built the first time a picture is tinted.
pub(super) struct Kit {
    layout: wgpu::BindGroupLayout,
    shrink: wgpu::RenderPipeline,
    marks: wgpu::RenderPipeline,
    tint: wgpu::RenderPipeline,
    lut: Mutex<Lut>,
}

/// The false colour table as it sits on the card, 256 x 1.
struct Lut {
    colours: [[u8; 3]; 256],
    texture: wgpu::Texture,
}

impl Kit {
    fn new(gpu: &Gpu) -> Kit {
        let layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tint"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), tex_entry(3), sampler_entry(4), uniform_entry(5)],
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
        Kit {
            shrink: pipeline("tint shrink", "fs_shrink"),
            marks: pipeline("tint marks", "fs_marks"),
            tint: pipeline("tint", "fs_tint"),
            lut: Mutex::new(Lut { colours: [[0; 3]; 256], texture }),
            layout,
        }
    }
}

/// The size the tools judge a w x h picture at: as scopes::shrunk_size has it, at most WORK_WIDTH wide, the shape kept.
fn work_size(w: u32, h: u32) -> (u32, u32) {
    if w <= WORK_WIDTH {
        return (w, h);
    }
    (WORK_WIDTH, (((h as u64 * WORK_WIDTH as u64) as f64 / w as f64).round_ties_even() as u32).max(1))
}

impl Gpu {
    fn tint_kit(&self) -> Arc<Kit> {
        self.tint_kit.lock().unwrap().get_or_insert_with(|| Arc::new(Kit::new(self))).clone()
    }

    /// Draws `src` (as paint() leaves it) into `dst`, the same size, with the tools of `spec` on it; a `src` wider than 1280 is judged shrunk, as on the CPU.
    pub fn tint(&self, src: &wgpu::Texture, dst: &wgpu::Texture, spec: &TintSpec) -> Result<(), String> {
        self.card_ok()?;
        let (w, h) = (src.width(), src.height());
        if (dst.width(), dst.height()) != (w, h) || src.format() != WORK || dst.format() != WORK || w == 0 || h == 0 {
            return Err(format!("can't tint {w}x{h} {:?} into {}x{} {:?}", src.format(), dst.width(), dst.height(), dst.format()));
        }
        let kit = self.tint_kit();
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let (ww, wh) = work_size(w, h);
        let smaller = (ww, wh) != (w, h);
        let colours = spec.false_colour.map(|lut| {
            let mut kept = kit.lut.lock().unwrap();
            if kept.colours != *lut {
                let rgba: Vec<u8> = lut.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect();
                self.write_plane(&kept.texture, &rgba, 256 * 4);
                kept.colours = *lut;
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
        let bind = |work: &wgpu::TextureView, marks: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tint"),
                layout: &kit.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&picture) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(work) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(marks) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&colours) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.linear) },
                    wgpu::BindGroupEntry { binding: 5, resource: buf.as_entire_binding() },
                ],
            })
        };
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
        self.give_back(used);
        Ok(())
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
