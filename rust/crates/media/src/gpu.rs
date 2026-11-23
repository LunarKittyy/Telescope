//! Frames on the graphics card through wgpu: plugins' shader steps (core::shader has what a shader may use) and the
//! preview's scaling and colour conversion. A frame stays up while shader steps follow each other; pipeline.rs decides when it comes down.
//! wgpu because it's one API over Vulkan, Direct3D 12 and OpenGL, runs WGSL (what Lua and WASM plugins hand over) and
//! is what Slint 1.18 can take textures from (unstable-wgpu-30), so the preview can later skip the download too.

use crate::pool::Pool;
use crate::transfers;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::gpu_marker::{self, Card};
use telescope_core::gpu_pick::{self, Api, Candidate, Kind};
use telescope_core::frame::{Frame, PixelFormat, ShaderStep};
use telescope_core::image::{spare_bytes, spare_room};
use telescope_core::shader;
use telescope_core::time::{monotonic_ns, span};
use wgpu::naga;

/// Set on the convert shader's mode to read YUV chroma by index, a sample for its 2 x 2 block, as the CPU does (taps stay 1).
const BLOCK_CHROMA: u32 = 8;
/// Set on the convert shader's mode so ToY/ToU/ToV read directly from NV12 planes instead of an RGBA texture.
#[cfg(all(any(target_os = "linux", windows), ffmpeg))]
const FROM_NV12: u32 = 64;
const CONVERT: &str = include_str!("shaders/convert.wgsl");
const BLUR: &str = include_str!("shaders/blur.wgsl");
/// What the blurred copies are worked in: more than 8 bits, so the weighted blur's sums don't band.
const BLUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
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

/// The most memory the kept textures take: enough for active 4K frame work, with the sizes least recently used dropped first.
const POOL_BUDGET: usize = 64 * 1024 * 1024;
const KEPT_DOWNLOADS: usize = 2;
/// The longest a readback waits for the card before the frame goes to the CPU instead.
const READBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// The same for a card that is the processor drawing in software (Windows' WARP, llvmpipe): a heavy shader on a big picture really takes that long there, and the wait only ends so a stream isn't stuck for good.
const SOFTWARE_READBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(300);
/// How long one wait on the card lasts before a stop is looked for.
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

#[path = "gpu_tint.rs"]
mod tint;
#[path = "gpu_fit.rs"]
mod fit;
#[cfg(test)]
pub(crate) use fit::cpu_fit;
pub use tint::{CardPreview, Scope, ScopeRead, ScopeSamples};

/// What a test makes go wrong on the next readback.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    /// The commands are refused, as when a pass is invalid: the copy into the download buffer never runs.
    RefusedSubmit,
    /// wgpu panics while waiting, as it can when a driver resets.
    PollPanic,
    /// The device-lost callback fires.
    Lost,
    /// wgpu panics while the picture goes up.
    LiftPanic,
    /// wgpu panics while a shader is recorded.
    ShadePanic,
    /// wgpu panics while a shader is built.
    PreparePanic,
    /// The next picture or mask going up is refused, as when the card can't allocate for it.
    BadUpload,
}

// wgpu compares devices by id alone, and two instances hand out the same ids, so each connection gets its own number
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

pub struct Gpu {
    serial: u64,
    device: wgpu::Device,
    #[cfg(all(any(target_os = "linux", windows), ffmpeg))]
    import_capabilities: Mutex<ImportCapabilities>,
    queue: wgpu::Queue,
    /// For the convert shader: three planes, a sampler and its settings.
    convert_layout: wgpu::BindGroupLayout,
    convert: Mutex<HashMap<wgpu::TextureFormat, Arc<wgpu::RenderPipeline>>>,
    convert_module: wgpu::ShaderModule,
    /// Download buffers to use again: mapping a new one each frame costs the card a flush.
    downloads: Mutex<Vec<wgpu::Buffer>>,
    /// For plugin shaders: the picture so far, the sampler, the frame's info, the person mask and the step's last frame.
    frame_layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    /// Stands in for planes a format doesn't have.
    blank: wgpu::TextureView,
    pool: Mutex<Pool<(u32, u32, wgpu::TextureFormat), wgpu::Texture>>,
    /// The tint shader's pipelines (gpu_tint.rs), built when a picture is first tinted.
    tint_kit: Mutex<Option<Arc<tint::Kit>>>,
    /// The camera fit's pipeline (gpu_fit.rs), built when a picture is first fitted.
    fit_kit: OnceLock<fit::Kit>,
    /// Set once fitting a picture on this card failed: the processor fits them from then on.
    fit_refused: AtomicBool,
    pub adapter_name: String,
    /// Set when the device is lost or wgpu panicked: nothing more goes to this card.
    lost: Arc<AtomicBool>,
    /// How long a readback waits for the card (READBACK_WAIT, or SOFTWARE_READBACK_WAIT for a software one).
    wait_limit: std::time::Duration,
    #[cfg(test)]
    fault: Mutex<Option<Fault>>,
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

/// A pass's picture: read by textureLoad only, so an exact float one binds too.
fn loaded_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
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
#[cfg(all(target_os = "linux", ffmpeg))]
type ImportCapabilities = crate::vaapi_import::Capabilities;
#[cfg(all(windows, ffmpeg))]
type ImportCapabilities = crate::d3d11_import::Capabilities;
#[cfg(not(all(any(target_os = "linux", windows), ffmpeg)))]
type ImportCapabilities = ();

/// The device features that let decoded pictures stay on the card: DMA-BUF import on Linux; NV12 textures and, on Vulkan, Win32 external memory on Windows.
/// Nothing is asked for when a part is missing, so a card that can't share keeps its device as it was.
pub fn surface_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let have = adapter.features();
    if cfg!(target_os = "linux") {
        have & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
    } else if cfg!(windows) {
        let wanted = if adapter.get_info().backend == wgpu::Backend::Vulkan { wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32 } else { wgpu::Features::TEXTURE_FORMAT_NV12 };
        if have.contains(wanted) { wanted } else { wgpu::Features::empty() }
    } else {
        wgpu::Features::empty()
    }
}

const BACKENDS: [wgpu::Backends; 2] = [wgpu::Backends::PRIMARY, wgpu::Backends::PRIMARY.union(wgpu::Backends::GL)];

/// The adapter as the crash marker and the skip list name it: the driver version is part of it (borrowed from the same card on another API
/// when its own driver says none), so an updated driver is tried again.
fn card_of(info: &wgpu::AdapterInfo, all: &[gpu_pick::Info]) -> Card {
    let own = gpu_pick::Info { vendor: info.vendor, device: info.device, driver: info.driver.clone(), driver_info: info.driver_info.clone() };
    Card::new(&info.name, &format!("{:?}", info.backend), &gpu_pick::driver_text(&own, all)).with_bus(&info.device_pci_bus_id)
}

fn infos_of(adapters: &[wgpu::Adapter]) -> Vec<gpu_pick::Info> {
    adapters.iter().map(|a| { let i = a.get_info(); gpu_pick::Info { vendor: i.vendor, device: i.device, driver: i.driver, driver_info: i.driver_info } }).collect()
}

/// The adapters of one instance in the order to try them. WGPU_POWER_PREF set: wgpu's own pick for it. Otherwise real GPUs first, DX12
/// before Vulkan on Windows and Vulkan first elsewhere (`gpu_pick::order`), and software or odd adapters last; wgpu's no-preference
/// request would take whatever is listed first, software adapters included.
fn adapters_to_try(instance: &wgpu::Instance, backends: wgpu::Backends, preference: Option<wgpu::PowerPreference>) -> Vec<(wgpu::Adapter, Card)> {
    let listed = gpu_marker::marked_while(&gpu_marker::probe_card(), || pollster::block_on(instance.enumerate_adapters(backends)));
    let all = infos_of(&listed);
    let candidates: Vec<Candidate> = listed.iter().enumerate().filter_map(|(index, a)| {
        let info = a.get_info();
        let api = match info.backend {
            wgpu::Backend::Vulkan => Api::Vulkan,
            wgpu::Backend::Dx12 => Api::Dx12,
            wgpu::Backend::Metal => Api::Metal,
            wgpu::Backend::Gl => Api::Gl,
            _ => return None,
        };
        let kind = match info.device_type {
            wgpu::DeviceType::IntegratedGpu => Kind::Integrated,
            wgpu::DeviceType::DiscreteGpu => Kind::Discrete,
            wgpu::DeviceType::VirtualGpu => Kind::Virtual,
            wgpu::DeviceType::Cpu => Kind::Cpu,
            wgpu::DeviceType::Other => Kind::Other,
        };
        Some(Candidate { api, kind, index, vendor: info.vendor, device: info.device, windows: cfg!(windows) })
    }).collect();
    let names: Vec<String> = listed.iter().map(|a| a.get_info().name).collect();
    gpu_pick::note_listed(&candidates, &names);
    let choice = gpu_pick::choice();
    let wanted = gpu_pick::chosen(&candidates, &choice);
    let mut by_choice = gpu_pick::with_choice(gpu_pick::order(&candidates, gpu_pick::system_preference()), &candidates, &choice);
    if !wanted.is_empty() {
        log::info!("Graphics card chosen in Settings: {}", names[wanted[0]]);
        if let Some(power_preference) = preference {
            log::info!("WGPU_POWER_PREF is ignored, the graphics card chosen in Settings goes first ({power_preference:?})");
        }
    } else if let Some(power_preference) = preference {
        // The environment's preference still goes through the skip list below, and wgpu's pick only moves to the front
        let picked = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference, ..Default::default() }));
        let same = |a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo| a.name == b.name && a.backend == b.backend && a.vendor == b.vendor && a.device == b.device;
        if let Some(index) = picked.ok().and_then(|p| listed.iter().position(|a| same(&a.get_info(), &p.get_info()))) {
            log::info!("Graphics card {} ({:?}) picked by WGPU_POWER_PREF", names[index], listed[index].get_info().backend);
            by_choice = gpu_pick::move_to_front(by_choice, &candidates, index);
        }
    }
    let mut listed: Vec<Option<wgpu::Adapter>> = listed.into_iter().map(Some).collect();
    let ordered: Vec<(wgpu::Adapter, Card)> = by_choice.into_iter().filter_map(|i| listed[i].take()).map(|a| { let card = card_of(&a.get_info(), &all); (a, card) }).collect();
    let skipped = gpu_marker::skipped();
    for (_, card) in ordered.iter().filter(|(_, card)| skipped.iter().any(|s| card.is_listed_as(s))) {
        log::warn!("Graphics card {card} is skipped, the last start closed while opening it");
    }
    let ordered = gpu_marker::without_skipped(ordered, |(_, card)| card.clone(), &skipped);
    let name = |(a, card): &(wgpu::Adapter, Card)| { let i = a.get_info(); format!("{} ({:?}, {:?}, driver {})", i.name, i.backend, i.device_type, card.driver) };
    if let Some(first) = ordered.first() {
        log::info!("Graphics card picked: {}; also listed: {}", name(first), ordered.iter().skip(1).map(name).collect::<Vec<_>>().join(", "));
    }
    ordered
}

static SHARED_GPU: Mutex<Option<Option<Arc<Gpu>>>> = Mutex::new(None);

