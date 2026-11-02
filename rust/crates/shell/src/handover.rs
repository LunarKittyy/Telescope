//! view_controller's _Handover: a phone share sent while steering, until its frames arrive. The frames before it still
//! come cropped the old way, and cropping them for the new one would make the picture jump for a moment. A shader's crop
//! is fixed when it's built, so the worker asks this for each frame and swaps the view's steps on the one that shows it.

use std::sync::Mutex;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::View;
use telescope_core::view::{share_shift, PhoneZoom};

/// A new phone share that hasn't shown up in the frames by then is taken to have (seconds).
const HANDOVER_S: f64 = 1.0;
/// The size frames are compared at while a new phone share is on its way.
const THUMB: (usize, usize) = (128, 72);
/// A frame counts as the new share once it matches that this much better than the old one.
const ARRIVED: f32 = 0.6;
/// Every this many pixels of a frame go into its thumb.
const SKIP: usize = 4;

/// A frame, small, grey and blurred.
#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    px: Vec<f32>,
}

fn grey(frame: &Frame, x: usize, y: usize) -> f32 {
    let w = frame.width as usize;
    match frame.format {
        PixelFormat::I420 | PixelFormat::Nv12 => frame.data[y * w + x] as f32,
        PixelFormat::Rgba | PixelFormat::Bgra => {
            let p = &frame.data[(y * w + x) * 4..][..3];
            let (r, b) = if frame.format == PixelFormat::Rgba { (p[0], p[2]) } else { (p[2], p[0]) };
            0.299 * r as f32 + 0.587 * p[1] as f32 + 0.114 * b as f32
        }
        _ => 0.0,
    }
}

/// _thumb: every fourth pixel averaged down to THUMB, then cv2.GaussianBlur's 5 x 5.
pub fn thumb(frame: &Frame) -> Thumb {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (tw, th) = THUMB;
    let (sw, sh) = (w.div_ceil(SKIP).max(1), h.div_ceil(SKIP).max(1));
    let mut small = vec![0f32; tw * th];
    for ty in 0..th {
        let (y0, y1) = (ty * sh / th, ((ty + 1) * sh / th).max(ty * sh / th + 1).min(sh));
        for tx in 0..tw {
            let (x0, x1) = (tx * sw / tw, ((tx + 1) * sw / tw).max(tx * sw / tw + 1).min(sw));
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += grey(frame, x * SKIP, y * SKIP);
                }
            }
            small[ty * tw + tx] = sum / ((y1 - y0) * (x1 - x0)) as f32;
        }
    }
    // The edge reflects without repeating itself, as cv2's default border does
    let reflect = |i: isize, n: usize| -> usize {
        let n = n as isize;
        (if i < 0 { -i } else if i >= n { 2 * n - 2 - i } else { i }) as usize
    };
    const KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let mut across = vec![0f32; tw * th];
    for y in 0..th {
        for x in 0..tw {
            across[y * tw + x] = (0..5).map(|k| KERNEL[k] * small[y * tw + reflect(x as isize + k as isize - 2, tw)]).sum();
        }
    }
    let mut px = vec![0f32; tw * th];
    for y in 0..th {
        for x in 0..tw {
            px[y * tw + x] = (0..5).map(|k| KERNEL[k] * across[reflect(y as isize + k as isize - 2, th) * tw + x]).sum();
        }
    }
    Thumb { px }
}

/// moved_like: `now` looks like `before` moved by shift (share_shift) clearly better than like before as it was.
pub fn moved_like(before: &Thumb, now: &Thumb, (scale, dx, dy): (f64, f64, f64)) -> bool {
    let (w, h) = THUMB;
    let (tx, ty) = (dx * w as f64 + (scale - 1.0) * 0.5, dy * h as f64 + (scale - 1.0) * 0.5);
    let (mut seen, mut stayed, mut went) = (0usize, 0f32, 0f32);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = ((x as f64 - tx) / scale, (y as f64 - ty) / scale);
            if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f64 || sy > (h - 1) as f64 {
                continue;
            }
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = ((sx - x0 as f64) as f32, (sy - y0 as f64) as f32);
            let at = |x: usize, y: usize| before.px[y * w + x];
            let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
            let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
            let moved = top * (1.0 - fy) + bottom * fy;
            let shown = now.px[y * w + x];
            seen += 1;
            stayed += (shown - before.px[y * w + x]).abs();
            went += (shown - moved).abs();
        }
    }
    if seen * 5 < w * h {
        return false;
    }
    let (stayed, went) = (stayed / seen as f32, went / seen as f32);
    stayed > 1.5 && went < ARRIVED * stayed
}

