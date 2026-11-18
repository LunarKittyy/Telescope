//! The Windows side of Spout2 for any plugin: the file mappings and named objects senders publish (sender list, a sender's info, its
//! frame counter and texture lock) and the D3D11 device, shared textures and staging textures that read them, with the pixel shaders that
//! turn a picture into I420 planes on the card. Only the reading half is here; a sender's half (register a name, create a texture, write the
//! info, count frames) goes next to it.

use super::planes;
use super::poll::{poll, Deadline, Polled};
use super::protocol::{self, SharedTextureInfo};
use ::windows::core::{Interface, PCWSTR};
use ::windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, WAIT_ABANDONED, WAIT_OBJECT_0};
use ::windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D,
};
use ::windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11ClassLinkage, ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext, ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState,
    ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_RASTERIZER_DESC, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
    D3D11_SDK_VERSION, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIEWPORT,
};
use ::windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC};
use ::windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1, IDXGIKeyedMutex, DXGI_ERROR_WAS_STILL_DRAWING};
use ::windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS};
use ::windows::Win32::System::Threading::{
    OpenMutexW, OpenSemaphoreW, ReleaseMutex, ReleaseSemaphore, WaitForSingleObject, MUTEX_MODIFY_STATE, SEMAPHORE_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};
use std::ffi::c_void;

/// How long Spout's own code waits on its mutexes: four frames at 60 fps.
const LOCK_WAIT_MS: u32 = 67;

// The converter's shaders, built from shaders/convert.hlsl by shaders/compile.ps1 and checked in so nothing compiles at run time
const SHADER_VERTEX: &[u8] = include_bytes!("shaders/convert_vs.cso");
const SHADER_LUMA: &[u8] = include_bytes!("shaders/convert_y.cso");
const SHADER_CHROMA: &[u8] = include_bytes!("shaders/convert_uv.cso");

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Why reading a picture back from the card gave nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The stream was stopped while the card was still working.
    Cancelled,
    /// The card didn't finish in the time given.
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Cancelled => f.write_str("stopped while the card was still working"),
            ReadError::TimedOut => f.write_str("the card didn't finish in time"),
            ReadError::Failed(why) => f.write_str(why),
        }
    }
}

/// A kernel handle, closed when dropped.
struct Owned(HANDLE);

// SAFETY: a kernel handle is process-wide and may be used and closed from any thread
unsafe impl Send for Owned {}
// SAFETY: as above
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is closed once, here
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Whether the object was got (signalled, or left by an owner that died, which is ours all the same).
fn got(handle: HANDLE, ms: u32) -> bool {
    // SAFETY: a live handle
    let result = unsafe { WaitForSingleObject(handle, ms) };
    result == WAIT_OBJECT_0 || result == WAIT_ABANDONED
}

/// A named file mapping a sender or the sender list publishes, read-only, with the mutex Spout keeps beside it.
pub struct SharedMemory {
    _map: Owned,
    view: *mut c_void,
    size: usize,
    mutex: Option<Owned>,
}

