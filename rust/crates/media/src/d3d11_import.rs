//! D3D11VA surfaces retained across decode, a copy on the card into a texture both D3D11 and the pipeline's device can open, and a lazy RAM read.
//! On DX12 the pipeline's queue waits for the copy through a shared fence, and each shared texture stays opened between pictures.

use ash::vk;
use ffmpeg_sys_next as ff;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use telescope_core::frame::{DecodedSurface, Frame, PixelFormat};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIResource1};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use wgpu::hal::api::{Dx12, Vulkan};

/// Decoder pictures kept on the card at once, and shared textures in flight, per stream.
pub(crate) const MAX_SURFACES: usize = 3;
/// How many decoder pictures a stream may hold back at once: the newest one, those queued for the camera and shown, and the preview's.
pub(crate) const MAX_HELD: usize = 6;

/// How long after the first picture the camera still gets whole ones: the card is slow to settle, and a first scaled picture can take over 100 ms, which drops frames.
const WARM_UP: Duration = Duration::from_millis(1000);
/// The decoder this module imports from.
pub(crate) const HW: crate::ffmpeg::Hw = crate::ffmpeg::Hw::D3d11va;
/// How long the copy into the shared texture may take before the picture goes through memory instead.
const COPY_WAIT_MS: u32 = 250;
/// GENERIC_READ for a DXGI shared handle: DXGI_SHARED_RESOURCE_READ
const SHARED_READ: u32 = 0x8000_0000;
/// GENERIC_ALL for a shared fence handle, which D3D12 asks for to open it
const SHARED_ALL: u32 = 0x1000_0000;

#[derive(Default)]
pub(crate) struct Capabilities {
    approved: HashSet<(wgpu::Backend, [u8; 8])>,
}

impl Capabilities {
    fn device(&mut self, key: (wgpu::Backend, [u8; 8]), query: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        if !self.approved.contains(&key) {
            query()?;
            self.approved.insert(key);
        }
        Ok(())
    }
}

/// libavutil/hwcontext_d3d11va.h's AVD3D11VADeviceContext.
#[repr(C)]
pub(crate) struct DeviceContext {
    pub(crate) device: *mut c_void,
    pub(crate) device_context: *mut c_void,
    pub(crate) video_device: *mut c_void,
    pub(crate) video_context: *mut c_void,
    pub(crate) lock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) lock_ctx: *mut c_void,
}

/// One shared texture: made by FFmpeg's D3D11 device, opened by the pipeline's device for every picture that uses it.
struct Slot {
    texture: ID3D11Texture2D,
    handle: HANDLE,
    size: (u32, u32),
    busy: Arc<AtomicBool>,
    opened: Arc<Mutex<Option<Opened>>>,
}

/// A slot's texture as the pipeline's device opened it, kept for the next picture that lands in the slot.
struct Opened {
    device: wgpu::Device,
    texture: wgpu::Texture,
}

/// A free slot handed to one picture: the D3D11 texture pointer, its NT handle, the busy flag and the texture as last opened.
struct Claim {
    texture: *mut c_void,
    handle: HANDLE,
    busy: Arc<AtomicBool>,
    opened: Arc<Mutex<Option<Opened>>>,
}

// SAFETY: the handle is a plain NT handle value and the texture is a free-threaded D3D11 object used under FFmpeg's device lock.
unsafe impl Send for Slot {}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the handle was made by CreateSharedHandle for this slot and is closed once.
        unsafe { let _ = CloseHandle(self.handle); }
    }
}

struct Fence {
    fence: ID3D11Fence,
    event: HANDLE,
    value: AtomicU64,
    /// The fence's NT handle when it could be shared, so a DX12 queue can wait on it.
    shared: Option<HANDLE>,
    /// The fence as the pipeline's DX12 device opened it.
    on_dx12: Mutex<Option<(ID3D12Device, ID3D12Fence)>>,
}

impl Fence {
    /// Wait on the processor until the copy that signals value has finished.
    fn wait(&self, value: u64) -> Result<(), String> {
        // SAFETY: the fence and event are live and owned by self.
        unsafe {
            self.fence.SetEventOnCompletion(value, self.event).map_err(|e| e.to_string())?;
            if WaitForSingleObject(self.event, COPY_WAIT_MS) != WAIT_OBJECT_0 {
                return Err("the copy into the shared texture didn't finish in time".into());
            }
        }
        Ok(())
    }

    /// This fence opened on the pipeline's DX12 device, or None when it isn't shared or that device can't open it.
    fn dx12(&self, device: &wgpu::Device) -> Option<ID3D12Fence> {
        let handle = self.shared?;
        // SAFETY: the guard only borrows wgpu's DX12 device for this lookup.
        let d12: ID3D12Device = unsafe { device.as_hal::<Dx12>() }?.raw_device().clone();
        let mut kept = self.on_dx12.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, fence)) = kept.as_ref().filter(|(owner, _)| owner.as_raw() == d12.as_raw()) {
            return Some(fence.clone());
        }
        let mut fence: Option<ID3D12Fence> = None;
        // SAFETY: the handle is this fence's live NT handle, made on the same card (checked by LUID).
        if let Err(e) = unsafe { d12.OpenSharedHandle(handle, &mut fence) } {
            log::info!("DX12 couldn't open the copy's fence ({e}); waiting for each copy on the processor");
            return None;
        }
        let fence = fence?;
        *kept = Some((d12, fence.clone()));
        Some(fence)
    }
}

