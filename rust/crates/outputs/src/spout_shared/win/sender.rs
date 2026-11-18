//! A Spout sender's half of Spout2 on Windows: a shareable texture on a device of ours, the file mapping that tells receivers about it, the entry in the sender list, the
//! mutex that keeps receivers from reading mid-write and the frame counter. Gone from the list and closed when dropped.
//!
//! What this leaves out against Spout2's own sender (2.007, from memory): it doesn't make the "ActiveSenderName" mapping, so Spout's "use the active sender" receivers
//! won't pick it, and it never asks whether the texture is shared with a keyed mutex (it isn't, the named access mutex is used).

use super::super::protocol::{self, SharedTextureInfo};
use super::{got, wide, Gpu, Owned, LOCK_WAIT_MS};
use ::windows::core::{Interface, PCWSTR};
use ::windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE};
use ::windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use ::windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use ::windows::Win32::Graphics::Dxgi::IDXGIResource;
use ::windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use ::windows::Win32::System::Threading::{CreateMutexW, CreateSemaphoreW, ReleaseMutex, ReleaseSemaphore};
use std::ffi::c_void;

/// How many names Spout's sender list holds when the first sender makes it.
const LIST_CAPACITY: usize = 10;
/// The DXGI number of the texture's format, B8G8R8A8_UNORM.
const FORMAT: u32 = 87;

/// A writable named file mapping with the mutex Spout keeps beside it.
struct Mapping {
    _map: Owned,
    view: *mut c_void,
    size: usize,
    mutex: Owned,
    /// Whether another sender or program had made the mapping before this call.
    existed: bool,
}

// SAFETY: the view maps a kernel object and stays valid from any thread until drop; access goes through the mutex or a byte copy
unsafe impl Send for Mapping {}

fn mutex_named(name: &str) -> Result<Owned, String> {
    let wide_name = wide(name);
    // SAFETY: NUL-terminated name that outlives the call
    unsafe { CreateMutexW(None, false, PCWSTR(wide_name.as_ptr())) }.map(Owned).map_err(|e| format!("making the lock {name}: {e}"))
}

impl Mapping {
    /// The mapping called `name` with at least `bytes` bytes (made if nothing has), and its mutex.
    fn create(name: &str, bytes: usize) -> Result<Mapping, String> {
        let wide_name = wide(name);
        // SAFETY: NUL-terminated name that outlives the call; no file backs the mapping
        let map = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, bytes as u32, PCWSTR(wide_name.as_ptr())) }
            .map(Owned)
            .map_err(|e| format!("making the shared memory {name}: {e}"))?;
        // SAFETY: read straight after the call it describes, on the same thread
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: a live mapping handle; the whole mapping is wanted
        let view = unsafe { MapViewOfFile(map.0, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            return Err(format!("mapping the shared memory {name}"));
        }
        // From here the view is unmapped by Drop
        let mutex = mutex_named(&protocol::map_mutex(name));
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: info is an out buffer of the size given and the view is mapped
        let filled = unsafe { VirtualQuery(Some(view.Value as *const c_void), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        let memory = Mapping { _map: map, view: view.Value, size: if filled == 0 { 0 } else { info.RegionSize }, mutex: mutex?, existed };
        if memory.size < bytes {
            return Err(format!("the shared memory {name} is smaller than a sender needs"));
        }
        Ok(memory)
    }

    /// Runs `f` on the mapped bytes while holding the mapping's mutex; None if its holder doesn't let go in time.
    fn locked<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        if !got(self.mutex.0, LOCK_WAIT_MS) {
            return None;
        }
        // SAFETY: the view is `size` bytes, stays mapped while self lives, and the mutex held here keeps other Spout programs from writing it meanwhile
        let out = f(unsafe { std::slice::from_raw_parts_mut(self.view as *mut u8, self.size) });
        // SAFETY: this thread got the mutex just above
        unsafe {
            let _ = ReleaseMutex(self.mutex.0);
        }
        Some(out)
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the view was mapped by create and is unmapped once, here
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view });
        }
    }
}

/// A sender that shares BGRA pictures of one size.
pub struct TextureSender {
    gpu: Gpu,
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    name: String,
    list: Mapping,
    _info: Mapping,
    access: Owned,
    count: Owned,
    listed: bool,
}

// SAFETY: moved between threads whole and used by one at a time, as the device it belongs to is
unsafe impl Send for TextureSender {}

