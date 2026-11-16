//! The Windows side of Spout2 for any plugin: the file mappings and named objects senders publish (sender list, a sender's info, its
//! frame counter and texture lock) and the D3D11 device, shared textures and staging textures that read them. Only the reading half
//! is here; a sender's half (register a name, create a texture, write the info, count frames) goes next to it.

use super::protocol::{self, SharedTextureInfo};
use ::windows::core::{Interface, PCWSTR};
use ::windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, WAIT_ABANDONED, WAIT_OBJECT_0};
use ::windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0};
use ::windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use ::windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use ::windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, IDXGIKeyedMutex};
use ::windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS};
use ::windows::Win32::System::Threading::{
    OpenMutexW, OpenSemaphoreW, ReleaseMutex, ReleaseSemaphore, WaitForSingleObject, MUTEX_MODIFY_STATE, SEMAPHORE_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};
use std::ffi::c_void;

/// How long Spout's own code waits on its mutexes: four frames at 60 fps.
const LOCK_WAIT_MS: u32 = 67;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
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

/// A D3D11 device and its immediate context.
pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// For the log: which graphics adapter this is.
    pub adapter: String,
}

// SAFETY: moved between threads whole and used by one at a time, which D3D11's immediate context allows
unsafe impl Send for Gpu {}

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
            (Some(device), Some(context)) => Ok(Gpu { device, context, adapter: name }),
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

    /// The staging texture's pixels, rows packed with no padding.
    pub fn read(&self, from: &Staging) -> Result<Vec<u8>, String> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: mapped is a valid out buffer; the call waits for the copy to finish
        unsafe { self.context.Map(&from.texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| format!("reading the texture: {e}"))?;
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
