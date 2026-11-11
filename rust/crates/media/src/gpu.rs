//! Frames on the graphics card through wgpu: plugins' shader steps (core::shader has what a shader may use) and the
//! preview's scaling and colour conversion. A frame stays up while shader steps follow each other; pipeline.rs decides when it comes down.
//! wgpu because it's one API over Vulkan, Direct3D 12 and OpenGL, runs WGSL (what Lua and WASM plugins hand over) and
//! is what Slint 1.18 can take textures from (unstable-wgpu-30), so the preview can later skip the download too.

use crate::pool::Pool;
use crate::transfers;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::{Frame, PixelFormat, ShaderStep};
use telescope_core::image::spare_bytes;
use telescope_core::shader;
use telescope_core::time::{monotonic_ns, span};
use wgpu::naga;

/// Set on the convert shader's mode to read YUV chroma by index, a sample for its 2 x 2 block, as the CPU does (taps stay 1).
const BLOCK_CHROMA: u32 = 8;
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
const KEPT_DOWNLOADS: usize = 4;

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// For the convert shader: three planes, a sampler and its settings.
    convert_layout: wgpu::BindGroupLayout,
    convert: Mutex<HashMap<wgpu::TextureFormat, Arc<wgpu::RenderPipeline>>>,
    convert_module: wgpu::ShaderModule,
    /// Download buffers to use again: mapping a new one each frame costs the card a flush.
    downloads: Mutex<Vec<wgpu::Buffer>>,
    /// For plugin shaders: the picture so far, the sampler, the frame's info and the person mask.
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

