//! The preview card's picture: the focused stream's newest frame, drawn into a texture on the window's own graphics
//! card when Slint renders with wgpu (the painter comes from the app's wiring, so core never names wgpu), or scaled on
//! the CPU otherwise.

pub use slint::wgpu_30::wgpu;
use std::sync::{Arc, OnceLock};
use telescope_core::gpu_marker::{self, Card};
use telescope_core::gpu_pick::{self, Api, Candidate, Kind};
use telescope_core::media::{PipelineFrame, Scaler};
use telescope_core::preview::TintSpec;
use telescope_core::Frame;

/// Draws frames for the preview at the size asked for.
pub trait Painter {
    /// A picture that may still be on the card, drawn once it is in memory: None before, since the window's thread never waits for the card.
    fn paint_picture(&mut self, frame: &PipelineFrame, width: u32, height: u32) -> Option<slint::Image> {
        self.paint(&frame.landed()?, width, height)
    }

    /// The picture, or None when it can't be shown (yet).
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image>;

    /// A finished picture is waiting to be taken in, so the window paints again whatever the cadence says.
    fn ready(&self) -> bool {
        false
    }

    /// Takes in a finished picture of this size without asking for any new one (the paint cadence says no new job yet).
    fn paint_ready(&mut self, _width: u32, _height: u32) -> Option<slint::Image> {
        None
    }

    /// A different stream: what was made for the last one isn't shown.
    fn restart(&mut self) {}

    /// Whether this painter can draw the Cinema tints itself on the card (and hand back a small copy for the scopes), so the next
    /// paints need no CPU copy of the picture.
    fn on_card(&self) -> bool {
        false
    }

    /// The card this painter draws on was lost: it can't paint anything again, and the window swaps in one that doesn't use a card.
    fn lost(&self) -> bool {
        false
    }

    /// What the next paint puts on the picture, and whether it also reads a small copy of the untinted picture back for the scopes.
    /// Only a painter that is `on_card` takes any notice.
    fn set_tint(&mut self, _spec: Option<TintSpec>, _scope: bool) {}

    /// The small copy of the last paint's picture that `set_tint` asked for, as RGBA.
    fn take_scope(&mut self) -> Option<Frame> {
        None
    }

    /// What the frames of the next paints are derived from besides their own data (Colour Compare's line and its before picture),
    /// so a frame made again from the same source is still the same job.
    fn derived_from(&mut self, _tag: u64) {}
}

/// Draws frames into textures on the device it was made on (media's scaler, through the app's wiring).
pub trait TexturePainter {
    /// As Painter::paint_picture: drawn once it is in memory, None before.
    fn paint_picture(&mut self, frame: &PipelineFrame, width: u32, height: u32, tint: Option<&TintSpec>) -> Option<wgpu::Texture> {
        self.paint(&frame.landed()?, width, height, tint)
    }

    /// The picture in a texture, with the tools of `tint` on it when there is one.
    fn paint(&mut self, frame: &Frame, width: u32, height: u32, tint: Option<&TintSpec>) -> Option<wgpu::Texture>;

    /// Whether the card was lost: nothing can be painted on it again.
    fn lost(&self) -> bool {
        false
    }

    /// Whether the tints and the scopes' copies are still the card's to make: not after paints it refused in a row, or a card too busy to
    /// bring a copy back in time. They are the CPU's from then on.
    fn on_card(&self) -> bool {
        true
    }

    /// Whether the next paints also start a small copy of the picture before any tint, for `take_scope`.
    fn scope(&mut self, _wanted: bool) {}

    /// The small copy that was started, when it has come back. Never waits for it: None until then.
    fn take_scope(&mut self) -> Option<Frame> {
        None
    }

    /// A different stream: a copy started for the last one is let go of.
    fn restart(&mut self) {}
}

/// Makes a texture painter on the device and queue Slint renders with.
pub type GpuPainterFactory = Arc<dyn Fn(wgpu::Device, wgpu::Queue) -> Box<dyn TexturePainter> + Send + Sync>;

/// A texture painter's textures, handed to Slint as they are.
pub struct GpuPainter {
    painter: Box<dyn TexturePainter>,
    tint: Option<TintSpec>,
}

impl GpuPainter {
    pub fn new(painter: Box<dyn TexturePainter>) -> GpuPainter {
        GpuPainter { painter, tint: None }
    }
}