// SAFETY: the view maps a kernel object and stays valid from any thread until drop; reads are plain byte copies
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    /// The mapping called `name`, or None if there is no such thing (no sender by that name, or none at all).
    pub fn open(name: &str) -> Option<SharedMemory> {
        let wide_name = wide(name);
        // SAFETY: wide_name is NUL-terminated and outlives the call
        let map = Owned(unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide_name.as_ptr())) }.ok()?);
        // SAFETY: a live mapping handle; the whole mapping is wanted
        let view: MEMORY_MAPPED_VIEW_ADDRESS = unsafe { MapViewOfFile(map.0, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            return None;
        }
        let mut memory = SharedMemory { _map: map, view: view.Value, size: 0, mutex: None };
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: info is an out buffer of the size given and the view is mapped
        let filled = unsafe { VirtualQuery(Some(view.Value as *const c_void), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if filled == 0 {
            return None;
        }
        memory.size = info.RegionSize;
        let mutex_name = wide(&protocol::map_mutex(name));
        // SAFETY: NUL-terminated name
        memory.mutex = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE, false, PCWSTR(mutex_name.as_ptr())) }.ok().map(Owned);
        Some(memory)
    }

    /// The mapped size in bytes (a whole number of pages, as the mapping can't be asked its exact size).
    pub fn size(&self) -> usize {
        self.size
    }

    /// The first `len` bytes (fewer if the mapping is smaller), copied while holding the mapping's mutex; None if its owner doesn't let go in time.
    pub fn read(&self, len: usize) -> Option<Vec<u8>> {
        let len = len.min(self.size);
        let held = match &self.mutex {
            Some(mutex) if got(mutex.0, LOCK_WAIT_MS) => Some(mutex),
            Some(_) => return None,
            None => None,
        };
        let mut out = vec![0u8; len];
        // SAFETY: the view is at least `size` >= len bytes and stays mapped; another process may write it, which a byte copy tolerates
        unsafe { std::ptr::copy_nonoverlapping(self.view as *const u8, out.as_mut_ptr(), len) };
        if let Some(mutex) = held {
            // SAFETY: this thread got the mutex just above
            unsafe {
                let _ = ReleaseMutex(mutex.0);
            }
        }
        Some(out)
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: the view was mapped by open and is unmapped once, here
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view });
        }
    }
}

/// The senders' names in list order, empty when there are none, None when the list couldn't be read just now (its mutex was held).
pub fn sender_names() -> Option<Vec<String>> {
    let Some(list) = SharedMemory::open(protocol::NAMES_MAP) else { return Some(Vec::new()) };
    list.read(list.size()).map(|bytes| protocol::parse_names(&bytes))
}

/// What looking up a sender's info found.
#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // made once per look at a sender and used at once, never stored
pub enum Info {
    /// Nothing is published under that name: the sender is gone.
    Gone,
    /// Its mapping is there but its mutex wasn't let go in time, so ask again.
    Busy,
    Found(SharedTextureInfo),
}

/// What the sender called `name` publishes about its texture.
pub fn sender_info(name: &str) -> Info {
    let Some(memory) = SharedMemory::open(name) else { return Info::Gone };
    match memory.read(SharedTextureInfo::SIZE).and_then(|bytes| SharedTextureInfo::parse(&bytes)) {
        Some(info) => Info::Found(info),
        None => Info::Busy,
    }
}

/// A sender's frame counter, a named semaphore that only exists while frame counting is on for the sender.
pub struct FrameCounter(Owned);

impl FrameCounter {
    pub fn open(sender: &str) -> Option<FrameCounter> {
        let name = wide(&protocol::count_semaphore(sender));
        // SAFETY: NUL-terminated name
        unsafe { OpenSemaphoreW(SYNCHRONIZATION_SYNCHRONIZE | SEMAPHORE_MODIFY_STATE, false, PCWSTR(name.as_ptr())) }.ok().map(|h| FrameCounter(Owned(h)))
    }

    /// The count as Spout's receivers read it (one less than the semaphore holds): 0 for a sender that doesn't count, and different
    /// from the last answer when it has made a frame since.
    pub fn read(&self) -> Option<i32> {
        if !got(self.0 .0, 0) {
            return None;
        }
        let mut before = 0i32;
        // SAFETY: gives back the one count taken above; before is a valid out value
        unsafe { ReleaseSemaphore(self.0 .0, 1, Some(&mut before)) }.ok()?;
        Some(before)
    }
}

/// What keeps a sender from writing its texture while it's read: the texture's own keyed mutex or else Spout's named mutex, which a
/// sender may not have.
pub struct AccessLock(Access);

enum Access {
    Keyed(IDXGIKeyedMutex),
    Named(Owned),
    Open,
}