/// What to ask for, in order: just looking for GL loads Mesa's big GL libraries, about 25 MB, so it waits until there's no Vulkan.
const BACKENDS: [wgpu::Backends; 2] = [wgpu::Backends::PRIMARY, wgpu::Backends::PRIMARY.union(wgpu::Backends::GL)];

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

    pub(crate) fn open() -> Result<Gpu, String> {
        let (mut found, mut why) = (None, String::new());
        for backends in BACKENDS {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = backends;
            let instance = wgpu::Instance::new(desc.with_env());
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })) {
                Ok(adapter) => {
                    found = Some(adapter);
                    break;
                }
                Err(e) => why = e.to_string(),
            }
        }
        let adapter = found.ok_or(why)?;
        let info = adapter.get_info();
        telescope_core::machine::set_gpu(info.vendor, &format!("{:?}", info.device_type), &info.name, &format!("{:?}", info.backend), &format!("{} {}", info.driver, info.driver_info));
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
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2), tex_entry(shader::BINDING_MASK)],
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
            downloads: Mutex::default(),
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

    /// Lets go of the kept textures and download buffers (the stream stopped), so they don't hold the card's memory while idle.
    pub fn release_idle(&self) {
        self.pool.lock().unwrap().clear();
        self.downloads.lock().unwrap().clear();
    }

    /// How many bytes of textures are kept for the next frame, for tests.
    #[cfg(test)]
    pub(crate) fn kept_bytes(&self) -> usize {
        self.pool.lock().unwrap().bytes()
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
        self.convert_pass_with(enc, planes, mode, target, taps, src_size, false);
    }

    fn convert_pass_with(&self, enc: &mut wgpu::CommandEncoder, planes: &[wgpu::TextureView; 3], mode: Mode, target: &wgpu::Texture, taps: u32, src_size: (u32, u32), block_chroma: bool) {
        let settings: [u32; 4] = [mode as u32 | if block_chroma { BLOCK_CHROMA } else { 0 }, taps.max(1), src_size.0, src_size.1];
        let buf = self.uniform(bytemuck_u32(&settings));
        let view = target.create_view(&Default::default());
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
        draw(enc, &self.convert_pipeline(target.format()), &[&bind], &view);
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

    fn download(&self, enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32]) -> Result<Vec<u8>, String> {
        self.download_timed(enc, textures, bpp).map(|(planes, _)| planes)
    }

    /// Copies textures into one buffer, waits for the card and hands back each one's rows without padding, the planes one after another in one buffer, and how long the wait was in ns.
    fn download_timed(&self, mut enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32]) -> Result<(Vec<u8>, u64), String> {
        let mut offsets = Vec::new();
        let mut size = 0u64;
        for (t, b) in textures.iter().zip(bpp) {
            offsets.push(size);
            size += aligned_row(t.width() * b) as u64 * t.height() as u64;
        }
        let kept = {
            let mut kept = self.downloads.lock().unwrap();
            kept.iter().position(|b| b.size() == size).map(|i| kept.swap_remove(i))
        };
        let buf = kept.unwrap_or_else(|| self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("download"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }));
        for ((t, b), off) in textures.iter().zip(bpp).zip(&offsets) {
            enc.copy_texture_to_buffer(t.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: *off, bytes_per_row: Some(aligned_row(t.width() * b)), rows_per_image: None },
            }, t.size());
        }
        let began = monotonic_ns();
        self.queue.submit([enc.finish()]);
        span("submit", began);
        let began = monotonic_ns();
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        span("gpu_wait", began);
        let waited = monotonic_ns().saturating_sub(began);
        let began = monotonic_ns();
        let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
        // Rows go straight into one buffer from the spares, so the readback is the only copy
        let total: usize = textures.iter().zip(bpp).map(|(t, b)| (t.width() * b) as usize * t.height() as usize).sum();
        let mut out = spare_bytes(total);
        let mut put = 0;
        for ((t, b), off) in textures.iter().zip(bpp).zip(&offsets) {
            let (row, stride) = ((t.width() * b) as usize, aligned_row(t.width() * b) as usize);
            for y in 0..t.height() as usize {
                let at = *off as usize + y * stride;
                out[put..put + row].copy_from_slice(&mapped[at..at + row]);
                put += row;
            }
        }
        drop(mapped);
        buf.unmap();
        let mut kept = self.downloads.lock().unwrap();
        // Only what a few sizes of picture need stays; the newest is at the end
        if kept.len() == KEPT_DOWNLOADS {
            kept.remove(0);
        }
        kept.push(buf);
        span("readback", began);
        Ok((out, waited))
    }

    /// Puts a frame on the card, ready for shaders.
    pub fn lift(&self, frame: &Frame) -> Result<Held, String> {
        self.lift_with(frame, false)
    }

    /// As lift, with block_chroma reading YUV chroma a sample to its 2 x 2 block as the CPU does, where lift blends it linearly.
    pub fn lift_with(&self, frame: &Frame, block_chroma: bool) -> Result<Held, String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return Err(format!("{:?} frames can't go on the card", frame.format));
        }
        let (w, h) = (frame.width, frame.height);
        let began = monotonic_ns();
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let (a, b) = (self.texture(w, h, WORK), self.texture(w, h, WORK));
        self.convert_pass_with(&mut enc, &planes.views, planes.mode, &a, 1, (planes.width, planes.height), block_chroma);
        span("upload", began);
        Ok(Held { enc, a, b, used, width: w, height: h, format: frame.format, number: frame.number, time_ns: frame.time_ns, waited_ns: 0 })
    }

    /// One shader step on the held picture. Nothing runs yet; land sends it all to the card in one go.
    pub fn shade(&self, held: &mut Held, slot: &ShaderSlot, time: f32, mask: Option<&MaskPlane>) {
        let info = self.uniform(&shader::frame_uniform(held.width, held.height, time, held.number, mask.is_some()));
        slot.draw(self, &mut held.enc, &held.a, &held.b, &info, mask);
        std::mem::swap(&mut held.a, &mut held.b);
    }

    /// The picture so far, read back in the frame's own format. The card keeps its copy, so more shaders can follow without an upload.
    pub fn land(&self, held: &mut Held) -> Result<Frame, String> {
        let mut enc = std::mem::replace(&mut held.enc, self.device.create_command_encoder(&Default::default()));
        let (w, h) = (held.width, held.height);
        let work = [held.a.create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
        let mut targets = Vec::new();
        let mut target = |width: u32, height: u32, format: wgpu::TextureFormat| {
            let t = self.texture(width, height, format);
            targets.push(t.clone());
            t
        };
        let (out, waited) = match held.format {
            PixelFormat::I420 => {
                let (y, u, v) = (target(w, h, wgpu::TextureFormat::R8Unorm), target(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                                 target(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToU, &u, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToV, &v, 1, (w, h));
                self.download_timed(enc, &[&y, &u, &v], &[1, 1, 1])?
            }
            PixelFormat::Nv12 => {
                let (y, uv) = (target(w, h, wgpu::TextureFormat::R8Unorm), target(chroma(w), chroma(h), wgpu::TextureFormat::Rg8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToUv, &uv, 1, (w, h));
                self.download_timed(enc, &[&y, &uv], &[1, 2])?
            }
            PixelFormat::Bgra => {
                let t = target(w, h, wgpu::TextureFormat::Bgra8Unorm);
                self.convert_pass(&mut enc, &work, Mode::Rgba, &t, 1, (w, h));
                self.download_timed(enc, &[&t], &[4])?
            }
            _ => self.download_timed(enc, &[&held.a], &[4])?,
        };
        held.waited_ns += waited;
        self.give_back(targets);
        let mut result = Frame::new(w, h, held.format, out);
        result.number = held.number;
        result.time_ns = held.time_ns;
        Ok(result)
    }

    /// Done with a held picture: its textures go back for the next frame.
    pub fn put_down(&self, held: Held) {
        let Held { a, b, mut used, .. } = held;
        used.extend([a, b]);
        self.give_back(used);
    }

    /// The person mask on the card in kept, uploaded only if it isn't the one already there. False for a mask of another size than w x h.
    pub fn keep_mask(&self, kept: &mut Option<MaskPlane>, mask: &Arc<Vec<u8>>, w: u32, h: u32) -> bool {
        if mask.len() != w as usize * h as usize {
            return false;
        }
        if let Some(k) = kept.as_ref().filter(|k| (k.texture.width(), k.texture.height()) == (w, h)) {
            if Arc::ptr_eq(&k.source, mask) || *k.source == **mask {
                return true;
            }
        }
        let texture = match kept.take() {
            Some(k) if (k.texture.width(), k.texture.height()) == (w, h) => k.texture,
            _ => self.texture(w, h, wgpu::TextureFormat::R8Unorm),
        };
        self.write_plane(&texture, mask, w);
        transfers::mask_up();
        let view = texture.create_view(&Default::default());
        *kept = Some(MaskPlane { source: mask.clone(), texture, view });
        true
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
        transfers::preview_up();
        let taps = (frame.width.div_ceil(out_w)).max(frame.height.div_ceil(out_h)).clamp(1, 8);
        self.convert_pass(&mut enc, &planes.views, planes.mode, target, taps, (planes.width, planes.height));
        self.queue.submit([enc.finish()]);
        self.give_back(used);
        Ok(())
    }

    /// How long the card would make a frame wait right now, in ns: the frame up and a pass over it and back, as big as a real one, and
    /// nothing kept. core::balance's way of looking at the card while frames stay off it.
    pub fn trial(&self, frame: &Frame) -> Result<u64, String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return Err(format!("{:?} frames can't go on the card", frame.format));
        }
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let target = self.texture(frame.width, frame.height, WORK);
        self.convert_pass(&mut enc, &planes.views, planes.mode, &target, 1, (planes.width, planes.height));
        let (_, waited) = self.download_timed(enc, &[&target], &[4])?;
        used.push(target);
        self.give_back(used);
        Ok(waited)
    }

    /// As if the card went away, for tests.
    #[cfg(test)]
    pub(crate) fn lose(&self) {
        self.device.destroy();
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
        transfers::preview_up();
        transfers::preview_down();
        let target = self.texture(out_w, out_h, WORK);
        // Enough samples per output pixel that shrinking a lot still averages instead of skipping pixels
        let taps = (frame.width.div_ceil(out_w)).max(frame.height.div_ceil(out_h)).clamp(1, 8);
        self.convert_pass(&mut enc, &planes.views, planes.mode, &target, taps, (planes.width, planes.height));
        let out = self.download(enc, &[&target], &[4])?;
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
    wants_mask: bool,
    built: Option<Result<Built, String>>,
}

/// A picture on the card with shaders recorded on it. land reads it back and it can go on from there.
pub struct Held {
    enc: wgpu::CommandEncoder,
    /// The picture so far, and where the next shader draws.
    a: wgpu::Texture,
    b: wgpu::Texture,
    used: Vec<wgpu::Texture>,
    width: u32,
    height: u32,
    format: PixelFormat,
    number: u64,
    time_ns: u64,
    /// What reading it back has waited for the card since this was last asked.
    waited_ns: u64,
}

impl Held {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn take_waited(&mut self) -> u64 {
        std::mem::take(&mut self.waited_ns)
    }
}

/// A stream's person mask on the card, staying there until the mask changes.
pub struct MaskPlane {
    source: Arc<Vec<u8>>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Built {
    pipeline: wgpu::RenderPipeline,
    /// Group 1: the plugin's uniforms and textures, or None when it has neither.
    extras: Option<wgpu::BindGroup>,
    /// The Params buffer, kept so new uniforms can be written into it.
    ubuf: Option<wgpu::Buffer>,
    _keep: Vec<wgpu::Texture>,
}

impl ShaderSlot {
    pub fn new(step: ShaderStep) -> ShaderSlot {
        ShaderSlot { wants_mask: shader::uses_person_mask(&step.wgsl), step, built: None }
    }

    /// Changes the shader's uniforms; once built, the new values go into its buffer. A shader with no Params ignores them.
    pub fn set_uniforms(&mut self, gpu: &Gpu, uniforms: serde_json::Value) {
        if uniforms == self.step.uniforms {
            return;
        }
        match shader::pack_uniforms(&self.step.wgsl, &uniforms) {
            Ok(bytes) => {
                if let Some(Ok(Built { ubuf: Some(buf), .. })) = &self.built {
                    gpu.queue.write_buffer(buf, 0, &bytes);
                }
                self.step.uniforms = uniforms;
            }
            Err(why) => log::warn!("A shader's uniforms were refused: {why}"),
        }
    }

    /// Whether the shader reads the person mask.
    pub fn wants_mask(&self) -> bool {
        self.wants_mask
    }

    /// Builds the shader the first time; Err for one that won't ever run.
    pub fn prepare(&mut self, gpu: &Gpu) -> Result<(), String> {
        if self.built.is_none() {
            self.built = Some(build(gpu, &self.step));
        }
        self.built.as_ref().unwrap().as_ref().map(|_| ()).map_err(Clone::clone)
    }

    fn draw(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, dst: &wgpu::Texture, info: &wgpu::Buffer, mask: Option<&MaskPlane>) {
        let Some(Ok(built)) = &self.built else { return };
        let (src_view, dst_view) = (src.create_view(&Default::default()), dst.create_view(&Default::default()));
        let frame_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: &gpu.frame_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.linear) },
                wgpu::BindGroupEntry { binding: 2, resource: info.as_entire_binding() },
                wgpu::BindGroupEntry { binding: shader::BINDING_MASK, resource: wgpu::BindingResource::TextureView(mask.map_or(&gpu.blank, |m| &m.view)) },
            ],
        });
        let mut binds = vec![&frame_bind];
        binds.extend(built.extras.as_ref());
        draw(enc, &built.pipeline, &binds, &dst_view);
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
    Ok(Built { pipeline, extras, ubuf, _keep: keep })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gl_is_only_asked_for_after_the_rest() {
        assert!(!BACKENDS[0].contains(wgpu::Backends::GL));
        assert!(BACKENDS[1].contains(wgpu::Backends::GL | wgpu::Backends::PRIMARY));
    }
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
        let mut held = gpu.lift(frame).unwrap();
        for slot in steps {
            slot.prepare(gpu).unwrap();
            gpu.shade(&mut held, slot, 0.0, None);
        }
        let out = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        out
    }

    const IDENTITY: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";

    #[test]
    fn the_colour_lut_on_the_cpu_gives_the_picture_the_shader_does() {
        use telescope_core::colour::{bake, lattice};
        use telescope_core::colour_cpu::Graded;
        use telescope_core::frame::{CpuStep, FrameContext};
        use telescope_core::host::ColourStage;
        struct Nothing;
        impl FrameContext for Nothing {
            fn source_id(&self) -> &str {
                ""
            }
            fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
                None
            }
            fn colour_preview(&self, frame: &Frame) -> Frame {
                frame.clone()
            }
        }
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut rgb = lattice(33);
        for px in rgb.as_chunks_mut::<3>().0 {
            *px = [px[0].sqrt(), 0.3 * px[1] + 0.7 * px[2], 1.0 - px[0] * px[2]];
        }
        let cube = bake(&[&ColourStage::new(33, Arc::new(rgb), None)], None).unwrap();
        let table = bake(&[&ColourStage::new(33, Arc::new(lattice(33).iter().map(|v| v * v).collect()),
                                             Some(Arc::new((0..256).flat_map(|i| [(i as f32 / 255.0).powi(2); 3]).collect())))], None).unwrap();
        let mut x = 5u32;
        let mut next = move || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        };
        for (w, h) in [(64u32, 36u32), (320, 180)] {
            let data: Vec<u8> = (0..PixelFormat::I420.size(w, h)).map(|_| next()).collect();
            let frame = Frame::new(w, h, PixelFormat::I420, data);
            for lut in [&cube, &table] {
                let mut slots = [ShaderSlot::new(lut.shader())];
                let card = run(&gpu, &frame, &mut slots);
                let mut cpu = frame.clone();
                Graded::new(lut).process(&mut cpu, &Nothing);
                let worst = card.data.iter().zip(cpu.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max();
                // A CI runner's software GL filters a 3D texture a touch coarser than any graphics card
                let limit = if gpu.adapter_name.contains("llvmpipe") { 3 } else { 2 };
                assert!(worst <= Some(limit), "{w}x{h} {} on {}: {worst:?}", lut.kind(), gpu.adapter_name);
            }
        }
    }

    #[test]
    fn a_trial_says_how_long_the_card_took() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        assert!(gpu.trial(&yuv_frame(64, 36, |_, _| 100, 128, 128)).unwrap() > 0);
    }

    #[test]
    fn a_handed_device_paints_the_preview_into_a_texture_it_keeps() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let window = Gpu::on_device(gpu.device.clone(), gpu.queue.clone());
        let frame = yuv_frame(64, 36, |x, _| (16 + x * 3) as u8, 128, 128);
        let target = window.picture(32, 18);
        window.paint(&frame, &target).unwrap();
        let enc = window.device.create_command_encoder(&Default::default());
        let painted = window.download(enc, &[&target], &[4]).unwrap();
        assert_eq!(painted, gpu.rgba(&frame, 32, 18).unwrap());
    }

    #[test]
    fn a_plugin_shader_can_use_derivatives_and_discard() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let wgsl = "fn effect(uv: vec2<f32>) -> vec4<f32> {
    if uv.x < 0.5 + 100.0 * abs(dpdx(uv.y)) && fwidth(uv.x) > 0.0 { discard; }
    return sample(uv);
}";
        let out = run(&gpu, &yuv_frame(16, 8, |_, _| 200, 128, 128), &mut [ShaderSlot::new(step(wgsl, json!({})))]);
        assert!(out.data[0] <= 18 && (out.data[15] as i32 - 200).abs() <= 3, "{} {}", out.data[0], out.data[15]);
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

    // test_colour.py::test_an_identity_lut_leaves_frames_alone: the lattice isn't rounded to whole levels on its way to the card
    #[test]
    fn an_identity_lut_on_the_card_leaves_frames_alone() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut x = 11u32;
        let mut next = || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        };
        let mut data = Vec::new();
        for _ in 0..128 * 72 {
            data.extend([next(), next(), next(), 255]);
        }
        let frame = Frame::new(128, 72, PixelFormat::Rgba, data);
        for size in telescope_core::colour::SIZES.map(|s| s as usize) {
            let lut = telescope_core::colour::Baked::Cube { size, rgb: telescope_core::colour::lattice(size) };
            let out = run(&gpu, &frame, &mut [ShaderSlot::new(lut.shader())]);
            assert_eq!(out.data, frame.data, "size {size}");
        }
    }

    // test_colour.py::test_opencv_applies_a_3d_lut_like_trilinear_interpolation_does: here the card applies it
    #[test]
    fn the_card_applies_a_3d_lut_like_trilinear_interpolation_does() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut x = 7u32;
        let mut next = || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        };
        let mut data = Vec::new();
        for _ in 0..128 * 72 {
            data.extend([next(), next(), next(), 255]);
        }
        let frame = Frame::new(128, 72, PixelFormat::Rgba, data);
        for size in telescope_core::colour::SIZES.map(|s| s as usize) {
            let mut cube = telescope_core::colour::lattice(size);
            for px in cube.as_chunks_mut::<3>().0 {
                let (r, g, b) = (px[0], px[1], px[2]);
                px.copy_from_slice(&[r.powf(0.8), g * 0.9 + b * 0.1, b.sqrt()]);
            }
            let lut = telescope_core::colour::Baked::Cube { size, rgb: cube.clone() };
            let out = run(&gpu, &frame, &mut [ShaderSlot::new(lut.shader())]);
            let point = |b: usize, g: usize, r: usize, c: usize| cube[(b * size * size + g * size + r) * 3 + c] as f64;
            let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
            let mut worst = 0;
            for (px, shown) in frame.data.chunks(4).zip(out.data.chunks(4)) {
                let cell = [px[0], px[1], px[2]].map(|v| {
                    let at = v as f64 / 255.0 * (size - 1) as f64;
                    let low = (at.floor() as usize).min(size - 2);
                    (low, at - low as f64)
                });
                let (r, g, b) = (cell[0], cell[1], cell[2]);
                for (c, got) in shown.iter().take(3).enumerate() {
                    let line = |bi: usize, gi: usize| lerp(point(bi, gi, r.0, c), point(bi, gi, r.0 + 1, c), r.1);
                    let plane = |bi: usize| lerp(line(bi, g.0), line(bi, g.0 + 1), g.1);
                    let want = (lerp(plane(b.0), plane(b.0 + 1), b.1) * 255.0 + 0.5) as i32;
                    worst = worst.max((*got as i32 - want).abs());
                }
            }
            assert!(worst <= 1, "off by {worst} at size {size}");
        }
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
    fn odd_sized_pictures_come_back_from_the_card_with_the_chroma_the_cpu_makes() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        for (w, h) in [(641usize, 361usize), (7, 5), (640, 361), (641, 360)] {
            // Hard edges in the colour, the last column and row included, so any shifted chroma cell shows
            let mut rgba = vec![255u8; w * h * 4];
            let mut n = 7u32;
            for (i, p) in rgba.chunks_exact_mut(4).enumerate() {
                let (x, y) = (i % w, i / w);
                n = n.wrapping_mul(1664525).wrapping_add(1013904223);
                let edge = if (x / 3 + y / 5) % 2 == 0 { 220 } else { 30 };
                let last = if x + 1 == w || y + 1 == h { 255 } else { 0 };
                p[0] = (edge as u32 + (n >> 29)).min(255) as u8;
                p[1] = (x * 255 / w) as u8;
                p[2] = (last as u32).max((y * 255 / h) as u32) as u8;
            }
            let frame = Frame::new(w as u32, h as u32, PixelFormat::Rgba, rgba);
            let rgb = telescope_core::image::frame_rgb(&frame).unwrap();
            for nv12 in [false, true] {
                let mut expected = vec![0u8; w * h + 2 * chroma(w as u32) as usize * chroma(h as u32) as usize];
                telescope_core::image::rgb_to_420(&rgb, w, h, &mut expected, nv12);
                let mut held = gpu.lift(&frame).unwrap();
                held.format = if nv12 { PixelFormat::Nv12 } else { PixelFormat::I420 };
                let out = gpu.land(&mut held).unwrap();
                gpu.put_down(held);
                assert_eq!(out.data.len(), expected.len());
                let worst = out.data.iter().zip(expected.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert!(worst <= 2, "{w}x{h} nv12 {nv12}: worst difference {worst}");
            }
        }
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

#[cfg(all(test, ffmpeg))]
mod timing {
    use super::*;
    use crate::ffmpeg::{Codec, Hw};
    use crate::testenc;
    use std::time::Instant;

    fn line(what: &str, mut ns: Vec<u64>) {
        ns.sort();
        let ms = |n: u64| n as f64 / 1e6;
        println!("  {what:<46} median {:6.2} ms, 95% {:6.2} ms", ms(ns[ns.len() / 2]), ms(ns[ns.len() * 95 / 100]));
    }

    /// What a picture costs each way, for deciding about importing decoded frames straight into Vulkan. Run with
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_a_crossing_costs
    #[test]
    #[ignore = "a measurement, needs a VA-API card"]
    fn what_a_crossing_costs() {
        let gpu = Gpu::shared().expect("a graphics card");
        println!("on {}", gpu.adapter_name);
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            println!("{w}x{h}");
            let values: Vec<u8> = (0..180).map(|i| 40 + (i % 150) as u8).collect();
            let packets = testenc::h264_noisy(&values, w, h, 30).expect("an H.264 encoder");
            let mut frames = Vec::new();
            for (i420, name) in [(true, "as I420"), (false, "as NV12, the card's own")] {
                let mut codec = Codec::h264(Some(Hw::Vaapi)).expect("VA-API");
                codec.card_gives_i420 = i420;
                codec.copy_ns = Some(Vec::new());
                let mut decoded = Vec::new();
                for p in &packets {
                    codec.parse(p, |c, unit| {
                        c.decode(unit, |f| decoded.push(f)).unwrap();
                    });
                }
                let times = codec.copy_ns.take().unwrap();
                let times = &times[30.min(times.len() / 2)..];
                line(&format!("download {name}, waiting on the decoder"), times.iter().map(|t| t.0).collect());
                line(&format!("download {name}, decoder already done"), times.iter().map(|t| t.1).collect());
                frames = decoded;
            }
            let (mut written, mut converted, mut landed) = (Vec::new(), Vec::new(), Vec::new());
            for f in &frames[30..] {
                let start = Instant::now();
                let mut used = Vec::new();
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                let planes = gpu.upload(f, &mut used);
                written.push(start.elapsed().as_nanos() as u64);
                let t = gpu.texture(w, h, WORK);
                gpu.convert_pass(&mut enc, &planes.views, planes.mode, &t, 1, (w, h));
                gpu.queue.submit([enc.finish()]);
                gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                converted.push(start.elapsed().as_nanos() as u64);
                used.push(t);
                gpu.give_back(used);

                let mut held = gpu.lift(f).unwrap();
                let slot = &mut ShaderSlot::new(ShaderStep { wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }".into(), uniforms: serde_json::json!({}), textures: vec![] });
                slot.prepare(&gpu).unwrap();
                gpu.shade(&mut held, slot, 0.0, None);
                gpu.queue.submit([]);
                let start = Instant::now();
                gpu.land(&mut held).unwrap();
                landed.push(start.elapsed().as_nanos() as u64);
                gpu.put_down(held);
            }
            line("upload I420, writing the planes", written);
            line("upload I420, on the card as RGBA and done", converted);
            line("back down as I420 after a shader", landed);
        }
    }
}

#[cfg(test)]
mod preview_bench {
    use super::*;
    use std::time::Instant;

    /// A picture with gradients, edges and noise, as I420 or NV12.
    pub(super) fn picture(w: u32, h: u32, nv12: bool) -> Frame {
        let (cw, ch) = (chroma(w) as usize, chroma(h) as usize);
        let mut noise = 2463534242u32;
        let mut data = Vec::with_capacity((w * h) as usize + 2 * cw * ch);
        for y in 0..h {
            for x in 0..w {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                let edge = if (x / 64 + y / 64) % 5 == 0 { 40 } else { 0 };
                let fine = if (x / 3 + y / 3) % 2 == 0 { 12 } else { 0 };
                data.push(16u8.saturating_add((x * 150 / w + y * 60 / h) as u8).saturating_add(edge).saturating_add(fine).saturating_add((noise & 7) as u8).min(235));
            }
        }
        let u = |i: usize| (90 + (i % cw) * 70 / cw + (i / cw) % 7) as u8;
        let v = |i: usize| (150 - (i % cw) * 60 / cw + (i / cw) % 5) as u8;
        if nv12 {
            for i in 0..cw * ch {
                data.push(u(i));
                data.push(v(i));
            }
            Frame::new(w, h, PixelFormat::Nv12, data)
        } else {
            data.extend((0..cw * ch).map(u));
            data.extend((0..cw * ch).map(v));
            Frame::new(w, h, PixelFormat::I420, data)
        }
    }

    /// What painting the preview costs the thread that asks for it (the UI's), before and after the card has finished.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_preview_paint_costs
    #[test]
    #[ignore = "a measurement, needs a graphics card"]
    fn what_the_preview_paint_costs() {
        let gpu = Gpu::shared().expect("a graphics card");
        println!("on {}", gpu.adapter_name);
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            for nv12 in [false, true] {
                // Several frames in turn, so each one comes in cold from memory as a decoded one does
                let frames: Vec<Frame> = (0..8).map(|_| picture(w, h, nv12)).collect();
                let target = gpu.picture(960, 540);
                let (mut asked, mut done) = (Vec::new(), Vec::new());
                for i in 0..80 {
                    let start = Instant::now();
                    gpu.paint(&frames[i % frames.len()], &target).unwrap();
                    let a = start.elapsed().as_nanos() as u64;
                    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    if i >= 10 {
                        asked.push(a);
                        done.push(start.elapsed().as_nanos() as u64);
                    }
                }
                let med = |mut v: Vec<u64>| { v.sort(); v[v.len() / 2] as f64 / 1e6 };
                println!("{w}x{h} {:<5} paint() returns after {:5.2} ms, card done after {:5.2} ms", if nv12 { "NV12" } else { "I420" }, med(asked), med(done));
            }
        }
    }

    /// What the fallback costs the UI thread when the card can't hand its texture to the window: the readback, and the processor's own scaler.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_preview_fallback_costs
    #[test]
    #[ignore = "a measurement, needs a graphics card"]
    fn what_the_preview_fallback_costs() {
        let gpu = Gpu::shared().expect("a graphics card");
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            let frames: Vec<Frame> = (0..8).map(|_| picture(w, h, false)).collect();
            let (mut card, mut cpu) = (Vec::new(), Vec::new());
            #[cfg(ffmpeg)]
            let mut scaler = crate::ffmpeg::CpuScaler::default();
            #[cfg(ffmpeg)]
            use telescope_core::media::Scaler;
            for i in 0..60 {
                let f = &frames[i % frames.len()];
                let start = Instant::now();
                let rgba = gpu.rgba(f, 960, 540).unwrap();
                let mut rgb = vec![0u8; 960 * 540 * 3];
                telescope_core::image::drop_alpha(&rgba, 960, &mut rgb);
                let a = start.elapsed().as_nanos() as u64;
                #[cfg(ffmpeg)]
                let b = {
                    let start = Instant::now();
                    let _ = scaler.rgb(f, 960, 540);
                    start.elapsed().as_nanos() as u64
                };
                #[cfg(not(ffmpeg))]
                let b = 0;
                if i >= 10 {
                    card.push(a);
                    cpu.push(b);
                }
            }
            let med = |mut v: Vec<u64>| { v.sort(); v[v.len() / 2] as f64 / 1e6 };
            println!("{w}x{h} I420 to 960x540 RGB: through the card {:5.2} ms, swscale on the processor {:5.2} ms", med(card), med(cpu));
        }
    }
}