impl Gpu {
    /// The app's one graphics card connection, opened on first use; None when there's no card wgpu can use or
    /// TELESCOPE_NO_GPU is set (for a driver that misbehaves, and for comparing). One that was lost is opened again by the same path, so a
    /// stream that starts afterwards gets a fresh device and caches; streams still running keep the lost one and stay on the CPU.
    pub fn shared() -> Option<Arc<Gpu>> {
        let mut kept = SHARED_GPU.lock().unwrap_or_else(|e| e.into_inner());
        if kept.as_ref().and_then(Option::as_ref).is_some_and(|gpu| gpu.is_lost()) {
            log::warn!("The graphics card was lost; opening it again for the streams that start now");
            *kept = None;
        }
        kept.get_or_insert_with(|| match std::env::var_os("TELESCOPE_NO_GPU").map_or_else(Gpu::open, |_| Err("TELESCOPE_NO_GPU is set".into())) {
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

    /// Use the window's connection for new pipelines; existing streams keep their own device.
    pub fn share_with_streams(gpu: Arc<Gpu>) {
        if std::env::var_os("TELESCOPE_NO_GPU").is_none() {
            *SHARED_GPU.lock().unwrap_or_else(|e| e.into_inner()) = Some(Some(gpu));
        }
    }

    pub(crate) fn open() -> Result<Gpu, String> {
        if gpu_marker::probe_failed() {
            return Err("the last start closed while looking for graphics cards".into());
        }
        let mut why = String::from("no graphics card found");
        for backends in BACKENDS {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = backends;
            let instance = gpu_marker::marked_while(&gpu_marker::probe_card(), || wgpu::Instance::new(desc.with_env()));
            for (adapter, card) in adapters_to_try(&instance, backends, wgpu::PowerPreference::from_env()) {
                match Gpu::open_on(&adapter, &card) {
                    Ok(gpu) => return Ok(gpu),
                    Err(e) => {
                        log::warn!("Graphics card {} ({:?}) can't be used: {e}", adapter.get_info().name, adapter.get_info().backend);
                        why = e;
                    }
                }
            }
        }
        // Nothing enumerated worked: the plain high performance request, as before 5.1.1
        let instance = gpu_marker::marked_while(&gpu_marker::probe_card(), || wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env()));
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).map_err(|e| format!("{why}; {e}"))?;
        let card = card_of(&adapter.get_info(), &infos_of(&pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY))));
        if gpu_marker::is_skipped(&card) {
            return Err(format!("{why}; the only card left is skipped"));
        }
        Gpu::open_on(&adapter, &card)
    }

    fn open_on(adapter: &wgpu::Adapter, card: &Card) -> Result<Gpu, String> {
        let info = adapter.get_info();
        let (device, queue) = gpu_marker::marked_while(card, || pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("telescope frames"),
            required_features: surface_features(adapter),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::Manual { suballocated_device_memory_block_size: (4 << 20)..(8 << 20) },
            ..Default::default()
        }))).map_err(|e| e.to_string())?;
        telescope_core::machine::set_gpu(info.vendor, &format!("{:?}", info.device_type), &info.name, &format!("{:?}", info.backend), &card.driver);
        if crate::decode_card::set_picked(info.vendor, info.device, &info.device_pci_bus_id) {
            #[cfg(ffmpeg)]
            crate::h264::refresh_hardware_probe();
        }
        let limit = if info.device_type == wgpu::DeviceType::Cpu { SOFTWARE_READBACK_WAIT } else { READBACK_WAIT };
        Ok(Gpu::on(device, queue, format!("{} ({:?})", info.name, info.backend), limit))
    }

    /// The same shaders on a device someone else opened (the window's, so the preview can hand its texture to Slint).
    pub fn on_device(device: wgpu::Device, queue: wgpu::Queue) -> Gpu {
        let info = device.adapter_info();
        if crate::decode_card::set_picked(info.vendor, info.device, &info.device_pci_bus_id) {
            #[cfg(ffmpeg)]
            crate::h264::refresh_hardware_probe();
        }
        let limit = if info.device_type == wgpu::DeviceType::Cpu { SOFTWARE_READBACK_WAIT } else { READBACK_WAIT };
        Gpu::on(device, queue, info.name, limit)
    }

    fn on(device: wgpu::Device, queue: wgpu::Queue, adapter_name: String, wait_limit: std::time::Duration) -> Gpu {
        // Without this a mistake in a plugin's shader that slips past the checks would end the app
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::error!("Graphics card error: {e}")));

        let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), sampler_entry(3), uniform_entry(4)],
        });
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2), tex_entry(shader::BINDING_MASK), tex_entry(shader::BINDING_HISTORY)],
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
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |reason, why| {
            if matches!(reason, wgpu::DeviceLostReason::Unknown) {
                log::error!("The graphics card was lost ({reason:?}): {why}");
            }
            flag.store(true, Ordering::SeqCst);
        });
        Gpu {
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            device,
            #[cfg(all(any(target_os = "linux", windows), ffmpeg))]
            import_capabilities: Mutex::default(),
            queue,
            convert_layout,
            convert: Mutex::default(),
            convert_module,
            downloads: Mutex::default(),
            frame_layout,
            linear,
            blank,
            pool: Mutex::new(Pool::new(POOL_BUDGET)),
            tint_kit: Mutex::default(),
            fit_kit: OnceLock::new(),
            fit_refused: AtomicBool::new(false),
            adapter_name,
            lost,
            wait_limit,
            #[cfg(test)]
            fault: Mutex::new(None),
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

    /// A texture of its own (not the pool's) that a step keeps its last frame in.
    fn history_texture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("history"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
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
        // wgpu only hands the freed memory back to the driver when it is polled
        let _ = self.device.poll(wgpu::PollType::Poll);
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
        // Too little data for the texture: wgpu refuses the write and says so as an error
        #[cfg(test)]
        let data = if self.take_fault(|f| matches!(f, Fault::BadUpload)) {
            let block = tex.format().block_copy_size(None).unwrap_or(1) as usize;
            let needed = tex.height().saturating_sub(1) as usize * bytes_per_row as usize + tex.width() as usize * block;
            &data[..data.len().min(needed.saturating_sub(1))]
        } else {
            data
        };
        self.queue.write_texture(
            tex.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: None },
            tex.size(),
        );
    }

    /// Runs work that sends pictures to the card with the card's errors caught: a refused allocation or write comes out as an error, not only a line in the log with the card's old contents left to be used.
    fn scoped<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        let scopes: Vec<_> = [wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal, wgpu::ErrorFilter::Validation].into_iter().map(|f| self.device.push_error_scope(f)).collect();
        let out = work();
        let errors: Vec<String> = scopes.into_iter().rev().filter_map(|scope| pollster::block_on(scope.pop())).map(|e| e.to_string()).collect();
        match errors.first() {
            Some(first) => Err(format!("the card refused a picture ({first})")),
            None => Ok(out),
        }
    }

    /// Puts a frame on the card as it is (textures to give back once used).
    fn upload(&self, frame: &Frame, used: &mut Vec<wgpu::Texture>) -> Planes {
        let (w, h) = (frame.width, frame.height);
        let (cw, ch) = (chroma(w), chroma(h));
        let ram;
        let frame = if frame.surface().is_some() { ram = frame.materialise(); &ram } else { frame };
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
        self.convert_pass_encoded(enc, planes, mode, target, taps, src_size, block_chroma, 0);
    }

    fn convert_pass_encoded(&self, enc: &mut wgpu::CommandEncoder, planes: &[wgpu::TextureView; 3], mode: Mode, target: &wgpu::Texture, taps: u32, src_size: (u32, u32), block_chroma: bool, colour: u32) {
        let mut settings: [u32; 12] = [mode as u32 | if block_chroma { BLOCK_CHROMA } else { 0 } | colour, taps.max(1), src_size.0, src_size.1, 0, 0, 0, 0, 0, 0, 0, 0];
        #[cfg(all(any(target_os = "linux", windows), ffmpeg))]
        if colour & FROM_NV12 != 0 {
            // The decoder's own encoding (16 BT.709, 32 full range), which the shader turns into BT.601 limited as the CPU would
            let enc = crate::yuv::Encoding { matrix: if colour & 16 != 0 { 1 } else { 5 }, full_range: colour & 32 != 0 };
            settings[4..].copy_from_slice(&crate::yuv::card_terms(enc).map(|t| t as u32));
        }
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

    /// Whether the device was lost or wgpu panicked on it: nothing more goes to it.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// Whether this card is the processor drawing in software (WARP, llvmpipe), where work on the card only costs CPU.
    pub(crate) fn is_software(&self) -> bool {
        self.wait_limit == SOFTWARE_READBACK_WAIT
    }

    /// Whether the frame is a decoder's picture this card can take in where it is, with no copy through memory.
    pub(crate) fn importable(frame: &Frame) -> bool {
        frame.surface().is_some_and(|source| decoder_surface(source.as_any()))
    }

    /// Whether this card can still be used.
    fn card_ok(&self) -> Result<(), String> {
        if self.lost.load(Ordering::SeqCst) { Err("the graphics card was lost".into()) } else { Ok(()) }
    }

    /// Runs work that waits on the card so a panic inside wgpu (a driver reset can cause one while polling) comes out as an error and the card counts as lost.
    fn guarded<T>(&self, what: &str, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.card_ok()?;
        catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
            self.lost.store(true, Ordering::SeqCst);
            let said = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_default();
            log::error!("wgpu panicked while {what}: {said}");
            Err(format!("the graphics card failed while {what} ({said})"))
        })
    }

    /// Makes the next readback go wrong, for tests.
    #[cfg(test)]
    pub(crate) fn inject(&self, fault: Fault) {
        *self.fault.lock().unwrap() = Some(fault);
    }

    #[cfg(test)]
    fn fault(&self) -> Option<Fault> {
        let mut fault = self.fault.lock().unwrap();
        // The panics above belong to their own calls, so a readback leaves them for those
        if matches!(*fault, Some(Fault::LiftPanic | Fault::ShadePanic | Fault::PreparePanic | Fault::BadUpload)) { None } else { fault.take() }
    }

    /// Takes the fault if it is the one asked for, for tests.
    #[cfg(test)]
    fn take_fault(&self, wanted: fn(Fault) -> bool) -> bool {
        let mut fault = self.fault.lock().unwrap();
        let hit = fault.is_some_and(wanted);
        if hit {
            *fault = None;
        }
        hit
    }

    /// Copies textures into one buffer, waits for the card and hands back each one's rows without padding, the planes one after another in one buffer, and how long the wait was in ns.
    fn download_timed(&self, enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32]) -> Result<(Vec<u8>, u64), String> {
        self.download_until(enc, textures, bpp, &|| false)
    }

    /// As download_timed, without reusing or keeping the staging buffer afterwards (for one-off latency trials).
    fn download_unkept(&self, enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32], cancel: &dyn Fn() -> bool) -> Result<(Vec<u8>, u64), String> {
        self.guarded("reading a picture back", || self.read_back(enc, textures, bpp, cancel, false))
    }

    /// As download_timed, giving up with an error as soon as cancel says so (a stop doesn't wait out a card that has stalled).
    fn download_until(&self, enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32], cancel: &dyn Fn() -> bool) -> Result<(Vec<u8>, u64), String> {
        self.guarded("reading a picture back", || self.read_back(enc, textures, bpp, cancel, true))
    }

    fn read_back(&self, mut enc: wgpu::CommandEncoder, textures: &[&wgpu::Texture], bpp: &[u32], cancel: &dyn Fn() -> bool, keep_buf: bool) -> Result<(Vec<u8>, u64), String> {
        let mut offsets = Vec::new();
        let mut size = 0u64;
        for (t, b) in textures.iter().zip(bpp) {
            offsets.push(size);
            size += aligned_row(t.width() * b) as u64 * t.height() as u64;
        }
        let kept = keep_buf.then(|| {
            let mut kept = self.downloads.lock().unwrap();
            kept.iter().position(|b| b.size() == size).map(|i| kept.swap_remove(i))
        }).flatten();
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
        // A refused submission leaves a reused buffer holding another picture, so every error from here on is caught and the buffer isn't kept
        let scopes: Vec<_> = [wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal, wgpu::ErrorFilter::Validation].into_iter().map(|f| self.device.push_error_scope(f)).collect();
        #[cfg(test)]
        let fault = self.fault();
        #[cfg(test)]
        if matches!(fault, Some(Fault::RefusedSubmit)) {
            enc.copy_buffer_to_buffer(&buf, 0, &buf, 0, Some(size));
        }
        let began = monotonic_ns();
        let index = self.queue.submit([enc.finish()]);
        span("submit", began);
        let began = monotonic_ns();
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        #[cfg(test)]
        if matches!(fault, Some(Fault::PollPanic)) {
            panic!("an injected panic while polling");
        }
        let polled = self.wait_for(index, cancel);
        #[cfg(test)]
        if matches!(fault, Some(Fault::Lost)) {
            self.lost.store(true, Ordering::SeqCst);
        }
        let errors: Vec<String> = scopes.into_iter().rev().filter_map(|scope| pollster::block_on(scope.pop())).map(|e| e.to_string()).collect();
        if let Some(first) = errors.first() {
            return Err(format!("the card refused the readback ({first})"));
        }
        polled?;
        self.card_ok()?;
        // wgpu 30 on DX12 counts a removed device's fence as done without saying the device was lost, which leaves the buffer as it was
        if let Some(why) = device_removed(&self.device) {
            self.lost.store(true, Ordering::SeqCst);
            log::error!("The graphics card was removed ({why})");
            return Err(format!("the graphics card was removed ({why})"));
        }
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        span("gpu_wait", began);
        let waited = monotonic_ns().saturating_sub(began);
        let began = monotonic_ns();
        let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
        // Rows go straight into one buffer from the spares, so the readback is the only copy
        let total: usize = textures.iter().zip(bpp).map(|(t, b)| (t.width() * b) as usize * t.height() as usize).sum();
        let mut out = spare_room(total);
        for ((t, b), off) in textures.iter().zip(bpp).zip(&offsets) {
            let (row, stride) = ((t.width() * b) as usize, aligned_row(t.width() * b) as usize);
            for y in 0..t.height() as usize {
                let at = *off as usize + y * stride;
                out.extend_from_slice(&mapped[at..at + row]);
            }
        }
        drop(mapped);
        buf.unmap();
        if keep_buf {
            let mut kept = self.downloads.lock().unwrap();
            // Only what a few sizes of picture need stays; the newest is at the end
            if kept.len() == KEPT_DOWNLOADS {
                kept.remove(0);
            }
            kept.push(buf);
        } else {
            drop(buf);
        }
        span("readback", began);
        Ok((out, waited))
    }

    /// Waits for the card to finish the submission, in short slices so cancel is looked at between them, for at most the card's wait limit.
    fn wait_for(&self, index: wgpu::SubmissionIndex, cancel: &dyn Fn() -> bool) -> Result<(), String> {
        let until = std::time::Instant::now() + self.wait_limit;
        loop {
            if cancel() {
                return Err("stopped while waiting for the graphics card".into());
            }
            let slice = until.saturating_duration_since(std::time::Instant::now()).min(WAIT_SLICE);
            match self.device.poll(wgpu::PollType::Wait { submission_index: Some(index.clone()), timeout: Some(slice) }) {
                Ok(_) => return Ok(()),
                Err(wgpu::PollError::Timeout) if !until.saturating_duration_since(std::time::Instant::now()).is_zero() => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Puts a frame on the card, ready for shaders.
    pub fn lift(&self, frame: &Frame) -> Result<Held, String> {
        self.lift_with(frame, false)
    }

    /// As lift, with block_chroma reading YUV chroma a sample to its 2 x 2 block as the CPU does, where lift blends it linearly.
    pub fn lift_with(&self, frame: &Frame, block_chroma: bool) -> Result<Held, String> {
        #[cfg(test)]
        let _timing = transfers::measurement::time(transfers::measurement::Stage::Lift);
        self.guarded("putting a picture on the card", || self.lift_unguarded(frame, block_chroma))
    }

    fn lift_unguarded(&self, frame: &Frame, block_chroma: bool) -> Result<Held, String> {
        #[cfg(test)]
        if self.take_fault(|f| matches!(f, Fault::LiftPanic)) {
            panic!("an injected panic while lifting");
        }
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.card_ok()?;
        #[cfg(all(target_os = "linux", ffmpeg))]
        if let Some(source) = frame.surface() {
            if let Some(surface) = source.as_any().downcast_ref::<crate::vaapi_import::Surface>() {
                match self.lift_surface(frame, source.clone(), surface, block_chroma) {
                    Ok(held) => return Ok(held),
                    Err(why) => surface.disable(&why),
                }
            }
            return self.lift_unguarded(&frame.materialise(), block_chroma);
        }
        #[cfg(all(windows, ffmpeg))]
        if let Some(source) = frame.surface() {
            if let Some(surface) = source.as_any().downcast_ref::<crate::d3d11_import::Surface>() {
                match self.lift_d3d11(frame, source.clone(), surface, block_chroma) {
                    Ok(Some(held)) => return Ok(held),
                    // Every shared texture was busy: this picture goes through memory, the stream keeps sharing
                    Ok(None) => {}
                    Err(why) => surface.disable(&why),
                }
            }
            return self.lift_unguarded(&frame.materialise(), block_chroma);
        }
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return Err(format!("{:?} frames can't go on the card", frame.format));
        }
        let (w, h) = (frame.width, frame.height);
        let began = monotonic_ns();
        let (enc, used, kept_planes, a) = self.scoped(|| {
            let mut used = Vec::new();
            let mut enc = self.device.create_command_encoder(&Default::default());
            let planes = self.upload(frame, &mut used);
            // I420 stays as its three planes until a shader or readback asks for RGBA, avoiding a 33 MB 4K texture for plane steps
            if frame.format == PixelFormat::I420 {
                let kept_planes = {
                    let mut iter = used.drain(..3);
                    [iter.next().unwrap(), iter.next().unwrap(), iter.next().unwrap()]
                };
                (enc, used, Some(kept_planes), None)
            } else {
                let a = self.texture(w, h, WORK);
                self.convert_pass_with(&mut enc, &planes.views, planes.mode, &a, 1, (planes.width, planes.height), block_chroma);
                (enc, used, None, Some(a))
            }
        })?;
        span("upload", began);
        Ok(Held { enc, a, b: None, used, width: w, height: h, format: frame.format, number: frame.number, time_ns: frame.time_ns, capture_ns: frame.capture_ns, waited_ns: 0, planes: kept_planes, block_chroma, imported: false })
    }

    #[cfg(all(target_os = "linux", ffmpeg))]
    fn lift_surface(&self, frame: &Frame, source: Arc<dyn telescope_core::frame::DecodedSurface>, surface: &crate::vaapi_import::Surface, block_chroma: bool) -> Result<Held, String> {
        let planes = surface.import(&self.device, &self.import_capabilities)?;
        let (w, h) = (frame.width, frame.height);
        let (a, kept_planes) = self.scoped(|| -> Result<_, String> {
            // wgpu refuses one encoder that mixes its own commands with raw ones, so the two barriers get encoders of their own
            let mut acquire = self.device.create_command_encoder(&Default::default());
            crate::vaapi_import::Surface::ownership(&self.device, &mut acquire, &planes, true)?;
            let mut enc = self.device.create_command_encoder(&Default::default());
            let views = [planes[0].create_view(&Default::default()), planes[1].create_view(&Default::default()), self.blank.clone()];
            let out = if frame.format == PixelFormat::I420 {
                let i420 = [
                    self.texture(w, h, wgpu::TextureFormat::R8Unorm),
                    self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                    self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                ];
                let flags = FROM_NV12 | surface.colour();
                for (plane, mode) in i420.iter().zip([Mode::ToY, Mode::ToU, Mode::ToV]) {
                    self.convert_pass_encoded(&mut enc, &views, mode, plane, 1, (w, h), false, flags);
                }
                (None, Some(i420))
            } else {
                let a = self.texture(w, h, WORK);
                self.convert_pass_encoded(&mut enc, &views, Mode::Nv12, &a, 1, (w, h), block_chroma, surface.colour());
                (Some(a), None)
            };
            let mut release = self.device.create_command_encoder(&Default::default());
            crate::vaapi_import::Surface::ownership(&self.device, &mut release, &planes, false)?;
            self.queue.submit([acquire.finish(), enc.finish(), release.finish()]);
            // Keep both the decoder's AVFrame and its imported images until the conversion submission has finished.
            self.queue.on_submitted_work_done(move || drop((source, planes)));
            Ok(out)
        })??;
        Ok(Held { enc: self.device.create_command_encoder(&Default::default()), a, b: None, used: Vec::new(), width: w, height: h, format: frame.format, number: frame.number, time_ns: frame.time_ns, capture_ns: frame.capture_ns, waited_ns: 0, planes: kept_planes, block_chroma, imported: true })
    }

    /// Ensures held.a holds the current picture as RGBA, converting from held.planes on demand.
    fn ensure_rgba(&self, held: &mut Held) -> wgpu::Texture {
        if let Some(a) = &held.a {
            return a.clone();
        }
        let planes = held.planes.as_ref().expect("held picture has neither RGBA nor I420 planes");
        let views = planes.each_ref().map(|t| t.create_view(&Default::default()));
        let a = self.texture(held.width, held.height, WORK);
        self.convert_pass_with(&mut held.enc, &views, Mode::I420, &a, 1, (held.width, held.height), held.block_chroma);
        held.a = Some(a.clone());
        a
    }

    #[cfg(all(windows, ffmpeg))]
    fn lift_d3d11(&self, frame: &Frame, source: Arc<dyn telescope_core::frame::DecodedSurface>, surface: &crate::d3d11_import::Surface, block_chroma: bool) -> Result<Option<Held>, String> {
        let Some(mut imported) = surface.import(&self.device, &self.import_capabilities)? else { return Ok(None) };
        let (w, h) = (frame.width, frame.height);
        let vulkan = imported.backend == wgpu::Backend::Vulkan;
        let (a, kept_planes) = self.scoped(|| -> Result<_, String> {
            // wgpu refuses one encoder that mixes its own commands with raw ones, so the two barriers get encoders of their own
            let mut acquire = self.device.create_command_encoder(&Default::default());
            if vulkan {
                crate::d3d11_import::Surface::ownership(&self.device, &mut acquire, &imported.texture, true)?;
            }
            let mut enc = self.device.create_command_encoder(&Default::default());
            let plane = |aspect, format| imported.texture.create_view(&wgpu::TextureViewDescriptor { aspect, format: Some(format), ..Default::default() });
            let views = [plane(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm), plane(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm), self.blank.clone()];
            let out = if frame.format == PixelFormat::I420 {
                let i420 = [
                    self.texture(w, h, wgpu::TextureFormat::R8Unorm),
                    self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                    self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                ];
                let flags = FROM_NV12 | surface.colour();
                for (plane, mode) in i420.iter().zip([Mode::ToY, Mode::ToU, Mode::ToV]) {
                    self.convert_pass_encoded(&mut enc, &views, mode, plane, 1, (w, h), false, flags);
                }
                (None, Some(i420))
            } else {
                let a = self.texture(w, h, WORK);
                self.convert_pass_encoded(&mut enc, &views, Mode::Nv12, &a, 1, (w, h), block_chroma, surface.colour());
                (Some(a), None)
            };
            let mut release = self.device.create_command_encoder(&Default::default());
            if vulkan {
                crate::d3d11_import::Surface::ownership(&self.device, &mut release, &imported.texture, false)?;
            }
            // On DX12 the queue waits for the copy into the shared texture on the card
            imported.wait_on(&self.queue)?;
            self.queue.submit([acquire.finish(), enc.finish(), release.finish()]);
            // Keep the decoder's AVFrame, the shared texture's slot and the imported image until the conversion submission has finished.
            self.queue.on_submitted_work_done(move || drop((source, imported)));
            Ok(out)
        })??;
        Ok(Some(Held { enc: self.device.create_command_encoder(&Default::default()), a, b: None, used: Vec::new(), width: w, height: h, format: frame.format, number: frame.number, time_ns: frame.time_ns, capture_ns: frame.capture_ns, waited_ns: 0, planes: kept_planes, block_chroma, imported: true }))
    }

    /// One shader step on the held picture. Nothing runs yet; land sends it all to the card in one go.
    pub fn shade(&self, held: &mut Held, slot: &ShaderSlot, time: f32, mask: Option<&MaskPlane>) -> Result<(), String> {
        self.shade_with(held, slot, time, mask, true)
    }

    /// As shade, with history false the shader isn't given the step's last frame (previous() has nothing), as for a picture that is not the one that frame belongs to.
    pub fn shade_with(&self, held: &mut Held, slot: &ShaderSlot, time: f32, mask: Option<&MaskPlane>, history: bool) -> Result<(), String> {
        self.guarded("recording a shader", || {
            self.shade_unguarded(held, slot, time, mask, history);
            Ok(())
        })
    }

    fn shade_unguarded(&self, held: &mut Held, slot: &ShaderSlot, time: f32, mask: Option<&MaskPlane>, history: bool) {
        #[cfg(test)]
        if self.take_fault(|f| matches!(f, Fault::ShadePanic)) {
            panic!("an injected panic while shading");
        }
        let size = (held.width, held.height);
        if slot.wants_planes(held.width, held.height) {
            if let Some(input_planes) = self.take_planes(held) {
                let blurred = slot.blur(self, &mut held.enc, size, None, Some(&input_planes), &mut held.used);
                let room = blurred.as_ref().map(|(_, room)| room.clone()).or_else(|| slot.room_picture());
                if let Some(room) = &room {
                    if let Some(composed) = slot.compose(self, &mut held.enc, &input_planes, room) {
                        // When compose produced the I420 planes, slot.draw is dead work unless keep() reads the drawn RGBA for history.
                        if slot.wants_history {
                            let a = match &held.a {
                                Some(a) => a.clone(),
                                None => {
                                    let views = input_planes.each_ref().map(|t| t.create_view(&Default::default()));
                                    let a = self.texture(held.width, held.height, WORK);
                                    self.convert_pass_with(&mut held.enc, &views, Mode::I420, &a, 1, size, held.block_chroma);
                                    held.a = Some(a.clone());
                                    a
                                }
                            };
                            let b = held.b.take().unwrap_or_else(|| self.texture(held.width, held.height, WORK));
                            let past = slot.past(held).filter(|_| history);
                            let info = self.uniform(&shader::frame_uniform(held.width, held.height, time, held.number, mask.is_some(), past.is_some()));
                            let passed = slot.passes(self, &mut held.enc, &a, &info, mask, past.as_ref(), blurred.as_ref(), &mut held.used);
                            slot.draw(self, &mut held.enc, &a, &b, &info, mask, past.as_ref(), blurred.as_ref(), &passed);
                            slot.keep(self, &mut held.enc, &b, held.time_ns);
                            held.used.push(b);
                        }
                        if let Some(a) = held.a.take() {
                            held.used.push(a);
                        }
                        held.used.extend(input_planes);
                        held.planes = Some(composed);
                        return;
                    }
                }
                held.planes = Some(input_planes);
            }
        }
        let a = self.ensure_rgba(held);
        if let Some(old_planes) = held.planes.take() {
            held.used.extend(old_planes);
        }
        let blurred = slot.blur(self, &mut held.enc, size, Some(&a), None, &mut held.used);
        let b = held.b.take().unwrap_or_else(|| self.texture(held.width, held.height, WORK));
        let past = slot.past(held).filter(|_| history);
        let info = self.uniform(&shader::frame_uniform(held.width, held.height, time, held.number, mask.is_some(), past.is_some()));
        let passed = slot.passes(self, &mut held.enc, &a, &info, mask, past.as_ref(), blurred.as_ref(), &mut held.used);
        slot.draw(self, &mut held.enc, &a, &b, &info, mask, past.as_ref(), blurred.as_ref(), &passed);
        slot.keep(self, &mut held.enc, &b, held.time_ns);
        held.a = Some(b);
        held.b = Some(a);
    }

    /// Takes an I420 picture's planes as the picture is now: the ones it already holds, or else what landing would make of its colours. None for other formats.
    fn take_planes(&self, held: &mut Held) -> Option<[wgpu::Texture; 3]> {
        if held.format != PixelFormat::I420 {
            return None;
        }
        if let Some(planes) = held.planes.take() {
            return Some(planes);
        }
        let (w, h) = (held.width, held.height);
        let a = held.a.as_ref()?;
        let work = [a.create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
        let planes = [self.texture(w, h, wgpu::TextureFormat::R8Unorm), self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm), self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm)];
        for (plane, mode) in planes.iter().zip([Mode::ToY, Mode::ToU, Mode::ToV]) {
            self.convert_pass(&mut held.enc, &work, mode, plane, 1, (w, h));
        }
        Some(planes)
    }

    /// The picture so far, read back in the frame's own format. The card keeps its copy, so more shaders can follow without an upload.
    pub fn land(&self, held: &mut Held) -> Result<Frame, String> {
        self.land_until(held, &|| false)
    }

    /// As land, giving up with an error as soon as cancel says so while the card is waited for.
    pub fn land_until(&self, held: &mut Held, cancel: &dyn Fn() -> bool) -> Result<Frame, String> {
        #[cfg(test)]
        let _timing = transfers::measurement::time(transfers::measurement::Stage::Land);
        self.card_ok()?;
        let (w, h) = (held.width, held.height);
        let rgba = if held.format == PixelFormat::I420 && held.planes.is_some() {
            None
        } else {
            Some(self.ensure_rgba(held))
        };
        let mut enc = std::mem::replace(&mut held.enc, self.device.create_command_encoder(&Default::default()));
        let mut targets = Vec::new();
        let mut target = |width: u32, height: u32, format: wgpu::TextureFormat| {
            let t = self.texture(width, height, format);
            targets.push(t.clone());
            t
        };
        let (out, waited) = match held.format {
            PixelFormat::I420 if held.planes.is_some() => {
                let [y, u, v] = held.planes.clone().unwrap();
                self.download_until(enc, &[&y, &u, &v], &[1, 1, 1], cancel)?
            }
            PixelFormat::I420 => {
                let work = [rgba.as_ref().unwrap().create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
                let (y, u, v) = (target(w, h, wgpu::TextureFormat::R8Unorm), target(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                                 target(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToU, &u, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToV, &v, 1, (w, h));
                self.download_until(enc, &[&y, &u, &v], &[1, 1, 1], cancel)?
            }
            PixelFormat::Nv12 => {
                let work = [rgba.as_ref().unwrap().create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
                let (y, uv) = (target(w, h, wgpu::TextureFormat::R8Unorm), target(chroma(w), chroma(h), wgpu::TextureFormat::Rg8Unorm));
                self.convert_pass(&mut enc, &work, Mode::ToY, &y, 1, (w, h));
                self.convert_pass(&mut enc, &work, Mode::ToUv, &uv, 1, (w, h));
                self.download_until(enc, &[&y, &uv], &[1, 2], cancel)?
            }
            PixelFormat::Bgra => {
                let work = [rgba.as_ref().unwrap().create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
                let t = target(w, h, wgpu::TextureFormat::Bgra8Unorm);
                self.convert_pass(&mut enc, &work, Mode::Rgba, &t, 1, (w, h));
                self.download_until(enc, &[&t], &[4], cancel)?
            }
            _ => self.download_until(enc, &[rgba.as_ref().unwrap()], &[4], cancel)?,
        };
        held.waited_ns += waited;
        self.give_back(targets);
        let mut result = Frame::new(w, h, held.format, out);
        result.number = held.number;
        result.time_ns = held.time_ns;
        result.capture_ns = held.capture_ns;
        Ok(result)
    }

    /// Submit the recorded steps without copying any pixels to RAM, after the pipeline's own last submission has finished.
    pub(crate) fn submit_held(&self, held: &mut Held, pacing: &mut Pacing, cancel: &dyn Fn() -> bool) -> Result<(), String> {
        self.guarded("submitting a card picture", || {
            // Wait for this pipeline's previous picture before queuing more work; other streams on the card have their own
            if let Some(index) = pacing.0.take().filter(|(serial, _)| *serial == self.serial).map(|(_, index)| index) {
                let began = monotonic_ns();
                self.wait_for(index, cancel)?;
                held.waited_ns += monotonic_ns().saturating_sub(began);
            }
            let index = self.scoped(|| {
                let enc = std::mem::replace(&mut held.enc, self.device.create_command_encoder(&Default::default()));
                self.queue.submit([enc.finish()])
            })?;
            pacing.0 = Some((self.serial, index));
            Ok(())
        })
    }

    fn paint_held(&self, picture: &Shown, (width, height): (u32, u32), target: &wgpu::Texture) -> Result<(), String> {
        self.guarded("painting a card picture", || self.scoped(|| {
            let mut enc = self.device.create_command_encoder(&Default::default());
            let taps = width.div_ceil(target.width()).max(height.div_ceil(target.height())).clamp(1, 8);
            match picture {
                Shown::Rgba(tex) => {
                    let views = [tex.create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
                    self.convert_pass(&mut enc, &views, Mode::Rgba, target, taps, (width, height));
                }
                Shown::Planes(planes, block_chroma) => {
                    let views = planes.each_ref().map(|t| t.create_view(&Default::default()));
                    self.convert_pass_with(&mut enc, &views, Mode::I420, target, taps, (width, height), *block_chroma);
                }
            }
            self.queue.submit([enc.finish()]);
        }))
    }

    /// Done with a held picture: its textures go back for the next frame.
    pub fn put_down(&self, held: Held) {
        let Held { a, b, planes, mut used, .. } = held;
        used.extend(a);
        used.extend(b);
        if let Some(planes) = planes {
            used.extend(planes);
        }
        self.give_back(used);
    }

    /// The person mask on the card in kept, uploaded only if it isn't the one already there. False for a mask of another size than w x h.
    pub fn keep_mask(&self, kept: &mut Option<MaskPlane>, mask: &Arc<Vec<u8>>, w: u32, h: u32) -> Result<bool, String> {
        if mask.len() != w as usize * h as usize {
            return Ok(false);
        }
        if let Some(k) = kept.as_ref().filter(|k| (k.texture.width(), k.texture.height()) == (w, h)) {
            if Arc::ptr_eq(&k.source, mask) || *k.source == **mask {
                return Ok(true);
            }
        }
        // Taken before the write, so a write that fails leaves no old mask behind to be drawn with
        let old = kept.take();
        let (texture, view) = self.scoped(|| {
            let texture = match old {
                Some(k) if (k.texture.width(), k.texture.height()) == (w, h) => k.texture,
                _ => self.texture(w, h, wgpu::TextureFormat::R8Unorm),
            };
            self.write_plane(&texture, mask, w);
            let view = texture.create_view(&Default::default());
            (texture, view)
        })?;
        transfers::mask_up();
        *kept = Some(MaskPlane { source: mask.clone(), texture, view });
        Ok(true)
    }

    /// The frame scaled and converted into target (RGBA, out_w x out_h), left on the card for whoever draws it.
    pub fn paint(&self, frame: &Frame, target: &wgpu::Texture) -> Result<(), String> {
        self.card_ok()?;
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
    /// nothing kept, giving up with an error as soon as cancel says so. core::balance's way of looking at the card while frames stay off it.
    pub fn trial(&self, frame: &Frame, cancel: &dyn Fn() -> bool) -> Result<u64, String> {
        self.card_ok()?;
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return Err(format!("{:?} frames can't go on the card", frame.format));
        }
        let mut used = Vec::new();
        let mut enc = self.device.create_command_encoder(&Default::default());
        let planes = self.upload(frame, &mut used);
        let target = self.texture(frame.width, frame.height, WORK);
        self.convert_pass(&mut enc, &planes.views, planes.mode, &target, 1, (planes.width, planes.height));
        let (_, waited) = self.download_unkept(enc, &[&target], &[4], cancel)?;
        drop(target);
        drop(used);
        let _ = self.device.poll(wgpu::PollType::Poll);
        Ok(waited)
    }

    /// As if this were the processor drawing in software, for tests (only the Linux VA-API ones use it).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn pretend_software(mut self) -> Self {
        self.wait_limit = SOFTWARE_READBACK_WAIT;
        self
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
        self.card_ok()?;
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

/// Whether a decoder's picture is one this build imports onto the card (VA-API on Linux, D3D11VA on Windows).
#[cfg(all(target_os = "linux", ffmpeg))]
fn decoder_surface(source: &dyn std::any::Any) -> bool {
    source.is::<crate::vaapi_import::Surface>()
}

#[cfg(all(windows, ffmpeg))]
fn decoder_surface(source: &dyn std::any::Any) -> bool {
    source.is::<crate::d3d11_import::Surface>()
}

#[cfg(not(any(all(target_os = "linux", ffmpeg), all(windows, ffmpeg))))]
fn decoder_surface(_source: &dyn std::any::Any) -> bool {
    false
}

/// Why a DX12 device was removed (a driver reset, the card unplugged); None on other backends and while it's fine.
#[cfg(windows)]
fn device_removed(device: &wgpu::Device) -> Option<String> {
    // SAFETY: the guard is only read from and dropped here, and nothing in it is destroyed
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }?;
    // SAFETY: GetDeviceRemovedReason only reads the device's state
    unsafe { hal.raw_device().GetDeviceRemovedReason() }.err().map(|e| e.to_string())
}

#[cfg(not(windows))]
fn device_removed(_device: &wgpu::Device) -> Option<String> {
    None
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
    wants_history: bool,
    built: Option<Result<Built, String>>,
    /// Uniforms that still have to go into a buffer taken over from an older slot (adopt).
    pending_uniforms: Option<serde_json::Value>,
    /// What the step drew for the stream's last frame, for a shader that asks for it (previous).
    history: Mutex<Option<Past>>,
    #[cfg(test)]
    draws: AtomicU64,
}

/// A step's last frame on the card, and the time of the picture it was drawn for.
struct Past {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    time_ns: u64,
}

/// A picture on the card with shaders recorded on it. land reads it back and it can go on from there.
pub struct Held {
    enc: wgpu::CommandEncoder,
    /// The picture so far as RGBA (allocated on demand when a step or readback needs RGBA), and scratch for the next shader.
    a: Option<wgpu::Texture>,
    b: Option<wgpu::Texture>,
    used: Vec<wgpu::Texture>,
    width: u32,
    height: u32,
    format: PixelFormat,
    number: u64,
    time_ns: u64,
    capture_ns: Option<u64>,
    /// What reading it back has waited for the card since this was last asked.
    waited_ns: u64,
    /// For an I420 picture: its Y, U and V planes as textures while they are the picture itself (as it went up, or as a step that lays planes made it), so a step that works on planes needn't make them from the colours.
    planes: Option<[wgpu::Texture; 3]>,
    /// Whether it went up with YUV chroma read by block, so what is made from planes later is read the same way.
    block_chroma: bool,
    imported: bool,
}

impl Held {
    pub(crate) fn was_imported(&self) -> bool { self.imported }
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn take_waited(&mut self) -> u64 {
        std::mem::take(&mut self.waited_ns)
    }
}

/// What a submitted card picture paints from: either its RGBA texture or its I420 planes directly.
enum Shown {
    Rgba(wgpu::Texture),
    Planes([wgpu::Texture; 3], bool),
}

/// What a pipeline's next submission waits for: its own last one on the card (and the card it was made on, since an index means nothing on another).
#[derive(Default)]
pub(crate) struct Pacing(Option<(u64, wgpu::SubmissionIndex)>);

#[derive(Clone, Copy)]
pub(crate) enum LandingFailure { LastGood, Dark }

/// A submitted pipeline picture with one shared RAM landing, kept entirely in media.
pub(crate) struct CardFrame {
    gpu: Arc<Gpu>,
    /// Only a landing takes this lock; painting uses `shown`, so the window never waits behind a readback.
    held: Mutex<Option<Held>>,
    shown: (Shown, (u32, u32)),
    landed: OnceLock<Frame>,
    /// What the card steps began from, only until the picture lands (its last good frame if the landing fails, and the cut-out it carries): a decoder surface in it isn't held longer.
    before: Mutex<Option<Frame>>,
    /// Whether the picture carries a cut-out, known from the start (`before` goes when a landing begins, which would leave a gap to read it in).
    cut_out: bool,
    /// What stands in for the picture once its landing failed.
    stood_in: OnceLock<telescope_core::media::StandIn>,
    shape: (u32, u32, PixelFormat),
    fallback: LandingFailure,
    failed: Arc<AtomicBool>,
    balance: Arc<telescope_core::balance::Balance>,
}

impl CardFrame {
    pub(crate) fn new(gpu: Arc<Gpu>, mut held: Held, before: Frame, fallback: LandingFailure, failed: Arc<AtomicBool>, balance: Arc<telescope_core::balance::Balance>) -> Self {
        let shape = (before.width, before.height, before.format);
        let cut_out = before.alpha().is_some();
        // The submission is already queued, so scratch textures (b, temporaries) can return to the pool right away.
        let mut release = std::mem::take(&mut held.used);
        release.extend(held.b.take());
        let shown = if let Some(planes) = &held.planes {
            release.extend(held.a.take());
            Shown::Planes(planes.clone(), held.block_chroma)
        } else {
            Shown::Rgba(held.a.clone().expect("submitted card picture has neither planes nor RGBA"))
        };
        gpu.give_back(release);
        let size = held.size();
        Self { gpu, shown: (shown, size), held: Mutex::new(Some(held)), landed: OnceLock::new(), before: Mutex::new(Some(before)), cut_out, stood_in: OnceLock::new(), shape, fallback, failed, balance }
    }

    /// The state a landing in progress leaves the picture in, for tests: what it began from is taken and nothing has landed yet.
    #[cfg(test)]
    pub(crate) fn begin_landing_for_test(&self) {
        self.before.lock().unwrap().take();
    }

    /// Whether this card can draw the picture itself: it is on the same device and the card didn't fail to land it.
    pub(crate) fn paintable(&self, gpu: &Gpu) -> bool {
        self.gpu.serial == gpu.serial && !(self.landed.get().is_some() && self.failed.load(Ordering::SeqCst))
    }

    fn paint(&self, gpu: &Gpu, target: &wgpu::Texture) -> Option<Result<(), String>> {
        self.paintable(gpu).then(|| gpu.paint_held(&self.shown.0, self.shown.1, target))
    }
}

impl telescope_core::media::CardPicture for CardFrame {
    fn land(&self) -> Frame {
        self.land_until(&|| false)
    }

    fn land_until(&self, cancel: &dyn Fn() -> bool) -> Frame {
        self.landed.get_or_init(|| {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            let before = self.before.lock().unwrap_or_else(|e| e.into_inner()).take();
            let began = monotonic_ns();
            let out = catch_unwind(AssertUnwindSafe(|| self.gpu.land_until(held.as_mut().unwrap(), cancel)));
            transfers::landed();
            span("readback", began);
            match out {
                Ok(Ok(mut frame)) => {
                    frame.geometry = before.as_ref().and_then(|frame| frame.geometry.clone());
                    frame.mask_space = before.as_ref().map_or(Default::default(), |frame| frame.mask_space);
                    if let Some(alpha) = before.as_ref().and_then(Frame::alpha) {
                        frame.set_alpha(alpha.clone());
                    }
                    self.balance.note_wait(held.as_mut().unwrap().take_waited());
                    frame
                }
                _ => {
                    if !cancel() {
                        log::warn!("Couldn't land a card picture; using its safe fallback and moving this stream to the CPU");
                        self.failed.store(true, Ordering::SeqCst);
                    }
                    match (self.fallback, before) {
                        (LandingFailure::LastGood, Some(before)) => {
                            let _ = self.stood_in.set(telescope_core::media::StandIn::Unprocessed);
                            before.materialise()
                        }
                        _ => {
                            let _ = self.stood_in.set(telescope_core::media::StandIn::Dark);
                            Frame::dark(self.shape.0, self.shape.1, self.shape.2)
                        }
                    }
                }
            }
        }).clone()
    }

    fn landed(&self) -> Option<Frame> {
        self.landed.get().cloned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn fits_on_card(&self, width: u32, height: u32) -> bool {
        let (w, h, format) = self.shape;
        // A cut-out goes over the camera's background before the fit, which the card doesn't do; a picture that failed to land is its stand-in now
        format == PixelFormat::I420 && (w, h) != (width, height) && !self.cut_out && self.stood_in.get().is_none() && !self.gpu.fit_refused.load(Ordering::Relaxed)
    }

    fn stood_in(&self) -> Option<telescope_core::media::StandIn> {
        self.stood_in.get().copied()
    }

    fn land_fitted(&self, width: u32, height: u32, cancel: &dyn Fn() -> bool) -> Option<Frame> {
        // A picture in memory is fitted from there: reading it back from the card again would be a second readback
        if !self.fits_on_card(width, height) || self.landed.get().is_some() {
            return None;
        }
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        // A landing that held the lock before this one is through: its picture is the one to use
        if self.landed.get().is_some() {
            return None;
        }
        let held = held.as_mut()?;
        let began = monotonic_ns();
        let out = catch_unwind(AssertUnwindSafe(|| self.gpu.land_fitted(held, width, height, cancel)));
        span("readback", began);
        match out {
            Ok(Ok(frame)) => {
                transfers::landed();
                self.balance.note_wait(held.take_waited());
                Some(frame)
            }
            // The whole picture lands instead; a card that can't fit pictures isn't asked again
            Ok(Err(why)) if !cancel() => {
                if !self.gpu.fit_refused.swap(true, Ordering::Relaxed) {
                    log::warn!("Couldn't fit a picture to the camera on the graphics card ({why}); the processor fits them from now on");
                }
                None
            }
            Err(_) => {
                self.gpu.fit_refused.store(true, Ordering::Relaxed);
                None
            }
            Ok(Err(_)) => None,
        }
    }
}

impl Drop for CardFrame {
    fn drop(&mut self) {
        if let Some(held) = self.held.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            self.gpu.put_down(held);
        }
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
    /// Group 1: the plugin's uniforms and textures, or None when it has neither (or a blur, whose group is made for each frame).
    extras: Option<wgpu::BindGroup>,
    extras_layout: Option<wgpu::BindGroupLayout>,
    /// The Params buffer, kept so new uniforms can be written into it.
    ubuf: Option<wgpu::Buffer>,
    /// What each of the step's textures is bound to now, and what it went up as at first.
    views: Vec<wgpu::TextureView>,
    first_views: Vec<wgpu::TextureView>,
    /// For a texture the CPU half has replaced: the data it came from, to know an unchanged one, and the texture itself.
    sources: Vec<Option<Arc<Vec<u8>>>>,
    replaced: Vec<Option<wgpu::Texture>>,
    blur: Option<BlurKit>,
    /// The step's passes (shader::passes) in order: the pipeline and the format its picture is kept in.
    passes: Vec<(wgpu::RenderPipeline, wgpu::TextureFormat)>,
    _keep: Vec<wgpu::Texture>,
}

#[cfg(test)]
impl ShaderSlot {
    /// Whether the shader is built (or has failed to be), for tests.
    pub(crate) fn is_built(&self) -> bool {
        self.built.is_some()
    }
}

#[cfg(test)]
impl Built {
    /// How many textures it holds for the plugin's pictures, for tests.
    fn kept_textures(&self) -> usize {
        self._keep.len()
    }
}

/// What a shader that calls blurred() needs: the passes that shrink the picture and blur it, and for an I420 picture the ones that work on its planes.
struct BlurKit {
    layout: wgpu::BindGroupLayout,
    shrink: wgpu::RenderPipeline,
    shrink_planes: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    combine: wgpu::RenderPipeline,
    compose: wgpu::RenderPipeline,
}

impl BlurKit {
    fn new(gpu: &Gpu) -> BlurKit {
        let layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[tex_entry(0), tex_entry(1), uniform_entry(2), tex_entry(3), tex_entry(4), tex_entry(5), sampler_entry(6)],
        });
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("blur"), source: wgpu::ShaderSource::Wgsl(BLUR.into()) });
        let pipeline_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("blur"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let make = |fs: &str, target| gpu.render_pipeline("blur", &pipeline_layout, &module, ("vs_main", fs), target);
        BlurKit {
            shrink: make("fs_shrink", BLUR_FORMAT),
            shrink_planes: make("fs_shrink_yuv", BLUR_FORMAT),
            blur: make("fs_blur", BLUR_FORMAT),
            combine: make("fs_combine", BLUR_FORMAT),
            compose: make("fs_composite", wgpu::TextureFormat::R8Unorm),
            layout,
        }
    }

    /// One pass into target; the textures (up to five) are bound in order, what's left over is blank, and the settings are the Pass struct in blur.wgsl.
    fn pass(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, pipeline: &wgpu::RenderPipeline, inputs: &[&wgpu::TextureView], target: &wgpu::Texture, settings: Pass) {
        let mut bytes = Vec::with_capacity(48);
        for v in [settings.src.0, settings.src.1, target.width(), target.height(), settings.dir.0, settings.dir.1, settings.chroma.0, settings.chroma.1, settings.taps, settings.weighted] {
            bytes.extend(v.to_le_bytes());
        }
        bytes.extend((settings.sigma as f32).to_le_bytes());
        bytes.extend(settings.plane.to_le_bytes());
        let buf = gpu.uniform(&bytes);
        let at = |i: usize| inputs.get(i).copied().unwrap_or(&gpu.blank);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(at(0)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(at(1)) },
                wgpu::BindGroupEntry { binding: 2, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(at(2)) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(at(3)) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(at(4)) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&gpu.linear) },
            ],
        });
        draw(enc, pipeline, &[&bind], &target.create_view(&Default::default()));
    }
}