impl Painter for GpuPainter {
    fn paint_picture(&mut self, frame: &PipelineFrame, width: u32, height: u32) -> Option<slint::Image> {
        let tint = self.tint.take();
        slint::Image::try_from(self.painter.paint_picture(frame, width, height, tint.as_ref())?).map_err(|e| log::warn!("Slint didn't take the preview's texture: {e}")).ok()
    }

    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        // Set again before each paint, so a tint that was switched off isn't painted again
        let tint = self.tint.take();
        slint::Image::try_from(self.painter.paint(frame, width, height, tint.as_ref())?).map_err(|e| log::warn!("Slint didn't take the preview's texture: {e}")).ok()
    }

    fn on_card(&self) -> bool {
        !self.painter.lost() && self.painter.on_card()
    }

    fn lost(&self) -> bool {
        self.painter.lost()
    }

    fn set_tint(&mut self, spec: Option<TintSpec>, scope: bool) {
        self.tint = spec;
        self.painter.scope(scope);
    }

    fn take_scope(&mut self) -> Option<Frame> {
        self.painter.take_scope()
    }

    fn restart(&mut self) {
        self.tint = None;
        self.painter.scope(false);
        self.painter.restart();
    }
}

/// Swaps a painter whose card was lost for one that doesn't use a card (made by `make`), so the window goes on painting: left in place it would
/// refuse every frame while the passes had already moved to the CPU's copy. What it had been told to tint with goes with it. True when swapped.
pub(crate) fn replace_lost(painter: &mut Option<Box<dyn Painter>>, make: impl FnOnce() -> Option<Box<dyn Painter>>) -> bool {
    if !painter.as_ref().is_some_and(|p| p.lost()) {
        return false;
    }
    match make() {
        Some(fresh) => {
            log::warn!("The graphics card was lost, so the preview is painted on the CPU now");
            *painter = Some(fresh);
            true
        }
        None => false,
    }
}

static GPU_PAINTER: OnceLock<GpuPainterFactory> = OnceLock::new();

/// The binary hands over how to paint on the window's card before the window opens.
pub fn use_gpu_painter(factory: GpuPainterFactory) {
    let _ = GPU_PAINTER.set(factory);
}

/// The device features that let the stream's decoded pictures stay on the card (the same as media's `surface_features`, since the window's device is the one the streams use).
fn surface_features(adapter: &wgpu::Adapter) -> wgpu::Features {
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

/// The adapter as the crash marker and the skip list name it: the driver version is part of it (borrowed from the same card on another API
/// when its own driver says none), so an updated driver is tried again.
fn card_of(info: &wgpu::AdapterInfo, all: &[gpu_pick::Info]) -> Card {
    let own = gpu_pick::Info { vendor: info.vendor, device: info.device, driver: info.driver.clone(), driver_info: info.driver_info.clone() };
    Card::new(&info.name, &format!("{:?}", info.backend), &gpu_pick::driver_text(&own, all)).with_bus(&info.device_pci_bus_id)
}

fn infos_of(adapters: &[wgpu::Adapter]) -> Vec<gpu_pick::Info> {
    adapters.iter().map(|a| { let i = a.get_info(); gpu_pick::Info { vendor: i.vendor, device: i.device, driver: i.driver, driver_info: i.driver_info } }).collect()
}

/// The adapters to try for the window, best first. WGPU_POWER_PREF set: wgpu's own pick for it. Otherwise real GPUs first, DX12 before
/// Vulkan on Windows and Vulkan first elsewhere (`gpu_pick::order`), and software or odd adapters last; wgpu's no-preference request
/// would take whatever is listed first, software adapters included.
fn adapters_to_try(instance: &wgpu::Instance, preference: Option<wgpu::PowerPreference>) -> Vec<(wgpu::Adapter, Card)> {
    let listed = gpu_marker::marked_while(&gpu_marker::probe_card(), || block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY)));
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
    let mut by_choice = gpu_pick::with_choice(gpu_pick::order(&candidates, gpu_pick::system_preference(), gpu_pick::Role::Window), &candidates, &choice);
    if !wanted.is_empty() {
        log::info!("Graphics card chosen in Settings: {}", names[wanted[0]]);
        if let Some(power_preference) = preference {
            log::info!("WGPU_POWER_PREF is ignored, the graphics card chosen in Settings goes first ({power_preference:?})");
        }
    } else if let Some(power_preference) = preference {
        // The environment's preference still goes through the skip list below, and wgpu's pick only moves to the front
        let picked = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference, ..Default::default() }));
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
        log::info!("Window graphics card picked: {}; also listed: {}", name(first), ordered.iter().skip(1).map(name).collect::<Vec<_>>().join(", "));
    }
    ordered
}

/// The card's limits, but with a small binding count: DX12 sizes its shader-visible descriptor heap from it.
fn window_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let mut limits = adapter.limits();
    limits.max_non_sampler_bindings = limits.max_non_sampler_bindings.min(16384);
    limits
}

