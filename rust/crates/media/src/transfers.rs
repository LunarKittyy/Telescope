//! How often pictures cross between the processor and the graphics card, the app over. A crossing costs far more than anything a
//! shader does, so later work and the stress test read this, and each stream's pipeline logs what a frame costs when that changes.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES: AtomicU64 = AtomicU64::new(0);
static UP: AtomicU64 = AtomicU64::new(0);
static DOWN: AtomicU64 = AtomicU64::new(0);
static MASK_UP: AtomicU64 = AtomicU64::new(0);
static DECODED_DOWN: AtomicU64 = AtomicU64::new(0);
static PREVIEW_UP: AtomicU64 = AtomicU64::new(0);
static PREVIEW_DOWN: AtomicU64 = AtomicU64::new(0);

/// Totals since the app started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfers {
    /// Frames through a pipeline.
    pub frames: u64,
    /// A frame step's picture going up to the card.
    pub up: u64,
    /// A frame step's picture coming back.
    pub down: u64,
    /// A person mask going up.
    pub mask_up: u64,
    /// A picture a card decoder made coming down.
    pub decoded_down: u64,
    /// A picture going up to be shown.
    pub preview_up: u64,
    /// A shown picture read back (a preview that isn't drawn straight on the card).
    pub preview_down: u64,
}

pub fn totals() -> Transfers {
    let read = |n: &AtomicU64| n.load(Ordering::Relaxed);
    Transfers {
        frames: read(&FRAMES),
        up: read(&UP),
        down: read(&DOWN),
        mask_up: read(&MASK_UP),
        decoded_down: read(&DECODED_DOWN),
        preview_up: read(&PREVIEW_UP),
        preview_down: read(&PREVIEW_DOWN),
    }
}

pub(crate) fn frame(up: u32, down: u32) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    UP.fetch_add(up as u64, Ordering::Relaxed);
    DOWN.fetch_add(down as u64, Ordering::Relaxed);
}

pub(crate) fn mask_up() {
    MASK_UP.fetch_add(1, Ordering::Relaxed);
}

#[cfg_attr(not(ffmpeg), allow(dead_code))]
pub(crate) fn decoded_down() {
    DECODED_DOWN.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn preview_up() {
    PREVIEW_UP.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn preview_down() {
    PREVIEW_DOWN.fetch_add(1, Ordering::Relaxed);
}