// SAFETY: the fence is free-threaded; the event handle is used by one waiter at a time, under the stream's lock; the D3D12 objects are free-threaded.
unsafe impl Send for Fence {}
unsafe impl Sync for Fence {}

impl Drop for Fence {
    fn drop(&mut self) {
        // SAFETY: the event and shared handle were made in Stream::fence and are closed once.
        unsafe {
            let _ = CloseHandle(self.event);
            if let Some(handle) = self.shared { let _ = CloseHandle(handle); }
        }
    }
}

#[derive(Default)]
pub(crate) struct Stream {
    disabled: AtomicBool,
    live: AtomicUsize,
    download: Mutex<Option<crate::ffmpeg::Codec>>,
    /// Scaling pictures on the card failed once: the camera gets whole pictures from then on.
    fit_failed: AtomicBool,
    /// When the first picture was kept: scaling waits until the card has settled (see `WARM_UP`).
    first: OnceLock<Instant>,
    slots: Mutex<Vec<Slot>>,
    fence: Mutex<Option<Arc<Fence>>>,
    /// Whether the log already says this stream shares pictures on the card.
    announced: AtomicBool,
    #[cfg(test)]
    forced: Mutex<Option<&'static str>>,
    /// How often a shared texture was opened on the pipeline's device.
    #[cfg(test)]
    opens: AtomicUsize,
}

impl Stream {
    pub(crate) fn enabled(&self) -> bool {
        !self.disabled.load(Ordering::Acquire)
    }

    pub(crate) fn disable(&self, why: &str) {
        if !self.disabled.swap(true, Ordering::AcqRel) {
            #[cfg(test)]
            eprintln!("sharing disabled: {why}");
            log::warn!("D3D11 surface sharing failed ({why}); copying decoded pictures through RAM for the rest of this stream");
        }
    }

    /// Sharing is off for this stream for a known reason, not a failure: the pictures go through memory, said once at info level.
    fn decline(&self, why: &str) {
        if !self.disabled.swap(true, Ordering::AcqRel) {
            log::info!("D3D11 surface sharing is off ({why}); copying decoded pictures through RAM");
        }
    }

    fn attempt<T>(&self, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if !self.enabled() {
            return Err("surface sharing is disabled for this stream".into());
        }
        if std::env::var_os("TELESCOPE_NO_SURFACE_IMPORT").is_some() {
            self.disable("TELESCOPE_NO_SURFACE_IMPORT is set");
            return Err("TELESCOPE_NO_SURFACE_IMPORT is set".into());
        }
        #[cfg(test)]
        if let Some(why) = *self.forced.lock().unwrap() {
            self.disable(why);
            return Err(why.into());
        }
        work().inspect_err(|why| self.disable(why))
    }
}

struct AvFrame(*mut ff::AVFrame);

// SAFETY: each reference owns an AVFrame; access is serialised and FFmpeg's buffer references are thread safe.
unsafe impl Send for AvFrame {}

impl Drop for AvFrame {
    fn drop(&mut self) {
        // SAFETY: this frame was allocated or cloned by FFmpeg and is freed exactly once.
        unsafe { ff::av_frame_free(&mut self.0) }
    }
}

pub(crate) struct Surface {
    frame: Mutex<AvFrame>,
    ram: OnceLock<Frame>,
    stream: Arc<Stream>,
    size: (u32, u32),
    colour: u32,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3d11Surface").field("size", &self.size).field("landed", &self.ram.get().is_some()).finish()
    }
}

/// A picture copied into a shared texture and opened by the pipeline's device; the slot is free again when this is dropped.
pub(crate) struct Imported {
    pub(crate) texture: wgpu::Texture,
    pub(crate) backend: wgpu::Backend,
    wait: Option<CardWait>,
    _slot: SlotGuard,
}

/// The DX12 fence and value the pipeline's queue waits for before it reads the shared texture.
struct CardWait(ID3D12Fence, u64);

// SAFETY: D3D12 fences are free-threaded.
unsafe impl Send for CardWait {}

impl Imported {
    /// Stage the wait for the copy on the pipeline's next submission; a picture without one was already waited for on the processor.
    pub(crate) fn wait_on(&mut self, queue: &wgpu::Queue) -> Result<(), String> {
        let Some(CardWait(fence, value)) = self.wait.take() else { return Ok(()) };
        // SAFETY: the guard only borrows wgpu's DX12 queue to stage a wait; the queue waits before the next submission's command lists.
        let hal = unsafe { queue.as_hal::<Dx12>() }.ok_or("no DX12 queue")?;
        hal.add_wait_fence(fence, value);
        Ok(())
    }
}

struct SlotGuard(Arc<AtomicBool>);

impl Drop for SlotGuard {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}