/// Looks for a graphics card the window can draw on and opens it. `select` gets the configuration to hand Slint, and says whether it took.
fn open_wgpu(select: &mut dyn FnMut(slint::wgpu_30::WGPUConfiguration) -> Result<(), String>) -> Result<(wgpu::Device, wgpu::Queue), String> {
    (|| {
        // Not GL: looking for it loads Mesa's big GL libraries, about 25 MB, and a GL adapter found here can't present to the window
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        // Loading the drivers can crash too, so the card isn't the only thing the marker covers
        let instance = gpu_marker::marked_while(&gpu_marker::probe_card(), || wgpu::Instance::new(desc.with_env()));
        let mut why = String::from("no graphics card found");
        for (adapter, card) in adapters_to_try(&instance, wgpu::PowerPreference::from_env()) {
            let info = adapter.get_info();
            // WGPU_BACKEND can still pick it
            if info.backend == wgpu::Backend::Gl {
                why = "GL can't draw this window".to_string();
                continue;
            }
            let tried = (|| {
                gpu_marker::mark(&card);
                let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("telescope window"),
            required_features: surface_features(&adapter),
                    required_limits: window_limits(&adapter),
                    memory_hints: wgpu::MemoryHints::Manual { suballocated_device_memory_block_size: (4 << 20)..(8 << 20) },
                    ..Default::default()
                })).map_err(|e| e.to_string())?;
                let config = slint::wgpu_30::WGPUConfiguration::Manual { instance: instance.clone(), adapter: adapter.clone(), device: device.clone(), queue: queue.clone() };
                select(config)?;
                Ok::<_, String>((device, queue))
            })();
            match tried {
                Ok(opened) => {
                    gpu_pick::set_opened(&info.name, &gpu_pick::card_id(info.vendor, info.device), if info.backend == wgpu::Backend::Dx12 { Api::Dx12 } else { Api::Vulkan });
                    return Ok(opened);
                }
                Err(e) => {
                    log::warn!("Graphics card {} ({:?}) can't draw the window: {e}", info.name, info.backend);
                    why = e;
                }
            }
        }
        // Nothing enumerated worked: the plain high performance request, as before 5.1.1
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).map_err(|e| format!("{why}; {e}"))?;
        if adapter.get_info().backend == wgpu::Backend::Gl {
            return Err("GL can't draw this window".to_string());
        }
        let card = card_of(&adapter.get_info(), &infos_of(&block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY))));
        if gpu_marker::is_skipped(&card) {
            return Err(format!("{why}; the only card left is skipped"));
        }
        gpu_marker::mark(&card);
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("telescope window"),
                    required_features: surface_features(&adapter),
            required_limits: window_limits(&adapter),
            memory_hints: wgpu::MemoryHints::Manual { suballocated_device_memory_block_size: (4 << 20)..(8 << 20) },
            ..Default::default()
        })).map_err(|e| e.to_string())?;
        let config = slint::wgpu_30::WGPUConfiguration::Manual { instance, adapter, device: device.clone(), queue: queue.clone() };
        select(config)?;
        Ok::<_, String>((device, queue))
    })()
}

/// Whether a graphics card is looked for at all: a painter to share it with, no TELESCOPE_NO_GPU and no crash on the last start.
fn wgpu_wanted() -> Option<&'static GpuPainterFactory> {
    let factory = GPU_PAINTER.get()?;
    if std::env::var_os("TELESCOPE_NO_GPU").is_some() {
        return None;
    }
    if gpu_marker::probe_failed() {
        log::warn!("The last start closed while looking for graphics cards, so none is looked for now");
        return None;
    }
    Some(factory)
}

/// The painter on the card that was opened, or None (logged) when none was.
fn painter_on(factory: &GpuPainterFactory, opened: Result<(wgpu::Device, wgpu::Queue), String>) -> Option<Box<dyn Painter>> {
    match opened {
        Ok((device, queue)) => {
            log::info!("The preview paints on the window's graphics card");
            Some(Box::new(GpuPainter::new(factory(device, queue))))
        }
        Err(e) => {
            log::warn!("The window can't render with wgpu, so the preview is scaled on the CPU: {e}");
            None
        }
    }
}

/// Opens a graphics card for Slint's wgpu renderer when there's a GPU painter to share it with, and makes the painter on
/// the same device: every window (the pop-out too) then renders with it, so their textures go anywhere. None keeps the
/// default renderer (no painter, TELESCOPE_NO_GPU set, or no card wgpu can use).
pub(crate) fn select_wgpu_backend() -> Option<Box<dyn Painter>> {
    let factory = wgpu_wanted()?;
    let opened = open_wgpu(&mut |config| crate::parent::selector().require_wgpu_30(config).select().map_err(|e| e.to_string()));
    painter_on(factory, opened)
}

/// A graphics card being opened on a thread of its own, while the window's document compiles.
pub(crate) struct EarlyCard(std::thread::JoinHandle<Result<(wgpu::Device, wgpu::Queue, Option<slint::wgpu_30::WGPUConfiguration>), String>>);