// SAFETY: acquired and released by one thread at a time, which is all D3D11 asks of a keyed mutex
unsafe impl Send for AccessLock {}

impl AccessLock {
    pub fn for_texture(sender: &str, texture: &SharedTexture) -> AccessLock {
        if texture.keyed {
            if let Ok(mutex) = texture.texture.cast::<IDXGIKeyedMutex>() {
                return AccessLock(Access::Keyed(mutex));
            }
        }
        let name = wide(&protocol::access_mutex(sender));
        // SAFETY: NUL-terminated name
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE, false, PCWSTR(name.as_ptr())) } {
            Ok(handle) => AccessLock(Access::Named(Owned(handle))),
            Err(_) => AccessLock(Access::Open),
        }
    }

    /// Whether the texture may be read now (always, without a lock to wait on). Release from the same thread after.
    pub fn acquire(&self) -> bool {
        match &self.0 {
            // SAFETY: the vtable call is AcquireSync, whose plain HRESULT is read here because the Result form would take a timeout (0x102) for success
            Access::Keyed(mutex) => unsafe { (Interface::vtable(mutex).AcquireSync)(Interface::as_raw(mutex), 0, LOCK_WAIT_MS) }.0 == 0,
            Access::Named(mutex) => got(mutex.0, LOCK_WAIT_MS),
            Access::Open => true,
        }
    }

    pub fn release(&self) {
        match &self.0 {
            Access::Keyed(mutex) => {
                // SAFETY: a live keyed mutex this thread acquired
                unsafe {
                    let _ = mutex.ReleaseSync(0);
                }
            }
            Access::Named(mutex) => {
                // SAFETY: this thread got the mutex in acquire
                unsafe {
                    let _ = ReleaseMutex(mutex.0);
                }
            }
            Access::Open => {}
        }
    }
}

/// A texture a sender shared, opened on a device of ours.
pub struct SharedTexture {
    texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// The DXGI format number.
    pub format: u32,
    /// Whether it has a keyed mutex.
    pub keyed: bool,
}

// SAFETY: moved between threads whole and used by one at a time, as the device it belongs to is
unsafe impl Send for SharedTexture {}

/// A texture the CPU can read, the size and format of one shared, that copies of it are made into.
pub struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
}

// SAFETY: as SharedTexture
unsafe impl Send for Staging {}

/// One plane of the converter's picture: the target the shader draws it in and the staging texture it is read back from.
struct Plane {
    target: ID3D11Texture2D,
    view: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

/// What turns pictures of one size and format into I420 on the card (a copy of the sender's texture, the shaders, a target per plane), used with the Gpu that made it.
pub struct Converter {
    source: ID3D11Texture2D,
    source_view: ID3D11ShaderResourceView,
    vertex: ID3D11VertexShader,
    luma_shader: ID3D11PixelShader,
    chroma_shader: ID3D11PixelShader,
    raster: ID3D11RasterizerState,
    luma: Plane,
    u: Plane,
    v: Plane,
    width: u32,
    height: u32,
    /// Makes the next convert fail, for the tests of what comes after a failure.
    #[cfg(test)]
    pub(crate) fail_next: std::sync::atomic::AtomicBool,
}

// SAFETY: as SharedTexture
unsafe impl Send for Converter {}

/// A D3D11 device and its immediate context.
pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// For the log: which graphics adapter this is.
    pub adapter: String,
    /// The adapter's locally unique id (0 when it can't be told), which tells two cards of one laptop apart.
    pub luid: u64,
    /// How many of the next map attempts report the card as still drawing, for the tests of waiting.
    #[cfg(test)]
    pub(crate) busy: std::sync::atomic::AtomicU32,
}

// SAFETY: moved between threads whole and used by one at a time, which D3D11's immediate context allows
unsafe impl Send for Gpu {}