/// The settings of one blur pass.
#[derive(Clone, Copy)]
struct Pass {
    src: (u32, u32),
    dir: (u32, u32),
    chroma: (u32, u32),
    taps: u32,
    weighted: u32,
    sigma: f64,
    plane: u32,
}

impl Pass {
    fn of(src: (u32, u32)) -> Pass {
        Pass { src, dir: (0, 0), chroma: (0, 0), taps: 1, weighted: 0, sigma: 1.0, plane: 0 }
    }
}

impl ShaderSlot {
    pub fn new(step: ShaderStep) -> ShaderSlot {
        ShaderSlot {
            wants_mask: shader::uses_person_mask(&step.wgsl),
            wants_history: shader::uses_history(&step.wgsl),
            step,
            built: None,
            pending_uniforms: None,
            history: Mutex::new(None),
            #[cfg(test)]
            draws: AtomicU64::new(0),
        }
    }

    /// Whether the step says it leaves every pixel where it was (ShaderStep::keeps_geometry).
    pub fn keeps_geometry(&self) -> bool {
        self.step.keeps_geometry
    }

    /// The last frame this step drew, as a view, when there is one that still goes with held's picture: the same size and not too old.
    fn past(&self, held: &Held) -> Option<wgpu::TextureView> {
        if !self.wants_history {
            return None;
        }
        let kept = self.history.lock().unwrap();
        let p = kept.as_ref()?;
        let age = held.time_ns.checked_sub(p.time_ns)?;
        ((p.width, p.height) == (held.width, held.height) && age < shader::HISTORY_MAX_AGE_NS).then(|| p.texture.create_view(&Default::default()))
    }

