//! Experiment: a counting allocator that samples live allocations with their call stacks and dumps them to LEAK_HEAPS.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering::Relaxed};

const SLOTS: usize = 1 << 16;
const FRAMES: usize = 8;
const EVERY: usize = 256;

struct Slot { ptr: AtomicUsize, size: AtomicUsize, frames: [AtomicUsize; FRAMES] }
#[allow(clippy::declare_interior_mutable_const)]
const SLOT: Slot = Slot { ptr: AtomicUsize::new(0), size: AtomicUsize::new(0), frames: [const { AtomicUsize::new(0) }; FRAMES] };
static TABLE: [Slot; SLOTS] = [SLOT; SLOTS];
static ON: AtomicBool = AtomicBool::new(false);
static TICK: AtomicUsize = AtomicUsize::new(0);
static NET: AtomicI64 = AtomicI64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);

extern "system" {
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut core::ffi::c_void, hash: *mut u32) -> u16;
    fn GetModuleHandleExW(flags: u32, addr: *const core::ffi::c_void, out: *mut *mut core::ffi::c_void) -> i32;
    fn GetModuleFileNameW(module: *mut core::ffi::c_void, name: *mut u16, size: u32) -> u32;
}

pub struct Probe;

fn home(ptr: usize) -> usize { (ptr >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40 & (SLOTS - 1) }

fn sample(ptr: *mut u8, size: usize) {
    let mut frames = [std::ptr::null_mut(); FRAMES];
    // SAFETY: the buffer holds FRAMES entries
    let n = unsafe { RtlCaptureStackBackTrace(2, FRAMES as u32, frames.as_mut_ptr(), std::ptr::null_mut()) } as usize;
    let h = home(ptr as usize);
    for i in 0..16 {
        let s = &TABLE[(h + i) & (SLOTS - 1)];
        if s.ptr.compare_exchange(0, ptr as usize, Relaxed, Relaxed).is_ok() {
            s.size.store(size, Relaxed);
            for (k, f) in s.frames.iter().enumerate() {
                f.store(if k < n { frames[k] as usize } else { 0 }, Relaxed);
            }
            return;
        }
    }
}

fn unsample(ptr: *mut u8) {
    let h = home(ptr as usize);
    for i in 0..16 {
        let s = &TABLE[(h + i) & (SLOTS - 1)];
        if s.ptr.load(Relaxed) == ptr as usize {
            s.ptr.store(0, Relaxed);
            return;
        }
    }
}

// SAFETY: every call forwards to the system allocator; the extra bookkeeping only touches atomics
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            NET.fetch_add(l.size() as i64, Relaxed);
            LIVE.fetch_add(1, Relaxed);
            if ON.load(Relaxed) && TICK.fetch_add(1, Relaxed) % EVERY == 0 {
                sample(p, l.size());
            }
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Relaxed);
        LIVE.fetch_sub(1, Relaxed);
        if ON.load(Relaxed) {
            unsample(p);
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            NET.fetch_add(new as i64 - l.size() as i64, Relaxed);
            if ON.load(Relaxed) {
                unsample(p);
            }
        }
        q
    }
}

fn module_of(addr: usize) -> String {
    let mut m = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS (4) | UNCHANGED_REFCOUNT (2) looks the module up without holding it
    if unsafe { GetModuleHandleExW(6, addr as *const _, &mut m) } == 0 {
        return "?".into();
    }
    let mut buf = [0u16; 260];
    // SAFETY: the buffer holds 260 units
    let n = unsafe { GetModuleFileNameW(m, buf.as_mut_ptr(), 260) } as usize;
    let name = String::from_utf16_lossy(&buf[..n]);
    format!("{}+{:#x}", name.rsplit('\\').next().unwrap_or("?"), addr - m as usize)
}

/// Starts the sampler; every 20 s the live samples are summed by their top frames and appended to `path`.
pub fn start(path: std::ffi::OsString) {
    ON.store(true, Relaxed);
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(20));
            let mut by: std::collections::HashMap<String, (usize, usize)> = Default::default();
            let mut total = (0usize, 0usize);
            for s in TABLE.iter() {
                if s.ptr.load(Relaxed) == 0 { continue; }
                let size = s.size.load(Relaxed);
                let f: Vec<usize> = s.frames.iter().map(|f| f.load(Relaxed)).filter(|&a| a != 0).collect();
                let key = f.iter().take(4).map(|&a| module_of(a)).collect::<Vec<_>>().join(" < ");
                let e = by.entry(key).or_default();
                e.0 += 1;
                e.1 += size;
                total.0 += 1;
                total.1 += size;
            }
            let mut rows: Vec<_> = by.into_iter().collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.1 .1));
            let mut out = format!("t={} net_kb={} live={} samples={} sampled_kb={}\n", t0.elapsed().as_secs(), NET.load(Relaxed) >> 10, LIVE.load(Relaxed), total.0, total.1 >> 10);
            for (k, (c, b)) in rows.iter().take(14) {
                out += &format!("  {c} {}kb {k}\n", b >> 10);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                use std::io::Write;
                let _ = f.write_all(out.as_bytes());
            }
        }
    });
}