/// The locally unique id of the adapter a device is on, 0 when it can't be had.
fn luid_of(device: &ID3D11Device) -> u64 {
    // SAFETY: plain COM calls on the device and the objects it gives back
    let desc = unsafe { device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetDesc()) };
    desc.map_or(0, |d| (u64::from(d.AdapterLuid.HighPart as u32) << 32) | u64::from(d.AdapterLuid.LowPart))
}

impl Gpu {
    /// A device on graphics adapter number `adapter` of the system, or on the default one for None.
    pub fn new(adapter: Option<u32>) -> Result<Gpu, String> {
        let (chosen, driver, name) = match adapter {
            None => (None, D3D_DRIVER_TYPE_HARDWARE, "the default adapter".to_string()),
            Some(index) => {
                // SAFETY: plain COM calls on objects made here
                let (found, name) = unsafe {
                    let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
                    let found = factory.EnumAdapters1(index).map_err(|e| format!("adapter {index}: {e}"))?;
                    let desc = found.GetDesc1().map_err(|e| format!("adapter {index}: {e}"))?;
                    let end = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
                    (found.cast::<IDXGIAdapter>().map_err(|e| format!("adapter {index}: {e}"))?, String::from_utf16_lossy(&desc.Description[..end]))
                };
                (Some(found), D3D_DRIVER_TYPE_UNKNOWN, name)
            }
        };
        let levels: [D3D_FEATURE_LEVEL; 3] = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
        let (mut device, mut context) = (None, None);
        // SAFETY: the out pointers are valid Options, the adapter (if any) is alive for the call
        unsafe {
            D3D11CreateDevice(chosen.as_ref(), driver, HMODULE(std::ptr::null_mut()), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&levels), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))
                .map_err(|e| format!("Direct3D on {name}: {e}"))?;
        }
        match (device, context) {
            (Some(device), Some(context)) => {
                let luid = luid_of(&device);
                Ok(Gpu { device, context, adapter: name, luid, #[cfg(test)] busy: std::sync::atomic::AtomicU32::new(0) })
            }
            _ => Err(format!("Direct3D on {name} gave no device")),
        }
    }

    /// How many graphics adapters the system has.
    pub fn adapter_count() -> u32 {
        // SAFETY: plain COM calls on a factory made here
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return 0 };
            (0..).take_while(|i| factory.EnumAdapters1(*i).is_ok()).count() as u32
        }
    }

    /// The texture behind a share handle a sender published, if this device is on the adapter the sender's texture lives on.
    pub fn open_shared(&self, handle: u32) -> Result<SharedTexture, String> {
        let mut opened: Option<ID3D11Texture2D> = None;
        // SAFETY: a share handle is only a number, which the device checks; opened is a valid out Option
        unsafe { self.device.OpenSharedResource(HANDLE(handle as usize as *mut c_void), &mut opened) }.map_err(|e| format!("opening the shared texture: {e}"))?;
        let texture = opened.ok_or("opening the shared texture gave nothing")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc is a valid out buffer
        unsafe { texture.GetDesc(&mut desc) };
        Ok(SharedTexture {
            texture, width: desc.Width, height: desc.Height, format: desc.Format.0 as u32, keyed: desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32 != 0,
        })
    }

    /// A staging texture to read copies of `like` from, whose pixels are `bytes_per_pixel` bytes each.
    pub fn staging(&self, like: &SharedTexture, bytes_per_pixel: u32) -> Result<Staging, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: like.width,
            Height: like.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT(like.format as i32),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut made: Option<ID3D11Texture2D> = None;
        // SAFETY: desc is valid for the call and made is a valid out Option
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut made)) }.map_err(|e| format!("making the staging texture: {e}"))?;
        Ok(Staging { texture: made.ok_or("making the staging texture gave nothing")?, width: like.width, height: like.height, bytes_per_pixel })
    }

    /// Copies the shared texture into the staging one and sends it to the graphics card, which the caller's lock has to cover; the copy is
    /// done by the time `read` returns.
    pub fn copy(&self, into: &Staging, from: &SharedTexture) {
        // SAFETY: both textures belong to this device and match in size and format
        unsafe {
            self.context.CopySubresourceRegion(&into.texture, 0, 0, 0, 0, &from.texture, 0, None);
            self.context.Flush();
        }
    }

    /// The staging texture's pixels, rows packed with no padding, once the copy into it has finished; gives up at the deadline or when its stop is set.
    pub fn read(&self, from: &Staging, deadline: &Deadline) -> Result<Vec<u8>, ReadError> {
        let mapped = self.map_when_ready(&from.texture, deadline, "reading the texture")?;
        let row = from.width as usize * from.bytes_per_pixel as usize;
        let mut out = Vec::with_capacity(row * from.height as usize);
        for y in 0..from.height as usize {
            // SAFETY: a mapped texture has `height` rows RowPitch bytes apart, each holding at least `row` bytes
            out.extend_from_slice(unsafe { std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row) });
        }
        // SAFETY: mapped above and unmapped once
        unsafe { self.context.Unmap(&from.texture, 0) };
        Ok(out)
    }

    /// Maps a staging texture for reading as soon as the card is done with it. A plain Map would block with no way out when the card stalls, so this asks again
    /// with DO_NOT_WAIT until the deadline or the stop; the work is flushed first, as a poll never makes the card start it.
    fn map_when_ready(&self, texture: &ID3D11Texture2D, deadline: &Deadline, what: &str) -> Result<D3D11_MAPPED_SUBRESOURCE, ReadError> {
        // SAFETY: flushing the immediate context has no preconditions
        unsafe { self.context.Flush() };
        let polled = poll(deadline, || {
            #[cfg(test)]
            if self.busy.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                self.busy.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return None;
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: mapped is a valid out buffer and the texture is a staging texture of this device
            match unsafe { self.context.Map(texture, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut mapped)) } {
                Ok(()) => Some(Ok(mapped)),
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => None,
                Err(e) => Some(Err(format!("{what}: {e}"))),
            }
        });
        match polled {
            Polled::Ready(Ok(mapped)) => Ok(mapped),
            Polled::Ready(Err(why)) => Err(ReadError::Failed(why)),
            Polled::Cancelled => Err(ReadError::Cancelled),
            Polled::TimedOut => Err(ReadError::TimedOut),
        }
    }

    /// Whether the device has been removed or reset, as a driver does after a fault; nothing on it works any more.
    pub fn lost(&self) -> bool {
        // SAFETY: a plain query of the device
        unsafe { self.device.GetDeviceRemovedReason() }.is_err()
    }
}