    /// Copies what the step just drew, so the next frame can use it.
    fn keep(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, drawn: &wgpu::Texture, time_ns: u64) {
        if !self.wants_history {
            return;
        }
        let mut kept = self.history.lock().unwrap();
        let (width, height) = (drawn.width(), drawn.height());
        if !kept.as_ref().is_some_and(|p| (p.width, p.height) == (width, height)) {
            *kept = Some(Past { texture: gpu.history_texture(width, height), width, height, time_ns });
        }
        let p = kept.as_mut().unwrap();
        enc.copy_texture_to_texture(drawn.as_image_copy(), p.texture.as_image_copy(), wgpu::Extent3d { width, height, depth_or_array_layers: 1 });
        p.time_ns = time_ns;
    }

    /// Changes the shader's uniforms; once built, the new values go into its buffer. A shader with no Params ignores them.
    pub fn set_uniforms(&mut self, gpu: &Gpu, uniforms: serde_json::Value) {
        // Newer than any an adopted buffer was still waiting for
        self.pending_uniforms = None;
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

    /// Takes over the shader `old` built (the same shader with no pictures of its own, from a step made again since), so a step that is made
    /// again for every frame (the view's crop while Auto framing moves it) isn't built again each time. Its own uniforms go in before it next
    /// draws. False when there's nothing to take.
    pub fn adopt(&mut self, old: &mut ShaderSlot) -> bool {
        if self.built.is_some() || old.built.is_none() || self.step.wgsl != old.step.wgsl || !self.step.textures.is_empty() || !old.step.textures.is_empty() {
            return false;
        }
        self.built = old.built.take();
        // The buffer holds old's values until these replace them
        let wanted = std::mem::replace(&mut self.step.uniforms, old.step.uniforms.clone());
        self.pending_uniforms = Some(wanted);
        true
    }

    /// Takes over what `old` (the same shader from a step the plugin has since replaced) drew for the last frame, so a step whose settings
    /// changed goes on from its history instead of starting again. Nothing happens for a different shader or one that keeps none.
    pub fn take_history_from(&self, old: &ShaderSlot) {
        if self.wants_history && old.wants_history && self.step.wgsl == old.step.wgsl {
            let kept = old.history.lock().unwrap().take();
            *self.history.lock().unwrap() = kept;
        }
    }

    /// A number for the shader and what it is given, for the pipeline to see a View stage step change.
    pub fn geometry_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.step.wgsl.hash(&mut hasher);
        self.step.uniforms.to_string().hash(&mut hasher);
        hasher.finish()
    }