/// Starts opening the card (None when there is nothing to open). Only the card is opened here: Slint is told about it by `finish`, on the window's thread.
pub(crate) fn begin_wgpu_backend() -> Option<EarlyCard> {
    // Only Linux, where it was measured: the other systems' drivers may want the thread they were first used on
    if !cfg!(target_os = "linux") {
        return None;
    }
    wgpu_wanted()?;
    let spawned = std::thread::Builder::new().name("graphics card".into()).spawn(|| {
        let mut config = None;
        let (device, queue) = open_wgpu(&mut |c| { config = Some(c); Ok(()) })?;
        Ok((device, queue, config))
    });
    spawned.map_err(|e| log::warn!("The graphics card couldn't be opened on a thread: {e}")).ok().map(EarlyCard)
}

/// What `select_wgpu_backend` gives, for a card opened by `begin_wgpu_backend`. Has to run before the first window is made.
pub(crate) fn finish_wgpu_backend(early: Option<EarlyCard>) -> Option<Box<dyn Painter>> {
    let Some(EarlyCard(thread)) = early else { return select_wgpu_backend() };
    let Ok(done) = thread.join() else {
        log::warn!("Opening the graphics card failed");
        return None;
    };
    let factory = wgpu_wanted()?;
    match done {
        Ok((device, queue, Some(config))) => match crate::parent::selector().require_wgpu_30(config).select() {
            Ok(()) => painter_on(factory, Ok((device, queue))),
            // Slint refused the card: the blocking way goes through the other cards the way it always did
            Err(e) => {
                log::warn!("The window can't use the card that was opened: {e}");
                drop((device, queue));
                select_wgpu_backend()
            }
        },
        other => painter_on(factory, other.map(|(device, queue, _)| (device, queue))),
    }
}

/// wgpu's requests finish at once on desktop drivers; this waits for one without an async runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
        std::thread::yield_now();
    }
}

/// The CPU fallback: media's scaler to RGB, straight into the buffer Slint uploads from.
pub struct CpuPainter(pub Box<dyn Scaler>);

impl Painter for CpuPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        self.0.rgb_into(frame, width, height, buffer.make_mut_bytes()).then(|| slint::Image::from_rgb8(buffer))
    }
}

/// Which picture a threaded painter has: the frame's number, the size and what the frame is derived from.
type PaintKey = (u64, u32, u32, u64);

struct PaintJob {
    frame: Frame,
    key: PaintKey,
    epoch: u64,
}

type Made = (PaintKey, slint::SharedPixelBuffer<slint::Rgb8Pixel>);

/// The CPU fallback off the window's thread: the scaler (which may wait on a graphics card's readback) runs on a thread of its own, and the
/// window gets the picture when it's made (`wake` asks it to paint again). The newest picture made for the stream at the size asked for is
/// shown even when a newer frame has come since, so a scaler slower than the frames still puts pictures up at its own pace; a newer frame
/// replaces one still waiting to be made.
pub struct ThreadedPainter {
    scaler: Option<Box<dyn Scaler>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<crate::preview_passes::Worker<PaintJob>>,
    made: Arc<std::sync::Mutex<Option<Made>>>,
    asked: Option<PaintKey>,
    tag: u64,
    // Counts streams, so a picture still being made for the last one is dropped
    epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl ThreadedPainter {
    /// Wakes the window's own event loop when a picture is made.
    pub fn for_window(scaler: Box<dyn Scaler>) -> ThreadedPainter {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
            let _ = slint::invoke_from_event_loop(|| crate::window::with_window(|w| w.frame_ready()));
        });
        ThreadedPainter::new(scaler, wake)
    }

    /// The thread starts with the first frame to paint.
    pub fn new(scaler: Box<dyn Scaler>, wake: Arc<dyn Fn() + Send + Sync>) -> ThreadedPainter {
        ThreadedPainter { scaler: Some(scaler), wake, worker: None, made: Arc::default(), asked: None, tag: 0, epoch: Arc::default() }
    }

    fn ask(&mut self, frame: &Frame, key: PaintKey) {
        if self.worker.is_none() {
            let Some(mut scaler) = self.scaler.take() else { return };
            let (made, wake, epoch) = (self.made.clone(), self.wake.clone(), self.epoch.clone());
            self.worker = Some(crate::preview_passes::Worker::start("preview-paint", move |job: PaintJob| {
                let (width, height) = (job.key.1, job.key.2);
                let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
                if scaler.rgb_into(&job.frame, width, height, buffer.make_mut_bytes()) {
                    let mut slot = made.lock().unwrap_or_else(|e| e.into_inner());
                    if job.epoch == epoch.load(std::sync::atomic::Ordering::SeqCst) {
                        *slot = Some((job.key, buffer));
                        drop(slot);
                        wake();
                    }
                }
            }));
        }
        self.asked = Some(key);
        if let Some(worker) = &self.worker {
            worker.put(PaintJob { frame: frame.clone(), key, epoch: self.epoch.load(std::sync::atomic::Ordering::SeqCst) });
        }
    }
}