impl Gpu {
    /// A texture of this device.
    fn make_texture(&self, (width, height): (u32, u32), format: DXGI_FORMAT, usage: D3D11_USAGE, bind: u32, cpu_access: u32) -> Result<ID3D11Texture2D, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: usage,
            BindFlags: bind,
            CPUAccessFlags: cpu_access,
            MiscFlags: 0,
        };
        let mut made: Option<ID3D11Texture2D> = None;
        // SAFETY: desc is valid for the call and made is a valid out Option
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut made)) }.map_err(|e| format!("making a {width}x{height} texture: {e}"))?;
        made.ok_or_else(|| "making a texture gave nothing".to_string())
    }

    fn make_plane(&self, size: (u32, u32)) -> Result<Plane, String> {
        let target = self.make_texture(size, DXGI_FORMAT_R8_UNORM, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0)?;
        let staging = self.make_texture(size, DXGI_FORMAT_R8_UNORM, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32)?;
        let mut made: Option<ID3D11RenderTargetView> = None;
        // SAFETY: target is a live render target texture of this device and made is a valid out Option
        unsafe { self.device.CreateRenderTargetView(&target, None, Some(&mut made)) }.map_err(|e| format!("making a render target: {e}"))?;
        let view = made.ok_or("making a render target gave nothing")?;
        Ok(Plane { target, view, staging, width: size.0, height: size.1 })
    }

    /// What converts pictures like `like` to I420 on the card; made once for a texture.
    pub fn converter(&self, like: &SharedTexture) -> Result<Converter, String> {
        let (typeless, view_format) = planes::card_formats(like.format).ok_or_else(|| format!("the card can't convert DXGI format {}", like.format))?;
        let (width, height) = (like.width, like.height);
        if width == 0 || height == 0 {
            return Err("the picture has no size".into());
        }
        let (cw, ch) = planes::chroma_size(width, height);
        let source = self.make_texture((width, height), DXGI_FORMAT(typeless as i32), D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE.0 as u32, 0)?;
        let view_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT(view_format as i32),
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
        };
        let mut made_view: Option<ID3D11ShaderResourceView> = None;
        // SAFETY: source is a live texture of this device, view_desc is valid for the call and made_view is a valid out Option
        unsafe { self.device.CreateShaderResourceView(&source, Some(&view_desc), Some(&mut made_view)) }.map_err(|e| format!("making the shader's view: {e}"))?;
        let source_view = made_view.ok_or("making the shader's view gave nothing")?;
        let (mut vertex, mut luma_shader, mut chroma_shader): (Option<ID3D11VertexShader>, Option<ID3D11PixelShader>, Option<ID3D11PixelShader>) = (None, None, None);
        // SAFETY: the bytecode slices live for the whole program and the out Options are valid
        unsafe {
            self.device.CreateVertexShader(SHADER_VERTEX, None::<&ID3D11ClassLinkage>, Some(&mut vertex)).map_err(|e| format!("making the vertex shader: {e}"))?;
            self.device.CreatePixelShader(SHADER_LUMA, None::<&ID3D11ClassLinkage>, Some(&mut luma_shader)).map_err(|e| format!("making the luma shader: {e}"))?;
            self.device.CreatePixelShader(SHADER_CHROMA, None::<&ID3D11ClassLinkage>, Some(&mut chroma_shader)).map_err(|e| format!("making the chroma shader: {e}"))?;
        }
        let raster_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..D3D11_RASTERIZER_DESC::default()
        };
        let mut raster: Option<ID3D11RasterizerState> = None;
        // SAFETY: raster_desc is valid for the call and raster is a valid out Option
        unsafe { self.device.CreateRasterizerState(&raster_desc, Some(&mut raster)) }.map_err(|e| format!("making the rasterizer state: {e}"))?;
        Ok(Converter {
            source,
            source_view,
            vertex: vertex.ok_or("making the vertex shader gave nothing")?,
            luma_shader: luma_shader.ok_or("making the luma shader gave nothing")?,
            chroma_shader: chroma_shader.ok_or("making the chroma shader gave nothing")?,
            raster: raster.ok_or("making the rasterizer state gave nothing")?,
            luma: self.make_plane((width, height))?,
            u: self.make_plane((cw, ch))?,
            v: self.make_plane((cw, ch))?,
            width,
            height,
            #[cfg(test)]
            fail_next: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Copies the shared texture into the converter's own and sends it to the card; the caller's lock covers this and can be let go after, as the rest reads the copy.
    pub fn copy_to_converter(&self, into: &Converter, from: &SharedTexture) -> Result<(), String> {
        if (from.width, from.height) != (into.width, into.height) {
            return Err(format!("the texture is {}x{} but the converter {}x{}", from.width, from.height, into.width, into.height));
        }
        // SAFETY: both textures belong to this device, match in size and are of one format family
        unsafe {
            self.context.CopySubresourceRegion(&into.source, 0, 0, 0, 0, &from.texture, 0, None);
            self.context.Flush();
        }
        Ok(())
    }

    /// The picture last copied into the converter as I420 (luma, U, V, packed with no padding), made by the shaders; waits for the card to finish, until the deadline or the stop at the most.
    pub fn convert(&self, converter: &Converter, deadline: &Deadline) -> Result<Vec<u8>, ReadError> {
        #[cfg(test)]
        if converter.fail_next.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(ReadError::Failed("made to fail by a test".into()));
        }
        let viewport = |plane: &Plane| D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: plane.width as f32, Height: plane.height as f32, MinDepth: 0.0, MaxDepth: 1.0 };
        let context = &self.context;
        // SAFETY: everything bound is a live object of this device that the converter keeps, the slices live through each call, and the state is cleared before the converter can go
        unsafe {
            context.IASetInputLayout(None::<&ID3D11InputLayout>);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&converter.vertex, None);
            context.RSSetState(&converter.raster);
            context.PSSetShaderResources(0, Some(&[Some(converter.source_view.clone())]));
            context.OMSetRenderTargets(Some(&[Some(converter.luma.view.clone())]), None::<&ID3D11DepthStencilView>);
            context.RSSetViewports(Some(&[viewport(&converter.luma)]));
            context.PSSetShader(&converter.luma_shader, None);
            context.Draw(3, 0);
            context.OMSetRenderTargets(Some(&[Some(converter.u.view.clone()), Some(converter.v.view.clone())]), None::<&ID3D11DepthStencilView>);
            context.RSSetViewports(Some(&[viewport(&converter.u)]));
            context.PSSetShader(&converter.chroma_shader, None);
            context.Draw(3, 0);
            context.ClearState();
            for plane in [&converter.luma, &converter.u, &converter.v] {
                context.CopyResource(&plane.staging, &plane.target);
            }
        }
        let mut out = Vec::with_capacity(planes::i420_len(converter.width, converter.height));
        for plane in [&converter.luma, &converter.u, &converter.v] {
            self.read_plane(plane, deadline, &mut out)?;
        }
        Ok(out)
    }

    /// Appends one plane's bytes, rows packed with no padding.
    fn read_plane(&self, plane: &Plane, deadline: &Deadline, out: &mut Vec<u8>) -> Result<(), ReadError> {
        let mapped = self.map_when_ready(&plane.staging, deadline, "reading a plane")?;
        let (row, height, pitch) = (plane.width as usize, plane.height as usize, mapped.RowPitch as usize);
        let packed = if mapped.pData.is_null() {
            None
        } else {
            // SAFETY: a mapped texture holds `height` rows `RowPitch` bytes apart, each at least `row` bytes, and this stops short of the padding after the last
            let bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, planes::mapped_len(pitch, row, height)) };
            planes::pack_rows(bytes, pitch, row, height, out)
        };
        // SAFETY: mapped above and unmapped once, whatever came of the packing
        unsafe { self.context.Unmap(&plane.staging, 0) };
        packed.ok_or_else(|| ReadError::Failed(format!("a plane's rows are {pitch} bytes apart, shorter than the {row} they hold")))
    }

    /// Copies the picture last copied into the converter into the BGRA staging texture, for a frame the shaders failed on (`read` waits for it).
    pub fn copy_source(&self, into: &Staging, from: &Converter) {
        // SAFETY: both textures belong to this device and match in size and format family
        unsafe {
            self.context.CopyResource(&into.texture, &from.source);
        }
    }
}

/// A device that can open the texture behind `handle`, with the texture: the default adapter first, then each of the others, since the
/// sender's texture lives on one adapter and a laptop has two.
pub fn open_shared_texture(handle: u32) -> Result<(Gpu, SharedTexture), String> {
    let mut last = "no graphics adapter".to_string();
    let adapters = std::iter::once(None).chain((0..Gpu::adapter_count()).map(Some));
    for adapter in adapters {
        match Gpu::new(adapter).and_then(|gpu| gpu.open_shared(handle).map(|texture| (gpu, texture))) {
            Ok(found) => return Ok(found),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