    /// Drops the last frame it drew: the next one starts again (the picture's geometry changed, so the old one doesn't line up).
    pub fn forget_history(&self) {
        *self.history.lock().unwrap() = None;
    }

    /// Which shader with which settings and pictures this is, to tell a step from the one a frame already went through.
    pub fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.step.wgsl.hash(&mut h);
        self.step.uniforms.to_string().hash(&mut h);
        for (name, f) in &self.step.textures {
            (name, f.width, f.height, Arc::as_ptr(&f.data) as usize).hash(&mut h);
        }
        h.finish()
    }

    /// Whether the step keeps its last frame for the shader.
    pub fn keeps_history(&self) -> bool {
        self.wants_history
    }

    /// Whether this is the same shader (whatever its settings are).
    pub fn same_shader(&self, other: &ShaderSlot) -> bool {
        self.step.wgsl == other.step.wgsl
    }

    /// Whether the shader reads the person mask.
    pub fn wants_mask(&self) -> bool {
        self.wants_mask
    }

    /// Builds the shader the first time; Err for one that won't ever run.
    pub fn prepare(&mut self, gpu: &Gpu) -> Result<(), String> {
        gpu.guarded("building a shader", || {
            #[cfg(test)]
            if gpu.take_fault(|f| matches!(f, Fault::PreparePanic)) {
                panic!("an injected panic while building");
            }
            if self.built.is_none() {
                self.built = Some(build(gpu, &self.step));
            }
            if let Some(uniforms) = self.pending_uniforms.take() {
                self.set_uniforms(gpu, uniforms);
            }
            self.built.as_ref().unwrap().as_ref().map(|_| ()).map_err(Clone::clone)
        })
    }

    fn draw(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, dst: &wgpu::Texture, info: &wgpu::Buffer, mask: Option<&MaskPlane>, past: Option<&wgpu::TextureView>, blurred: Option<&(wgpu::TextureView, wgpu::TextureView)>, passed: &[wgpu::TextureView]) {
        let Some(Ok(built)) = &self.built else { return };
        #[cfg(test)]
        self.draws.fetch_add(1, Ordering::Relaxed);
        let frame_bind = frame_group(gpu, src, info, mask, past);
        let made = self.frame_extras(gpu, built, blurred, passed);
        let mut binds = vec![&frame_bind];
        binds.extend(made.as_ref().or(built.extras.as_ref()));
        draw(enc, &built.pipeline, &binds, &dst.create_view(&Default::default()));
    }

    /// Group 1 made for this frame, for a step whose blurred copies or passes are new each frame; None for one whose group was made once.
    fn frame_extras(&self, gpu: &Gpu, built: &Built, blurred: Option<&(wgpu::TextureView, wgpu::TextureView)>, passed: &[wgpu::TextureView]) -> Option<wgpu::BindGroup> {
        let layout = built.extras_layout.as_ref()?;
        if blurred.is_none() && built.passes.is_empty() {
            return None;
        }
        let mut more: Vec<(u32, &wgpu::TextureView)> = Vec::new();
        if let (Some(b), Some((p, w))) = (blurred, self.blur_bindings()) {
            more.extend([(p, &b.0), (w, &b.1)]);
        }
        let first = shader::pass_binding(&self.step);
        more.extend((0..built.passes.len()).map(|k| (first + k as u32, passed.get(k).unwrap_or(&gpu.blank))));
        Some(extras_group(gpu, layout, built.ubuf.as_ref(), &built.views, &more))
    }

    /// Records the step's passes (shader::passes) one after the other, each into a picture of its own at its size, and gives their views for effect(); empty for a step without any.
    fn passes(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::Texture, info: &wgpu::Buffer, mask: Option<&MaskPlane>, past: Option<&wgpu::TextureView>, blurred: Option<&(wgpu::TextureView, wgpu::TextureView)>, used: &mut Vec<wgpu::Texture>) -> Vec<wgpu::TextureView> {
        let Some(Ok(built)) = &self.built else { return Vec::new() };
        if built.passes.is_empty() {
            return Vec::new();
        }
        let frame_bind = frame_group(gpu, src, info, mask, past);
        let most = gpu.device.limits().max_texture_dimension_2d;
        let mut passed = Vec::with_capacity(built.passes.len());
        for (k, (pipeline, format)) in built.passes.iter().enumerate() {
            let (w, h) = shader::pass_size(&self.step.uniforms, k, src.width(), src.height());
            let target = gpu.texture(w.min(most), h.min(most), *format);
            used.push(target.clone());
            let extras = self.frame_extras(gpu, built, blurred, &passed);
            let mut binds = vec![&frame_bind];
            binds.extend(extras.as_ref());
            draw(enc, pipeline, &binds, &target.create_view(&Default::default()));
            passed.push(target.create_view(&Default::default()));
        }
        passed
    }

    /// The step's picture for the room (shader::ROOM_PICTURE) as it is bound now.
    fn room_picture(&self) -> Option<wgpu::TextureView> {
        let Some(Ok(Built { views, .. })) = &self.built else { return None };
        self.step.textures.iter().position(|(n, _)| n == shader::ROOM_PICTURE).map(|i| views[i].clone())
    }

    fn blur_bindings(&self) -> Option<(u32, u32)> {
        shader::blur_bindings(&self.step)
    }

    /// The step's uniforms for the blur: how much smaller it is worked out and its sigma.
    fn blur_settings(&self) -> (u32, f64) {
        let get = |name: &str| self.step.uniforms.get(name).and_then(serde_json::Value::as_f64);
        (get(shader::BLUR_SHRINK).map_or(8, |v| v.max(1.0) as u32), get(shader::BLUR_SIGMA).map_or(2.0, |v| v.max(0.1)))
    }

    /// Whether the step lays the blur over an I420 picture plane by plane (see shader::BLUR_PLANES), which it does for pictures of at least 2 x 2.
    fn wants_planes(&self, w: u32, h: u32) -> bool {
        let asked = self.step.uniforms.get(shader::BLUR_PLANES).and_then(serde_json::Value::as_f64).is_some_and(|v| v >= 0.5);
        asked && w >= 2 && h >= 2 && matches!(&self.built, Some(Ok(Built { blur: Some(_), .. })))
    }

    /// Records the passes that make the blurred copies of src (shrunk, from planes when it has them, blurred and, where the CPU half gave weights, blurred without what they leave out) and gives (plain, weighted) as views; None for a step that doesn't ask.
    fn blur(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, size: (u32, u32), src: Option<&wgpu::Texture>, planes: Option<&[wgpu::Texture; 3]>, used: &mut Vec<wgpu::Texture>) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        let Some(Ok(Built { blur: Some(kit), views, .. })) = &self.built else { return None };
        // A step with a picture for its room and no blur of its own only has the kit for composing
        self.blur_bindings()?;
        let (shrink, sigma) = self.blur_settings();
        let (sw, sh) = shader::blur_size(size.0, size.1, shrink);
        let taps = shader::blur_taps(sigma);
        let mut texture = || {
            let t = gpu.texture(sw, sh, BLUR_FORMAT);
            used.push(t.clone());
            t
        };
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let (small, across, plain) = (texture(), texture(), texture());
        match planes {
            Some(planes) => {
                let [y, u, v] = planes.each_ref().map(view);
                kit.pass(gpu, enc, &kit.shrink_planes, &[&y, &u, &v], &small, Pass { chroma: (planes[1].width(), planes[1].height()), ..Pass::of(size) });
            }
            None => kit.pass(gpu, enc, &kit.shrink, &[&view(src.expect("blur without planes needs RGBA source"))], &small, Pass::of(size)),
        }
        let blur = |dir, weighted| Pass { src: (sw, sh), dir, taps, weighted, sigma, ..Pass::of((sw, sh)) };
        kit.pass(gpu, enc, &kit.blur, &[&view(&small)], &across, blur((1, 0), 0));
        kit.pass(gpu, enc, &kit.blur, &[&view(&across)], &plain, blur((0, 1), 0));
        // Weights count only at the size the blur is worked at, whether the step declared them or its CPU half gave them
        let weights = self.step.textures.iter().position(|(n, _)| n == shader::BLUR_WEIGHTS)
            .filter(|&i| (views[i].texture().width(), views[i].texture().height()) == (sw, sh));
        let Some(weights) = weights else { return Some((view(&plain), view(&plain))) };
        let total = texture();
        kit.pass(gpu, enc, &kit.blur, &[&view(&small), &views[weights]], &across, blur((1, 0), 1));
        kit.pass(gpu, enc, &kit.blur, &[&view(&across)], &total, blur((0, 1), 0));
        kit.pass(gpu, enc, &kit.combine, &[&view(&plain), &view(&total)], &across, Pass::of((sw, sh)));
        Some((view(&plain), view(&across)))
    }

    /// The picture's own planes laid over the blur plane by plane where the step's cutout says (shader::BLUR_CUTOUT, nothing where it has none), made as the CPU makes them so what the cutout keeps is the picture's samples untouched; the three planes as textures.
    fn compose(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, planes: &[wgpu::Texture; 3], room: &wgpu::TextureView) -> Option<[wgpu::Texture; 3]> {
        let Some(Ok(Built { blur: Some(kit), views, .. })) = &self.built else { return None };
        let cutout = self.step.textures.iter().position(|(n, _)| n == shader::BLUR_CUTOUT).map_or(&gpu.blank, |i| &views[i]);
        let own = planes.each_ref().map(|t| t.create_view(&Default::default()));
        let out = planes.each_ref().map(|t| gpu.texture(t.width(), t.height(), wgpu::TextureFormat::R8Unorm));
        for (plane, target) in out.iter().enumerate() {
            kit.pass(gpu, enc, &kit.compose, &[&own[0], &own[1], &own[2], room, cutout], target, Pass { plane: plane as u32, ..Pass::of((target.width(), target.height())) });
        }
        Some(out)
    }

    /// Takes the pictures the CPU half has for the shader (see CpuStep::card_textures) in place of the step's own of those names; one it leaves out goes back to the first. A picture goes up only when its data isn't the one already there. A picture the card refuses is an error, and the shader isn't to be drawn with what was there before it.
    pub fn set_textures(&mut self, gpu: &Gpu, given: &[(String, Frame)]) -> Result<(), String> {
        let has_blur = shader::blur_bindings(&self.step).is_some();
        let Some(Ok(built)) = &mut self.built else { return Ok(()) };
        let mut changed = false;
        let mut refused = None;
        for (i, (name, _)) in self.step.textures.iter().enumerate() {
            match given.iter().find(|(n, _)| n == name) {
                Some((_, f)) if built.sources[i].as_ref().is_some_and(|s| Arc::ptr_eq(s, &f.data)) => {}
                Some((_, f)) => {
                    let texture = match gpu.picture_texture(f, is_red_mask(name)) {
                        Ok(Some(texture)) => texture,
                        Ok(None) => {
                            log::warn!("Shader texture {name} wasn't used: {:?} {}x{} can't be uploaded", f.format, f.width, f.height);
                            continue;
                        }
                        Err(why) => {
                            refused.get_or_insert(why);
                            continue;
                        }
                    };
                    built.views[i] = texture.create_view(&Default::default());
                    built.sources[i] = Some(f.data.clone());
                    gpu.give_back(built.replaced[i].replace(texture));
                    transfers::mask_up();
                    changed = true;
                }
                None if built.sources[i].is_some() => {
                    built.views[i] = built.first_views[i].clone();
                    built.sources[i] = None;
                    gpu.give_back(built.replaced[i].take());
                    changed = true;
                }
                None => {}
            }
        }
        if changed && !has_blur && built.passes.is_empty() {
            built.extras = built.extras_layout.as_ref().map(|l| extras_group(gpu, l, built.ubuf.as_ref(), &built.views, &[]));
        }
        refused.map_or(Ok(()), Err)
    }
}

