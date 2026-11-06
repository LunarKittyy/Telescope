//! Windows: kernel32 and the registry by hand, so there's nothing extra to build (as platform/windows.py and vcam.py
//! do with ctypes): shared memory, named events and mutexes, and which camera is registered.

#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type Handle = *mut c_void;
type HKey = *mut c_void;

pub const FILE_MAP_WRITE: u32 = 0x0002;
pub const FILE_MAP_READ: u32 = 0x0004;
pub const SYNCHRONIZE: u32 = 0x0010_0000;
pub const EVENT_MODIFY_STATE: u32 = 0x0002;
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_ABANDONED: u32 = 0x80;

const HKEY_CLASSES_ROOT: HKey = 0x8000_0000_usize as HKey;
const HKEY_LOCAL_MACHINE: HKey = 0x8000_0002_usize as HKey;
const KEY_READ: u32 = 0x20019;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;

#[repr(C)]
struct MemoryInfo {
    base: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(map: Handle, access: u32, high: u32, low: u32, bytes: usize) -> *mut c_void;
    fn UnmapViewOfFile(address: *const c_void) -> i32;
    fn VirtualQuery(address: *const c_void, info: *mut MemoryInfo, length: usize) -> usize;
    fn CloseHandle(handle: Handle) -> i32;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn CreateEventW(attributes: *mut c_void, manual: i32, initial: i32, name: *const u16) -> Handle;
    fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
    fn ReleaseMutex(handle: Handle) -> i32;
    fn SetEvent(handle: Handle) -> i32;
}

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

/// FOLDERID_ProgramFiles
const PROGRAM_FILES: Guid = Guid(0x905e63b6, 0xc1bf, 0x494e, [0xb2, 0x9c, 0x65, 0xb7, 0x32, 0xd3, 0xd2, 0x1a]);

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(folder: *const Guid, flags: u32, token: Handle, path: *mut *mut u16) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(memory: *mut c_void);
}

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(key: HKey, sub: *const u16, options: u32, access: u32, out: *mut HKey) -> i32;
    fn RegQueryValueExW(key: HKey, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
    fn RegEnumKeyExW(key: HKey, index: u32, name: *mut u16, len: *mut u32, reserved: *mut u32, class: *mut u16,
                     class_len: *mut u32, time: *mut c_void) -> i32;
    fn RegCloseKey(key: HKey) -> i32;
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A handle closed when dropped.
pub struct Owned(pub Handle);

unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open and is closed once
        unsafe { CloseHandle(self.0) };
    }
}

impl Owned {
    fn new(h: Handle) -> Option<Owned> {
        (!h.is_null()).then_some(Owned(h))
    }
}

/// A named mapping, mapped whole: unmapped and closed when dropped.
pub struct View {
    _map: Owned,
    address: *mut u8,
    len: usize,
}

unsafe impl Send for View {}

impl View {
    pub fn open(name: &str, access: u32) -> Option<View> {
        let n = wide(name);
        // SAFETY: plain Win32 calls on a name we own; the view's size comes from VirtualQuery
        unsafe {
            let map = Owned::new(OpenFileMappingW(access, 0, n.as_ptr()))?;
            let address = MapViewOfFile(map.0, access, 0, 0, 0);
            if address.is_null() {
                return None;
            }
            let mut info: MemoryInfo = std::mem::zeroed();
            if VirtualQuery(address, &mut info, std::mem::size_of::<MemoryInfo>()) == 0 {
                UnmapViewOfFile(address);
                return None;
            }
            Some(View { _map: map, address: address as *mut u8, len: info.region_size })
        }
    }