impl Surface {
    /// Retain the decoder's reference without copying or downloading its pixels.
    pub(crate) unsafe fn capture(frame: *const ff::AVFrame, stream: Arc<Stream>, _node: Option<String>) -> Result<Option<Arc<Self>>, String> {
        // SAFETY: the caller owns a live D3D11 frame for this call; only metadata and a new FFmpeg reference are taken.
        let (size, colour, copy) = unsafe {
            if (*frame).width <= 0 || (*frame).height <= 0 || (*frame).hw_frames_ctx.is_null() || (*frame).data[0].is_null() {
                return Err("empty D3D11 surface".into());
            }
            let ctx = &*((*(*frame).hw_frames_ctx).data as *const ff::AVHWFramesContext);
            if ctx.sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 || (*frame).width % 2 != 0 || (*frame).height % 2 != 0 {
                return Err("surface sharing needs even-sized NV12".into());
            }
            let matrix = (*frame).colorspace as i32;
            let coefficients = std::slice::from_raw_parts(ff::sws_getCoefficients(matrix), 4);
            let bt601 = std::slice::from_raw_parts(ff::sws_getCoefficients(5), 4);
            let bt709 = std::slice::from_raw_parts(ff::sws_getCoefficients(1), 4);
            let colour = if coefficients == bt601 { 0 } else if coefficients == bt709 { 16 } else { return Err("unsupported surface colour matrix".into()) }
                | if (*frame).color_range == ff::AVColorRange::AVCOL_RANGE_JPEG { 32 } else { 0 };
            stream.first.get_or_init(Instant::now);
            if stream.live.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_HELD).then_some(n + 1)).is_err() {
                return Ok(None);
            }
            (((*frame).width as u32, (*frame).height as u32), colour, ff::av_frame_clone(frame))
        };
        if copy.is_null() {
            stream.live.fetch_sub(1, Ordering::AcqRel);
            return Err("couldn't retain a D3D11 surface".into());
        }
        Ok(Some(Arc::new(Self { frame: Mutex::new(AvFrame(copy)), ram: OnceLock::new(), stream, size, colour })))
    }

    pub(crate) fn disable(&self, why: &str) { self.stream.disable(why); }

    pub(crate) fn colour(&self) -> u32 {
        self.colour
    }

    /// Copy this picture into one of the stream's shared textures and open the texture on the pipeline's device (or reuse it as opened before).
    /// On DX12 the pipeline's queue waits for the copy on the card (see Imported::wait_on); otherwise the processor waits here.
    /// None: every shared texture is in use, so this picture goes through memory (the stream keeps sharing).
    pub(crate) fn import(&self, device: &wgpu::Device, capabilities: &Mutex<Capabilities>) -> Result<Option<Imported>, String> {
        self.stream.attempt(|| {
            let backend = device.adapter_info().backend;
            // Vulkan imports of D3D11 NV12 textures stay off: opening the texture again or taking it from UNDEFINED loses the device on the NVIDIA drivers tried (a 2019 and a 2025 one),
            // and without those the chroma plane reads wrong (docs/roadmap/next.md)
            if backend == wgpu::Backend::Vulkan && std::env::var_os("TELESCOPE_VULKAN_SURFACE_IMPORT").is_none() {
                self.stream.decline("sharing with Vulkan isn't switched on yet");
                return Err("sharing with Vulkan isn't switched on yet".into());
            }
            if !device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
                return Err("the pipeline device has no NV12 textures".into());
            }
            let source = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the frame is a live D3D11 frame; its device context and texture stay valid while the reference is held.
            let (d3d, texture, index) = unsafe { frame_parts(source.0)? };
            // SAFETY: the pointers come from the retained frame above.
            let d3d_device: ID3D11Device = unsafe { ID3D11Device::from_raw_borrowed(&d3d.device) }.ok_or("no D3D11 device")?.clone();
            let luid = adapter_luid(&d3d_device)?;
            capabilities.lock().unwrap_or_else(|e| e.into_inner()).device((backend, luid), || match backend {
                wgpu::Backend::Vulkan => same_vulkan_device(device, luid),
                wgpu::Backend::Dx12 => same_dx12_device(device, luid),
                other => Err(format!("the pipeline uses the {other:?} backend")),
            })?;
            let Some(claim) = self.stream.slot(&d3d_device, self.size)? else { return Ok(None) };
            let guard = SlotGuard(claim.busy.clone());
            let fence = self.stream.fence(&d3d_device)?;
            // SAFETY: texture and index belong to the retained frame; the claimed texture is this stream's and idle; the context is locked.
            let value = unsafe { self.copy(d3d, texture, index, claim.texture, &fence)? };
            drop(source);
            let on_card = if backend == wgpu::Backend::Dx12 { fence.dx12(device) } else { None };
            let wait = match on_card {
                Some(fence) => Some(CardWait(fence, value)),
                None => {
                    fence.wait(value)?;
                    None
                }
            };
            let mut opened = claim.opened.lock().unwrap_or_else(|e| e.into_inner());
            let kept = opened.as_ref().filter(|o| o.device == *device).map(|o| o.texture.clone());
            let texture = match kept {
                Some(texture) => texture,
                None => {
                    let texture = match backend {
                        wgpu::Backend::Vulkan => import_vulkan(device, claim.handle, self.size)?,
                        _ => import_dx12(device, claim.handle, self.size)?,
                    };
                    *opened = Some(Opened { device: device.clone(), texture: texture.clone() });
                    #[cfg(test)]
                    self.stream.opens.fetch_add(1, Ordering::Relaxed);
                    texture
                }
            };
            if !self.stream.announced.swap(true, Ordering::Relaxed) {
                log::info!("D3D11 surface sharing is on ({backend:?}); decoded pictures stay on the card");
            }
            Ok(Some(Imported { texture, backend, wait, _slot: guard }))
        })
    }

    /// Enqueue the copy of this picture's array slice into the shared texture and signal the stream's fence after it; returns the value signalled.
    unsafe fn copy(&self, d3d: &DeviceContext, source: *mut c_void, index: u32, dest: *mut c_void, fence: &Fence) -> Result<u64, String> {
        // SAFETY: see the caller; the lock functions are FFmpeg's own for this device.
        unsafe {
            let source = ID3D11Resource::from_raw_borrowed(&source).ok_or("no decoder texture")?;
            let dest = ID3D11Resource::from_raw_borrowed(&dest).ok_or("no shared texture")?;
            let context = ID3D11DeviceContext::from_raw_borrowed(&d3d.device_context).ok_or("no D3D11 context")?;
            let value = fence.value.fetch_add(1, Ordering::AcqRel) + 1;
            let region = D3D11_BOX { left: 0, top: 0, front: 0, right: self.size.0, bottom: self.size.1, back: 1 };
            if let Some(lock) = d3d.lock { lock(d3d.lock_ctx) }
            let queued = (|| -> Result<(), String> {
                context.CopySubresourceRegion(dest, 0, 0, 0, 0, source, index, Some(&region));
                let context4 = context.cast::<ID3D11DeviceContext4>().map_err(|e| e.to_string())?;
                context4.Signal(&fence.fence, value).map_err(|e| e.to_string())?;
                context.Flush();
                Ok(())
            })();
            if let Some(unlock) = d3d.unlock { unlock(d3d.lock_ctx) }
            queued?;
            Ok(value)
        }
    }

    /// Acquire or release external queue ownership of the shared image on Vulkan.
    /// The image is taken from GENERAL, the layout wgpu tracks it in (from UNDEFINED the discrete card's driver lost the device); after the convert pass wgpu has left it SHADER_READ_ONLY_OPTIMAL.
    pub(crate) fn ownership(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, acquire: bool) -> Result<(), String> {
        // SAFETY: all handles remain borrowed for the call; this encoder exclusively records the barrier outside any render pass.
        unsafe {
            let hal = device.as_hal::<Vulkan>().ok_or("no Vulkan device")?;
            let image = texture.as_hal::<Vulkan>().ok_or("no Vulkan texture")?;
            let barrier = vk::ImageMemoryBarrier::default()
                .image(image.raw_handle())
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::PLANE_0 | vk::ImageAspectFlags::PLANE_1).level_count(1).layer_count(1))
                .old_layout(if acquire { vk::ImageLayout::GENERAL } else { vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL })
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .src_access_mask(if acquire { vk::AccessFlags::empty() } else { vk::AccessFlags::SHADER_READ })
                .dst_access_mask(if acquire { vk::AccessFlags::SHADER_READ } else { vk::AccessFlags::empty() });
            enc.as_hal_mut::<Vulkan, _, _>(|encoder| {
                let encoder = encoder.ok_or("no Vulkan command encoder")?;
                hal.raw_device().cmd_pipeline_barrier(encoder.raw_handle(),
                    if acquire { vk::PipelineStageFlags::TOP_OF_PIPE } else { vk::PipelineStageFlags::FRAGMENT_SHADER },
                    if acquire { vk::PipelineStageFlags::FRAGMENT_SHADER } else { vk::PipelineStageFlags::BOTTOM_OF_PIPE },
                    vk::DependencyFlags::empty(), &[], &[], &[barrier]);
                Ok(())
            })
        }
    }
}

