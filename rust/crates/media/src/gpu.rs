//! Frames on the graphics card through wgpu: plugins' shader steps (core::shader has what a shader may use) and the
//! preview's scaling and colour conversion. A frame goes up once, through every shader in a run, and comes back once.
//! wgpu because it's one API over Vulkan, Direct3D 12 and OpenGL, runs WGSL (what Lua and WASM plugins hand over) and
//! is what Slint 1.18 can take textures from (unstable-wgpu-30), so the preview can later skip the download too.

use crate::pool::Pool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::{Frame, PixelFormat, ShaderStep};
use telescope_core::shader;
use wgpu::naga;

const CONVERT: &str = include_str!("shaders/convert.wgsl");

/// What a working picture on the card is kept as: gamma-encoded RGBA, like the camera's own values.
const WORK: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The convert shader's modes (convert.wgsl).
#[derive(Clone, Copy)]
#[repr(u32)]
enum Mode {
    I420 = 0,
    Nv12 = 1,
    Rgba = 2,
    ToY = 3,
    ToU = 4,
    ToV = 5,
    ToUv = 6,
}

/// The most memory the kept textures take: a few 4K pictures' worth, with the sizes least recently used dropped first.
const POOL_BUDGET: usize = 256 * 1024 * 1024;

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// For the convert shader: three planes, a sampler and its settings.
    convert_layout: wgpu::BindGroupLayout,
    convert: Mutex<HashMap<wgpu::TextureFormat, Arc<wgpu::RenderPipeline>>>,
    convert_module: wgpu::ShaderModule,
    /// For plugin shaders: the picture so far, the sampler and the frame's info.
    frame_layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    /// Stands in for planes a format doesn't have.
    blank: wgpu::TextureView,
    pool: Mutex<Pool<(u32, u32, wgpu::TextureFormat), wgpu::Texture>>,
    pub adapter_name: String,
}

