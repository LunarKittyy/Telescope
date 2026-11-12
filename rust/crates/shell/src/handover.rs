//! view_controller's _Handover: a phone share sent while steering, until its frames arrive. The frames before it still
//! come cropped the old way, and cropping them for the new one would make the picture jump for a moment. A shader's crop
//! is fixed when it's built, so the worker asks this for each frame and swaps the view's steps on the one that shows it.

use std::sync::Mutex;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::View;
use telescope_core::view::{phone_window, rest_within, share_shift, window_centre, PhoneZoom};

/// A new phone share that hasn't shown up in the frames by then is taken to have (seconds). A lens that takes a while to switch needs it.
const HANDOVER_S: f64 = 2.0;
/// How many steps along the way from the old share to the new one a frame is looked for at (its crop follows).
const STEPS: usize = 12;
/// A frame the phone's picture hasn't changed since is kept as the one to compare with for this long (seconds), and any frame at most BASE_MAX_S.
const BASE_S: f64 = 0.25;
const BASE_MAX_S: f64 = 1.0;
/// A frame this much (grey levels) and this many times more different from the one before than they usually are is a cut.
const CUT_MIN: f32 = 25.0;
const CUT_TIMES: f32 = 10.0;
/// A picture that has been moving and holds still this long (seconds) has got there.
const SETTLED_S: f64 = 0.3;
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

/// How far `now` is from `before` as it was and as moved by shift (share_shift), on average; None when too little of the picture is in both.
fn compare(before: &Thumb, now: &Thumb, (scale, dx, dy): (f64, f64, f64)) -> Option<(f32, f32)> {
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
    (seen * 5 >= w * h).then(|| (stayed / seen as f32, went / seen as f32))
}

/// moved_like: `now` looks like `before` moved by shift (share_shift) clearly better than like before as it was.
pub fn moved_like(before: &Thumb, now: &Thumb, shift: (f64, f64, f64)) -> bool {
    compare(before, now, shift).is_some_and(|(stayed, went)| stayed > 1.5 && went < ARRIVED * stayed)
}

/// The share a fraction of the way from old to new: the same window the phone would send along the way (log-spaced size).
fn along(old: &PhoneZoom, new: &PhoneZoom, f: f64) -> PhoneZoom {
    if f <= 0.0 {
        return *old;
    }
    if f >= 1.0 {
        return *new;
    }
    let ((oz, opx, opy), (nz, npx, npy)) = (phone_window(Some(old)), phone_window(Some(new)));
    let total = (oz.ln() + (nz.ln() - oz.ln()) * f).exp();
    let centre = |o: f64, n: f64| window_centre(oz, o) + (window_centre(nz, n) - window_centre(oz, o)) * f;
    // As a share on the main lens, which says only the window
    PhoneZoom { ratio: 1.0, crop: total, x: centre(opx, npx), y: centre(opy, npy) }
}

struct Handover {
    old: PhoneZoom,
    new: PhoneZoom,
    /// The view for frames of the old share, and of the new one; the window keeps them current.
    views: (View, View),
    /// The framing the crops are for, kept current like the views.
    framing: View,
    since: f64,
    arrived: bool,
    /// How far along from old to new the frames have been seen to get, 0 to 1.
    progress: f64,
    /// A frame to compare the next ones with, from when the picture was at `base_at`, and when it was taken.
    base: Option<(Thumb, f64, f64)>,
    /// How much consecutive frames differ when nothing happens.
    calm: f32,
    /// The frame the following ones are held against, and since when they have all been like it.
    still: Option<(Thumb, f64)>,
}

impl Handover {
    fn share(&self, f: f64) -> PhoneZoom {
        along(&self.old, &self.new, f)
    }

    /// The view for frames that are `progress` of the way to the new share.
    fn view(&self) -> View {
        if self.arrived || self.progress >= 1.0 {
            return self.views.1.clone();
        }
        if self.progress <= 0.0 {
            return self.views.0.clone();
        }
        let f = &self.framing;
        let crop = rest_within(f.zoom as f64, f.pan_x as f64, f.pan_y as f64, &self.share(self.progress), true).unwrap_or((self.views.0.zoom as f64, self.views.0.pan_x as f64, self.views.0.pan_y as f64));
        View { zoom: crop.0 as f32, pan_x: crop.1 as f32, pan_y: crop.2 as f32, ..self.views.0.clone() }
    }