impl Stream {
    /// A free shared texture of this size, made when the stream has fewer than MAX_SURFACES.
    /// None when all of them are in use.
    fn slot(&self, device: &ID3D11Device, size: (u32, u32)) -> Result<Option<Claim>, String> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        // A resolution change retires the idle slots of the old size
        slots.retain(|s| s.size == size || s.busy.load(Ordering::Acquire));
        for slot in slots.iter().filter(|s| s.size == size) {
            if slot.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Ok(Some(Claim { texture: slot.texture.as_raw(), handle: slot.handle, busy: slot.busy.clone(), opened: slot.opened.clone() }));
            }
        }
        if slots.len() >= MAX_SURFACES {
            return Ok(None);
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0, Height: size.1, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0 as u32,
        };
        let mut texture = None;
        // SAFETY: plain D3D11 calls on FFmpeg's device with an owned descriptor; the texture and its handle are released with the slot.
        let handle = unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut texture)).map_err(|e| format!("couldn't make a shared NV12 texture: {e}"))?;
            let texture: &ID3D11Texture2D = texture.as_ref().ok_or("no shared texture")?;
            texture.cast::<IDXGIResource1>().and_then(|r| r.CreateSharedHandle(None, SHARED_READ, PCWSTR::null())).map_err(|e| format!("couldn't share the texture: {e}"))?
        };
        let texture = texture.ok_or("no shared texture")?;
        let busy = Arc::new(AtomicBool::new(true));
        let claim = Claim { texture: texture.as_raw(), handle, busy: busy.clone(), opened: Arc::default() };
        slots.push(Slot { texture, handle, size, busy, opened: claim.opened.clone() });
        Ok(Some(claim))
    }

    fn fence(&self, device: &ID3D11Device) -> Result<Arc<Fence>, String> {
        let mut kept = self.fence.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fence) = kept.as_ref() {
            return Ok(fence.clone());
        }
        // SAFETY: plain D3D11 calls; the fence and event are owned by the Fence and closed with it.
        let made = unsafe {
            let device5 = device.cast::<ID3D11Device5>().map_err(|e| format!("no D3D11 fences: {e}"))?;
            let mut made_fence: Option<ID3D11Fence> = None;
            // A shared fence lets a DX12 queue wait for the copy; without one the processor waits
            if device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut made_fence).is_err() {
                device5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut made_fence).map_err(|e| format!("no D3D11 fences: {e}"))?;
            }
            let fence = made_fence.ok_or("no D3D11 fence")?;
            let shared = fence.CreateSharedHandle(None, SHARED_ALL, PCWSTR::null()).ok();
            let event = CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?;
            Arc::new(Fence { fence, event, value: AtomicU64::new(0), shared, on_dx12: Mutex::default() })
        };
        *kept = Some(made.clone());
        Ok(made)
    }
}

