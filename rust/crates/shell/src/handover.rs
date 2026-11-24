//! view_controller's _Handover: a phone share sent while zooming, until its frames arrive. The frames before it still
//! come cropped the old way, and cropping them for the new one would make the picture jump for a moment. A shader's crop
//! is fixed when it's built, so the worker asks this for each frame and swaps the view's steps on the one that shows it.
//! A newer phone tags each frame with the crop its camera reported for it (Frame::phone_share). Once a tag has come, a tagged frame
//! is cropped by its tag. A frame whose tag is missing has the crop the last one had while nothing is on its way; while a share is
//! on its way its crop isn't known, so it is found by comparing it with the frames before it, as for a phone without tags.

use std::sync::Mutex;
use std::collections::HashMap;
use serde_json::Value;
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
/// A frame's crop tag is the share sent when their zooms are this close (as a log) and their centres this close (of the whole view): the camera rounds the crop to whole pixels.
const TAG_ZOOM: f64 = 0.01;
const TAG_CENTRE: f64 = 0.004;

/// Whether a frame tagged with `tag` shows `share`.
fn shows(tag: &PhoneZoom, share: &PhoneZoom) -> bool {
    let ((tz, tx, ty), (sz, sx, sy)) = (phone_window(Some(tag)), phone_window(Some(share)));
    (tz.ln() - sz.ln()).abs() < TAG_ZOOM && (window_centre(tz, tx) - window_centre(sz, sx)).abs() < TAG_CENTRE && (window_centre(tz, ty) - window_centre(sz, sy)).abs() < TAG_CENTRE
}

/// What the frames of a phone that tags them have said, since the stream or its connection last changed.
#[derive(Default)]
struct Tags {
    /// The share sent last, the view for frames that show it and the framing the crops are for, as the window keeps them.
    target: Option<(PhoneZoom, View, View)>,
    /// A tag has come: from then on the tags say how frames are cropped.
    tagged: bool,
    /// The crop the phone's picture is known to have: the last tag's, the whole view after a reconnect, None after a lens change.
    known: Option<PhoneZoom>,
    /// When the last tag came.
    tag_at: Option<f64>,
    /// The crop the last frame shown was taken to have.
    showing: Option<PhoneZoom>,
    /// When the share was sent, while it is on its way: until a frame shows it, or for HANDOVER_S (a camera may settle on another crop, or ignore it).
    pending: Option<f64>,
    /// Untagged frames in a row.
    misses: u32,
    /// How much consecutive frames differ when nothing happens (Handover::calm), kept across the comparisons a tagged frame starts.
    calm: f32,
    /// While a share is on its way and tags are missing: where the frames since the last tagged one are found to be.
    compare: Option<Handover>,
}

impl Tags {
    fn on(&self) -> bool {
        self.tagged && self.target.is_some()
    }

    fn reset(&mut self) {
        *self = Tags { target: self.target.take(), ..Tags::default() };
    }

    /// The crop the phone's picture is taken to have now.
    fn current(&self) -> Option<PhoneZoom> {
        self.showing.or(self.known).or(self.target.as_ref().map(|t| t.0))
    }

    /// The view for a frame that shows `tag`, with the window's current turn and flips, and the phone's window it shows.
    fn shown(&self, tag: &PhoneZoom) -> Option<(View, (f64, f64, f64))> {
        let (sent, view, framing) = self.target.as_ref()?;
        if shows(tag, sent) {
            return Some((view.clone(), phone_window(Some(sent))));
        }
        let crop = rest_within(framing.zoom as f64, framing.pan_x as f64, framing.pan_y as f64, tag, true)?;
        Some((View { zoom: crop.0 as f32, pan_x: crop.1 as f32, pan_y: crop.2 as f32, ..view.clone() }, phone_window(Some(tag))))
    }

    fn last_shown(&self) -> Option<(View, (f64, f64, f64))> {
        self.current().filter(|_| self.on()).and_then(|t| self.shown(&t))
    }
}

/// What ViewGate::for_frame_checked says of a frame it doesn't hold back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    /// The view to swap in and the phone's window the frame shows, when they aren't the ones in place.
    pub changed: Option<(View, (f64, f64, f64))>,
    /// The crop named for the frame is a guess.
    pub unsure: bool,
    /// The phone's window the frame shows, when the frames of a phone that tags them say it (else the share sent).
    pub crop: Option<(f64, f64, f64)>,
}

/// Frames that come this long (seconds) or more after a new share was sent carry a crop that may be well off, until a frame has fitted the whole move (or
/// HANDOVER_S is over): the phone's picture may have started to move by then, and what the frames show of it is only found a step at a time, late, and not
/// at all in a scene too flat to find it in.
const UNSURE_S: f64 = 0.1;

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
    // A decoder's picture still on the card has no pixels here yet
    let frame = &frame.materialise();
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

/// Whether the crop named for frames of a new share is a guess: from UNSURE_S after it was sent until the picture holds still after a frame fitted the whole
/// move, or HANDOVER_S is over. It outlives the handover, which is ended as soon as the share has been taken to arrive.
#[derive(Clone)]
struct Guess {
    after: f64,
    until: f64,
    /// A frame fitted the picture of the whole move (not just a cut in it): the picture holding still after that is the phone on the new share, since a frame
    /// that fits it while the picture still moves may be one a bit before the end.
    fitted: bool,
    confirmed: bool,
    /// The frame the following ones are held against, and since when they have all been like it.
    still: Option<(Thumb, f64)>,
}