    /// What a frame says about how far along the phone's picture is: the furthest step it fits much better than the picture staying as it was.
    fn seen(&mut self, now: &Thumb, before: Option<&Thumb>, at: f64) {
        let Some((base, base_at, base_time)) = self.base.clone() else { return };
        let from = self.share(base_at);
        let mut best: Option<(f64, f32)> = None;
        let mut overlap = false;
        for i in 1..=STEPS {
            let f = i as f64 / STEPS as f64;
            if f <= self.progress {
                continue;
            }
            let Some((stayed, went)) = compare(&base, now, share_shift(&from, &self.share(f))) else { continue };
            overlap |= i == STEPS;
            if stayed > 1.5 && went < ARRIVED * stayed && best.is_none_or(|(_, w)| went / stayed < w) {
                best = Some((f, went / stayed));
            }
        }
        // Where the new share shows too little of the old picture to find it in, a cut in the picture is what shows
        let change = before.and_then(|b| compare(b, now, (1.0, 0.0, 0.0))).map_or(0.0, |(stayed, _)| stayed);
        if !overlap && change > CUT_MIN && change > CUT_TIMES * self.calm.max(1.0) {
            best = Some((1.0, 0.0));
        } else {
            self.calm += (change - self.calm) * 0.1;
        }
        match best {
            Some((f, _)) => {
                // From here on, the picture is taken to be where this says, which keeps what's compared near
                self.progress = f;
                self.base = Some((now.clone(), f, at));
            }
            // A frame the picture hasn't changed in since (or one that has gone unmatched too long) is the one to compare with
            None if self.base.as_ref().is_some_and(|b| b.1 == self.progress) => {
                let quiet = compare(&base, now, (1.0, 0.0, 0.0)).is_some_and(|(stayed, _)| stayed <= 1.5);
                if (quiet && at - base_time >= BASE_S) || at - base_time >= BASE_MAX_S {
                    self.base = Some((now.clone(), self.progress, at));
                }
            }
            None => {}
        }
        // Once it has been seen to move and then holds still, it has got where it was going
        let holds = self.still.as_ref().is_some_and(|(frame, _)| compare(frame, now, (1.0, 0.0, 0.0)).is_some_and(|(stayed, _)| stayed <= 1.5));
        if !holds {
            self.still = Some((now.clone(), at));
        }
        if self.progress >= 1.0 || (self.progress > 0.0 && self.still.as_ref().is_some_and(|(_, since)| at - since >= SETTLED_S)) {
            self.arrived = true;
        }
    }
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

    pub fn begin(&self, old: PhoneZoom, new: PhoneZoom, views: (View, View), framing: &View, now: f64) {
        let mut inner = self.0.lock().unwrap();
        // The last frame came in the old share, so it is the one to compare with
        let base = inner.last.clone().map(|b| (b, 0.0, now));
        inner.handover = Some(Handover { old, new, views, framing: framing.clone(), since: now, arrived: false, progress: 0.0, base, calm: 0.0, still: None });
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
        self.0.lock().unwrap().handover.as_ref().filter(|h| !h.arrived).map(Handover::view)
    }

    /// The old share while still waiting for `sent` to show up (_still_handing_over); None when that's over.
    pub fn still_for(&self, sent: Option<PhoneZoom>, steering: bool, now: f64) -> Option<PhoneZoom> {
        let inner = self.0.lock().unwrap();
        let h = inner.handover.as_ref()?;
        (!h.arrived && steering && Some(h.new) == sent && now - h.since <= HANDOVER_S).then_some(h.old)
    }

    pub fn retarget(&self, views: (View, View), framing: &View) {
        if let Some(h) = self.0.lock().unwrap().handover.as_mut() {
            h.views = views;
            h.framing = framing.clone();
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
            if let Some(t) = &thumb {
                if h.base.is_none() {
                    h.base = before.clone().map(|b| (b, h.progress, now));
                }
                h.seen(t, before.as_ref(), now);
            }
            if now - h.since > HANDOVER_S {
                (h.arrived, h.progress) = (true, 1.0);
            }
        }
        let view = h.view();
        (replaced || inner.shown.as_ref() != Some(&view)).then(|| {
            inner.shown = Some(view.clone());
            view
        })
    }
}