    pub fn buf(&mut self) -> &mut [u8] {
        // SAFETY: the view is mapped for len bytes until drop
        unsafe { std::slice::from_raw_parts_mut(self.address, self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: mapped in open, unmapped once
        unsafe { UnmapViewOfFile(self.address as *const c_void) };
    }
}

/// Whether a named mapping exists right now.
pub fn mapping_exists(name: &str) -> bool {
    let n = wide(name);
    // SAFETY: open and close straight away
    unsafe { Owned::new(OpenFileMappingW(FILE_MAP_READ, 0, n.as_ptr())).is_some() }
}

pub fn open_event(name: &str, access: u32) -> Option<Owned> {
    let n = wide(name);
    // SAFETY: plain Win32 call
    unsafe { Owned::new(OpenEventW(access, 0, n.as_ptr())) }
}

/// Opens the auto-reset event, or makes it if nobody has yet.
pub fn create_event(name: &str) -> Option<Owned> {
    let n = wide(name);
    // SAFETY: plain Win32 call
    unsafe { Owned::new(CreateEventW(std::ptr::null_mut(), 0, 0, n.as_ptr())) }
}

pub fn open_mutex(name: &str) -> Option<Owned> {
    let n = wide(name);
    // SAFETY: plain Win32 call
    unsafe { Owned::new(OpenMutexW(SYNCHRONIZE, 0, n.as_ptr())) }
}

pub fn wait(handle: &Owned, millis: u32) -> bool {
    // SAFETY: a live handle
    let got = unsafe { WaitForSingleObject(handle.0, millis) };
    // A mutex whose owner died is ours now too
    got == WAIT_OBJECT_0 || got == WAIT_ABANDONED
}

pub fn release_mutex(handle: &Owned) {
    // SAFETY: a live handle this thread holds
    unsafe { ReleaseMutex(handle.0) };
}

pub fn set_event(handle: &Owned) {
    // SAFETY: a live handle
    unsafe { SetEvent(handle.0) };
}

/// Windows' own system folder (System32), asked of Windows rather than PATH or %SystemRoot%, so the programs started
/// from it are the real ones.
pub fn system_dir() -> Option<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer's length is passed with it
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// Program Files from Windows' own setting (SHGetKnownFolderPath), not %ProgramFiles%, which whoever starts Telescope can point anywhere.
pub fn program_files() -> Option<std::path::PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: Windows allocates the string it answers with, which is read to its end and freed once
    unsafe {
        if SHGetKnownFolderPath(&PROGRAM_FILES, 0, std::ptr::null_mut(), &mut out) != 0 || out.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *out.add(i) != 0).count();
        let path = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
        CoTaskMemFree(out as *mut c_void);
        Some(std::path::PathBuf::from(path))
    }
}

/// Windows' build number (22000 and up is Windows 11), 0 if it can't be told.
pub fn windows_build() -> u32 {
    // SAFETY: RtlGetVersion fills the struct we size
    unsafe {
        let mut info: OsVersionInfo = std::mem::zeroed();
        info.size = std::mem::size_of::<OsVersionInfo>() as u32;
        if RtlGetVersion(&mut info) == 0 { info.build } else { 0 }
    }
}

struct Key(HKey);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: opened by RegOpenKeyExW
        unsafe { RegCloseKey(self.0) };
    }
}

fn open_key(root: HKey, path: &str) -> Option<Key> {
    let p = wide(path);
    let mut out: HKey = std::ptr::null_mut();
    // SAFETY: plain registry call
    (unsafe { RegOpenKeyExW(root, p.as_ptr(), 0, KEY_READ, &mut out) } == 0).then_some(Key(out))
}

fn query_string(key: &Key, name: &str) -> Option<String> {
    let n = wide(name);
    let mut kind = 0u32;
    let mut len = 0u32;
    // SAFETY: first ask the size, then read into a buffer that big
    unsafe {
        if RegQueryValueExW(key.0, n.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut len) != 0 {
            return None;
        }
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
        let mut len2 = (buf.len() * 2) as u32;
        if RegQueryValueExW(key.0, n.as_ptr(), std::ptr::null_mut(), &mut kind, buf.as_mut_ptr() as *mut u8, &mut len2) != 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

fn query_dword(key: &Key, name: &str) -> Option<u32> {
    let n = wide(name);
    let mut kind = 0u32;
    let mut value = 0u32;
    let mut len = 4u32;
    // SAFETY: a 4-byte value read into a u32
    let ok = unsafe { RegQueryValueExW(key.0, n.as_ptr(), std::ptr::null_mut(), &mut kind, &mut value as *mut u32 as *mut u8, &mut len) } == 0;
    (ok && kind == 4).then_some(value)
}

fn subkeys(key: &Key) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        // SAFETY: the buffer and its length go together
        let rc = unsafe {
            RegEnumKeyExW(key.0, i, buf.as_mut_ptr(), &mut len, std::ptr::null_mut(), std::ptr::null_mut(),
                          std::ptr::null_mut(), std::ptr::null_mut())
        };
        if rc != 0 {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

/// A string value under HKEY_CLASSES_ROOT ("" for the default value).
pub fn classes_string(path: &str, name: &str) -> Option<String> {
    query_string(&open_key(HKEY_CLASSES_ROOT, path)?, name)
}

/// A string or number value under HKEY_CLASSES_ROOT, as a number.
pub fn classes_number(path: &str, name: &str) -> Option<i64> {
    let key = open_key(HKEY_CLASSES_ROOT, path)?;
    query_dword(&key, name).map(i64::from).or_else(|| query_string(&key, name)?.trim().parse().ok())
}

pub fn machine_string(path: &str, name: &str) -> Option<String> {
    query_string(&open_key(HKEY_LOCAL_MACHINE, path)?, name)
}

pub fn machine_subkeys(path: &str) -> Vec<String> {
    open_key(HKEY_LOCAL_MACHINE, path).map(|k| subkeys(&k)).unwrap_or_default()
}

pub fn classes_subkeys(path: &str) -> Vec<String> {
    open_key(HKEY_CLASSES_ROOT, path).map(|k| subkeys(&k)).unwrap_or_default()
}