/// The device context, texture and array slice of a D3D11 frame (data[0] is the texture, data[1] the slice index).
pub(crate) unsafe fn frame_parts(frame: *const ff::AVFrame) -> Result<(&'static DeviceContext, *mut c_void, u32), String> {
    // SAFETY: the caller holds the frame; hwctx of a D3D11VA device context is an AVD3D11VADeviceContext.
    unsafe {
        let frames = &*((*(*frame).hw_frames_ctx).data as *const ff::AVHWFramesContext);
        if frames.device_ctx.is_null() || (*frames.device_ctx).hwctx.is_null() {
            return Err("no D3D11 device context".into());
        }
        let d3d = &*((*frames.device_ctx).hwctx as *const DeviceContext);
        if d3d.device.is_null() || d3d.device_context.is_null() {
            return Err("no D3D11 device".into());
        }
        Ok((d3d, (*frame).data[0] as *mut c_void, (*frame).data[1] as usize as u32))
    }
}

fn adapter_luid(device: &ID3D11Device) -> Result<[u8; 8], String> {
    // SAFETY: plain DXGI queries on a live device.
    let luid = unsafe {
        let adapter = device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).map_err(|e| e.to_string())?;
        adapter.GetDesc().map_err(|e| e.to_string())?.AdapterLuid
    };
    Ok(luid_bytes(luid.LowPart, luid.HighPart))
}

fn luid_bytes(low: u32, high: i32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&low.to_le_bytes());
    bytes[4..].copy_from_slice(&high.to_le_bytes());
    bytes
}

fn same_dx12_device(device: &wgpu::Device, luid: [u8; 8]) -> Result<(), String> {
    // SAFETY: the guard only borrows wgpu's DX12 device for this query.
    let hal = unsafe { device.as_hal::<Dx12>() }.ok_or("no DX12 device")?;
    // SAFETY: a plain query on a live device.
    let own = unsafe { hal.raw_device().GetAdapterLuid() };
    if luid_bytes(own.LowPart, own.HighPart) == luid { Ok(()) } else { Err("the decoder and pipeline use different cards".into()) }
}

fn same_vulkan_device(device: &wgpu::Device, luid: [u8; 8]) -> Result<(), String> {
    if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32) {
        return Err("Vulkan's Win32 external memory is unavailable on the pipeline device".into());
    }
    // SAFETY: the guard only borrows wgpu's Vulkan device for these queries.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("no Vulkan device")?;
    let instance = hal.shared_instance().raw_instance();
    // SAFETY: the physical device belongs to this live instance; only its properties are queried and the output structs outlive each call.
    unsafe {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        instance.get_physical_device_properties2(hal.raw_physical_device(), &mut properties);
        if id.device_luid_valid == 0 || id.device_luid != luid {
            return Err(format!("the decoder and pipeline use different cards (Vulkan LUID {:?}, valid {}, the decoder's {luid:?})", id.device_luid, id.device_luid_valid));
        }
        let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE);
        let info = vk::PhysicalDeviceImageFormatInfo2::default().format(vk::Format::G8_B8R8_2PLANE_420_UNORM).ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL).usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE).push_next(&mut external);
        let mut out = vk::ExternalImageFormatProperties::default();
        let mut properties = vk::ImageFormatProperties2::default().push_next(&mut out);
        instance.get_physical_device_image_format_properties2(hal.raw_physical_device(), &info, &mut properties).map_err(|e| format!("Vulkan can't share NV12 with D3D11: {e}"))?;
        if !out.external_memory_properties.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE) {
            return Err("Vulkan can't import D3D11 NV12 textures".into());
        }
    }
    Ok(())
}

fn nv12_descriptor(size: (u32, u32)) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("D3D11 decoded picture"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::NV12,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
    }
}

fn import_vulkan(device: &wgpu::Device, handle: HANDLE, size: (u32, u32)) -> Result<wgpu::Texture, String> {
    let desc = nv12_descriptor(size);
    // SAFETY: the guard only borrows wgpu's Vulkan device; no wgpu calls are made until it is dropped.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("no Vulkan device")?;
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: desc.label, size: desc.size, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: desc.format,
        usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC, memory_flags: wgpu::hal::MemoryFlags::empty(), view_formats: Vec::new(),
    };
    // SAFETY: the handle is a live NT handle of an NV12 texture of exactly this size, kept open by its slot.
    let imported = unsafe { hal.texture_from_d3d11_shared_handle(handle, &hal_desc) }.map_err(|e| e.to_string())?;
    drop(hal);
    // SAFETY: the image belongs to this device and matches desc; shared contents use GENERAL, acquired before sampling.
    Ok(unsafe { device.create_texture_from_hal::<Vulkan>(imported, &desc, wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC) })
}