impl Guess {
    /// A share sent at `now`, on top of `earlier` when that is still a guess (its old share is only where that was guessed to be).
    fn new(now: f64, earlier: Option<&Guess>) -> Guess {
        let after = earlier.filter(|g| !g.confirmed && now <= g.until).map_or(now + UNSURE_S, |g| g.after);
        Guess { after, until: now + HANDOVER_S, fitted: false, confirmed: false, still: None }
    }

    fn unsure(&self, now: f64) -> bool {
        !self.confirmed && now >= self.after && now <= self.until
    }

    fn watch(&mut self, now: &Thumb, at: f64) {
        if !self.fitted || self.confirmed {
            return;
        }
        let holds = self.still.as_ref().is_some_and(|(frame, _)| compare(frame, now, (1.0, 0.0, 0.0)).is_some_and(|(stayed, _)| stayed <= 1.5));
        if !holds {
            self.still = Some((now.clone(), at));
        }
        self.confirmed = self.still.as_ref().is_some_and(|(_, since)| at - since >= SETTLED_S);
    }
}

struct Handover {
    old: PhoneZoom,
    new: PhoneZoom,
    /// The view for frames of the old share, and of the new one; the window keeps them current.
    views: (View, View),
    /// The framing the crops are for, kept current like the views.
    framing: View,
    since: f64,
    guess: Guess,
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
    /// Frames of a tagging phone looked for on their way from `old` to `new`, from `base` (a frame at `old`).
    fn comparing(old: PhoneZoom, new: PhoneZoom, base: Option<Thumb>, calm: f32, now: f64) -> Handover {
        Handover { old, new, views: Default::default(), framing: View::default(), since: now, guess: Guess::new(now, None), arrived: false, progress: 0.0,
                   base: base.map(|b| (b, 0.0, now)), calm, still: None }
    }