impl Drop for ThreadedPainter {
    fn drop(&mut self) {
        // The scaler may be holding a graphics card's readback: it ends before the painter's owner goes on
        if let Some(worker) = self.worker.take() {
            worker.finish();
        }
    }
}

impl Painter for ThreadedPainter {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        let key = (frame.number, width, height, self.tag);
        let ready = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            // A picture of another size is no use for this one
            made.take_if(|(k, _)| (k.1, k.2) != (width, height));
            made.take()
        };
        if let Some((made_key, buffer)) = ready {
            if made_key == key {
                // Painted again later (a repaint of the same frame), it's made again
                self.asked = None;
            } else if self.asked != Some(key) {
                // An older frame's picture goes up while the newest is made
                self.ask(frame, key);
            }
            return Some(slint::Image::from_rgb8(buffer));
        }
        if self.asked != Some(key) {
            self.ask(frame, key);
        }
        None
    }

    fn paint_ready(&mut self, width: u32, height: u32) -> Option<slint::Image> {
        let (key, buffer) = {
            let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
            made.take_if(|(k, _)| (k.1, k.2) != (width, height));
            made.take()?
        };
        // What was asked for has come, so painting that frame again makes it again
        if self.asked == Some(key) {
            self.asked = None;
        }
        Some(slint::Image::from_rgb8(buffer))
    }

    fn ready(&self) -> bool {
        self.made.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn restart(&mut self) {
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *made = None;
        self.asked = None;
    }

    fn derived_from(&mut self, tag: u64) {
        self.tag = tag;
    }
}

/// What a painter's `derived_from` is given: what the picture besides the frame's own data comes from (Colour Compare) and the slot's
/// count, since a stalled stream's frame replaced in place keeps its own number but is another picture.
pub fn paint_tag(derived: u64, revision: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (derived, revision).hash(&mut hasher);
    hasher.finish()
}

/// What one look at the preview does about its painter.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Nothing to paint now.
    Skip,
    /// Only a picture that has finished goes up; no new one is asked for.
    Take,
    /// Paint the frame, asking for its picture when it isn't made yet.
    Paint,
}

/// `due` is the paint cadence (or the same frame again), `ready` a finished picture waiting, `tints` a pass painting the picture itself.
pub fn step(due: bool, ready: bool, tints: bool) -> Step {
    match (due, ready) {
        (true, _) => Step::Paint,
        (false, false) => Step::Skip,
        // A tinting pass paints the picture itself, so a fallback picture waiting is of no use
        (false, true) if tints => Step::Skip,
        (false, true) => Step::Take,
    }
}

/// Whether a pass asked for the overlay to be drawn again and it hasn't been yet: the picture it goes on may land later than the ask.
#[derive(Default)]
pub struct OverlayAsk(std::cell::Cell<bool>);

impl OverlayAsk {
    /// A look at the preview: `asked` is a new ask since the last. Returns whether one is still waiting.
    pub fn look(&self, asked: bool) -> bool {
        if asked {
            self.0.set(true);
        }
        self.0.get()
    }

    /// The overlay was drawn.
    pub fn drawn(&self) {
        self.0.set(false);
    }
}

/// An RGBA picture width x height as the RGB image Slint shows, filled in one pass.
pub fn rgb_image(rgba: &[u8], width: u32, height: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    telescope_core::image::drop_alpha(rgba, width as usize, buffer.make_mut_bytes());
    slint::Image::from_rgb8(buffer)
}