/// Group 0 for a step: the picture so far, the sampler, the frame's info, the mask and the step's last frame.
fn frame_group(gpu: &Gpu, src: &wgpu::Texture, info: &wgpu::Buffer, mask: Option<&MaskPlane>, past: Option<&wgpu::TextureView>) -> wgpu::BindGroup {
    let src_view = src.create_view(&Default::default());
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("frame"),
        layout: &gpu.frame_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.linear) },
            wgpu::BindGroupEntry { binding: 2, resource: info.as_entire_binding() },
            wgpu::BindGroupEntry { binding: shader::BINDING_MASK, resource: wgpu::BindingResource::TextureView(mask.map_or(&gpu.blank, |m| &m.view)) },
            wgpu::BindGroupEntry { binding: shader::BINDING_HISTORY, resource: wgpu::BindingResource::TextureView(past.unwrap_or(&gpu.blank)) },
        ],
    })
}

/// Group 1 for the step: its Params buffer, its textures and what is made for it each frame (the blurred copies, the passes' pictures) by binding.
fn extras_group(gpu: &Gpu, layout: &wgpu::BindGroupLayout, params: Option<&wgpu::Buffer>, views: &[wgpu::TextureView], more: &[(u32, &wgpu::TextureView)]) -> wgpu::BindGroup {
    let mut e = Vec::new();
    if let Some(b) = params {
        e.push(wgpu::BindGroupEntry { binding: shader::BINDING_PARAMS, resource: b.as_entire_binding() });
    }
    for (i, v) in views.iter().enumerate() {
        e.push(wgpu::BindGroupEntry { binding: shader::BINDING_FIRST_TEXTURE + i as u32, resource: wgpu::BindingResource::TextureView(v) });
    }
    for &(binding, v) in more {
        e.push(wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) });
    }
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("plugin"), layout, entries: &e })
}

/// Single-channel red textures named by core::shader (blur_cutout, blur_weights) whose shaders only read `.r`.
fn is_red_mask(name: &str) -> bool {
    name == shader::BLUR_CUTOUT || name == shader::BLUR_WEIGHTS
}

