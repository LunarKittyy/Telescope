//! Test builds only: every allocation gets a canary tail, and one found changed when it's freed is noted by its size.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

const CANARY: usize = 32;
const FILL: u8 = 0xA5;
const SLOTS: usize = 32;

static OVERRUNS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Guard;

// SAFETY: a thin wrapper over System that asks for CANARY more bytes than the caller sees
unsafe impl GlobalAlloc for Guard {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(Layout::from_size_align_unchecked(layout.size() + CANARY, layout.align()));
        if !p.is_null() {
            std::ptr::write_bytes(p.add(layout.size()), FILL, CANARY);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        let tail = std::slice::from_raw_parts(p.add(layout.size()), CANARY);
        if tail.iter().any(|&b| b != FILL) {
            // A fixed table, since allocating here would come straight back to this function
            let at = NEXT.fetch_add(1, Ordering::Relaxed);
            if at < SLOTS {
                OVERRUNS[at].store(layout.size(), Ordering::Relaxed);
            }
        }
        System.dealloc(p, Layout::from_size_align_unchecked(layout.size() + CANARY, layout.align()));
    }
}

/// Whether an allocation of exactly this many bytes was freed with its tail written to.
pub fn overran(size: usize) -> bool {
    OVERRUNS.iter().any(|s| s.load(Ordering::Relaxed) == size)
}