fn import_dx12(device: &wgpu::Device, handle: HANDLE, size: (u32, u32)) -> Result<wgpu::Texture, String> {
    let desc = nv12_descriptor(size);
    // SAFETY: the guard only borrows wgpu's DX12 device; no wgpu calls are made until it is dropped.
    let hal = unsafe { device.as_hal::<Dx12>() }.ok_or("no DX12 device")?;
    let d12: ID3D12Device = hal.raw_device().clone();
    drop(hal);
    let mut resource: Option<ID3D12Resource> = None;
    // SAFETY: the handle is a live NT handle of a texture made on the same adapter (checked by LUID), kept open by its slot.
    unsafe { d12.OpenSharedHandle(handle, &mut resource) }.map_err(|e| format!("DX12 couldn't open the shared texture: {e}"))?;
    let resource = resource.ok_or("no DX12 resource")?;
    // SAFETY: the resource is an NV12 texture of this size; D3D12 promotes a COMMON resource to a read state on first use.
    unsafe {
        let hal_texture = wgpu::hal::dx12::Device::texture_from_raw(resource, desc.format, desc.dimension, desc.size, 1, 1);
        Ok(device.create_texture_from_hal::<Dx12>(hal_texture, &desc, wgpu::TextureUses::RESOURCE))
    }
}

impl DecodedSurface for Surface {
    fn ram(&self) -> &Frame {
        self.ram.get_or_init(|| {
            let source = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            let mut download = self.stream.download.lock().unwrap_or_else(|e| e.into_inner());
            let mut failed = String::new();
            // A second try with a fresh download decoder before the picture is given up on
            for _ in 0..2 {
                let tried = (|| {
                    if download.is_none() {
                        *download = Some(crate::ffmpeg::Codec::h264(None).map_err(|e| format!("{e:?}"))?);
                    }
                    download.as_mut().unwrap().download_surface(source.0).map_err(|e| format!("{e:?}"))
                })();
                match tried {
                    Ok(frame) => return frame,
                    Err(why) => {
                        *download = None;
                        failed = why;
                    }
                }
            }
            self.stream.disable(&failed);
            let (w, h) = self.size;
            Frame::dark(w, h, PixelFormat::I420)
        })
    }

    fn as_any(&self) -> &dyn std::any::Any { self }

    fn landed(&self) -> Option<&Frame> {
        self.ram.get()
    }

    // Full-range pictures stay whole: the processor works in studio range and some drivers would clamp them
    fn can_fit(&self, width: u32, height: u32) -> bool {
        self.stream.first.get().is_some_and(|at| at.elapsed() >= WARM_UP) && self.scales_to(width, height)
    }

    fn fitted(&self, width: u32, height: u32) -> Option<Frame> {
        if !self.scales_to(width, height) {
            return None;
        }
        let source = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        let mut download = self.stream.download.lock().unwrap_or_else(|e| e.into_inner());
        let tried = (|| {
            if download.is_none() {
                *download = Some(crate::ffmpeg::Codec::h264(None).map_err(|e| format!("{e:?}"))?);
            }
            download.as_mut().unwrap().fit_surface(source.0, (width, height)).map_err(|e| format!("{e:?}"))
        })();
        match tried {
            Ok(frame) => {
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| log::info!("The graphics card scales the decoder's pictures to the camera's size ({width}x{height}) before they are copied"));
                Some(frame)
            }
            Err(why) => {
                *download = None;
                if !self.stream.fit_failed.swap(true, Ordering::AcqRel) {
                    log::warn!("Couldn't scale a picture to the camera on the graphics card ({why}); the processor copies whole pictures from now on");
                }
                None
            }
        }
    }
}

impl Surface {
    fn scales_to(&self, width: u32, height: u32) -> bool {
        self.stream.enabled() && self.colour & 32 == 0 && !self.stream.fit_failed.load(Ordering::Acquire) && crate::surface_fit::scales_to(self.size, (width, height))
    }
}