impl TextureSender {
    /// Makes the sender `name` for pictures of this size and lists it; Err when the name is taken, the list is full or the card won't share a texture.
    pub fn new(name: &str, width: u32, height: u32) -> Result<TextureSender, String> {
        if name.is_empty() || name.len() >= protocol::NAME_LEN || name.contains('\0') {
            return Err("the sender's name doesn't fit Spout's list".into());
        }
        if width == 0 || height == 0 {
            return Err("the picture has no size".into());
        }
        let gpu = Gpu::new(None)?;
        let texture = make_shared_texture(&gpu, width, height)?;
        let handle = share_handle(&texture)?;
        let access = mutex_named(&protocol::access_mutex(name))?;
        let count_name = wide(&protocol::count_semaphore(name));
        // The count starts at 1 so receivers read 0 until a first frame, as Spout's own counter does
        // SAFETY: NUL-terminated name that outlives the call
        let count = unsafe { CreateSemaphoreW(None, 1, i32::MAX, PCWSTR(count_name.as_ptr())) }.map(Owned).map_err(|e| format!("making the frame counter: {e}"))?;
        let mut info = Mapping::create(name, SharedTextureInfo::SIZE)?;
        if info.existed {
            return Err(format!("a sender called {name} is already running"));
        }
        let mut description = [0u8; 256];
        let program = std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_string)).unwrap_or_else(|| "Telescope".to_string());
        let mut end = program.len().min(255);
        while !program.is_char_boundary(end) {
            end -= 1;
        }
        description[..end].copy_from_slice(&program.as_bytes()[..end]);
        let bytes = SharedTextureInfo { share_handle: handle, width, height, format: FORMAT, usage: 0, description, partner_id: 0 }.to_bytes();
        info.locked(|memory| memory[..bytes.len()].copy_from_slice(&bytes)).ok_or("the sender's own memory is locked")?;
        let list = Mapping::create(protocol::NAMES_MAP, LIST_CAPACITY * protocol::NAME_LEN)?;
        let mut sender = TextureSender { gpu, texture, width, height, name: name.to_string(), list, _info: info, access, count, listed: false };
        sender.list_name()?;
        Ok(sender)
    }

    /// Adds the name to the sender list.
    fn list_name(&mut self) -> Result<(), String> {
        let name = self.name.clone();
        self.list
            .locked(|memory| {
                // Spout's default of ten, though the mapping is a page long, so no sender takes a place a program with the default would not have
                let slots = LIST_CAPACITY.min(memory.len() / protocol::NAME_LEN);
                let names = protocol::parse_names(memory);
                if names.contains(&name) {
                    return Err(format!("a sender called {name} is already listed"));
                }
                if names.len() >= slots {
                    return Err("Spout's list of senders is full".to_string());
                }
                let at = names.len() * protocol::NAME_LEN;
                memory[at..at + protocol::NAME_LEN].fill(0);
                memory[at..at + name.len()].copy_from_slice(name.as_bytes());
                Ok(())
            })
            .ok_or("Spout's list of senders is busy")??;
        self.listed = true;
        Ok(())
    }

    /// Takes the name off the sender list, closing the gap.
    fn unlist_name(&mut self) {
        let name = self.name.clone();
        let _ = self.list.locked(|memory| {
            let mut names = protocol::parse_names(memory);
            let Some(at) = names.iter().position(|n| *n == name) else { return };
            names.remove(at);
            memory.fill(0);
            for (i, n) in names.iter().enumerate() {
                memory[i * protocol::NAME_LEN..i * protocol::NAME_LEN + n.len()].copy_from_slice(n.as_bytes());
            }
        });
    }

    /// Puts the next picture in the shared texture (B, G, R, A per pixel, top row first, at the size given to `new`) and counts the frame. A picture a receiver
    /// is still reading from is dropped, as the next one is a frame away.
    pub fn send(&mut self, bgra: &[u8]) -> Result<(), String> {
        let (row, rows) = (self.width as usize * 4, self.height as usize);
        if bgra.len() != row * rows {
            return Err(format!("a picture of {} bytes for a {}x{} sender", bgra.len(), self.width, self.height));
        }
        if self.gpu.lost() {
            return Err("the graphics card was reset".into());
        }
        if !got(self.access.0, LOCK_WAIT_MS) {
            return Ok(());
        }
        // SAFETY: the texture is a default-usage texture of this device and bgra holds `rows` rows of `row` bytes, as the pitch says
        unsafe {
            self.gpu.context.UpdateSubresource(&self.texture, 0, None, bgra.as_ptr() as *const c_void, row as u32, 0);
            self.gpu.context.Flush();
        }
        // SAFETY: this thread got the mutex above
        unsafe {
            let _ = ReleaseMutex(self.access.0);
        }
        // SAFETY: a live semaphore; a count at its limit fails, which a frame counter can do without
        unsafe {
            let _ = ReleaseSemaphore(self.count.0, 1, None);
        }
        Ok(())
    }
}

impl Drop for TextureSender {
    fn drop(&mut self) {
        if self.listed {
            self.unlist_name();
        }
    }
}

/// A texture a receiver can open by share handle.
fn make_shared_texture(gpu: &Gpu, width: u32, height: u32) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
    };
    let mut made: Option<ID3D11Texture2D> = None;
    // SAFETY: desc is valid for the call and made is a valid out Option
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut made)) }.map_err(|e| format!("making the {width}x{height} shared texture: {e}"))?;
    made.ok_or_else(|| "making the shared texture gave nothing".to_string())
}

/// The legacy share handle of a texture, which Spout publishes as 32 bits.
fn share_handle(texture: &ID3D11Texture2D) -> Result<u32, String> {
    // SAFETY: plain COM calls on a live texture
    let handle: HANDLE = unsafe { texture.cast::<IDXGIResource>().and_then(|r| r.GetSharedHandle()) }.map_err(|e| format!("sharing the texture: {e}"))?;
    Ok(handle.0 as usize as u32)
}