    /// The share the frames are taken to be at.
    fn at(&self) -> PhoneZoom {
        self.share(if self.arrived { 1.0 } else { self.progress })
    }

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
        let mut cut = false;
        if !overlap && change > CUT_MIN && change > CUT_TIMES * self.calm.max(1.0) {
            best = Some((1.0, 0.0));
            cut = true;
        } else {
            self.calm += (change - self.calm) * 0.1;
        }
        match best {
            Some((f, _)) => {
                // From here on, the picture is taken to be where this says, which keeps what's compared near
                self.progress = f;
                // A cut says only that the picture changed, not by how much
                self.guess.fitted |= f >= 1.0 && !cut;
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
    handover: Option<Handover>,
    /// The guess about the crop of a handover that has ended, for as long as it lasts.
    guess: Option<Guess>,
    /// That stream's last frame, small, while a new share is on its way or its crop is a guess, and always for a frame in memory.
    last: Option<Thumb>,
    shown: Option<(View, (f64, f64, f64))>,
    cameras: HashMap<String, (Value, u64)>,
    tags: Tags,
}

impl Inner {
    /// Whatever the frames said about the crop says nothing of the next ones: the stream, its connection or its lens changed.
    fn reset(&mut self) {
        self.last = None;
        self.handover = None;
        self.guess = None;
        self.shown = None;
        self.tags = Tags::default();
    }

    /// The share on its way to a tagging phone has shown up, or had all the time it is given: frames are no longer looked at.
    fn settle(&mut self) {
        (self.tags.pending, self.tags.compare) = (None, None);
        if self.tags.on() {
            self.last = None;
        }
    }
}

/// Shared by the window, which starts and ends handovers, and the workers, which see the frames.
#[derive(Default)]
pub struct ViewGate(Mutex<Inner>);

impl ViewGate {
    pub fn note_camera(&self, source: &str, camera: &Value) {
        let Some(id) = camera.get("id") else { return };
        let mut inner = self.0.lock().unwrap();
        let entry = inner.cameras.entry(source.to_string()).or_insert_with(|| (id.clone(), 1));
        if entry.0 != *id {
            entry.0 = id.clone();
            entry.1 += 1;
            // A tag from the lens before says nothing of a frame from this one, and neither does the crop it put in place
            if inner.source.as_deref() == Some(source) {
                (inner.tags.known, inner.tags.showing, inner.shown) = (None, None, None);
            }
        }
    }

    /// The worker opened a new connection to `source`'s phone, before its first frame: the phone starts it at its whole view, and what the
    /// frames before said about the crop says nothing of the next ones. The window may hear of it later or earlier, so the share it sent stays.
    pub fn connection_reset(&self, source: &str) {
        let mut inner = self.0.lock().unwrap();
        if inner.source.as_deref() != Some(source) {
            return;
        }
        let neutral = inner.tags.target.as_ref().map(|_| PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 });
        let tags = &mut inner.tags;
        (tags.known, tags.showing, tags.compare, tags.misses) = (neutral, None, None, 0);
        (inner.last, inner.shown) = (None, None);
    }

    pub fn source_epoch(&self, source: &str) -> u64 {
        self.0.lock().unwrap().cameras.get(source).map_or(0, |(_, epoch)| *epoch)
    }

    pub fn follow(&self, source: Option<&str>) {
        let mut inner = self.0.lock().unwrap();
        if inner.source.as_deref() != source {
            inner.source = source.map(str::to_string);
            inner.reset();
        }
    }

    pub fn begin(&self, old: PhoneZoom, new: PhoneZoom, mut views: (View, View), framing: &View, now: f64) {
        let mut inner = self.0.lock().unwrap();
        inner.tags.pending = Some(now);
        inner.tags.target = Some((new, views.1.clone(), framing.clone()));
        if inner.tags.on() {
            // Frames already being looked for go on from where they were found to be, towards the new share
            if let Some(h) = inner.tags.compare.take() {
                let base = inner.last.clone();
                inner.tags.compare = Some(Handover::comparing(h.at(), new, base, h.calm, now));
            }
            inner.shown = None;
            return;
        }
        let old = inner.handover.as_ref().filter(|h| !h.arrived).map_or(old, |h| h.share(h.progress));
        let guess = Guess::new(now, inner.handover.as_ref().map(|h| &h.guess).or(inner.guess.as_ref()));
        inner.guess = None;
        if let Some((zoom, pan_x, pan_y)) = rest_within(framing.zoom as f64, framing.pan_x as f64, framing.pan_y as f64, &old, true) {
            views.0 = View { zoom: zoom as f32, pan_x: pan_x as f32, pan_y: pan_y as f32, ..views.0 };
        }
        // The last frame came in the old share, so it is the one to compare with; with none kept, the first frame after this one is (the phone can't
        // have shown the new share by then)
        let base = inner.last.clone().map(|b| (b, 0.0, now));
        inner.handover = Some(Handover { old, new, views, framing: framing.clone(), since: now, guess, arrived: false, progress: 0.0, base, calm: 0.0, still: None });
    }

    pub fn end(&self) {
        let mut inner = self.0.lock().unwrap();
        // The share is taken to have arrived, but until a frame has shown it the crop named for frames is still a guess
        if let Some(h) = inner.handover.take() {
            inner.guess = Some(h.guess).filter(|g| !g.confirmed);
        }
        inner.shown = None;
    }

    /// For a phone that tags its frames: the share sent last (None for a phone that doesn't), the view for frames that show it and the framing,
    /// as the window has them now. A tagged frame is cropped by these, so its turn and flips are the window's current ones.
    pub fn aim(&self, sent: Option<PhoneZoom>, view: View, framing: &View) {
        let mut inner = self.0.lock().unwrap();
        inner.tags.target = sent.map(|s| (s, view, framing.clone()));
        if inner.tags.target.is_none() {
            inner.tags.reset();
        }
    }

    /// Waiting for frames of the share sent last.
    pub fn waiting(&self) -> bool {
        let inner = self.0.lock().unwrap();
        if inner.tags.on() {
            return inner.tags.pending.is_some();
        }
        inner.handover.as_ref().is_some_and(|h| !h.arrived)
    }

    pub fn underway(&self) -> bool {
        self.0.lock().unwrap().handover.is_some()
    }

    /// The desktop view and phone crop from the same handover progress.
    pub fn waiting_snapshot(&self) -> Option<(View, (f64, f64, f64))> {
        let inner = self.0.lock().unwrap();
        // The tags say where the picture is, whether or not a share is on its way
        if inner.tags.on() {
            return inner.tags.last_shown();
        }
        inner.handover.as_ref().filter(|h| !h.arrived).map(|h| (h.view(), phone_window(Some(&h.share(h.progress)))))
    }

    /// The old share while still waiting for `sent` to show up (_still_handing_over); None when that's over.
    pub fn still_for(&self, sent: Option<PhoneZoom>, now: f64) -> Option<PhoneZoom> {
        let inner = self.0.lock().unwrap();
        let h = inner.handover.as_ref()?;
        (!h.arrived && Some(h.new) == sent && now - h.since <= HANDOVER_S).then_some(h.old)
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
        self.for_frame_snapshot(source, frame, now, replaced).map(|s| s.0)
    }

    pub fn for_frame_snapshot(&self, source: &str, frame: &Frame, now: f64, replaced: bool) -> Option<(View, (f64, f64, f64))> {
        self.for_frame_labelled(source, frame, now, replaced).0
    }

    /// for_frame_snapshot, and whether the crop it names for this frame is a guess: the phone's zoom is on its way, and the frames show how far it has got
    /// only a step at a time, late (and in a scene too flat to find it in, not at all), so the crop the frame really has may be well off the one it's labelled with.
    pub fn for_frame_labelled(&self, source: &str, frame: &Frame, now: f64, replaced: bool) -> (Option<(View, (f64, f64, f64))>, bool) {
        self.for_frame_checked(source, frame, now, replaced).map_or((None, false), |c| (c.changed, c.unsure))
    }

    /// for_frame_labelled, or None for a frame to hold back: the first of a run of frames whose tags are missing while a share is on its way to a
    /// phone that tags them, which may be the first of the new crop or the last of the old one. The rest of the run is found by comparing pictures.
    pub fn for_frame_checked(&self, source: &str, frame: &Frame, now: f64, replaced: bool) -> Option<Checked> {
        let mut inner = self.0.lock().unwrap();
        if inner.source.as_deref() != Some(source) {
            return Some(Checked::default());
        }
        if let Some(at) = inner.tags.pending.filter(|at| now - at > HANDOVER_S) {
            let tags = &mut inner.tags;
            // No tag since it was sent (the tags stopped) or frames found to show it: it is taken to have arrived, as for a phone without tags
            if tags.tag_at.is_none_or(|t| t < at) || tags.compare.as_ref().is_some_and(|h| h.arrived) {
                tags.known = tags.target.as_ref().map(|t| t.0).or(tags.known);
                tags.showing = None;
            }
            inner.settle();
        }
        // The first tag puts the tags in charge, and what comparing frames found says nothing any more
        if frame.phone_share.is_some() && inner.tags.target.is_some() && !inner.tags.tagged {
            inner.tags.tagged = true;
            (inner.handover, inner.guess, inner.last) = (None, None, None);
        }
        if inner.tags.on() {
            return self.tagged(inner, source, frame, now, replaced);
        }
        // A guess past the time the phone is given says nothing any more
        if inner.guess.as_ref().is_some_and(|g| now > g.until) {
            inner.guess = None;
        }
        // A picture still on the card is copied off it to be looked at, so it is only looked at while a share is on its way or its crop is a guess (and
        // outside the lock the window takes). One already in memory costs next to nothing, and the last of those is the one to compare a new share with
        let thumb = if inner.handover.is_some() || inner.guess.is_some() || frame.surface().is_none() {
            drop(inner);
            let thumb = thumb(frame);
            inner = self.0.lock().unwrap();
            if inner.source.as_deref() != Some(source) {
                return Some(Checked::default());
            }
            Some(thumb)
        } else {
            None
        };
        let before = std::mem::replace(&mut inner.last, thumb.clone());
        let Some(h) = inner.handover.as_mut() else {
            let unsure = inner.guess.as_mut().is_some_and(|g| {
                if let Some(t) = &thumb {
                    g.watch(t, now);
                }
                g.unsure(now)
            });
            return Some(Checked { unsure, ..Checked::default() });
        };
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
        } else if let Some(t) = &thumb {
            h.guess.watch(t, now);
        }
        let view = h.view();
        let source_crop = phone_window(Some(&h.share(if h.arrived { 1.0 } else { h.progress })));
        let unsure = h.guess.unsure(now);
        let shown = (view.clone(), source_crop);
        let changed = (replaced || inner.shown.as_ref() != Some(&shown)).then(|| {
            inner.shown = Some(shown.clone());
            shown
        });
        Some(Checked { changed, unsure, crop: None })
    }