impl Drop for Surface {
    fn drop(&mut self) { self.stream.live.fetch_sub(1, Ordering::AcqRel); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::{Codec, Hw};
    use crate::gpu::Gpu;
    use crate::testenc;

    #[test]
    fn luids_compare_as_the_vulkan_and_dxgi_bytes() {
        assert_eq!(luid_bytes(0x0102_0304, -1), [4, 3, 2, 1, 255, 255, 255, 255]);
    }

    #[test]
    fn failures_disable_only_that_stream_and_are_not_retried() {
        let stream = Stream::default();
        assert!(stream.attempt(|| Err::<(), _>("boom".into())).is_err());
        assert!(!stream.enabled());
        let mut ran = false;
        assert!(stream.attempt(|| { ran = true; Ok(()) }).is_err());
        assert!(!ran, "a disabled stream doesn't try again");
        assert!(Stream::default().enabled());
    }

    /// TS_CARD=<vendor>:<device> (like 10de:1c20) picks the card the way Settings would; WGPU_BACKEND picks Vulkan or DX12.
    fn card() -> Option<std::sync::Arc<Gpu>> {
        struct Stderr;
        impl log::Log for Stderr {
            fn enabled(&self, m: &log::Metadata) -> bool { m.level() <= log::Level::Warn }
            fn log(&self, r: &log::Record) { if self.enabled(r.metadata()) { eprintln!("[{}] {}: {}", r.level(), r.target(), r.args()); } }
            fn flush(&self) {}
        }
        if let Ok(id) = std::env::var("TS_CARD") {
            // Only a hand-picked card is a diagnostic run: a logger here would take the one global slot from the tests that read the log
            let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Warn));
            telescope_core::gpu_pick::set_choice(&id);
        }
        let gpu = Gpu::shared();
        if let Some(gpu) = &gpu {
            println!("pipeline card: {}", gpu.adapter_name);
        }
        gpu
    }

    fn decode_surface(codec: &mut Codec, packets: &[Vec<u8>]) -> Option<Frame> {
        let mut frame = None;
        for packet in packets {
            codec.parse(packet, |codec, unit| {
                codec.decode_newest(unit).unwrap();
                if let Some(newest) = codec.take_newest().unwrap() { frame = Some(newest); }
            });
        }
        frame
    }

    #[test]
    fn a_shared_d3d11_picture_matches_the_transfer_path() {
        let Some(gpu) = card() else { return eprintln!("no graphics card; skipped") };
        if gpu.adapter_name.contains("Vulkan") && std::env::var_os("TELESCOPE_VULKAN_SURFACE_IMPORT").is_none() {
            return eprintln!("sharing with Vulkan is off; skipped");
        }
        let Ok(mut codec) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
        codec.surfaces(true);
        for (w, h) in [(256, 144), (1920, 1080)] {
            for (y, u, v, matrix, range) in [(70, 112, 145, "smpte170m", "tv"), (170, 150, 110, "bt709", "tv"), (110, 100, 152, "bt709", "pc")] {
                let Some(packets) = testenc::h264_yuv(y, u, v, w, h, matrix, range) else { return eprintln!("no H.264 encoder; skipped") };
                let frame = decode_surface(&mut codec, &packets).expect("a decoded frame");
                assert!(frame.data.is_empty(), "decode downloaded pixels before a RAM reader");
                let surface = frame.surface().unwrap().as_any().downcast_ref::<Surface>().unwrap();
                let mut shared = gpu.lift_with(&frame, true).unwrap();
                assert!(shared.was_imported(), "D3D11VA decoded but sharing fell back (disabled: {})", !surface.stream.enabled());
                let shared_pixels = gpu.land(&mut shared).unwrap();
                gpu.put_down(shared);
                assert!(surface.ram.get().is_none(), "sharing used the transfer path");
                let transfer = frame.materialise();
                let mut uploaded = gpu.lift_with(&transfer, true).unwrap();
                let transferred_pixels = gpu.land(&mut uploaded).unwrap();
                gpu.put_down(uploaded);
                let worst = shared_pixels.data.iter().zip(transferred_pixels.data.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                let mean = shared_pixels.data.iter().zip(transferred_pixels.data.iter()).map(|(a, b)| a.abs_diff(*b) as u64).sum::<u64>() as f64 / shared_pixels.data.len() as f64;
                println!("{w}x{h} {matrix}/{range}: shared against transferred, worst {worst} levels, mean {mean:.4}");
                assert!(worst <= 3, "{w}x{h} {matrix}/{range}: shared and transferred pixels differ by {worst} levels");
            }
        }
    }

    #[test]
    fn a_stream_with_no_steps_converts_the_decoders_picture_on_the_card_as_the_cpu_would() {
        use crate::pipeline::{tests::NoContext, Pipeline};
        use telescope_core::balance::{Balance, Place};
        use telescope_core::media::FramePipeline;
        if !crate::pipeline::plain_allowed() {
            return eprintln!("a plain stream converting on the card is switched off (TELESCOPE_PLAIN_ON_CARD=0); skipped");
        }
        let Some(gpu) = card() else { return eprintln!("no graphics card; skipped") };
        if gpu.adapter_name.contains("Vulkan") && std::env::var_os("TELESCOPE_VULKAN_SURFACE_IMPORT").is_none() {
            return eprintln!("sharing with Vulkan is off; skipped");
        }
        let Ok(mut codec) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
        codec.surfaces(true);
        let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        for (w, h) in [(256, 144), (1920, 1080)] {
            for (y, u, v, matrix, range) in [(70, 112, 145, "smpte170m", "tv"), (170, 150, 110, "bt709", "tv"), (110, 100, 152, "bt709", "pc"), (110, 100, 152, "smpte170m", "pc"), (200, 40, 230, "bt709", "pc")] {
                let Some(packets) = testenc::h264_yuv(y, u, v, w, h, matrix, range) else { return eprintln!("no H.264 encoder; skipped") };
                let frame = decode_surface(&mut codec, &packets).expect("a decoded frame");
                let out = pipeline.process_picture(frame.clone(), &NoContext);
                assert!(out.picture().is_some(), "{w}x{h} {matrix}/{range}: the picture didn't stay on the card");
                let on_card = out.land();
                let surface = frame.surface().unwrap().as_any().downcast_ref::<Surface>().unwrap();
                assert!(surface.ram.get().is_none(), "{w}x{h} {matrix}/{range}: the CPU converted the picture as well");
                let cpu = frame.materialise();
                assert_eq!(on_card.data.len(), cpu.data.len());
                let worst = on_card.data.iter().zip(cpu.data.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert_eq!(worst, 0, "{w}x{h} {matrix}/{range}: the card made other bytes than the CPU");
            }
        }
    }

    #[test]
    fn a_failed_share_still_gives_this_and_following_pictures_without_a_dark_frame() {
        let Some(gpu) = card() else { return eprintln!("no graphics card; skipped") };
        let Ok(mut codec) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
        codec.surfaces(true);
        let Some(packets) = testenc::h264(&[70, 120, 180], 256, 144, 1) else { return eprintln!("no H.264 encoder; skipped") };
        let first = decode_surface(&mut codec, &packets[..1]).expect("a frame");
        let surface = first.surface().unwrap().as_any().downcast_ref::<Surface>().unwrap();
        *surface.stream.forced.lock().unwrap() = Some("forced share failure");
        let mut held = gpu.lift_with(&first, true).unwrap();
        assert!(!held.was_imported());
        let out = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        // the encoder gets greys in limited range, so luma 70 is 76 and 180 is 171
        assert!(out.data[0].abs_diff(76) <= 3, "fallback lost the original pixels");
        assert!(!surface.stream.enabled());
        let next = decode_surface(&mut codec, &packets[1..]).expect("another frame");
        assert!(next.surface().is_none(), "the decoder kept offering surfaces after fallback");
        assert!(next.data[0].abs_diff(171) <= 3);
    }

    #[test]
    fn each_shared_texture_is_opened_once_and_reused_for_later_pictures() {
        let Some(gpu) = card() else { return eprintln!("no graphics card; skipped") };
        if gpu.adapter_name.contains("Vulkan") && std::env::var_os("TELESCOPE_VULKAN_SURFACE_IMPORT").is_none() {
            return eprintln!("sharing with Vulkan is off; skipped");
        }
        let Ok(mut codec) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
        codec.surfaces(true);
        let lumas = [60u8, 100, 140, 180, 140, 100, 60, 100, 140, 180];
        let Some(packets) = testenc::h264(&lumas, 256, 144, 1) else { return eprintln!("no H.264 encoder; skipped") };
        let mut stream = None;
        for (packet, luma) in packets.iter().zip(lumas) {
            let frame = decode_surface(&mut codec, std::slice::from_ref(packet)).expect("a frame");
            stream = frame.surface().and_then(|s| s.as_any().downcast_ref::<Surface>()).map(|s| s.stream.clone()).or(stream);
            let mut held = gpu.lift_with(&frame, true).unwrap();
            assert!(held.was_imported(), "a picture fell back to memory");
            let out = gpu.land(&mut held).unwrap();
            gpu.put_down(held);
            // limited range: 16 + luma * 219 / 255
            let want = 16 + u32::from(luma) * 219 / 255;
            assert!(u32::from(out.data[0]).abs_diff(want) <= 3, "luma {} for {luma}, wanted about {want}", out.data[0]);
        }
        let opens = stream.expect("a shared picture").opens.load(Ordering::Relaxed);
        assert!(opens <= MAX_SURFACES, "{opens} opens for {} pictures", lumas.len());
    }

    /// A long stream with the pictures shared: surfaces and shared textures stay within their bounds and the frames never go dark.
    /// BENCH_SECONDS sets the length (default 20); watch the card's memory meanwhile.
    #[test]
    #[ignore = "a soak run, needs a graphics card with D3D11VA"]
    fn sharing_for_a_long_stream_keeps_its_resources_bounded() {
        let Some(gpu) = card() else { return eprintln!("no graphics card; skipped") };
        let Ok(mut codec) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
        codec.surfaces(true);
        let Some(packets) = testenc::h264(&[60, 100, 140, 180, 140, 100], 1920, 1080, 1) else { return eprintln!("no H.264 encoder; skipped") };
        let seconds: u64 = std::env::var("BENCH_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
        let began = std::time::Instant::now();
        let (mut shown, mut round, mut dark) = (0u64, 0usize, 0u64);
        let mut stream = None;
        while began.elapsed().as_secs() < seconds {
            let packet = &packets[round % packets.len()];
            round += 1;
            let Some(frame) = decode_surface(&mut codec, std::slice::from_ref(packet)) else { continue };
            stream = frame.surface().and_then(|s| s.as_any().downcast_ref::<Surface>()).map(|s| s.stream.clone()).or(stream);
            let mut held = gpu.lift_with(&frame, true).unwrap();
            let out = gpu.land(&mut held).unwrap();
            gpu.put_down(held);
            dark += u64::from(out.data[0] < 20);
            shown += 1;
            if let Some(stream) = &stream {
                assert!(stream.live.load(Ordering::Acquire) <= MAX_HELD, "decoder surfaces are piling up");
                assert!(stream.slots.lock().unwrap().len() <= MAX_SURFACES, "shared textures are piling up");
            }
        }
        println!("{shown} pictures in {seconds} s, {dark} dark, shared textures {}", stream.map_or(0, |s| s.slots.lock().unwrap().len()));
        assert_eq!(dark, 0);
    }
}