impl Gpu {
    /// An Rgba picture as a texture of its own to sample, from the pool (as R8Unorm when `red_only` is set); Ok(None) for another format, and an error when the card refuses it.
    fn picture_texture(&self, f: &Frame, red_only: bool) -> Result<Option<wgpu::Texture>, String> {
        if f.format != PixelFormat::Rgba || f.width == 0 || f.height == 0 {
            return Ok(None);
        }
        // A pooled texture still holds the picture it had, so a refused write must not leave it to be sampled
        self.scoped(|| {
            if red_only {
                let red: Vec<u8> = f.data.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
                let t = self.texture(f.width, f.height, wgpu::TextureFormat::R8Unorm);
                self.write_plane(&t, &red, f.width);
                t
            } else {
                let t = self.texture(f.width, f.height, WORK);
                self.write_plane(&t, &f.data, f.width * 4);
                t
            }
        })
        .map(Some)
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
    let blur_bindings = shader::blur_bindings(step);
    entries.extend(blur_bindings.into_iter().flat_map(|(plain, weighted)| [tex_entry(plain), tex_entry(weighted)]));
    let kinds = shader::passes(&step.wgsl);
    let first_pass = shader::pass_binding(step);
    entries.extend((0..kinds.len()).map(|k| loaded_entry(first_pass + k as u32)));
    let extras_layout = (!entries.is_empty()).then(|| gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("plugin"),
        entries: &entries,
    }));
    let layouts: Vec<Option<&wgpu::BindGroupLayout>> = [Some(&gpu.frame_layout), extras_layout.as_ref()].into_iter().take(if extras_layout.is_some() { 2 } else { 1 }).collect();
    let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("plugin"), bind_group_layouts: &layouts, immediate_size: 0 });
    let pipeline = gpu.render_pipeline("plugin", &layout, &module, (shader::VERTEX_ENTRY, shader::FRAGMENT_ENTRY), WORK);
    // A number pass is packed into 8-bit RGBA (core::shader): every card can draw into that, where R32Float isn't renderable on all of them
    let passes = (0..kinds.len()).map(|k| (gpu.render_pipeline("plugin pass", &layout, &module, (shader::VERTEX_ENTRY, &shader::pass_entry(k)), WORK), WORK)).collect();

    // The plugin's pictures go up once, as RGBA (or R8Unorm for single-channel red blur masks) whatever they came as
    let mut keep = Vec::new();
    let mut staging = Vec::new();
    let mut views = Vec::new();
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    for (name, f) in &step.textures {
        let mut used = Vec::new();
        let planes = gpu.upload(f, &mut used);
        let format = if is_red_mask(name) { wgpu::TextureFormat::R8Unorm } else { WORK };
        let t = gpu.texture(f.width, f.height, format);
        gpu.convert_pass(&mut enc, &planes.views, planes.mode, &t, 1, (f.width, f.height));
        views.push(t.create_view(&Default::default()));
        keep.push(t);
        staging.extend(used);
    }
    gpu.queue.submit([enc.finish()]);
    // Only the converted pictures are used from here; wgpu frees the upload textures once the submit is done with them
    drop(staging);
    let ubuf = has_uniforms.then(|| gpu.uniform(&params));
    // With a blur the group is made for each frame, as its copies are new each time
    let extras = extras_layout.as_ref().filter(|_| blur_bindings.is_none() && kinds.is_empty()).map(|l| extras_group(gpu, l, ubuf.as_ref(), &views, &[]));
    let blur = (blur_bindings.is_some() || step.textures.iter().any(|(n, _)| n == shader::ROOM_PICTURE)).then(|| BlurKit::new(gpu));
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(e.to_string());
    }
    let n = views.len();
    Ok(Built { pipeline, extras, extras_layout, ubuf, first_views: views.clone(), views, sources: vec![None; n], replaced: vec![None; n], blur, passes, _keep: keep })
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
        ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms, textures: vec![] }
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
            gpu.shade(&mut held, slot, 0.0, None).unwrap();
        }
        let out = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        out
    }

    const IDENTITY: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";

    /// 0.2 on the first frame, then the last frame's red and 0.2 more.
    const COUNTS: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { if has_previous() { return vec4<f32>(previous(uv).r + 0.2, 0.0, 0.0, 1.0); } return vec4<f32>(0.2, 0.0, 0.0, 1.0); }";

    #[test]
    fn a_step_that_asks_gets_what_it_drew_for_the_last_frame_until_the_size_or_the_time_changes() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let frame = |w: u32, time_ns: u64| {
            let mut f = Frame::new(w, 4, PixelFormat::Rgba, vec![0; (w * 4 * 4) as usize]);
            f.time_ns = time_ns;
            f
        };
        let red = |f: &Frame| f.data[0];
        let mut slots = [ShaderSlot::new(step(COUNTS, json!({})))];
        let ms = 1_000_000;
        // 0.2 is 51 of 255, and every frame adds another
        assert_eq!(red(&run(&gpu, &frame(8, 100 * ms), &mut slots)), 51);
        assert_eq!(red(&run(&gpu, &frame(8, 133 * ms), &mut slots)), 102);
        assert_eq!(red(&run(&gpu, &frame(8, 166 * ms), &mut slots)), 153);
        // another size starts again, so does a gap too long for it to be the same scene, and so does a clock that went back
        assert_eq!(red(&run(&gpu, &frame(16, 199 * ms), &mut slots)), 51);
        assert_eq!(red(&run(&gpu, &frame(16, 232 * ms), &mut slots)), 102);
        assert_eq!(red(&run(&gpu, &frame(16, 232 * ms + shader::HISTORY_MAX_AGE_NS + 1), &mut slots)), 51);
        assert_eq!(red(&run(&gpu, &frame(16, 5 * ms), &mut slots)), 51);
        // a step that doesn't ask keeps nothing, and the frames after it are as they were
        let mut plain = [ShaderSlot::new(step(IDENTITY, json!({})))];
        run(&gpu, &frame(8, 400 * ms), &mut plain);
        assert!(plain[0].history.lock().unwrap().is_none());
    }

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

    /// The blur as a plain program would make it: the picture shrunk by area, blurred along each axis with reflected edges, read with linear filtering at each pixel's centre. rgb 0..1.
    mod blur_reference {
        pub struct Small {
            pub w: usize,
            pub h: usize,
            pub px: Vec<[f32; 4]>,
        }

        pub fn shrunk(rgba: &[u8], w: usize, h: usize, shrink: usize) -> Small {
            let (sw, sh) = ((w / shrink).max(1), (h / shrink).max(1));
            let mut px = vec![[0f32; 4]; sw * sh];
            for (i, out) in px.iter_mut().enumerate() {
                let (ox, oy) = (i % sw, i / sw);
                let (x0, x1, y0, y1) = (ox * w / sw, (ox + 1) * w / sw, oy * h / sh, (oy + 1) * h / sh);
                let n = ((x1 - x0) * (y1 - y0)) as f32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        for c in 0..3 {
                            out[c] += rgba[(y * w + x) * 4 + c] as f32 / 255.0 / n;
                        }
                    }
                }
                out[3] = 1.0;
            }
            Small { w: sw, h: sh, px }
        }

        fn reflect(i: isize, n: isize) -> usize {
            if n == 1 {
                return 0;
            }
            let mut i = i;
            while i < 0 || i >= n {
                i = if i < 0 { -i } else { 2 * (n - 1) - i };
            }
            i as usize
        }

        pub fn gauss(img: &Small, sigma: f64, taps: usize) -> Small {
            let half = (taps / 2) as isize;
            let weights: Vec<f64> = (-half..=half).map(|j| (-(j * j) as f64 / (2.0 * sigma * sigma)).exp()).collect();
            let total: f64 = weights.iter().sum();
            let across = |src: &Small, horizontal: bool| {
                let mut px = vec![[0f32; 4]; src.w * src.h];
                for y in 0..src.h {
                    for x in 0..src.w {
                        for (j, wt) in (-half..=half).zip(&weights) {
                            let (sx, sy) = if horizontal { (reflect(x as isize + j, src.w as isize), y) } else { (x, reflect(y as isize + j, src.h as isize)) };
                            let from = src.px[sy * src.w + sx];
                            for (o, v) in px[y * src.w + x].iter_mut().zip(from) {
                                *o += (v as f64 * wt / total) as f32;
                            }
                        }
                    }
                }
                Small { w: src.w, h: src.h, px }
            };
            across(&across(img, true), false)
        }

        /// What blurred_with does with the weights (0..1 a pixel at the small size).
        pub fn weighted(small: &Small, weights: &[f32], sigma: f64, taps: usize) -> Small {
            let plain = gauss(small, sigma, taps);
            let kept = Small { w: small.w, h: small.h, px: small.px.iter().zip(weights).map(|(p, w)| [p[0] * w, p[1] * w, p[2] * w, *w]).collect() };
            let total = gauss(&kept, sigma, taps);
            let px = plain.px.iter().zip(&total.px).map(|(p, t)| {
                let weight = (t[3] * 4.0).clamp(0.0, 1.0);
                let mut out = [0f32; 4];
                for (c, o) in out.iter_mut().enumerate().take(3) {
                    *o = t[c] / t[3].max(1e-3) * weight + p[c] * (1.0 - weight);
                }
                out
            }).collect();
            Small { w: small.w, h: small.h, px }
        }

        /// The picture at (x, y) of a w x h frame, 0..255.
        pub fn read(small: &Small, x: usize, y: usize, w: usize, h: usize) -> [f32; 3] {
            let at = |c: usize, size: usize, n: usize| (((c as f32 + 0.5) / size as f32 * n as f32) - 0.5).clamp(0.0, n as f32 - 1.0);
            let (fx, fy) = (at(x, w, small.w), at(y, h, small.h));
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(small.w - 1), (y0 + 1).min(small.h - 1));
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let mix = |c: usize| {
                let top = small.px[y0 * small.w + x0][c] * (1.0 - tx) + small.px[y0 * small.w + x1][c] * tx;
                let bottom = small.px[y1 * small.w + x0][c] * (1.0 - tx) + small.px[y1 * small.w + x1][c] * tx;
                (top * (1.0 - ty) + bottom * ty) * 255.0
            };
            [mix(0), mix(1), mix(2)]
        }
    }

    const BLURRED: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(blurred(uv).rgb, 1.0); }";
    const BLURRED_WEIGHTED: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(blurred_weighted(uv).rgb, 1.0); }";

    fn noisy_rgba(w: usize, h: usize) -> Frame {
        let mut x = 11u32;
        let data = (0..w * h * 4).map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            // blocks of colour with a little noise, so there are edges to blur
            let (px, py) = ((i / 4) % w, (i / 4) / w);
            let base = [(px / 13 * 40) % 256, (py / 11 * 55) % 256, ((px + py) / 17 * 70) % 256, 255][i % 4] as u32;
            (base + (x >> 30)).min(255) as u8
        }).collect();
        Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
    }

    fn weights_frame(w: u32, h: u32, share: impl Fn(u32, u32) -> u8) -> Frame {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend([share(x, y), 0, 0, 255]);
            }
        }
        Frame::new(w, h, PixelFormat::Rgba, data)
    }

    #[test]
    fn the_card_blurs_a_shrunk_copy_of_the_picture_as_a_program_would() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h, shrink) = (200usize, 120usize, 8usize);
        let frame = noisy_rgba(w, h);
        for sigma in [1.1f64, 3.5, 6.5] {
            let uniforms = json!({"blur_shrink": shrink, "blur_sigma": sigma});
            let out = run(&gpu, &frame, &mut [ShaderSlot::new(step(BLURRED, uniforms))]);
            let small = blur_reference::gauss(&blur_reference::shrunk(&frame.data, w, h, shrink), sigma, shader::blur_taps(sigma) as usize);
            let mut worst = 0f32;
            for y in 0..h {
                for x in 0..w {
                    let want = blur_reference::read(&small, x, y, w, h);
                    for (c, v) in want.iter().enumerate() {
                        worst = worst.max((out.data[(y * w + x) * 4 + c] as f32 - v).abs());
                    }
                }
            }
            assert!(worst <= 1.5, "sigma {sigma}: the card is {worst} levels from the reference");
        }
    }

    #[test]
    fn the_weighted_blur_leaves_out_what_the_weights_mark_and_a_missing_weight_is_the_plain_blur() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h, shrink, sigma) = (192usize, 128usize, 8usize, 2.9f64);
        let frame = noisy_rgba(w, h);
        let (sw, sh) = (w / shrink, h / shrink);
        let share = |x: u32, y: u32| if (4..12).contains(&x) && (3..11).contains(&y) { 0 } else { 255 };
        let weights = weights_frame(sw as u32, sh as u32, share);
        let placeholder = Frame::new(1, 1, PixelFormat::Rgba, vec![255, 0, 0, 255]);
        let weighted_step = || ShaderStep {
           keeps_geometry: false,
            wgsl: BLURRED_WEIGHTED.into(),
            uniforms: json!({"blur_shrink": shrink, "blur_sigma": sigma}),
            textures: vec![("blur_weights".into(), placeholder.clone())],
        };
        let compare = |out: &Frame, want: &blur_reference::Small| {
            let mut worst = 0f32;
            for y in 0..h {
                for x in 0..w {
                    let at = blur_reference::read(want, x, y, w, h);
                    (0..3).for_each(|c| worst = worst.max((out.data[(y * w + x) * 4 + c] as f32 - at[c]).abs()));
                }
            }
            worst
        };
        let taps = shader::blur_taps(sigma) as usize;
        let small = blur_reference::shrunk(&frame.data, w, h, shrink);
        let mut slot = ShaderSlot::new(weighted_step());
        slot.prepare(&gpu).unwrap();
        // no weights given yet: the plain blur
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        let worst = compare(&out, &blur_reference::gauss(&small, sigma, taps));
        assert!(worst <= 1.5, "without weights {worst} levels from the plain blur");
        // weights given
        let uploads = crate::transfers::totals().mask_up;
        slot.set_textures(&gpu, &[("blur_weights".into(), weights.clone())]).unwrap();
        assert_eq!(crate::transfers::totals().mask_up, uploads + 1);
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        let shares: Vec<f32> = (0..sw * sh).map(|i| share((i % sw) as u32, (i / sw) as u32) as f32 / 255.0).collect();
        let worst = compare(&out, &blur_reference::weighted(&small, &shares, sigma, taps));
        assert!(worst <= 2.0, "with weights {worst} levels from the reference");
        // the same data again goes up no more, a changed one does, and leaving it out goes back to the plain blur
        slot.set_textures(&gpu, &[("blur_weights".into(), weights.clone())]).unwrap();
        assert_eq!(crate::transfers::totals().mask_up, uploads + 1, "the same picture isn't sent again");
        slot.set_textures(&gpu, &[("blur_weights".into(), weights_frame(sw as u32, sh as u32, |_, _| 255))]).unwrap();
        assert_eq!(crate::transfers::totals().mask_up, uploads + 2);
        slot.set_textures(&gpu, &[]).unwrap();
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        assert!(compare(&out, &blur_reference::gauss(&small, sigma, taps)) <= 1.5);
    }

    #[test]
    fn weights_a_step_declares_count_like_the_ones_its_cpu_half_gives() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h, shrink, sigma) = (192usize, 128usize, 8usize, 2.9f64);
        let frame = noisy_rgba(w, h);
        let (sw, sh) = (w / shrink, h / shrink);
        let share = |x: u32, y: u32| if (4..12).contains(&x) && (3..11).contains(&y) { 0 } else { 255 };
        let declared = ShaderStep { keeps_geometry: false, wgsl: BLURRED_WEIGHTED.into(), uniforms: json!({"blur_shrink": shrink, "blur_sigma": sigma}), textures: vec![("blur_weights".into(), weights_frame(sw as u32, sh as u32, share))] };
        let out = run(&gpu, &frame, &mut [ShaderSlot::new(declared)]);
        let shares: Vec<f32> = (0..sw * sh).map(|i| share((i % sw) as u32, (i / sw) as u32) as f32 / 255.0).collect();
        let want = blur_reference::weighted(&blur_reference::shrunk(&frame.data, w, h, shrink), &shares, sigma, shader::blur_taps(sigma) as usize);
        let mut worst = 0f32;
        for y in 0..h {
            for x in 0..w {
                let at = blur_reference::read(&want, x, y, w, h);
                (0..3).for_each(|c| worst = worst.max((out.data[(y * w + x) * 4 + c] as f32 - at[c]).abs()));
            }
        }
        assert!(worst <= 2.0, "declared weights are {worst} levels from the reference");
    }

    /// The step texture a Background-like shader lays over its blur: a soft disc in the middle.
    fn disc(w: u32, h: u32) -> Frame {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let d = (((x as f32 - w as f32 / 2.0).powi(2) + (y as f32 - h as f32 / 2.0).powi(2)).sqrt() - h as f32 / 4.0) / 3.0;
                data.extend([(255.0 * (0.5 - d).clamp(0.0, 1.0)) as u8, 0, 0, 255]);
            }
        }
        Frame::new(w, h, PixelFormat::Rgba, data)
    }

    #[test]
    fn a_shader_keeps_its_pictures_on_the_card_and_not_the_textures_they_went_up_in() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let step = ShaderStep { keeps_geometry: false, wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }".into(), uniforms: json!({}), textures: vec![("overlay".into(), disc(64, 48)), ("dirt".into(), disc(32, 32))] };
        let built = build(&gpu, &step).expect("the shader builds");
        assert_eq!(built.kept_textures(), 2, "one texture per picture, no copy of the upload");
    }

    #[test]
    fn a_shader_after_a_plane_step_sees_the_planes_that_come_back() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h) = (160usize, 96usize);
        // colours well inside the range, so a trip through RGB and back has nothing to clip
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(70 + x * 90 / w + (x / 9 % 2) * 40) as u8, (80 + y * 80 / h + (y / 7 % 2) * 30) as u8, (130 + ((x + y) / 11 % 3) * 25) as u8]
        }).collect();
        let mut data = vec![0u8; PixelFormat::I420.size(w as u32, h as u32)];
        telescope_core::image::rgb_to_420(&rgb, w, h, &mut data, false);
        let frame = Frame::new(w as u32, h as u32, PixelFormat::I420, data);
        let like_background = || ShaderStep {
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(mix(blurred_weighted(uv).rgb, sample(uv).rgb, sample_texture(blur_cutout, uv).r), 1.0); }".into(),
            uniforms: json!({"blur_shrink": 8, "blur_sigma": 2.5, "blur_planes": 1}),
            textures: vec![("blur_cutout".into(), disc(w as u32, h as u32)), ("blur_weights".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]))],
        };
        let go = |more: &[&str]| {
            let mut held = gpu.lift_with(&frame, true).unwrap();
            let mut slots: Vec<ShaderSlot> = std::iter::once(ShaderSlot::new(like_background())).chain(more.iter().map(|w| ShaderSlot::new(step(w, json!({}))))).collect();
            for slot in &mut slots {
                slot.prepare(&gpu).unwrap();
                gpu.shade(&mut held, slot, 0.0, None).unwrap();
            }
            let out = gpu.land(&mut held).unwrap();
            gpu.put_down(held);
            out
        };
        let alone = go(&[]);
        // what the next shader is given is the picture that lands: the same colours as from those planes put on the card as they are
        let mut held = gpu.lift_with(&frame, true).unwrap();
        let mut slot = ShaderSlot::new(like_background());
        slot.prepare(&gpu).unwrap();
        gpu.shade(&mut held, &slot, 0.0, None).unwrap();
        let a = gpu.ensure_rgba(&mut held);
        let enc = std::mem::replace(&mut held.enc, gpu.device.create_command_encoder(&Default::default()));
        let next_sees = gpu.download(enc, &[&a], &[4]).unwrap();
        let landed = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        let mut again = gpu.lift_with(&landed, true).unwrap();
        let again_a = gpu.ensure_rgba(&mut again);
        let enc = std::mem::replace(&mut again.enc, gpu.device.create_command_encoder(&Default::default()));
        assert!(next_sees == gpu.download(enc, &[&again_a], &[4]).unwrap(), "the next shader is given a picture other than the one that comes back");
        gpu.put_down(again);
        assert_eq!(landed.data, alone.data);
        let then_identity = go(&[IDENTITY]);
        // an identity shader reads the planes' colours and writes them as planes again: a level here and there from the rounding, no more
        let worst = alone.data.iter().zip(then_identity.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 2, "an identity shader after the step changed the picture by {worst} levels");
    }

    #[test]
    fn plane_compose_skips_the_dead_draw_unless_history_needs_it() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h) = (160usize, 96usize);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(70 + x * 90 / w + (x / 9 % 2) * 40) as u8, (80 + y * 80 / h + (y / 7 % 2) * 30) as u8, (130 + ((x + y) / 11 % 3) * 25) as u8]
        }).collect();
        let mut data = vec![0u8; PixelFormat::I420.size(w as u32, h as u32)];
        telescope_core::image::rgb_to_420(&rgb, w, h, &mut data, false);
        let i420 = Frame::new(w as u32, h as u32, PixelFormat::I420, data);
        let cutout = disc(w as u32, h as u32);
        let weights = weights_frame((w / 8) as u32, (h / 8) as u32, |x, y| if (6..14).contains(&x) && (3..9).contains(&y) { 0 } else { 255 });
        let placeholder = Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]);
        let make = |wgsl: &str| ShaderSlot::new(ShaderStep {
            keeps_geometry: false,
            wgsl: wgsl.into(),
            uniforms: json!({"blur_shrink": 8, "blur_sigma": 2.5, "blur_planes": 1}),
            textures: vec![("blur_cutout".into(), placeholder.clone()), ("blur_weights".into(), placeholder.clone())],
        });
        let bg_wgsl = "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(mix(blurred_weighted(uv).rgb, sample(uv).rgb, sample_texture(blur_cutout, uv).r), 1.0); }";
        let bg_with_history = "fn effect(uv: vec2<f32>) -> vec4<f32> { if has_previous() { return previous(uv); } return vec4<f32>(mix(blurred_weighted(uv).rgb, sample(uv).rgb, sample_texture(blur_cutout, uv).r), 1.0); }";

        let shade_and_capture = |slot: &mut ShaderSlot, frame: &Frame| -> (Frame, Vec<u8>) {
            slot.prepare(&gpu).unwrap();
            slot.set_textures(&gpu, &[("blur_cutout".into(), cutout.clone()), ("blur_weights".into(), weights.clone())]).unwrap();
            let mut held = gpu.lift_with(frame, true).unwrap();
            gpu.shade(&mut held, slot, 0.0, None).unwrap();
            let a = gpu.ensure_rgba(&mut held);
            let enc = std::mem::replace(&mut held.enc, gpu.device.create_command_encoder(&Default::default()));
            let rgba = gpu.download(enc, &[&a], &[4]).unwrap();
            let landed = gpu.land(&mut held).unwrap();
            gpu.put_down(held);
            (landed, rgba)
        };

        let mut plain = make(bg_wgsl);
        let (plain_landed, plain_rgba) = shade_and_capture(&mut plain, &i420);
        assert_eq!(plain.draws.load(Ordering::Relaxed), 0, "I420 compose overwrites held.b, so slot.draw is skipped");
        assert!(plain.history.lock().unwrap().is_none());
        assert_ne!(plain_landed.data, i420.data);

        let mut with_history = make(bg_with_history);
        let (history_landed, history_rgba) = shade_and_capture(&mut with_history, &i420);
        assert_eq!(with_history.draws.load(Ordering::Relaxed), 1, "a step that keeps history still needs slot.draw");
        assert!(with_history.history.lock().unwrap().is_some());
        assert_eq!(plain_landed.data, history_landed.data, "skipping slot.draw keeps the landed I420 planes identical");
        assert_eq!(plain_rgba, history_rgba, "skipping slot.draw keeps the RGBA picture for later shaders identical");

        // On an RGBA frame compose doesn't run, so the same slot still draws
        let rgba_frame = noisy_rgba(w, h);
        let _ = shade_and_capture(&mut plain, &rgba_frame);
        assert_eq!(plain.draws.load(Ordering::Relaxed), 1, "non-I420 frames still run slot.draw");
    }

    #[test]
    fn blur_masks_use_r8_and_mask_the_right_pixels() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let (w, h) = (160u32, 96u32);
        let (sw, sh) = (w / 8, h / 8);
        fn mask_frame(mw: u32, mh: u32, keep: impl Fn(u32) -> bool) -> Frame {
            let mut data = Vec::with_capacity((mw * mh * 4) as usize);
            for _ in 0..mh {
                for x in 0..mw {
                    // Non-zero G/B where red is 0 so only the red channel can drive the mask
                    data.extend(if keep(x) { [255, 0, 0, 255] } else { [0, 255, 255, 255] });
                }
            }
            Frame::new(mw, mh, PixelFormat::Rgba, data)
        }
        let left_cutout = mask_frame(w, h, |x| x < w / 2);
        let right_cutout = mask_frame(w, h, |x| x >= w / 2);
        let zero_cutout = mask_frame(w, h, |_| false);
        let all_weights = mask_frame(sw, sh, |_| true);
        let right_weights = mask_frame(sw, sh, |x| x >= sw / 2);

        let mut slot = ShaderSlot::new(ShaderStep {
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(mix(blurred_weighted(uv).rgb, sample(uv).rgb, sample_texture(blur_cutout, uv).r), 1.0); }".into(),
            uniforms: json!({"blur_shrink": 8, "blur_sigma": 2.5, "blur_planes": 1}),
            textures: vec![("blur_cutout".into(), left_cutout), ("blur_weights".into(), all_weights.clone())],
        });
        slot.prepare(&gpu).unwrap();
        let built = slot.built.as_ref().unwrap().as_ref().unwrap();
        assert!(built.first_views.iter().all(|v| v.texture().format() == wgpu::TextureFormat::R8Unorm));

        let run_i420 = |slot: &ShaderSlot, frame: &Frame| -> Frame {
            let mut held = gpu.lift_with(frame, true).unwrap();
            gpu.shade(&mut held, slot, 0.0, None).unwrap();
            let out = gpu.land(&mut held).unwrap();
            gpu.put_down(held);
            out
        };

        // 4-pixel stripes average to 130 in every 8x8 blur block, so kept pixels stay 220/40 and masked ones blur to ~130
        let striped = yuv_frame(w, h, |x, _| if (x / 4) % 2 == 0 { 220 } else { 40 }, 128, 128);
        let out = run_i420(&slot, &striped);
        let y_at = |f: &Frame, x: u32, y: u32| f.data[(y * w + x) as usize];
        for y in 0..h {
            for x in 0..w / 2 - 2 {
                assert_eq!(y_at(&out, x, y), y_at(&striped, x, y), "initial R8 cutout should keep left pixel ({x}, {y})");
            }
            for x in w / 2 + 2..w {
                assert!(y_at(&out, x, y).abs_diff(130) <= 3, "initial R8 cutout should blur right pixel ({x}, {y}): got {}", y_at(&out, x, y));
            }
        }

        // Flip the cutout via set_textures so the picture_texture R8 upload path is checked too
        slot.set_textures(&gpu, &[("blur_cutout".into(), right_cutout), ("blur_weights".into(), all_weights.clone())]).unwrap();
        let built = slot.built.as_ref().unwrap().as_ref().unwrap();
        assert!(built.views.iter().all(|v| v.texture().format() == wgpu::TextureFormat::R8Unorm));
        let flipped = run_i420(&slot, &striped);
        for y in 0..h {
            for x in 0..w / 2 - 2 {
                assert!(y_at(&flipped, x, y).abs_diff(130) <= 3, "replaced R8 cutout should blur left pixel ({x}, {y}): got {}", y_at(&flipped, x, y));
            }
            for x in w / 2 + 2..w {
                assert_eq!(y_at(&flipped, x, y), y_at(&striped, x, y), "replaced R8 cutout should keep right pixel ({x}, {y})");
            }
        }

        // Check blur_weights R8 masking: excluding the bright left half leaves the blur just left of the seam dark
        let split = yuv_frame(w, h, |x, _| if x < w / 2 { 220 } else { 40 }, 128, 128);
        slot.set_textures(&gpu, &[("blur_cutout".into(), zero_cutout.clone()), ("blur_weights".into(), all_weights)]).unwrap();
        let plain_blur = run_i420(&slot, &split);
        slot.set_textures(&gpu, &[("blur_cutout".into(), zero_cutout), ("blur_weights".into(), right_weights)]).unwrap();
        let weighted_blur = run_i420(&slot, &split);
        let seam_x = w / 2 - 4;
        assert!(y_at(&plain_blur, seam_x, h / 2) >= 120, "unweighted blur near seam should mix both halves: {}", y_at(&plain_blur, seam_x, h / 2));
        assert!(y_at(&weighted_blur, seam_x, h / 2) <= 50, "R8 blur_weights should exclude the bright left half: {}", y_at(&weighted_blur, seam_x, h / 2));
    }

    #[test]
    fn bad_upload_fault_fires_on_a_one_by_one_r8_mask() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let step = ShaderStep {
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(sample_texture(blur_cutout, uv).r, 0.0, 0.0, 1.0); }".into(),
            uniforms: json!({}),
            textures: vec![("blur_cutout".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]))],
        };
        let mut slot = ShaderSlot::new(step);
        slot.prepare(&gpu).unwrap();
        gpu.inject(Fault::BadUpload);
        let one_by_one = Frame::new(1, 1, PixelFormat::Rgba, vec![255, 0, 0, 255]);
        let why = slot.set_textures(&gpu, &[("blur_cutout".into(), one_by_one.clone())]).unwrap_err();
        assert!(why.contains("refused"), "{why}");
        slot.set_textures(&gpu, &[("blur_cutout".into(), one_by_one)]).unwrap();
        let built = slot.built.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(built.views[0].texture().format(), wgpu::TextureFormat::R8Unorm);
    }

    #[test]
    fn a_lost_card_takes_no_more_work_and_a_stream_after_it_gets_a_new_one() {
        let name = "gpu::tests::lost_card_in_a_process_of_its_own";
        let run = std::process::Command::new(std::env::current_exe().unwrap()).args(["--ignored", "--exact", name, "--nocapture"]).output().unwrap();
        assert!(run.status.success(), "{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    }

    #[test]
    #[ignore = "a_lost_card_takes_no_more_work_and_a_stream_after_it_gets_a_new_one runs this"]
    fn lost_card_in_a_process_of_its_own() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let frame = yuv_frame(8, 8, |x, y| (x * 7 + y * 3) as u8 + 16, 100, 150);
        let target = gpu.picture(8, 8);
        let mut slot = ShaderSlot::new(step(IDENTITY, json!({})));
        slot.prepare(&gpu).unwrap();
        gpu.lost.store(true, Ordering::SeqCst);
        // nothing is allocated, uploaded or drawn once it's lost
        let pooled = gpu.kept_bytes();
        assert!(gpu.lift(&frame).is_err() && gpu.rgba(&frame, 4, 4).is_err() && gpu.paint(&frame, &target).is_err() && gpu.trial(&frame, &|| false).is_err());
        assert!(ShaderSlot::new(step(IDENTITY, json!({}))).prepare(&gpu).is_err());
        assert_eq!(gpu.kept_bytes(), pooled);
        let fresh = Gpu::shared().expect("a card again");
        assert!(!Arc::ptr_eq(&fresh, &gpu) && !fresh.is_lost(), "a stream that starts now gets a new device");
        assert!(Arc::ptr_eq(&Gpu::shared().unwrap(), &fresh), "and so does the next one, with no more opening");
        run(&fresh, &frame, &mut [ShaderSlot::new(step(IDENTITY, json!({})))]);
    }

    #[test]
    fn a_wait_for_the_card_that_is_stopped_gives_up_at_once() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut held = gpu.lift(&yuv_frame(8, 8, |x, y| (x * 7 + y * 3) as u8 + 16, 100, 150)).unwrap();
        let began = std::time::Instant::now();
        assert!(gpu.land_until(&mut held, &|| true).unwrap_err().contains("stopped"));
        assert!(began.elapsed() < std::time::Duration::from_secs(2));
        assert!(!gpu.is_lost(), "a stop isn't the card failing");
        gpu.put_down(held);
    }

    #[test]
    fn a_picture_the_cpu_half_gives_is_there_to_sample_by_its_name() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let step = ShaderStep {
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(sample_texture(soft, uv).r, 0.0, 0.0, 1.0); }".into(),
            uniforms: json!({}),
            textures: vec![("soft".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]))],
        };
        let mut slot = ShaderSlot::new(step);
        slot.prepare(&gpu).unwrap();
        let frame = yuv_frame(16, 8, |_, _| 100, 128, 128);
        assert_eq!(run(&gpu, &frame, std::slice::from_mut(&mut slot)).data[0], 16, "the picture it started with, which is black");
        // two columns wide: the left black and the right red, read with linear filtering
        slot.set_textures(&gpu, &[("soft".into(), Frame::new(2, 1, PixelFormat::Rgba, vec![0, 0, 0, 255, 255, 0, 0, 255]))]).unwrap();
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        let luma = |x: usize| out.data[x] as i32;
        assert!(luma(0) <= 17 && luma(15) >= 60, "{} {}", luma(0), luma(15));
    }

    #[test]
    fn a_picture_the_card_refuses_is_an_error_and_is_tried_again_not_taken_for_the_one_before() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let step = ShaderStep {
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(sample_texture(soft, uv).r, 0.0, 0.0, 1.0); }".into(),
            uniforms: json!({}),
            textures: vec![("soft".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]))],
        };
        let mut slot = ShaderSlot::new(step);
        slot.prepare(&gpu).unwrap();
        let red = Frame::new(1, 1, PixelFormat::Rgba, vec![255, 0, 0, 255]);
        slot.set_textures(&gpu, &[("soft".into(), Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]))]).unwrap();
        gpu.inject(Fault::BadUpload);
        let why = slot.set_textures(&gpu, &[("soft".into(), red.clone())]).unwrap_err();
        assert!(why.contains("refused"), "{why}");
        // The same data again is still to be sent, and then the shader draws it
        slot.set_textures(&gpu, &[("soft".into(), red)]).unwrap();
        let out = run(&gpu, &yuv_frame(16, 8, |_, _| 100, 128, 128), std::slice::from_mut(&mut slot));
        assert!(out.data[0] > 60, "{}", out.data[0]);
    }

    #[test]
    fn a_step_shaded_without_history_has_no_previous_frame_and_the_next_one_goes_on_from_it() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        // One time for every picture, so a slow card between them can't age the history out
        let frame = || { let mut f = Frame::new(8, 4, PixelFormat::Rgba, vec![0; 8 * 4 * 4]); f.time_ns = 1; f };
        let mut slot = ShaderSlot::new(step(COUNTS, json!({})));
        slot.prepare(&gpu).unwrap();
        assert_eq!(run(&gpu, &frame(), std::slice::from_mut(&mut slot)).data[0], 51);
        assert_eq!(run(&gpu, &frame(), std::slice::from_mut(&mut slot)).data[0], 102);
        let mut held = gpu.lift(&frame()).unwrap();
        gpu.shade_with(&mut held, &slot, 0.0, None, false).unwrap();
        let out = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        assert_eq!(out.data[0], 51, "previous() had nothing");
        assert_eq!(run(&gpu, &frame(), std::slice::from_mut(&mut slot)).data[0], 102, "and what it drew then is the history now");
    }

    #[test]
    fn a_trial_says_how_long_the_card_took() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        assert!(gpu.trial(&yuv_frame(64, 36, |_, _| 100, 128, 128), &|| false).unwrap() > 0);
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
            keeps_geometry: false,
            wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample_texture(logo, uv); }".into(),
            uniforms: json!({}),
            textures: vec![("logo".into(), logo)],
        };
        let out = run(&gpu, &yuv_frame(4, 4, |_, _| 16, 128, 128), &mut [ShaderSlot::new(s)]);
        assert!((out.data[0] as i32 - 235).abs() <= 2, "{}", out.data[0]);
    }

    /// Each pass sees the ones before it at the size it was given, an f32 pass keeps numbers past 8 bits exactly, and effect() reads them all.
    const PASSES: &str = "