    /// for_frame_checked for a phone whose frames have had tags.
    fn tagged<'a>(&'a self, mut inner: std::sync::MutexGuard<'a, Inner>, source: &str, frame: &Frame, now: f64, replaced: bool) -> Option<Checked> {
        // Frames are only looked at (and a picture on the card copied off it) while a share is on its way, a few frames at a time: a tagged one is
        // what an untagged one after it is compared with
        let thumb = if inner.tags.pending.is_some() {
            drop(inner);
            let thumb = thumb(frame);
            inner = self.0.lock().unwrap();
            if inner.source.as_deref() != Some(source) || !inner.tags.on() {
                return Some(Checked::default());
            }
            Some(thumb)
        } else {
            None
        };
        let before = std::mem::replace(&mut inner.last, thumb.clone());
        let sent = inner.tags.target.as_ref()?.0;
        let tags = &mut inner.tags;
        let (crop, unsure) = match frame.phone_share {
            Some(tag) => {
                (tags.known, tags.tag_at, tags.misses) = (Some(tag), Some(now), 0);
                if shows(&tag, &sent) {
                    tags.pending = None;
                }
                // How much frames differ from one to the next, for telling a cut from the picture's own movement
                if let Some(change) = before.as_ref().zip(thumb.as_ref()).and_then(|(b, t)| compare(b, t, (1.0, 0.0, 0.0))) {
                    tags.calm += (change.0 - tags.calm) * 0.1;
                }
                // The frames that lose their tags after it are looked for from this one
                let calm = tags.calm;
                tags.compare = tags.pending.map(|_| Handover::comparing(tag, sent, thumb, calm, now));
                (tag, false)
            }
            None if tags.pending.is_none() => (tags.known.unwrap_or(sent), false),
            None => {
                tags.misses += 1;
                let (from, calm) = (tags.current().unwrap_or(sent), tags.calm);
                let h = tags.compare.get_or_insert_with(|| Handover::comparing(from, sent, before.clone(), calm, now));
                match thumb {
                    Some(t) if h.base.is_some() => {
                        h.seen(&t, before.as_ref(), now);
                    }
                    // With nothing to compare it with, a frame this soon after the share was sent is taken to have the crop before
                    t => h.base = t.map(|t| (t, h.progress, now)),
                }
                let at = h.at();
                tags.calm = h.calm;
                // The first is held back, which keeps the picture before it up a frame longer (a comparison finds a gliding crop only roughly);
                // more would freeze it, so the next ones get what the comparison found
                if tags.misses == 1 {
                    if replaced {
                        inner.shown = None;
                    }
                    return None;
                }
                (at, true)
            }
        };
        if inner.tags.pending.is_none() {
            inner.settle();
        }
        inner.tags.showing = Some(crop);
        let Some(shown) = inner.tags.shown(&crop) else { return Some(Checked { unsure, ..Checked::default() }) };
        let crop = Some(shown.1);
        let changed = (replaced || inner.shown.as_ref() != Some(&shown)).then(|| {
            inner.shown = Some(shown.clone());
            shown
        });
        Some(Checked { changed, unsure, crop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use telescope_core::frame::{DecodedSurface, PixelFormat};

    #[test]
    fn source_camera_epochs_change_only_for_that_streams_lens() {
        let gate = ViewGate::default();
        let main = serde_json::json!({"id": "main"});
        let wide = serde_json::json!({"id": "wide"});
        gate.note_camera("a", &main);
        gate.note_camera("b", &main);
        let first = gate.source_epoch("a");
        gate.note_camera("a", &main);
        assert_eq!(gate.source_epoch("a"), first);
        gate.note_camera("a", &wide);
        assert_ne!(gate.source_epoch("a"), first);
        assert_eq!(gate.source_epoch("b"), first);
        gate.note_camera("a", &main);
        assert!(gate.source_epoch("a") > first + 1);
    }

    #[derive(Debug)]
    struct OnTheCard(Frame);

    impl DecodedSurface for OnTheCard {
        fn ram(&self) -> &Frame {
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn mask_source_coordinates_follow_the_phone_crop_through_a_handover() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.4, y: 0.5 };
        let new = PhoneZoom { ratio: 1.0, crop: 4.0, x: 0.6, y: 0.5 };
        gate.begin(old, new, (View::default(), View::default()), &View::default(), 0.0);
        assert_eq!(gate.waiting_snapshot().map(|s| s.1), Some(phone_window(Some(&old))));
        let frame = Frame::new(8, 8, PixelFormat::I420, vec![120; 96]);
        assert_eq!(gate.for_frame_snapshot("other", &frame, 0.0, false), None);
        gate.0.lock().unwrap().handover.as_mut().unwrap().progress = 0.5;
        let middle = phone_window(Some(&along(&old, &new, 0.5)));
        assert_eq!(gate.waiting_snapshot().map(|s| s.1), Some(middle));
        assert_eq!(gate.for_frame_snapshot("a", &frame, 0.0, false).unwrap().1, middle);
        gate.0.lock().unwrap().handover.as_mut().unwrap().arrived = true;
        assert_eq!(gate.waiting_snapshot().map(|s| s.1), None);
        assert_eq!(gate.for_frame_snapshot("a", &frame, 0.0, false).unwrap().1, phone_window(Some(&new)));
    }

    #[test]
    fn a_phone_crop_move_refreshes_mask_geometry_even_when_the_desktop_crop_is_unchanged() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 4.0, ..old };
        gate.begin(old, new, (View::default(), View::default()), &View::default(), 0.0);
        let frame = Frame::new(8, 8, PixelFormat::I420, vec![120; 96]);
        assert_eq!(gate.for_frame_snapshot("a", &frame, 0.0, false), Some((View::default(), phone_window(Some(&old)))));
        assert_eq!(gate.for_frame_snapshot("a", &frame, 0.0, false), None);
        gate.0.lock().unwrap().handover.as_mut().unwrap().progress = 0.5;
        assert_eq!(gate.for_frame_snapshot("a", &frame, 0.0, false), Some((View::default(), phone_window(Some(&along(&old, &new, 0.5))))));
    }