/// One picture on the card in its own format, ready for the convert shader.
struct Planes {
    views: [wgpu::TextureView; 3],
    mode: Mode,
    width: u32,
    height: u32,
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn chroma(n: u32) -> u32 {
    n.div_ceil(2)
}

fn aligned_row(bytes: u32) -> u32 {
    bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

impl Gpu {
    /// The app's one graphics card connection, opened on first use; None when there's no card wgpu can use or
    /// TELESCOPE_NO_GPU is set (for a driver that misbehaves, and for comparing).
    pub fn shared() -> Option<Arc<Gpu>> {
        static GPU: OnceLock<Option<Arc<Gpu>>> = OnceLock::new();
        GPU.get_or_init(|| match std::env::var_os("TELESCOPE_NO_GPU").map_or_else(Gpu::open, |_| Err("TELESCOPE_NO_GPU is set".into())) {
            Ok(gpu) => {
                log::info!("Frame steps run on {}", gpu.adapter_name);
                Some(Arc::new(gpu))
            }
            Err(why) => {
                log::warn!("No graphics card for frame steps: {why}");
                None
            }
        }).clone()
    }

    fn open() -> Result<Gpu, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(desc.with_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("telescope frames"),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        Ok(Gpu::on(device, queue, format!("{} ({:?})", info.name, info.backend)))
    }

    /// The same shaders on a device someone else opened (the window's, so the preview can hand its texture to Slint).
    pub fn on_device(device: wgpu::Device, queue: wgpu::Queue) -> Gpu {
        Gpu::on(device, queue, "the window's device".into())
    }

    fn on(device: wgpu::Device, queue: wgpu::Queue, adapter_name: String) -> Gpu {
        // Without this a mistake in a plugin's shader that slips past the checks would end the app
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::error!("Graphics card error: {e}")));

        let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), sampler_entry(3), uniform_entry(4)],
        });
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2)],
        });
        let convert_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("convert"),
            source: wgpu::ShaderSource::Wgsl(CONVERT.into()),
        });
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blank = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blank"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        }).create_view(&Default::default());
        Gpu {
            device,
            queue,
            convert_layout,
            convert: Mutex::default(),
            convert_module,
            frame_layout,
            linear,
            blank,
            pool: Mutex::new(Pool::new(POOL_BUDGET)),
            adapter_name,
        }
    }

    fn convert_pipeline(&self, target: wgpu::TextureFormat) -> Arc<wgpu::RenderPipeline> {
        let mut cache = self.convert.lock().unwrap();
        cache.entry(target).or_insert_with(|| {
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("convert"),
                bind_group_layouts: &[Some(&self.convert_layout)],
                immediate_size: 0,
            });
            Arc::new(self.render_pipeline("convert", &layout, &self.convert_module, ("vs_main", "fs_main"), target))
        }).clone()
    }

    fn render_pipeline(&self, label: &str, layout: &wgpu::PipelineLayout, module: &wgpu::ShaderModule, entries: (&str, &str), target: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState { module, entry_point: Some(entries.0), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(entries.1),
                compilation_options: Default::default(),
                targets: &[Some(target.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn texture(&self, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        if let Some(t) = self.pool.lock().unwrap().take((w, h, format)) {
            return t;
        }
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Hands textures back for the next frame of the same size.
    fn give_back(&self, textures: impl IntoIterator<Item = wgpu::Texture>) {
        let mut pool = self.pool.lock().unwrap();
        for t in textures {
            let bytes = t.width() as usize * t.height() as usize * t.format().block_copy_size(None).unwrap_or(8) as usize;
            pool.give((t.width(), t.height(), t.format()), t, bytes);
        }
    }

    fn write_plane(&self, tex: &wgpu::Texture, data: &[u8], bytes_per_row: u32) {
        self.queue.write_texture(
            tex.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: None },
            tex.size(),
        );
    }

    /// Puts a frame on the card as it is (textures to give back once used).
    fn upload(&self, frame: &Frame, used: &mut Vec<wgpu::Texture>) -> Planes {
        let (w, h) = (frame.width, frame.height);
        let (cw, ch) = (chroma(w), chroma(h));
        let data = &frame.data[..];
        let mut plane = |pw: u32, ph: u32, format: wgpu::TextureFormat, bytes: &[u8], bpp: u32| {
            let t = self.texture(pw, ph, format);
            self.write_plane(&t, bytes, pw * bpp);
            let v = t.create_view(&Default::default());
            used.push(t);
            v
        };
        let blank = || self.blank.clone();
        match frame.format {
            PixelFormat::I420 => {
                let (ys, cs) = ((w * h) as usize, (cw * ch) as usize);
                let y = plane(w, h, wgpu::TextureFormat::R8Unorm, &data[..ys], 1);
                let u = plane(cw, ch, wgpu::TextureFormat::R8Unorm, &data[ys..ys + cs], 1);
                let v = plane(cw, ch, wgpu::TextureFormat::R8Unorm, &data[ys + cs..ys + 2 * cs], 1);
                Planes { views: [y, u, v], mode: Mode::I420, width: w, height: h }
            }
            PixelFormat::Nv12 => {
                let ys = (w * h) as usize;
                let y = plane(w, h, wgpu::TextureFormat::R8Unorm, &data[..ys], 1);
                let uv = plane(cw, ch, wgpu::TextureFormat::Rg8Unorm, &data[ys..], 2);
                Planes { views: [y, uv, blank()], mode: Mode::Nv12, width: w, height: h }
            }
            PixelFormat::Rgba => {
                let p = plane(w, h, wgpu::TextureFormat::Rgba8Unorm, data, 4);
                Planes { views: [p, blank(), blank()], mode: Mode::Rgba, width: w, height: h }
            }
            // Sampling a BGRA texture gives RGBA, so it needs nothing else
            PixelFormat::Bgra => {
                let p = plane(w, h, wgpu::TextureFormat::Bgra8Unorm, data, 4);
                Planes { views: [p, blank(), blank()], mode: Mode::Rgba, width: w, height: h }
            }
            other => unreachable!("{other:?} frames can't go on the card yet"),
        }
    }

    /// One pass of the convert shader from planes into target. taps: samples a side per output pixel, for shrinking.
    fn convert_pass(&self, enc: &mut wgpu::CommandEncoder, planes: &[wgpu::TextureView; 3], mode: Mode, target: &wgpu::Texture, taps: u32, src_size: (u32, u32)) {
        let settings: [u32; 4] = [mode as u32, taps.max(1), src_size.0, src_size.1];
        let buf = self.uniform(bytemuck_u32(&settings));
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert"),
            layout: &self.convert_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&planes[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&planes[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&planes[2]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 4, resource: buf.as_entire_binding() },
            ],
        });
        let pipeline = self.convert_pipeline(target.format());
        let view = target.create_view(&Default::default());
        draw(enc, &pipeline, &[&bind], &view);
    }

    fn uniform(&self, bytes: &[u8]) -> wgpu::Buffer {
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (bytes.len().max(16) as u64).div_ceil(16) * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, bytes);
        buf
    }

    /// Copies textures into one buffer, waits for the card and hands back each one's rows without padding.
    fn download(&self, mut enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32]) -> Result<Vec<Vec<u8>>, String> {
        let mut offsets = Vec::new();
        let mut size = 0u64;
        for (t, b) in textures.iter().zip(bpp) {
            offsets.push(size);
            size += aligned_row(t.width() * b) as u64 * t.height() as u64;
        }
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("download"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for ((t, b), off) in textures.iter().zip(bpp).zip(&offsets) {
            enc.copy_texture_to_buffer(t.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: *off, bytes_per_row: Some(aligned_row(t.width() * b)), rows_per_image: None },
            }, t.size());
        }
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for ((t, b), off) in textures.iter().zip(bpp).zip(&offsets) {
            let (row, stride) = ((t.width() * b) as usize, aligned_row(t.width() * b) as usize);
            let mut plane = Vec::with_capacity(row * t.height() as usize);
            for y in 0..t.height() as usize {
                let at = *off as usize + y * stride;
                plane.extend_from_slice(&mapped[at..at + row]);
            }
            out.push(plane);
        }
        drop(mapped);
        buf.unmap();
        Ok(out)
    }

    /// Runs the shader steps one after the other on frame and hands back the result in the frame's own format.
    pub fn run_shaders(&self, frame: &Frame, run: &mut [ShaderRun], time: f32) -> Result<Frame, String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return Err(format!("{:?} frames can't go on the card", frame.format));
        }
        let (w, h) = (frame.width, frame.height);
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let mut a = self.texture(w, h, WORK);
        let mut b = self.texture(w, h, WORK);
        self.convert_pass(&mut enc, &planes.views, planes.mode, &a, 1, (planes.width, planes.height));
        let info = self.uniform(&shader::frame_uniform(w, h, time, frame.number));
        for r in run.iter_mut() {
            r.slot.draw(self, &mut enc, &a, &b, &info);
            std::mem::swap(&mut a, &mut b);
            *r.fails = 0;
        }
        let work = [a.create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
        let out = match frame.format {
            PixelFormat::I420 => {
                let (y, u, v) = (self.texture(w, h, wgpu::TextureFormat::R8Unorm), self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                                 self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToU, &u, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToV, &v, 1, (w, h));
                let planes = self.download(enc, &[&y, &u, &v], &[1, 1, 1])?;
                used.extend([y, u, v]);
                planes.concat()
            }
            PixelFormat::Nv12 => {
                let (y, uv) = (self.texture(w, h, wgpu::TextureFormat::R8Unorm), self.texture(chroma(w), chroma(h), wgpu::TextureFormat::Rg8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToUv, &uv, 1, (w, h));
                let planes = self.download(enc, &[&y, &uv], &[1, 2])?;
                used.extend([y, uv]);
                planes.concat()
            }
            PixelFormat::Bgra => {
                let t = self.texture(w, h, wgpu::TextureFormat::Bgra8Unorm);
                self.convert_pass(&mut enc, &work, Mode::Rgba, &t, 1, (w, h));
                let p = self.download(enc, &[&t], &[4])?;
                used.push(t);
                p.concat()
            }
            _ => self.download(enc, &[&a], &[4])?.concat(),
        };
        used.extend([a, b]);
        self.give_back(used);
        let mut result = Frame::new(w, h, frame.format, out);
        result.number = frame.number;
        result.time_ns = frame.time_ns;
        Ok(result)
    }

    /// The frame scaled and converted into target (RGBA, out_w x out_h), left on the card for whoever draws it.
    pub fn paint(&self, frame: &Frame, target: &wgpu::Texture) -> Result<(), String> {
        let (out_w, out_h) = (target.width(), target.height());
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) || out_w == 0 || out_h == 0 {
            return Err(format!("can't scale {:?} to {out_w}x{out_h}", frame.format));
        }
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let taps = (frame.width.div_ceil(out_w)).max(frame.height.div_ceil(out_h)).clamp(1, 8);
        self.convert_pass(&mut enc, &planes.views, planes.mode, target, taps, (planes.width, planes.height));
        self.queue.submit([enc.finish()]);
        self.give_back(used);
        Ok(())
    }

    /// A texture paint() can draw into and Slint can show.
    pub fn picture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK,
            // COPY_SRC so a snapshot of the window can read it back
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// The frame as RGBA at another size, scaled and converted on the card (the preview).
    pub fn rgba(&self, frame: &Frame, out_w: u32, out_h: u32) -> Result<Vec<u8>, String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) || out_w == 0 || out_h == 0 {
            return Err(format!("can't scale {:?} to {out_w}x{out_h}", frame.format));
        }
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let target = self.texture(out_w, out_h, WORK);
        // Enough samples per output pixel that shrinking a lot still averages instead of skipping pixels
        let taps = (frame.width.div_ceil(out_w)).max(frame.height.div_ceil(out_h)).clamp(1, 8);
        self.convert_pass(&mut enc, &planes.views, planes.mode, &target, taps, (planes.width, planes.height));
        let out = self.download(enc, &[&target], &[4])?.pop().unwrap_or_default();
        used.push(target);
        self.give_back(used);
        Ok(out)
    }
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding and any byte pattern is a valid u8
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn draw(enc: &mut wgpu::CommandEncoder, pipeline: &wgpu::RenderPipeline, binds: &[&wgpu::BindGroup], view: &wgpu::TextureView) {
    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    for (i, b) in binds.iter().enumerate() {
        pass.set_bind_group(i as u32, *b, &[]);
    }
    pass.draw(0..3, 0..1);
}

