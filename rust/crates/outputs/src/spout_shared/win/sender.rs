//! A Spout sender's half of Spout2 on Windows: a shareable texture on a device of ours, the file mapping that tells receivers about it, the entry in the sender list, the
//! mutex that keeps receivers from reading mid-write and the frame counter. Gone from the list and closed when dropped.
//!
//! What this leaves out against Spout2's own sender (2.007, from memory): it never asks whether the texture is shared with a keyed mutex (it isn't, the named access
//! mutex is used), and a sender that is the active one when it ends leaves the active name empty rather than handing it to another sender.

use super::super::protocol::{self, SharedTextureInfo};
use super::{got, wide, Gpu, Owned, SharedMemory, LOCK_WAIT_MS};
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

// How many views this thread has mapped and not yet unmapped, for the test that a failed Mapping::create leaves none.
#[cfg(test)]
thread_local! {
    static VIEWS: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

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
        // Before the view, which only Mapping's Drop unmaps: a lock that can't be made must not leave a view behind
        let mutex = mutex_named(&protocol::map_mutex(name))?;
        // SAFETY: a live mapping handle; the whole mapping is wanted
        let view = unsafe { MapViewOfFile(map.0, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            return Err(format!("mapping the shared memory {name}"));
        }
        // From here the view is unmapped by Drop
        #[cfg(test)]
        VIEWS.with(|n| n.set(n.get() + 1));
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: info is an out buffer of the size given and the view is mapped
        let filled = unsafe { VirtualQuery(Some(view.Value as *const c_void), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        let memory = Mapping { _map: map, view: view.Value, size: if filled == 0 { 0 } else { info.RegionSize }, mutex, existed };
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
        #[cfg(test)]
        VIEWS.with(|n| n.set(n.get() - 1));
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
    info: Mapping,
    access: Owned,
    count: Owned,
    listed: bool,
    active: Option<Mapping>,
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
        // The sender's own mapping first: one that is there already belongs to a running sender (or one a receiver still holds) of that name
        let mut info = Mapping::create(name, SharedTextureInfo::SIZE)?;
        if info.existed {
            return Err(format!("a sender called {name} is already running"));
        }
        let gpu = Gpu::new(None)?;
        let texture = make_shared_texture(&gpu, width, height)?;
        let access = mutex_named(&protocol::access_mutex(name))?;
        let count_name = wide(&protocol::count_semaphore(name));
        // The count starts at 1 so receivers read 0 until a first frame, as Spout's own counter does
        // SAFETY: NUL-terminated name that outlives the call
        let count = unsafe { CreateSemaphoreW(None, 1, i32::MAX, PCWSTR(count_name.as_ptr())) }.map(Owned).map_err(|e| format!("making the frame counter: {e}"))?;
        write_info(&mut info, &texture, width, height)?;
        let list = Mapping::create(protocol::NAMES_MAP, LIST_CAPACITY * protocol::NAME_LEN)?;
        let active = Mapping::create(protocol::ACTIVE_MAP, protocol::NAME_LEN).ok();
        let mut sender = TextureSender { gpu, texture, width, height, name: name.to_string(), list, info, access, count, listed: false, active };
        sender.list_name()?;
        sender.claim_active();
        Ok(sender)
    }

    /// Makes this the active sender when none is, or the one named is gone.
    fn claim_active(&mut self) {
        let name = self.name.clone();
        let Some(active) = self.active.as_mut() else { return };
        let _ = active.locked(|memory| {
            let held = &memory[..protocol::NAME_LEN];
            let current = String::from_utf8_lossy(&held[..held.iter().position(|b| *b == 0).unwrap_or(held.len())]).into_owned();
            // A name whose sender has no mapping any more is a leftover
            if current.is_empty() || SharedMemory::open(&current).is_none() {
                memory[..protocol::NAME_LEN].fill(0);
                memory[..name.len()].copy_from_slice(name.as_bytes());
            }
        });
    }

    /// Empties the active name when it is this sender's.
    fn release_active(&mut self) {
        let name = self.name.clone();
        let Some(active) = self.active.as_mut() else { return };
        let _ = active.locked(|memory| {
            if protocol::parse_names(&memory[..protocol::NAME_LEN]).first() == Some(&name) {
                memory[..protocol::NAME_LEN].fill(0);
            }
        });
    }

    /// Shares pictures of another size from now on under the same name, mapping and counter: a new texture and its handle go into the existing info.
    /// Ok(false) when a receiver holds the picture just now: nothing changed, and the sender (and so its place in the list) stays as it is for another try.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<bool, String> {
        if width == 0 || height == 0 {
            return Err("the picture has no size".into());
        }
        if (width, height) == (self.width, self.height) {
            return Ok(true);
        }
        let texture = make_shared_texture(&self.gpu, width, height)?;
        // Receivers are held off while the info and the texture change together
        if !got(self.access.0, LOCK_WAIT_MS) {
            return Ok(false);
        }
        let written = write_info(&mut self.info, &texture, width, height);
        if written.is_ok() {
            self.texture = texture;
            (self.width, self.height) = (width, height);
        }
        // SAFETY: this thread got the mutex above
        unsafe {
            let _ = ReleaseMutex(self.access.0);
        }
        written.map(|()| true)
    }

    /// Adds the name to the sender list.
    fn list_name(&mut self) -> Result<(), String> {
        let name = self.name.clone();
        self.list
            .locked(|memory| {
                // Spout's default of ten, though the mapping is a page long, so no sender takes a place a program with the default would not have
                let slots = LIST_CAPACITY.min(memory.len() / protocol::NAME_LEN);
                let names = protocol::parse_names(memory);
                // A name listed with no sender behind it (its program ended before taking it off) is ours to take over
                if names.contains(&name) {
                    return Ok(());
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
            self.release_active();
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

/// Writes what receivers read about the texture into the sender's info mapping.
fn write_info(info: &mut Mapping, texture: &ID3D11Texture2D, width: u32, height: u32) -> Result<(), String> {
    let mut description = [0u8; 256];
    let program = std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_string)).unwrap_or_else(|| "Telescope".to_string());
    let mut end = program.len().min(255);
    while !program.is_char_boundary(end) {
        end -= 1;
    }
    description[..end].copy_from_slice(&program.as_bytes()[..end]);
    let bytes = SharedTextureInfo { share_handle: share_handle(texture)?, width, height, format: FORMAT, usage: 0, description, partner_id: 0 }.to_bytes();
    info.locked(|memory| memory[..bytes.len()].copy_from_slice(&bytes)).ok_or_else(|| "the sender's own memory is locked".to_string())
}

/// Holds the sender list open with a name in it that no sender backs, as a program that ended without taking its name off leaves it.
#[cfg(test)]
pub struct StaleList {
    _list: Mapping,
}

#[cfg(test)]
impl StaleList {
    pub fn with(name: &str) -> StaleList {
        let mut list = Mapping::create(protocol::NAMES_MAP, LIST_CAPACITY * protocol::NAME_LEN).expect("the sender list");
        let name = name.to_string();
        list.locked(|memory| {
            let at = protocol::parse_names(memory).len() * protocol::NAME_LEN;
            memory[at..at + name.len()].copy_from_slice(name.as_bytes());
        })
        .expect("the list's lock");
        StaleList { _list: list }
    }
}

/// A receiver that holds a sender's access mutex, as OBS does while it reads the picture, until it is dropped.
#[cfg(test)]
pub struct HeldAccess {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(test)]
impl HeldAccess {
    pub fn of(sender: &str) -> HeldAccess {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (held, ready) = std::sync::mpsc::channel::<()>();
        let name = protocol::access_mutex(sender);
        let thread = std::thread::spawn(move || {
            let mutex = mutex_named(&name).expect("the access mutex");
            assert!(got(mutex.0, 5000), "the receiver couldn't take the access mutex");
            held.send(()).unwrap();
            let _ = wait.recv();
            // SAFETY: this thread got the mutex above
            unsafe {
                let _ = ReleaseMutex(mutex.0);
            }
        });
        ready.recv().expect("the receiver to hold the mutex");
        HeldAccess { release: Some(release), thread: Some(thread) }
    }
}

#[cfg(test)]
impl Drop for HeldAccess {
    fn drop(&mut self) {
        drop(self.release.take());
        let _ = self.thread.take().unwrap().join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapping_whose_lock_cant_be_made_leaves_no_view_behind() {
        let name = "Telescope test lockless mapping";
        // A semaphore under the name the lock would take: making the mutex fails
        let taken = wide(&protocol::map_mutex(name));
        // SAFETY: NUL-terminated name that outlives the call
        let _in_the_way = unsafe { CreateSemaphoreW(None, 1, 1, PCWSTR(taken.as_ptr())) }.map(Owned).expect("a semaphore");
        let before = VIEWS.with(|n| n.get());
        for _ in 0..3 {
            assert!(Mapping::create(name, 64).is_err());
        }
        assert_eq!(VIEWS.with(|n| n.get()), before, "a failed Mapping::create left a view mapped");
    }
}