struct Handover {
    old: PhoneZoom,
    new: PhoneZoom,
    shift: (f64, f64, f64),
    /// The view for frames of the old share, and of the new one; the window keeps them current.
    views: (View, View),
    since: f64,
    arrived: bool,
}

#[derive(Default)]
struct Inner {
    /// The stream the phone share is for.
    source: Option<String>,
    steering: bool,
    handover: Option<Handover>,
    /// That stream's last frame, small, while something steers.
    last: Option<Thumb>,
    shown: Option<View>,
}

/// Shared by the window, which starts and ends handovers, and the workers, which see the frames.
#[derive(Default)]
pub struct ViewGate(Mutex<Inner>);

impl ViewGate {
    pub fn follow(&self, source: Option<&str>, steering: bool) {
        let mut inner = self.0.lock().unwrap();
        if inner.source.as_deref() != source {
            inner.source = source.map(str::to_string);
            inner.last = None;
        }
        inner.steering = steering;
    }

    pub fn begin(&self, old: PhoneZoom, new: PhoneZoom, views: (View, View), now: f64) {
        self.0.lock().unwrap().handover = Some(Handover { old, new, shift: share_shift(&old, &new), views, since: now, arrived: false });
    }

    pub fn end(&self) {
        let mut inner = self.0.lock().unwrap();
        inner.handover = None;
        inner.shown = None;
    }

    /// Waiting for frames of the share sent last.
    pub fn waiting(&self) -> bool {
        self.0.lock().unwrap().handover.as_ref().is_some_and(|h| !h.arrived)
    }

    pub fn underway(&self) -> bool {
        self.0.lock().unwrap().handover.is_some()
    }

    /// The view frames get while it waits.
    pub fn waiting_view(&self) -> Option<View> {
        self.0.lock().unwrap().handover.as_ref().filter(|h| !h.arrived).map(|h| h.views.0.clone())
    }

    /// The old share while still waiting for `sent` to show up (_still_handing_over); None when that's over.
    pub fn still_for(&self, sent: Option<PhoneZoom>, steering: bool, now: f64) -> Option<PhoneZoom> {
        let inner = self.0.lock().unwrap();
        let h = inner.handover.as_ref()?;
        (!h.arrived && steering && Some(h.new) == sent && now - h.since <= HANDOVER_S).then_some(h.old)
    }

    pub fn retarget(&self, views: (View, View)) {
        if let Some(h) = self.0.lock().unwrap().handover.as_mut() {
            h.views = views;
        }
    }

    /// _crop_for, on the worker's thread, for each frame of source: the view to swap in, when it isn't the one in place.
    /// `replaced`: the steps were just replaced, with a view that may be a frame old.
    pub fn for_frame(&self, source: &str, frame: &Frame, now: f64, replaced: bool) -> Option<View> {
        let mut inner = self.0.lock().unwrap();
        if inner.source.as_deref() != Some(source) {
            return None;
        }
        let thumb = inner.steering.then(|| thumb(frame));
        let before = std::mem::replace(&mut inner.last, thumb.clone());
        let h = inner.handover.as_mut()?;
        if !h.arrived {
            let shown = matches!((&before, &thumb), (Some(b), Some(t)) if moved_like(b, t, h.shift));
            h.arrived = shown || now - h.since > HANDOVER_S;
        }
        let view = if h.arrived { h.views.1.clone() } else { h.views.0.clone() };
        (replaced || inner.shown.as_ref() != Some(&view)).then(|| {
            inner.shown = Some(view.clone());
            view
        })
    }
}