/// _fit_within: the frame's size scaled down to fit inside the stage, keeping its shape, never up.
pub fn fit_within(frame: (u32, u32), stage: (u32, u32)) -> (u32, u32) {
    let (w, h) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let scale = (stage.0 as f64 / w).min(stage.1 as f64 / h).min(1.0);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// Where the picture lands on the stage, in px: fit_within without the cap, since a small frame is drawn bigger. Overlays are
/// painted at this size, so their lines and text stay the same on any stream (the rect PreviewLayer.paint gets).
pub fn shown_size(frame: (u32, u32), stage: (u32, u32)) -> (u32, u32) {
    let (w, h) = (frame.0.max(1) as f64, frame.1.max(1) as f64);
    let scale = (stage.0 as f64 / w).min(stage.1 as f64 / h);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// A hidden or minimised window has nobody looking at its picture.
pub fn window_seen(visible: bool, minimized: bool) -> bool {
    visible && !minimized
}

/// How long the window's timer waits when the window is up but nothing is going on (no stream, no message, no touch for a moment).
pub(crate) const IDLE_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// How often the window's timer looks around: a window nobody can see (hidden or minimised) only needs it now and then, since the
/// window's own events (input::watch) bring it back to the fast tick as soon as it's restored. One that is up with nothing going
/// on only needs it twice a second, and anything that changes that (a touch, a stream, a message, an event) speeds it up at once.
pub(crate) fn housekeeping_interval(window_seen: bool, popout_visible: bool, plugin_visible: bool, quiet: bool) -> std::time::Duration {
    if popout_visible || plugin_visible || (window_seen && !quiet) {
        std::time::Duration::from_millis(100)
    } else if window_seen {
        IDLE_TICK
    } else {
        std::time::Duration::from_secs(1)
    }
}

/// process_frame's gate: the pop-out or card must be in a window someone can see.
pub fn wants_frames(popout_seen: bool, card: bool, window_seen: bool) -> bool {
    popout_seen || (card && window_seen)
}

/// Which of a stream's new frames the window's card paints: every second one, counted in frames so the gaps stay even at any
/// rate, or all of them.
#[derive(Default)]
pub struct Cadence {
    seen: u64,
    skip_next: bool,
}

impl Cadence {
    /// Whether to paint the newest frame. `always` is for a frame that has to be shown whatever the count, or all of them.
    pub fn due(&mut self, number: u64, every_frame: bool, always: bool) -> bool {
        if every_frame || always {
            self.seen = number;
            self.skip_next = true;
            return true;
        }
        if number == self.seen {
            return false;
        }
        self.seen = number;
        self.skip_next = !self.skip_next;
        self.skip_next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frames as a timer that looks several times sees them: each new one a few times
    fn painted(cadence: &mut Cadence, frames: std::ops::Range<u64>, every_frame: bool) -> Vec<u64> {
        frames.filter(|&n| (0..3).map(|_| cadence.due(n, every_frame, false)).filter(|d| *d).count() > 0).collect()
    }

    #[test]
    fn the_card_paints_every_second_frame_however_often_it_looks() {
        let mut c = Cadence::default();
        assert!(c.due(1, false, true), "a new stream's first frame shows at once");
        assert_eq!(painted(&mut c, 2..10, false), [3, 5, 7, 9]);
    }

    #[test]
    fn numbers_needn_t_be_neighbours() {
        // Frame numbers are shared by all the streams
        let mut c = Cadence::default();
        let got: Vec<u64> = [10, 13, 14, 20, 21, 30].into_iter().filter(|&n| c.due(n, false, false)).collect();
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn smooth_paints_every_frame_and_going_back_keeps_the_gaps_even() {
        let mut c = Cadence::default();
        assert_eq!(painted(&mut c, 1..5, true), [1, 2, 3, 4]);
        assert_eq!(painted(&mut c, 5..9, false), [6, 8]);
    }

    #[test]
    fn a_frame_that_has_to_show_doesn_t_upset_the_count() {
        let mut c = Cadence::default();
        assert_eq!(painted(&mut c, 1..4, false), [1, 3]);
        assert!(c.due(4, false, true));
        assert_eq!(painted(&mut c, 5..8, false), [6]);
    }

    #[test]
    fn a_small_stream_in_a_big_window_is_shown_at_the_windows_size() {
        assert_eq!(shown_size((640, 360), (1280, 720)), (1280, 720));
        assert_eq!(shown_size((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(shown_size((640, 360), (0, 0)), (1, 1));
    }

    #[test]
    fn a_window_hidden_in_the_tray_paints_nothing_but_the_pop_out_goes_on() {
        assert!(wants_frames(false, true, true));
        assert!(!wants_frames(false, true, false), "nothing to see, so a pass's painted_at must not move");
        assert!(wants_frames(true, true, false));
        assert!(!wants_frames(false, false, true));
    }

    #[test]
    fn hidden_and_minimised_windows_do_not_need_frames() {
        for visible in [false, true] {
            for minimized in [false, true] {
                let seen = window_seen(visible, minimized);
                assert_eq!(seen, visible && !minimized);
                assert_eq!(wants_frames(seen, false, false), seen, "the pop-out alone");
                assert_eq!(wants_frames(false, true, seen), seen, "the card alone");
                assert!(wants_frames(seen, true, true), "a visible card still needs frames");
            }
        }
    }

    #[test]
    fn housekeeping_slows_when_every_window_is_hidden_and_when_nothing_is_going_on() {
        let ms = |d: std::time::Duration| d.as_millis();
        for (state, visible, minimized) in [("hidden", false, false), ("visible", true, false), ("minimised", true, true)] {
            let seen = window_seen(visible, minimized);
            for popout in [false, true] {
                for plugin in [false, true] {
                    for quiet in [false, true] {
                        let expected = match (seen, popout || plugin, quiet) {
                            (_, true, _) | (true, false, false) => 100,
                            (true, false, true) => 500,
                            (false, false, _) => 1000,
                        };
                        assert_eq!(ms(housekeeping_interval(seen, popout, plugin, quiet)), expected, "{state}, popout={popout}, plugin={plugin}, quiet={quiet}");
                    }
                }
            }
        }
    }

    /// A scaler that paints the frame's first byte, after a pause, counting its runs.
    struct Flat(std::time::Duration, Arc<std::sync::atomic::AtomicUsize>);

    impl Scaler for Flat {
        fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![frame.data[0]; (width * height * 3) as usize]
        }
        fn rgb_into(&mut self, frame: &Frame, width: u32, height: u32, out: &mut [u8]) -> bool {
            std::thread::sleep(self.0);
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let rgb = self.rgb(frame, width, height);
            out.copy_from_slice(&rgb);
            true
        }
    }

    fn grey(level: u8, number: u64) -> Frame {
        let mut f = Frame::new(4, 2, telescope_core::PixelFormat::I420, vec![level; 12]);
        f.number = number;
        f
    }

    fn until(mut done: impl FnMut() -> bool) {
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < limit, "the painter's thread never finished");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn flat(ms: u64) -> (Box<dyn Scaler>, Arc<std::sync::atomic::AtomicUsize>) {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (Box::new(Flat(std::time::Duration::from_millis(ms), runs.clone())), runs)
    }

    #[test]
    fn the_threaded_painter_never_makes_the_caller_wait_and_hands_back_the_picture_for_the_same_frame() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(flat(50).0, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let frame = grey(90, 7);
        let began = std::time::Instant::now();
        assert!(painter.paint(&frame, 2, 2).is_none(), "nothing yet for a frame it hasn't seen");
        assert!(began.elapsed() < std::time::Duration::from_millis(40), "the caller waited for the scaler");
        assert!(!painter.ready());
        assert!(painter.paint(&frame, 2, 2).is_none(), "still being made, and asked for once only");
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.ready());
        let image = painter.paint(&frame, 2, 2).expect("the picture for the frame that asked");
        assert_eq!(image.to_rgb8().unwrap().as_slice()[0].r, 90);
        assert!(!painter.ready());
        // The same frame at another size is made afresh
        assert!(painter.paint(&frame, 4, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&frame, 4, 2).unwrap().to_rgb8().unwrap().width(), 4);
    }

    #[test]
    fn a_scaler_slower_than_the_frames_still_puts_pictures_up_at_a_steady_rate() {
        let (scaler, runs) = flat(40);
        let mut painter = ThreadedPainter::new(scaler, Arc::new(|| {}));
        let began = std::time::Instant::now();
        let (mut shown, mut times) = (Vec::new(), Vec::new());
        let mut number = 0;
        // 100 fps against a scaler that takes 40 ms: a picture is never for the newest frame
        while began.elapsed() < std::time::Duration::from_millis(800) {
            number += 1;
            if let Some(image) = painter.paint(&grey(number as u8, number), 2, 2) {
                shown.push(image.to_rgb8().unwrap().as_slice()[0].r as u64);
                times.push(began.elapsed().as_millis());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(shown.len() >= 10, "{} pictures in 800 ms", shown.len());
        assert!(shown.windows(2).all(|w| w[0] < w[1]), "pictures went back: {shown:?}");
        let longest = times.windows(2).map(|t| t[1] - t[0]).max().unwrap();
        assert!(longest < 200, "a gap of {longest} ms between pictures");
        assert!(runs.load(std::sync::atomic::Ordering::SeqCst) >= shown.len());
    }

    #[test]
    fn a_frame_made_again_from_the_same_source_is_scaled_once() {
        // Colour Compare gives each retry of a stalled stream's frame a new copy of its data
        let (scaler, runs) = flat(30);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        painter.derived_from(5);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        for _ in 0..5 {
            assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        }
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_some(), "the picture lands");
        assert!(!painter.ready());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The Compare line moved: the same frame is a new job
        painter.derived_from(6);
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_new_stream_doesn_t_show_the_last_ones_picture() {
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(flat(10).0, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(painter.paint(&grey(90, 7), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        painter.restart();
        assert!(!painter.ready());
        assert!(painter.paint(&grey(200, 7), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&grey(200, 7), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 200);
    }

    #[test]
    fn a_frame_replaced_in_place_is_another_picture() {
        assert_ne!(paint_tag(5, 10), paint_tag(5, 11));
        assert_eq!(paint_tag(5, 10), paint_tag(5, 10));
        let (scaler, runs) = flat(5);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        // The stream stalled on frame 3; its composite later replaces the pixels under the same number
        painter.derived_from(paint_tag(0, 10));
        assert!(painter.paint(&grey(60, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert_eq!(painter.paint(&grey(60, 3), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 60);
        painter.derived_from(paint_tag(0, 11));
        assert!(painter.paint(&grey(200, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert_eq!(painter.paint(&grey(200, 3), 2, 2).unwrap().to_rgb8().unwrap().as_slice()[0].r, 200);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_finished_picture_goes_up_without_asking_for_the_next() {
        assert_eq!(step(false, true, false), Step::Take);
        assert_eq!(step(false, false, false), Step::Skip);
        assert_eq!(step(true, true, false), Step::Paint);
        assert_eq!(step(false, true, true), Step::Skip, "a tinting pass paints the picture itself, so no repaints for a picture it never takes");
        let (scaler, runs) = flat(5);
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let mut painter = ThreadedPainter::new(scaler, Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(painter.paint(&grey(60, 1), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 1);
        assert!(painter.paint(&grey(60, 1), 2, 2).is_some());
        assert!(painter.paint(&grey(70, 2), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 2);
        assert!(painter.ready());
        // A picture of another size isn't taken
        assert!(painter.paint_ready(4, 2).is_none());
        assert!(!painter.ready());
        assert!(painter.paint(&grey(80, 3), 2, 2).is_none());
        until(|| woke.load(std::sync::atomic::Ordering::SeqCst) == 3);
        assert!(painter.paint_ready(2, 2).is_some());
        // Newer frames came, but the cadence said no: nothing is made
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!painter.ready());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn an_overlay_ask_waits_for_the_picture_it_goes_on() {
        let ask = OverlayAsk::default();
        assert!(!ask.look(false));
        assert!(ask.look(true));
        assert!(ask.look(false), "the picture hasn't landed, so the ask is still there");
        ask.drawn();
        assert!(!ask.look(false));
    }

    #[test]
    fn dropping_the_painter_waits_for_the_scaler_in_hand() {
        let (scaler, runs) = flat(150);
        let mut painter = ThreadedPainter::new(scaler, Arc::new(|| {}));
        assert!(painter.paint(&grey(60, 1), 2, 2).is_none());
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(painter);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1, "the scaler was still running when the painter went");
    }

    #[test]
    fn frames_shrink_to_the_stage_but_never_grow() {
        assert_eq!(fit_within((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(fit_within((1920, 1080), (400, 100)), (178, 100));
        assert_eq!(fit_within((640, 360), (1280, 720)), (640, 360));
        assert_eq!(fit_within((640, 360), (0, 0)), (1, 1));
    }

    /// A texture painter that says what it is told to, and counts the restarts.
    #[derive(Clone, Default)]
    struct Card {
        lost: Arc<std::sync::atomic::AtomicBool>,
        busy: Arc<std::sync::atomic::AtomicBool>,
        restarts: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl TexturePainter for Card {
        fn paint(&mut self, _frame: &Frame, _width: u32, _height: u32, _tint: Option<&TintSpec>) -> Option<wgpu::Texture> {
            None
        }
        fn lost(&self) -> bool {
            self.lost.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn on_card(&self) -> bool {
            !self.busy.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn restart(&mut self) {
            self.restarts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The painter that takes over: no card.
    struct NoCard;

    impl Painter for NoCard {
        fn paint(&mut self, _frame: &Frame, _width: u32, _height: u32) -> Option<slint::Image> {
            None
        }
    }

    #[test]
    fn a_painter_whose_card_was_lost_is_swapped_for_one_without_a_card_and_not_before() {
        use std::sync::atomic::Ordering::SeqCst;
        let card = Card::default();
        let mut painter: Option<Box<dyn Painter>> = Some(Box::new(GpuPainter::new(Box::new(card.clone()))));
        painter.as_mut().unwrap().set_tint(Some(TintSpec { zebra: Some(200), ..TintSpec::default() }), true);
        assert!(painter.as_ref().unwrap().on_card());
        // A card that is only too busy keeps its painter, and the tints leave it
        card.busy.store(true, SeqCst);
        assert!(!painter.as_ref().unwrap().on_card() && !painter.as_ref().unwrap().lost());
        assert!(!replace_lost(&mut painter, || panic!("nothing was lost")));
        card.busy.store(false, SeqCst);
        assert!(!replace_lost(&mut painter, || panic!("nothing was lost")));
        // Lost: the next look swaps it, once, and what is there then draws no tint on a card
        card.lost.store(true, SeqCst);
        assert!(painter.as_ref().unwrap().lost() && !painter.as_ref().unwrap().on_card());
        assert!(replace_lost(&mut painter, || Some(Box::new(NoCard))));
        assert!(!painter.as_ref().unwrap().lost() && !painter.as_ref().unwrap().on_card());
        assert!(!replace_lost(&mut painter, || panic!("already swapped")));
        // No stand-in to be had (no services yet): the painter stays rather than leaving the window with none
        let mut kept: Option<Box<dyn Painter>> = Some(Box::new(GpuPainter::new(Box::new(card))));
        assert!(!replace_lost(&mut kept, || None) && kept.is_some());
    }

    #[test]
    fn a_new_stream_lets_the_texture_painter_go_of_its_scope_copy() {
        let card = Card::default();
        let mut painter = GpuPainter::new(Box::new(card.clone()));
        painter.restart();
        assert_eq!(card.restarts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