fn pass_0(at: vec2<i32>) -> vec4<f32> { return vec4<f32>(sample((vec2<f32>(at) + 0.5) / frame.size).rgb, 1.0); }
fn pass_1(at: vec2<i32>) -> f32 {
    var total = 0.0;
    for (var x = 0; x < passed_size_0().x; x++) { total += round(passed_0(vec2<i32>(x, at.y)).r * 255.0); }
    return total * 1000.0 + f32(at.x);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let at = vec2<i32>(floor(uv * frame.size));
    let n = passed_1(vec2<i32>(at.x, 0));
    return vec4<f32>(floor(n / 1000.0) / 2550.0, (n % 1000.0) / 255.0, f32(passed_size_1().x) / 255.0, 1.0);
}";

    #[test]
    fn a_shader_with_passes_reads_each_pass_at_its_own_size() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let kinds = shader::passes(PASSES);
        assert_eq!(kinds, [shader::PassKind::Colour, shader::PassKind::Number]);
        let (w, h) = (16u32, 4u32);
        let picture: Vec<u8> = (0..w * h).flat_map(|i| [((i % w) * 15) as u8, 0, 0, 255]).collect();
        let frame = Frame::new(w, h, PixelFormat::Rgba, picture);
        // pass 0 is the picture at its own size, pass 1 one row of five
        let mut slot = ShaderSlot::new(step(PASSES, json!({"pass_sizes": [[16, 4], [5, 1]]})));
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        let sum: u32 = (0..w).map(|x| x * 15).sum();
        for x in 0..w as usize {
            let px = &out.data[x * 4..x * 4 + 3];
            // the sum was 1800, past what 8 bits hold, and came through whole; the last of five columns repeats past the edge
            assert_eq!(px[0] as u32, (sum as f32 / 10.0).round() as u32, "column {x}: {px:?}");
            assert_eq!(px[1] as usize, x.min(4), "column {x}: {px:?}");
            assert_eq!(px[2], 5, "column {x}: {px:?}");
        }
        // new sizes come with the uniforms
        slot.set_uniforms(&gpu, json!({"pass_sizes": [[16, 4], [9, 1]]}));
        let out = run(&gpu, &frame, std::slice::from_mut(&mut slot));
        assert_eq!(out.data[2], 9);
        assert_eq!(out.data[15 * 4 + 1], 8);
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
            for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
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
                let slot = &mut ShaderSlot::new(ShaderStep { keeps_geometry: false, wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }".into(), uniforms: serde_json::json!({}), textures: vec![] });
                slot.prepare(&gpu).unwrap();
                gpu.shade(&mut held, slot, 0.0, None).unwrap();
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