/// A shader step and what it became on the card, built the first time it runs.
pub struct ShaderSlot {
    step: ShaderStep,
    built: Option<Result<Built, String>>,
}

/// A shader step while a run uses it.
pub struct ShaderRun<'a> {
    pub slot: &'a mut ShaderSlot,
    pub fails: &'a mut u32,
}

struct Built {
    pipeline: wgpu::RenderPipeline,
    /// Group 1: the plugin's uniforms and textures, or None when it has neither.
    extras: Option<wgpu::BindGroup>,
    _keep: Vec<wgpu::Texture>,
}

impl ShaderSlot {
    pub fn new(step: ShaderStep) -> ShaderSlot {
        ShaderSlot { step, built: None }
    }

    /// Builds the shader the first time; Err for one that won't ever run.
    pub fn prepare(&mut self, gpu: &Gpu) -> Result<(), String> {
        if self.built.is_none() {
            self.built = Some(build(gpu, &self.step));
        }
        self.built.as_ref().unwrap().as_ref().map(|_| ()).map_err(Clone::clone)
    }

    fn draw(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, dst: &wgpu::Texture, info: &wgpu::Buffer) {
        let Some(Ok(built)) = &self.built else { return };
        let src_view = src.create_view(&Default::default());
        let frame_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: &gpu.frame_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.linear) },
                wgpu::BindGroupEntry { binding: 2, resource: info.as_entire_binding() },
            ],
        });
        let mut binds = vec![&frame_bind];
        binds.extend(built.extras.as_ref());
        draw(enc, &built.pipeline, &binds, &dst.create_view(&Default::default()));
    }
}