    #[test]
    fn a_frame_snapshot_keeps_the_desktop_and_phone_crops_together() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 4.0, ..old };
        let framing = View { zoom: 8.0, ..View::default() };
        gate.begin(old, new, (View { zoom: 4.0, ..View::default() }, View { zoom: 2.0, ..View::default() }), &framing, 0.0);
        gate.0.lock().unwrap().handover.as_mut().unwrap().progress = 0.5;
        let frame = Frame::new(8, 8, PixelFormat::I420, vec![120; 96]);
        let (view, source_crop) = gate.for_frame_snapshot("a", &frame, 0.0, false).unwrap();
        gate.0.lock().unwrap().handover.as_mut().unwrap().arrived = true;
        assert!((view.zoom as f64 * source_crop.0 - framing.zoom as f64).abs() < 1e-6);
        assert_eq!(source_crop, phone_window(Some(&along(&old, &new, 0.5))));
        let (view, source_crop) = gate.for_frame_snapshot("a", &frame, 0.0, false).unwrap();
        assert_eq!((view.zoom, source_crop.0), (2.0, 4.0));
    }

    #[test]
    fn a_label_is_a_guess_a_moment_after_a_new_zoom_is_sent_until_it_arrives() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 4.0, ..old };
        let flat = Frame::new(8, 8, PixelFormat::I420, vec![120; 96]);
        assert!(!gate.for_frame_labelled("a", &flat, 0.0, false).1, "nothing is on its way");
        gate.begin(old, new, (View::default(), View::default()), &View::default(), 1.0);
        assert!(!gate.for_frame_labelled("a", &flat, 1.05, false).1, "only just sent");
        assert!(gate.for_frame_labelled("a", &flat, 1.4, false).1, "the picture shows nothing of it a while after");
        assert!(!gate.for_frame_labelled("other", &flat, 1.4, false).1, "another stream's frames say nothing");
        // Another share sent on top of it starts from where the first was guessed to be
        gate.begin(new, old, (View::default(), View::default()), &View::default(), 1.5);
        assert!(gate.for_frame_labelled("a", &flat, 1.52, false).1, "still guessing");
        // A picture that holds still for a while is taken to have got there, which only guesses it
        gate.0.lock().unwrap().handover.as_mut().unwrap().arrived = true;
        assert!(gate.for_frame_labelled("a", &flat, 1.9, false).1, "still guessing");
        assert!(!gate.for_frame_labelled("a", &flat, 3.6, false).1, "the phone has had all the time it is given");
        gate.begin(old, new, (View::default(), View::default()), &View::default(), 4.0);
        assert!(gate.for_frame_labelled("a", &flat, 4.5, false).1, "the picture shows nothing of it");
        gate.0.lock().unwrap().handover.as_mut().unwrap().guess.confirmed = true;
        assert!(!gate.for_frame_labelled("a", &flat, 4.6, false).1, "a frame fitted the whole move");
        gate.begin(new, old, (View::default(), View::default()), &View::default(), 5.0);
        assert!(!gate.for_frame_labelled("a", &flat, 5.05, false).1, "a new start from where it was");
    }

    #[test]
    fn a_move_is_sure_once_the_picture_that_fitted_it_has_held_still() {
        let (old, flat) = (PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 }, Frame::new(8, 8, PixelFormat::I420, vec![120; 96]));
        let new = PhoneZoom { crop: 4.0, ..old };
        // The view controller ends the handover as soon as the share is taken to have arrived, and the guess goes on without it
        for ended in [false, true] {
            let begun = |fitted: bool| {
                let gate = ViewGate::default();
                gate.follow(Some("a"));
                gate.begin(old, new, (View::default(), View::default()), &View::default(), 1.0);
                let mut inner = gate.0.lock().unwrap();
                let h = inner.handover.as_mut().unwrap();
                (h.arrived, h.guess.fitted) = (true, fitted);
                drop(inner);
                if ended {
                    gate.end();
                }
                gate
            };
            // Arrived by the picture's holding still, with no frame having fitted the whole move: a guess until the phone has had all the time it is given
            let gate = begun(false);
            for t in [1.2, 1.4, 1.6, 1.9] {
                assert!(gate.for_frame_labelled("a", &flat, t, false).1, "at {t}, ended {ended}");
            }
            assert!(!gate.for_frame_labelled("a", &flat, 3.1, false).1, "the phone has had all the time it is given");
            let gate = begun(true);
            assert!(gate.for_frame_labelled("a", &flat, 1.2, false).1, "only just fitted, ended {ended}");
            assert!(gate.for_frame_labelled("a", &flat, 1.3, false).1, "not still for long enough, ended {ended}");
            assert!(!gate.for_frame_labelled("a", &flat, 1.55, false).1, "held still, ended {ended}");
        }
    }

    /// A picture on the card that counts how often it is copied off it.
    #[derive(Debug)]
    struct Counted(Frame, std::sync::atomic::AtomicUsize);

    impl DecodedSurface for Counted {
        fn ram(&self) -> &Frame {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn frames_are_only_copied_off_the_card_while_a_share_is_on_its_way_or_a_guess() {
        let (w, h) = (64, 48);
        let surface = Arc::new(Counted(Frame::new(w, h, PixelFormat::I420, vec![120; PixelFormat::I420.size(w, h)]), Default::default()));
        let on_card = Frame::from_surface(w, h, PixelFormat::I420, surface.clone());
        let copies = || surface.1.load(std::sync::atomic::Ordering::SeqCst);
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        for i in 0..10 {
            gate.for_frame_labelled("a", &on_card, i as f64 / 30.0, false);
        }
        assert_eq!(copies(), 0, "nothing is on its way, so no frame is looked at");
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        gate.begin(old, PhoneZoom { crop: 4.0, ..old }, (View::default(), View::default()), &View::default(), 1.0);
        gate.for_frame_labelled("a", &on_card, 1.03, false);
        assert_eq!(copies(), 1, "a share on its way is looked for in the frames");
        // Taken to have arrived without a frame fitting it: a guess until the phone has had all the time it is given, and then nothing
        gate.end();
        gate.for_frame_labelled("a", &on_card, 1.5, false);
        assert_eq!(copies(), 2);
        gate.for_frame_labelled("a", &on_card, 1.0 + HANDOVER_S + 0.1, false);
        gate.for_frame_labelled("a", &on_card, 1.0 + HANDOVER_S + 0.2, false);
        assert_eq!(copies(), 2, "an old guess says nothing about the frames");
    }

    /// The scene through the middle 1/crop of it, as a phone share of that crop sends it: a picture with something to find in it.
    fn through(crop: f64) -> Frame {
        let (w, h) = (512u32, 288u32);
        let mut data = vec![128u8; PixelFormat::I420.size(w, h)];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let (sx, sy) = (0.5 + (x as f64 / w as f64 - 0.5) / crop, 0.5 + (y as f64 / h as f64 - 0.5) / crop);
                data[y * w as usize + x] = (128.0 + 60.0 * (13.0 * sx).sin() * (9.0 * sy).cos() + 40.0 * (31.0 * sx + 7.0 * sy).sin()) as u8;
            }
        }
        Frame::new(w, h, PixelFormat::I420, data)
    }

    /// The scene's picture as a decoder's, still on the card.
    fn on_the_card(frame: Frame) -> Frame {
        Frame::from_surface(frame.width, frame.height, frame.format, Arc::new(OnTheCard(frame)))
    }

    #[test]
    fn a_share_sent_while_nothing_was_on_its_way_is_found_in_the_frames_on_the_card() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 };
        gate.for_frame_labelled("a", &on_the_card(through(1.0)), 0.0, false);
        gate.begin(old, PhoneZoom { crop: 2.0, ..old }, (View::default(), View::default()), &View::default(), 0.1);
        // Frames still come in the old share for a while (the phone hasn't had the new one yet), then in the new one
        gate.for_frame_labelled("a", &on_the_card(through(1.0)), 0.13, false);
        gate.for_frame_labelled("a", &on_the_card(through(2.0)), 0.16, false);
        let inner = gate.0.lock().unwrap();
        let h = inner.handover.as_ref().unwrap();
        assert!(h.arrived && h.progress >= 1.0, "progress {}", h.progress);
    }

    #[test]
    fn the_last_frame_in_memory_is_the_one_a_share_sent_after_it_is_compared_with() {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let old = PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 };
        gate.for_frame_labelled("a", &through(1.0), 0.0, false);
        gate.begin(old, PhoneZoom { crop: 2.0, ..old }, (View::default(), View::default()), &View::default(), 0.1);
        assert!(gate.0.lock().unwrap().handover.as_ref().unwrap().base.is_some());
    }

    #[test]
    fn a_picture_still_on_the_card_has_the_thumb_of_its_pixels() {
        let (w, h) = (64, 48);
        let grey = Frame::new(w, h, PixelFormat::I420, vec![120; PixelFormat::I420.size(w, h)]);
        let on_card = Frame::from_surface(w, h, PixelFormat::I420, Arc::new(OnTheCard(grey.clone())));
        assert_eq!(thumb(&on_card), thumb(&grey));
    }

    fn tagged(tag: Option<PhoneZoom>, surface: &Arc<Counted>) -> Frame {
        let mut frame = Frame::from_surface(8, 8, PixelFormat::I420, surface.clone());
        frame.phone_share = tag;
        frame
    }

    fn card() -> Arc<Counted> {
        Arc::new(Counted(Frame::new(8, 8, PixelFormat::I420, vec![120; 96]), Default::default()))
    }

    /// A gate following "a", aimed at `sent` with the frames of it shown at zoom 2 of a framing at 6.
    fn aimed(sent: PhoneZoom) -> (ViewGate, View) {
        let gate = ViewGate::default();
        gate.follow(Some("a"));
        let framing = View { zoom: 6.0, ..View::default() };
        gate.aim(Some(sent), View { zoom: 2.0, ..View::default() }, &framing);
        (gate, framing)
    }

    fn total(shown: &(View, (f64, f64, f64))) -> f64 {
        shown.0.zoom.max(1.0) as f64 * shown.1 .0
    }

    /// `frame` tagged with `tag`.
    fn with_tag(mut frame: Frame, tag: Option<PhoneZoom>) -> Frame {
        frame.phone_share = tag;
        frame
    }

    #[test]
    fn only_the_first_frame_without_its_tag_is_held_back_while_a_share_is_on_its_way() {
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 3.0, ..old };
        let (gate, framing) = aimed(old);
        let surface = card();
        let at_old = gate.for_frame_checked("a", &tagged(Some(old), &surface), 0.0, false).unwrap().changed.unwrap();
        assert_eq!(at_old.0.zoom, 2.0);
        // Nothing on its way: a frame that lost its tag has the crop the last one had, and isn't looked at
        assert_eq!(gate.for_frame_checked("a", &tagged(None, &surface), 0.03, false), Some(Checked { crop: Some(phone_window(Some(&old))), ..Checked::default() }));
        assert_eq!(surface.1.load(std::sync::atomic::Ordering::Relaxed), 0, "nothing was copied off the card");
        gate.begin(old, new, (View { zoom: 3.0, ..View::default() }, View { zoom: 2.0, ..View::default() }), &framing, 0.1);
        assert!(gate.waiting());
        // The first frame of the new crop may be the one whose tag is missing: it's held back, not shown cropped for the old one
        assert_eq!(gate.for_frame_checked("a", &tagged(None, &surface), 0.2, false), None);
        // The next ones aren't, so the picture never freezes: their crop is a guess
        for k in 1..5 {
            let checked = gate.for_frame_checked("a", &tagged(None, &surface), 0.2 + k as f64 * 0.03, false).expect("shown");
            assert!(checked.unsure);
        }
        let at_new = gate.for_frame_checked("a", &tagged(Some(new), &surface), 0.43, false).unwrap();
        assert!((total(&at_new.changed.unwrap()) - 6.0).abs() < 1e-4);
        assert!(!at_new.unsure && !gate.waiting());
    }

    #[test]
    fn dragging_the_zoom_while_tags_are_missing_never_freezes_the_picture() {
        let mut share = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let (gate, framing) = aimed(share);
        let surface = card();
        gate.for_frame_checked("a", &tagged(Some(share), &surface), 0.0, false);
        let mut held = 0;
        // 60 fps, with a new share every 50 ms
        for k in 1..120 {
            let now = k as f64 / 60.0;
            if k % 3 == 0 {
                let next = PhoneZoom { crop: share.crop * 1.02, ..share };
                gate.begin(share, next, (View::default(), View::default()), &framing, now);
                gate.aim(Some(next), View::default(), &framing);
                share = next;
            }
            held += gate.for_frame_checked("a", &tagged(None, &surface), now, false).is_none() as u32;
        }
        assert_eq!(held, 1, "only the first frame without its tag is held back");
    }

    #[test]
    fn frames_that_lost_their_tags_through_a_zoom_are_found_by_their_pictures() {
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 3.0, ..old };
        let (gate, framing) = aimed(old);
        gate.for_frame_checked("a", &with_tag(through(2.0), Some(old)), 0.0, false);
        gate.begin(old, new, (View { zoom: 3.0, ..View::default() }, View { zoom: 2.0, ..View::default() }), &framing, 0.1);
        gate.for_frame_checked("a", &with_tag(through(2.0), Some(old)), 0.13, false);
        // The camera switched on the first of four frames whose tags were lost: the first is held back, and the rest show the framing, not the old crop on top of the new one
        assert_eq!(gate.for_frame_checked("a", &with_tag(through(3.0), None), 0.16, false), None);
        for k in 1..4 {
            let checked = gate.for_frame_checked("a", &with_tag(through(3.0), None), 0.16 + k as f64 * 0.03, false).expect("not held back");
            assert!(checked.unsure);
            assert_eq!(checked.crop, Some(phone_window(Some(&new))), "frame {k}");
            assert!((total(&gate.waiting_snapshot().unwrap()) - 6.0).abs() < 1e-4);
        }
    }

    #[test]
    fn tags_that_stop_leave_the_share_sent_in_place_once_the_phone_has_had_its_time() {
        let old = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let new = PhoneZoom { crop: 3.0, ..old };
        let (gate, framing) = aimed(old);
        let surface = card();
        gate.for_frame_checked("a", &tagged(Some(old), &surface), 0.0, false);
        gate.begin(old, new, (View::default(), View { zoom: 2.0, ..View::default() }), &framing, 0.1);
        gate.aim(Some(new), View { zoom: 2.0, ..View::default() }, &framing);
        let mut t = 0.13;
        while t < 0.1 + HANDOVER_S + 0.1 {
            gate.for_frame_checked("a", &tagged(None, &surface), t, false);
            t += 0.03;
        }
        let checked = gate.for_frame_checked("a", &tagged(None, &surface), t, false).unwrap();
        assert!(!checked.unsure && !gate.waiting());
        assert_eq!(checked.crop, Some(phone_window(Some(&new))), "not the crop of the last tag before the share");
    }

    #[test]
    fn a_lens_change_leaves_no_crop_behind_for_the_next_frame() {
        let sent = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let (gate, _) = aimed(sent);
        let surface = card();
        gate.note_camera("a", &serde_json::json!({"id": "main"}));
        let corrected = gate.for_frame_checked("a", &tagged(Some(PhoneZoom { crop: 1.8, ..sent }), &surface), 0.0, false).unwrap().changed.unwrap();
        assert!((total(&corrected) - 6.0).abs() < 1e-4 && corrected.0.zoom != 2.0);
        gate.note_camera("a", &serde_json::json!({"id": "tele"}));
        // The new lens gives the share sent: an untagged frame of it is put back to the window's view for that, not left with the correction
        let checked = gate.for_frame_checked("a", &tagged(None, &surface), 0.1, false).unwrap();
        assert_eq!(checked.changed, Some((View { zoom: 2.0, ..View::default() }, phone_window(Some(&sent)))));
    }

    #[test]
    fn a_new_connection_forgets_the_crop_of_the_last_one_before_the_window_hears_of_it() {
        let sent = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let (gate, _) = aimed(sent);
        let surface = card();
        gate.for_frame_checked("a", &tagged(Some(sent), &surface), 0.0, false);
        gate.connection_reset("other");
        assert_eq!(gate.for_frame_checked("a", &tagged(None, &surface), 0.03, false).unwrap().crop, Some(phone_window(Some(&sent))));
        // The worker reconnected and its first frame lost its tag: the phone starts a connection at its whole view
        gate.connection_reset("a");
        let checked = gate.for_frame_checked("a", &tagged(None, &surface), 5.0, false).unwrap();
        let neutral = PhoneZoom { crop: 1.0, ..sent };
        assert_eq!(checked.crop, Some(phone_window(Some(&neutral))));
        assert!((total(&checked.changed.unwrap()) - 6.0).abs() < 1e-4);
    }

    #[test]
    fn a_frame_of_a_new_connection_is_cropped_by_its_own_tag_before_the_window_hears_of_it() {
        let sent = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let (gate, _) = aimed(sent);
        let surface = card();
        gate.for_frame_checked("a", &tagged(Some(sent), &surface), 0.0, false);
        // The camera reopened at its whole view, and the window hasn't been told of the reconnect yet
        let neutral = PhoneZoom { crop: 1.0, ..sent };
        let shown = gate.for_frame_checked("a", &tagged(Some(neutral), &surface), 0.1, false).unwrap().changed.unwrap();
        assert!((total(&shown) - 6.0).abs() < 1e-4, "{shown:?}");
        assert_eq!(shown.1, phone_window(Some(&neutral)));
    }

    #[test]
    fn a_tagged_frame_names_the_crop_it_came_with_for_vision() {
        // The phone was asked for 2x and stayed at its whole view
        let sent = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.6, y: 0.5 };
        let (gate, _) = aimed(sent);
        let got = PhoneZoom { crop: 1.0, x: 0.5, ..sent };
        let checked = gate.for_frame_checked("a", &tagged(Some(got), &card()), 0.0, false).unwrap();
        assert_eq!(checked.crop, Some(phone_window(Some(&got))));
    }

    #[test]
    fn a_camera_settled_on_another_crop_is_only_looked_at_while_the_share_is_on_its_way() {
        let sent = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        let got = PhoneZoom { crop: 1.8, ..sent };
        let (gate, framing) = aimed(sent);
        let surface = card();
        gate.for_frame_checked("a", &tagged(Some(got), &surface), 0.0, false);
        gate.begin(got, sent, (View { zoom: 3.0, ..View::default() }, View { zoom: 3.0, ..View::default() }), &framing, 0.1);
        let copies = || surface.1.load(std::sync::atomic::Ordering::Relaxed);
        let mut after = None;
        for k in 0..200 {
            let at = 0.2 + k as f64 * 0.03;
            if at > 0.1 + HANDOVER_S + 0.1 && after.is_none() {
                after = Some(copies());
            }
            let tag = (k % 10 == 0).then_some(got);
            gate.for_frame_checked("a", &tagged(tag, &surface), at, false);
        }
        assert_eq!(after, Some(copies()), "no frame is copied off the card once the share has had its time");
        let shown = gate.waiting_snapshot().unwrap();
        assert!((total(&shown) - 6.0).abs() < 1e-4 && shown.1 == phone_window(Some(&got)));
    }
}