/// Parses and checks the assembled module, for an error message that points at the plugin's line.
fn validate(src: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default())
        .validate(&module)
        .map_err(|e| e.emit_to_string(src))?;
    Ok(())
}

fn build(gpu: &Gpu, step: &ShaderStep) -> Result<Built, String> {
    // The contract lives in core::shader, shared with plugin-host's checks
    let src = shader::assemble(step)?;
    let params = shader::pack_uniforms(&step.wgsl, &step.uniforms)?;
    let has_uniforms = !params.is_empty();
    validate(&src)?;
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("plugin shader"),
        source: wgpu::ShaderSource::Wgsl(src.clone().into()),
    });
    let mut entries = Vec::new();
    if has_uniforms {
        entries.push(uniform_entry(shader::BINDING_PARAMS));
    }
    entries.extend((0..step.textures.len()).map(|i| tex_entry(shader::BINDING_FIRST_TEXTURE + i as u32)));
    let extras_layout = (!entries.is_empty()).then(|| gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("plugin"),
        entries: &entries,
    }));
    let layouts: Vec<Option<&wgpu::BindGroupLayout>> = [Some(&gpu.frame_layout), extras_layout.as_ref()].into_iter().take(if extras_layout.is_some() { 2 } else { 1 }).collect();
    let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("plugin"), bind_group_layouts: &layouts, immediate_size: 0 });
    let pipeline = gpu.render_pipeline("plugin", &layout, &module, (shader::VERTEX_ENTRY, shader::FRAGMENT_ENTRY), WORK);

    // The plugin's pictures go up once, as RGBA whatever they came as
    let mut keep = Vec::new();
    let mut views = Vec::new();
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    for (_, f) in &step.textures {
        let mut used = Vec::new();
        let planes = gpu.upload(f, &mut used);
        let t = gpu.texture(f.width, f.height, WORK);
        gpu.convert_pass(&mut enc, &planes.views, planes.mode, &t, 1, (f.width, f.height));
        views.push(t.create_view(&Default::default()));
        keep.push(t);
        keep.extend(used);
    }
    gpu.queue.submit([enc.finish()]);
    let ubuf = has_uniforms.then(|| gpu.uniform(&params));
    let extras = extras_layout.as_ref().map(|l| {
        let mut e = Vec::new();
        if let Some(b) = &ubuf {
            e.push(wgpu::BindGroupEntry { binding: shader::BINDING_PARAMS, resource: b.as_entire_binding() });
        }
        for (i, v) in views.iter().enumerate() {
            e.push(wgpu::BindGroupEntry { binding: shader::BINDING_FIRST_TEXTURE + i as u32, resource: wgpu::BindingResource::TextureView(v) });
        }
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("plugin"), layout: l, entries: &e })
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(e.to_string());
    }
    Ok(Built { pipeline, extras, _keep: keep })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(wgsl: &str, uniforms: serde_json::Value) -> ShaderStep {
        ShaderStep { wgsl: wgsl.into(), uniforms, textures: vec![] }
    }

    #[test]
    fn a_mistake_in_a_shader_points_at_it() {
        let src = shader::assemble(&step("fn effect(uv: vec2<f32>) -> vec4<f32> { return nope; }", json!({}))).unwrap();
        assert!(validate(&src).unwrap_err().contains("nope"));
    }

    fn yuv_frame(w: u32, h: u32, y: impl Fn(u32, u32) -> u8, u: u8, v: u8) -> Frame {
        let (cw, ch) = (chroma(w), chroma(h));
        let mut data: Vec<u8> = (0..h).flat_map(|r| (0..w).map(move |c| (r, c))).map(|(r, c)| y(c, r)).collect();
        data.extend(std::iter::repeat_n(u, (cw * ch) as usize));
        data.extend(std::iter::repeat_n(v, (cw * ch) as usize));
        Frame::new(w, h, PixelFormat::I420, data)
    }

    fn run(gpu: &Gpu, frame: &Frame, steps: &mut [ShaderSlot]) -> Frame {
        let mut fails = vec![0u32; steps.len()];
        let mut run: Vec<ShaderRun> = steps.iter_mut().zip(fails.iter_mut()).map(|(slot, fails)| {
            slot.prepare(gpu).unwrap();
            ShaderRun { slot, fails }
        }).collect();
        gpu.run_shaders(frame, &mut run, 0.0).unwrap()
    }

    const IDENTITY: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";

    #[test]
    fn a_handed_device_paints_the_preview_into_a_texture_it_keeps() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let window = Gpu::on_device(gpu.device.clone(), gpu.queue.clone());
        let frame = yuv_frame(64, 36, |x, _| (16 + x * 3) as u8, 128, 128);
        let target = window.picture(32, 18);
        window.paint(&frame, &target).unwrap();
        let enc = window.device.create_command_encoder(&Default::default());
        let painted = window.download(enc, &[&target], &[4]).unwrap().pop().unwrap();
        assert_eq!(painted, gpu.rgba(&frame, 32, 18).unwrap());
    }

    #[test]
    fn a_picture_comes_back_from_the_card_as_it_went() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        // Odd sizes and a gradient, so a plane put back one row or column off shows
        // Colours inside what RGB can hold, or clipping them would count as a difference
        let frame = yuv_frame(37, 21, |x, y| (40 + x * 3 + y * 2) as u8, 115, 140);
        let out = run(&gpu, &frame, &mut [ShaderSlot::new(step(IDENTITY, json!({})))]);
        assert_eq!((out.width, out.height, out.format), (37, 21, PixelFormat::I420));
        let worst = frame.data.iter().zip(out.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 3, "round trip off by {worst}");
    }

    #[test]
    fn shader_steps_run_one_after_the_other_with_their_uniforms() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let invert = "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(1.0 - c.rgb, 1.0); }";
        let scale = "struct Params { k: f32 }\nfn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * params.k; }";
        // Grey 235 is white: inverted it's black, and black scaled stays black; scaled first it's grey, then inverted
        let white = yuv_frame(8, 8, |_, _| 235, 128, 128);
        let out = run(&gpu, &white, &mut [ShaderSlot::new(step(scale, json!({"k": 0.5}))), ShaderSlot::new(step(invert, json!({})))]);
        assert!((out.data[0] as i32 - 126).abs() <= 3, "{}", out.data[0]);
        let out = run(&gpu, &white, &mut [ShaderSlot::new(step(invert, json!({}))), ShaderSlot::new(step(scale, json!({"k": 0.5})))]);
        assert!((out.data[0] as i32 - 16).abs() <= 2, "{}", out.data[0]);
    }

    #[test]
    fn a_plugin_texture_is_there_to_sample() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let logo = Frame::new(1, 1, PixelFormat::Rgba, vec![255, 255, 255, 255]);
        let s = ShaderStep {
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample_texture(logo, uv); }".into(),
            uniforms: json!({}),
            textures: vec![("logo".into(), logo)],
        };
        let out = run(&gpu, &yuv_frame(4, 4, |_, _| 16, 128, 128), &mut [ShaderSlot::new(s)]);
        assert!((out.data[0] as i32 - 235).abs() <= 2, "{}", out.data[0]);
    }

    #[test]
    fn a_broken_shader_is_refused_when_built() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut slot = ShaderSlot::new(step("fn effect(uv: vec2<f32>) -> vec4<f32> { return 1; }", json!({})));
        assert!(slot.prepare(&gpu).is_err());
        let mut slot = ShaderSlot::new(step("fn other() {}", json!({})));
        assert!(slot.prepare(&gpu).unwrap_err().contains("effect"));
    }

    #[test]
    fn the_preview_is_scaled_and_converted_on_the_card() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        // Left half black, right half white, shrunk four times: still black on the left and white on the right
        let frame = yuv_frame(64, 32, |x, _| if x < 32 { 16 } else { 235 }, 128, 128);
        let rgba = gpu.rgba(&frame, 16, 8).unwrap();
        assert_eq!(rgba.len(), 16 * 8 * 4);
        assert!(rgba[0] <= 3 && rgba[15 * 4] >= 252, "{} {}", rgba[0], rgba[15 * 4]);
        let grey = gpu.rgba(&yuv_frame(10, 10, |_, _| 126, 128, 128), 10, 10).unwrap();
        assert!((grey[0] as i32 - 128).abs() <= 2 && grey[0] == grey[1] && grey[1] == grey[2], "{:?}", &grey[..4]);
    }
}
