//! What plugins do to the preview's copy of the picture (overlays.PreviewLayer's tint, analyse and paint): while some pass tints
//! or wants a look, a worker thread takes a CPU copy at the size the preview paints (so the window never waits on the graphics
//! card for it), runs each pass's tint on it and hands the passes a copy from before any tint, on a thread of their own at the
//! rate they ask. The window paints the worker's latest result, one frame behind, and lays the pictures the passes paint over it.

use slint::{Image, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use telescope_core::media::Scaler;
use telescope_core::preview::PreviewPass;
use telescope_core::{Frame, PixelFormat};

/// A pass that paints is asked again at least this often, since some draw only what's recent (the level guide notes it was seen).
const REPAINT: Duration = Duration::from_millis(500);

/// The owners whose pass has failed, so it's logged once.
static FAILING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Runs one pass's work; a pass that panics leaves the picture as it was and the others carry on (overlays._failed).
fn guarded<T>(owner: &str, what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            if FAILING.lock().unwrap_or_else(|e| e.into_inner()).insert(owner.to_string()) {
                log::error!("Preview layer {owner} failed in {what}");
            }
            None
        }
    }
}

/// A hand-over to a thread that only wants the newest thing: a new one replaces one nobody took yet, which is dropped.
struct Mailbox<T> {
    held: Mutex<(Option<T>, bool)>,
    wake: Condvar,
}

impl<T> Mailbox<T> {
    fn new() -> Arc<Mailbox<T>> {
        Arc::new(Mailbox { held: Mutex::new((None, false)), wake: Condvar::new() })
    }

    fn put(&self, value: T) {
        let old = {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            held.0.replace(value)
        };
        // Dropped outside the lock: it may be a job whose drop does something
        drop(old);
        self.wake.notify_one();
    }

    /// Waits for the newest thing; None once the mailbox is closed.
    fn take(&self) -> Option<T> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if held.1 {
                return None;
            }
            if let Some(value) = held.0.take() {
                return Some(value);
            }
            held = self.wake.wait(held).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn close(&self) {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.wake.notify_one();
    }
}

/// A thread that lives as long as its handle, working on the newest job put in.
pub(crate) struct Worker<T: Send + 'static> {
    mailbox: Arc<Mailbox<T>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    pub(crate) fn start(name: &str, mut run: impl FnMut(T) + Send + 'static) -> Worker<T> {
        let mailbox = Mailbox::new();
        let inbox = mailbox.clone();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            while let Some(job) = inbox.take() {
                run(job);
            }
        });
        if let Err(e) = &spawned {
            log::error!("Couldn't start the {name} thread: {e}");
        }
        Worker { mailbox, thread: spawned.ok() }
    }

    /// Closes the mailbox and waits for the job in hand to finish, so nothing it uses is still in use afterwards.
    pub(crate) fn finish(mut self) {
        self.mailbox.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    pub(crate) fn put(&self, job: T) {
        self.mailbox.put(job);
    }
}

impl<T: Send + 'static> Drop for Worker<T> {
    fn drop(&mut self) {
        self.mailbox.close();
    }
}

/// An analysis that is running or waiting: marks its owner busy until it's finished or dropped unrun.
struct Busy(Arc<AtomicBool>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct AnalyseJob {
    pass: Arc<dyn PreviewPass>,
    picture: Frame,
    dirty: Arc<AtomicBool>,
    busy: Busy,
}

/// The passes' analysis threads, one for each owner that has analysed, made when it first does.
#[derive(Default)]
struct Scopes {
    workers: Mutex<BTreeMap<String, Worker<AnalyseJob>>>,
}

impl Scopes {
    fn put(&self, owner: &str, job: AnalyseJob) {
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let worker = workers.entry(owner.to_string()).or_insert_with(|| {
            let owner = owner.to_string();
            Worker::start("preview-scope", move |job: AnalyseJob| {
                if guarded(&owner, "analyse", || job.pass.analyse(&job.picture)).unwrap_or(false) {
                    job.dirty.store(true, Ordering::SeqCst);
                }
                drop(job.busy);
            })
        });
        worker.put(job);
    }
}

/// Which frame a tint was made for: the stream it came from (a new camera or a new stream of the same one is another) and its number there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameId {
    pub source: u64,
    pub number: u64,
}

/// A tint that nobody got in this long means the worker can't make one, and the window paints without it.
const TINT_GIVE_UP: Duration = Duration::from_millis(300);

/// What the tint worker made: the tinted picture as the buffer Slint shows, which run of tinting it belongs to and the frame it was made from.
#[derive(Clone)]
struct Tinted {
    epoch: u64,
    id: FrameId,
    buffer: SharedPixelBuffer<Rgb8Pixel>,
}

struct TintJob {
    frame: Frame,
    size: (u32, u32),
    passes: Vec<(String, Arc<dyn PreviewPass>)>,
    /// The passes that are due a look, with the mark that says they're busy until they've had it.
    looks: Vec<(String, Arc<dyn PreviewPass>, Busy)>,
    tints: bool,
    epoch: u64,
    id: FrameId,
    dirty: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct PassWork {
    scaler: Option<Box<dyn Scaler>>,
    /// By owner: when its analysis last started, and whether one is still running.
    analysed: BTreeMap<String, (Instant, Arc<AtomicBool>)>,
    painted_at: Option<Instant>,
    /// The size the overlay was last made for.
    size: (u32, u32),
    scopes: Arc<Scopes>,
    tinter: Option<Worker<TintJob>>,
    /// The newest tinted picture: it goes stale when tinting stops.
    tinted: Arc<Mutex<Option<Tinted>>>,
    epoch: Arc<AtomicU64>,
    /// The stream the last frame submitted came from.
    source: u64,
    /// The frame whose tint was last handed to the window.
    shown: Option<FrameId>,
    /// Since when the window has wanted a tint and had none.
    missing_since: Option<Instant>,
    /// The last frame submitted had a pass that tints.
    tinting: bool,
}

/// The frame as RGBA at this size.
fn copy_of(scaler: &mut dyn Scaler, frame: &Frame, w: u32, h: u32) -> Option<Frame> {
    let rgba = scaler.rgba(frame, w, h);
    if rgba.len() != (w * h * 4) as usize {
        return None;
    }
    Some(Frame::new(w, h, PixelFormat::Rgba, rgba))
}

impl PassWork {
    /// The passes in the order they draw: lower first.
    pub fn sorted(passes: &[(String, Arc<dyn PreviewPass>)]) -> Vec<(String, Arc<dyn PreviewPass>)> {
        let mut sorted = passes.to_vec();
        sorted.sort_by_key(|(_, p)| p.order());
        sorted
    }

    /// Whether the preview has to read the picture off the graphics card: a pass tints it, or one is due a look.
    pub fn needs_copy(&self, passes: &[(String, Arc<dyn PreviewPass>)]) -> bool {
        passes.iter().any(|(owner, p)| p.tints() || self.due(owner, p))
    }

    fn due(&self, owner: &str, pass: &Arc<dyn PreviewPass>) -> bool {
        let fps = pass.analyse_fps();
        if fps <= 0.0 {
            return false;
        }
        match self.analysed.get(owner) {
            Some((at, busy)) => !busy.load(Ordering::SeqCst) && at.elapsed().as_secs_f32() >= 1.0 / fps,
            None => true,
        }
    }

    /// The picture at w x h as RGBA, from this scaler (made on first use).
    pub fn copy(&mut self, make: impl FnOnce() -> Option<Box<dyn Scaler>>, frame: &Frame, w: u32, h: u32) -> Option<Frame> {
        if self.scaler.is_none() {
            self.scaler = make();
        }
        copy_of(self.scaler.as_mut()?.as_mut(), frame, w, h)
    }

    /// Hands each due pass's analysis to its thread, from the picture before any tint. When one finishes and asks for another
    /// paint, `dirty` is set.
    pub fn analyse(&mut self, passes: &[(String, Arc<dyn PreviewPass>)], copy: &Frame, dirty: &Arc<AtomicBool>) {
        for (owner, pass) in passes {
            if let Some(busy) = self.begin(owner, pass) {
                self.scopes.put(owner, AnalyseJob { pass: pass.clone(), picture: copy.clone(), dirty: dirty.clone(), busy });
            }
        }
    }

    /// Marks the pass's analysis as started if it is due.
    fn begin(&mut self, owner: &str, pass: &Arc<dyn PreviewPass>) -> Option<Busy> {
        if !self.due(owner, pass) {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(true));
        self.analysed.insert(owner.to_string(), (Instant::now(), flag.clone()));
        Some(Busy(flag))
    }

    /// Has the worker make the frame's copy at `size`, tint it and give each due pass its look, without the window waiting for any
    /// of it. `make` makes the worker's scaler the first time; false when there's none, and the window paints without the tint.
    /// The window paints what came out of the last one (`tinted`).
    pub fn submit(&mut self, make: impl FnOnce() -> Option<Box<dyn Scaler>>, passes: &[(String, Arc<dyn PreviewPass>)], frame: &Frame, id: FrameId, size: (u32, u32), dirty: &Arc<AtomicBool>) -> bool {
        if self.tinter.is_none() {
            let Some(mut scaler) = self.scaler.take().or_else(make) else { return false };
            let (scopes, tinted, current) = (self.scopes.clone(), self.tinted.clone(), self.epoch.clone());
            self.tinter = Some(Worker::start("preview-tint", move |job: TintJob| {
                let out = catch_unwind(AssertUnwindSafe(|| run_tint(scaler.as_mut(), &scopes, job, &tinted, &current)));
                if out.is_err() {
                    log::error!("The preview's tint thread failed on a frame");
                }
            }));
        }
        // Another stream's pictures are another run: nothing made for the last one is kept, or shown for the first frame of this one
        if id.source != self.source {
            self.source = id.source;
            self.restart_tinting();
        }
        let looks = passes.iter().filter_map(|(owner, pass)| Some((owner.clone(), pass.clone(), self.begin(owner, pass)?))).collect();
        let tints = passes.iter().any(|(_, p)| p.tints());
        self.tinting = tints;
        let epoch = self.epoch.load(Ordering::SeqCst);
        if let Some(tinter) = &self.tinter {
            tinter.put(TintJob { frame: frame.clone(), size, passes: passes.to_vec(), looks, tints, epoch, id, dirty: dirty.clone() });
        }
        true
    }

    /// The newest tinted picture at w x h from this stream as the image to show, if the worker has made one for this run of tinting.
    pub fn tinted(&mut self, w: u32, h: u32, source: u64) -> Option<Image> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let made = {
            let held = self.tinted.lock().unwrap_or_else(|e| e.into_inner());
            held.as_ref().filter(|t| t.epoch == epoch && t.id.source == source && (t.buffer.width(), t.buffer.height()) == (w, h)).map(|t| (t.id, t.buffer.clone()))
        };
        let Some((id, buffer)) = made else {
            self.missing_since.get_or_insert_with(Instant::now);
            return None;
        };
        self.missing_since = None;
        self.shown = Some(id);
        Some(Image::from_rgb8(buffer))
    }

    /// Whether the worker has a tint of this stream newer than the one last handed over: a frame painted from the one before it can be
    /// painted again, which is how a stream that stopped sending catches up.
    pub fn tint_newer(&self, source: u64) -> bool {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let held = self.tinted.lock().unwrap_or_else(|e| e.into_inner());
        held.as_ref().is_some_and(|t| t.epoch == epoch && t.id.source == source && self.shown.is_none_or(|s| s.source != source || t.id.number > s.number))
    }

    /// Whether the window has waited for a tint so long that the worker can't be making one (its copy failed), so the picture is
    /// painted without it rather than not at all.
    pub fn tint_gave_up(&self) -> bool {
        self.missing_since.is_some_and(|at| at.elapsed() >= TINT_GIVE_UP)
    }

    /// What the worker made so far is out of date, and a frame still being made is too.
    fn restart_tinting(&mut self) {
        let mut held = self.tinted.lock().unwrap_or_else(|e| e.into_inner());
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *held = None;
        self.shown = None;
        self.missing_since = None;
    }

    /// Nothing is tinting any more: what the worker made so far is out of date, and a frame still being made is too.
    pub fn stop_tinting(&mut self) {
        if std::mem::take(&mut self.tinting) {
            self.restart_tinting();
        }
    }

    /// Runs every pass's tint on the copy.
    pub fn tint(passes: &[(String, Arc<dyn PreviewPass>)], frame: &mut Frame) {
        for (owner, pass) in passes {
            if guarded(owner, "tint", || pass.tints()).unwrap_or(false) {
                guarded(owner, "tint", || pass.tint(frame));
            }
        }
    }

    /// Whether the overlay is due: something asked for another paint, the size changed, or it has been a while.
    pub fn overlay_due(&self, dirty: bool, size: (u32, u32)) -> bool {
        dirty || size != self.size || self.painted_at.is_none_or(|at| at.elapsed() >= REPAINT)
    }

    /// What every pass paints over the frame, as one picture the size it's shown at (None when nothing shows).
    pub fn overlay(&mut self, passes: &[(String, Arc<dyn PreviewPass>)], w: u32, h: u32, scale: f32) -> Option<Image> {
        self.painted_at = Some(Instant::now());
        self.size = (w, h);
        let mut out: Option<SharedPixelBuffer<Rgba8Pixel>> = None;
        for (owner, pass) in passes {
            let Some(layer) = guarded(owner, "paint", || pass.paint(w, h, scale)).flatten().filter(|l| (l.width, l.height) == (w, h) && l.format == PixelFormat::Rgba) else { continue };
            match out.as_mut() {
                // The first picture is copied once, into what Slint shows, and the pass can keep its own to paint on again
                None => out = Some(SharedPixelBuffer::clone_from_slice(&layer.data, w, h)),
                Some(below) => over(below.make_mut_bytes(), &layer.data),
            }
        }
        Some(Image::from_rgba8(out?))
    }
}

/// One frame's work on the tint thread: the copy, each due pass's look, the tint.
fn run_tint(scaler: &mut dyn Scaler, scopes: &Scopes, job: TintJob, held: &Mutex<Option<Tinted>>, current: &AtomicU64) {
    let (w, h) = job.size;
    let Some(copy) = copy_of(scaler, &job.frame, w, h) else { return };
    for (owner, pass, busy) in job.looks {
        scopes.put(&owner, AnalyseJob { pass, picture: copy.clone(), dirty: job.dirty.clone(), busy });
    }
    if !job.tints {
        return;
    }
    let mut shown = copy;
    PassWork::tint(&job.passes, &mut shown);
    if shown.data.len() != (w * h * 4) as usize {
        return;
    }
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
    telescope_core::image::drop_alpha(&shown.data, w as usize, buffer.make_mut_bytes());
    let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
    // A run of tinting that ended while this was made isn't kept
    if job.epoch != current.load(Ordering::SeqCst) {
        return;
    }
    // The first picture of a run, or of a new size, is painted as soon as the window next looks
    let fresh = held.as_ref().is_none_or(|t| t.epoch != job.epoch || (t.buffer.width(), t.buffer.height()) != (w, h));
    *held = Some(Tinted { epoch: job.epoch, id: job.id, buffer });
    if fresh {
        job.dirty.store(true, Ordering::SeqCst);
    }
}

/// `top` laid over `below`, both RGBA with straight alpha.
fn over(below: &mut [u8], top: &[u8]) {
    for (b, t) in below.as_chunks_mut::<4>().0.iter_mut().zip(top.as_chunks::<4>().0) {
        // Nothing of the top leaves what's below as it is, and all of it hides what's below: the sums come to the same
        match t[3] {
            0 => continue,
            255 => {
                *b = *t;
                continue;
            }
            _ => {}
        }
        let (ta, ba) = (t[3] as f32 / 255.0, b[3] as f32 / 255.0);
        let out = ta + ba * (1.0 - ta);
        if out <= 0.0 {
            continue;
        }
        for c in 0..3 {
            b[c] = ((t[c] as f32 * ta + b[c] as f32 * ba * (1.0 - ta)) / out).round() as u8;
        }
        b[3] = (out * 255.0).round() as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_laid_over_another_keeps_what_shows_through() {
        let mut below = vec![0, 0, 255, 255, 10, 20, 30, 0];
        over(&mut below, &[255, 255, 255, 128, 255, 0, 0, 0]);
        // half white over opaque blue
        assert_eq!(below[3], 255);
        assert!((below[0] as i32 - 128).abs() <= 1 && (below[2] as i32 - 255).abs() <= 1);
        // nothing over nothing leaves it
        assert_eq!(&below[4..], &[10, 20, 30, 0]);
    }

    /// `over` as it was before the clear and solid pixels got their short cuts.
    fn over_before(below: &mut [u8], top: &[u8]) {
        for (b, t) in below.as_chunks_mut::<4>().0.iter_mut().zip(top.as_chunks::<4>().0) {
            let (ta, ba) = (t[3] as f32 / 255.0, b[3] as f32 / 255.0);
            let out = ta + ba * (1.0 - ta);
            if out <= 0.0 {
                continue;
            }
            for c in 0..3 {
                b[c] = ((t[c] as f32 * ta + b[c] as f32 * ba * (1.0 - ta)) / out).round() as u8;
            }
            b[3] = (out * 255.0).round() as u8;
        }
    }

    #[test]
    fn laying_pictures_over_each_other_gives_the_bytes_it_always_did() {
        let mut s = 12345u32;
        let mut noise = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    (s >> 9) as u8
                })
                .collect()
        };
        let (mut below, mut top) = (noise(4 * 5000), noise(4 * 5000));
        // Plenty of clear and solid pixels among the in-between ones, on both
        for (i, (b, t)) in below.chunks_exact_mut(4).zip(top.chunks_exact_mut(4)).enumerate() {
            b[3] = [0, 255, b[3], b[3]][i % 4];
            t[3] = [t[3], 0, 255, t[3], 0][i % 5];
        }
        let mut before = below.clone();
        over_before(&mut before, &top);
        over(&mut below, &top);
        assert_eq!(below, before);
    }

    struct Tints;

    impl PreviewPass for Tints {
        fn tints(&self) -> bool {
            true
        }
        fn analyse_fps(&self) -> f32 {
            10.0
        }
    }

    #[test]
    fn a_pass_that_tints_needs_the_picture_and_one_that_looks_is_due_at_its_rate() {
        let mut work = PassWork::default();
        let passes: Vec<(String, Arc<dyn PreviewPass>)> = vec![("t".into(), Arc::new(Tints))];
        assert!(work.needs_copy(&passes));
        let frame = Frame::new(2, 2, PixelFormat::Rgba, vec![0; 16]);
        let dirty = Arc::new(AtomicBool::new(false));
        work.analyse(&passes, &frame, &dirty);
        // started just now, so not due again yet
        assert!(!work.due("t", &passes[0].1));
        assert!(!dirty.load(Ordering::SeqCst), "this pass asks for no repaint");
    }

    #[test]
    fn the_overlay_is_due_when_asked_for_resized_or_stale() {
        let mut work = PassWork::default();
        assert!(work.overlay_due(false, (10, 10)));
        assert!(work.overlay(&[], 10, 10, 1.0).is_none());
        assert!(!work.overlay_due(false, (10, 10)));
        assert!(work.overlay_due(true, (10, 10)));
        assert!(work.overlay_due(false, (11, 10)));
    }

    /// Sets the first pixel's red to `value`, in the order it was given.
    struct Colours {
        value: u8,
        order: i32,
        fps: f32,
        analysed: Arc<Mutex<Vec<u8>>>,
        painted: Arc<std::sync::atomic::AtomicUsize>,
    }

    fn colours(value: u8, order: i32) -> Colours {
        Colours { value, order, fps: 0.0, analysed: Default::default(), painted: Default::default() }
    }

    impl PreviewPass for Colours {
        fn order(&self) -> i32 {
            self.order
        }
        fn tints(&self) -> bool {
            true
        }
        fn tint(&self, frame: &mut Frame) {
            frame.data_mut()[0] = self.value;
        }
        fn analyse_fps(&self) -> f32 {
            self.fps
        }
        fn analyse(&self, frame: &Frame) -> bool {
            self.analysed.lock().unwrap().push(frame.data[0]);
            true
        }
        fn paint(&self, width: u32, height: u32, _scale: f32) -> Option<Frame> {
            self.painted.fetch_add(1, Ordering::SeqCst);
            Some(Frame::new(width, height, PixelFormat::Rgba, vec![255; (width * height * 4) as usize]))
        }
    }

    struct Broken;

    impl PreviewPass for Broken {
        fn tints(&self) -> bool {
            true
        }
        fn tint(&self, _frame: &mut Frame) {
            panic!("plugin bug");
        }
        fn paint(&self, _width: u32, _height: u32, _scale: f32) -> Option<Frame> {
            panic!("plugin bug");
        }
    }

    fn grid(value: u8) -> Frame {
        Frame::new(4, 4, PixelFormat::Rgba, vec![value; 4 * 4 * 4])
    }

    type Passes = Vec<(String, Arc<dyn PreviewPass>)>;

    fn passes(list: Vec<(&str, Arc<dyn PreviewPass>)>) -> Passes {
        PassWork::sorted(&list.into_iter().map(|(o, p)| (o.to_string(), p)).collect::<Vec<_>>())
    }

    #[test]
    fn layers_tint_in_order_and_a_broken_one_leaves_the_frame_alone() {
        let late: Arc<dyn PreviewPass> = Arc::new(colours(200, 5));
        let early: Arc<dyn PreviewPass> = Arc::new(colours(100, -5));
        let broken: Arc<dyn PreviewPass> = Arc::new(Broken);
        let all = passes(vec![("late", late), ("early", early.clone()), ("broken", broken.clone())]);
        let mut frame = grid(0);
        PassWork::tint(&all, &mut frame);
        assert_eq!(frame.data[0], 200, "the later layer tints over the earlier one");
        let mut frame = grid(0);
        PassWork::tint(&passes(vec![("early", early), ("broken", broken)]), &mut frame);
        assert_eq!(frame.data[0], 100);
        assert_eq!(&frame.data[1..4], &[0, 0, 0], "and the broken one touched nothing");
    }

    #[test]
    fn paint_reaches_every_layer_even_past_a_broken_one() {
        let good = Arc::new(colours(1, 0));
        let all = passes(vec![("broken", Arc::new(Broken) as Arc<dyn PreviewPass>), ("good", good.clone())]);
        assert!(PassWork::default().overlay(&all, 64, 36, 1.0).is_some());
        assert_eq!(good.painted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn analyse_gets_the_frame_before_tint_on_its_own_thread() {
        let watcher = Arc::new(Colours { fps: 1000.0, ..colours(77, 0) });
        let all = passes(vec![("scope", watcher.clone() as Arc<dyn PreviewPass>)]);
        let dirty = Arc::new(AtomicBool::new(false));
        let mut work = PassWork::default();
        let copy = grid(5);
        work.analyse(&all, &copy, &dirty);
        let mut tinted = copy.clone();
        PassWork::tint(&all, &mut tinted);
        assert_eq!(tinted.data[0], 77);
        let deadline = Instant::now() + Duration::from_secs(2);
        while (watcher.analysed.lock().unwrap().is_empty() || !dirty.load(Ordering::SeqCst)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*watcher.analysed.lock().unwrap(), [5]);
        assert!(dirty.load(Ordering::SeqCst), "it asked for a repaint");
    }

    /// A scaler that gives a flat grey picture.
    struct Flat(u8);

    impl Scaler for Flat {
        fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![self.0; (width * height * 3) as usize]
        }
    }

    /// A scaler whose picture is as grey as the first byte of the frame it's given, so a picture says which frame it came from.
    struct Shade;

    impl Scaler for Shade {
        fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![frame.data[0]; (width * height * 3) as usize]
        }
    }

    /// A scaler that can't make a copy.
    struct CantCopy;

    impl Scaler for CantCopy {
        fn rgb(&mut self, _frame: &Frame, _width: u32, _height: u32) -> Vec<u8> {
            Vec::new()
        }
    }

    fn id(source: u64, number: u64) -> FrameId {
        FrameId { source, number }
    }

    fn shaded(shade: u8) -> Frame {
        Frame::new(8, 8, PixelFormat::Rgba, vec![shade; 8 * 8 * 4])
    }

    fn grey_of(image: &Image) -> u8 {
        image.to_rgb8().unwrap().as_bytes()[3]
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(), "{what}");
    }

    #[test]
    fn a_tint_is_made_on_the_worker_and_painted_from_its_last_result() {
        let all = passes(vec![("tint", Arc::new(colours(200, 0)) as Arc<dyn PreviewPass>)]);
        let dirty = Arc::new(AtomicBool::new(false));
        let mut work = PassWork::default();
        let source = Frame::new(8, 8, PixelFormat::Rgba, vec![0; 8 * 8 * 4]);
        assert!(work.tinted(4, 4, 1).is_none(), "nothing before the worker has made one");
        assert!(work.submit(|| Some(Box::new(Flat(50))), &all, &source, id(1, 1), (4, 4), &dirty));
        wait_for("the first picture asks for a paint", || dirty.load(Ordering::SeqCst));
        let image = work.tinted(4, 4, 1).expect("the tinted picture");
        let rgb = image.to_rgb8().unwrap();
        assert_eq!(rgb.as_bytes()[..3], [200, 50, 50], "the pass tinted the first pixel");
        assert_eq!(rgb.as_bytes()[3..6], [50, 50, 50]);
        assert!(work.tinted(5, 5, 1).is_none(), "a picture of another size isn't used");
        work.stop_tinting();
        assert!(work.tinted(4, 4, 1).is_none(), "once tinting stops, the old picture is stale");
    }

    #[test]
    fn a_new_stream_never_shows_the_last_ones_tint_at_the_same_size() {
        let all = passes(vec![("tint", Arc::new(colours(200, 0)) as Arc<dyn PreviewPass>)]);
        let dirty = Arc::new(AtomicBool::new(false));
        let mut work = PassWork::default();
        assert!(work.submit(|| Some(Box::new(Shade)), &all, &shaded(40), id(1, 1), (4, 4), &dirty));
        wait_for("the first stream's picture", || work.tinted(4, 4, 1).is_some());
        // The camera is switched: its first frame is submitted, and the old picture is the same size
        assert!(work.tinted(4, 4, 2).is_none(), "another stream's picture isn't the new one's");
        assert!(work.submit(|| None, &all, &shaded(99), id(2, 1), (4, 4), &dirty));
        assert!(work.tinted(4, 4, 1).is_none(), "and the old stream's is dropped");
        let mut first = None;
        wait_for("the new stream's picture", || {
            first = work.tinted(4, 4, 2);
            first.is_some()
        });
        assert_eq!(grey_of(&first.unwrap()), 99, "its first picture is its own");
    }

    #[test]
    fn a_worker_that_cant_copy_lets_the_window_paint_without_the_tint() {
        let all = passes(vec![("tint", Arc::new(colours(200, 0)) as Arc<dyn PreviewPass>)]);
        let mut work = PassWork::default();
        let dirty = Arc::new(AtomicBool::new(false));
        assert!(work.submit(|| Some(Box::new(CantCopy)), &all, &shaded(1), id(1, 1), (4, 4), &dirty));
        assert!(work.tinted(4, 4, 1).is_none());
        assert!(!work.tint_gave_up(), "a worker gets a moment for its first picture");
        std::thread::sleep(TINT_GIVE_UP + Duration::from_millis(50));
        assert!(work.tinted(4, 4, 1).is_none());
        assert!(work.tint_gave_up(), "then the picture is painted without it");
    }

    #[test]
    fn a_newer_tint_than_the_one_shown_can_be_painted_again() {
        let all = passes(vec![("tint", Arc::new(colours(200, 0)) as Arc<dyn PreviewPass>)]);
        let mut work = PassWork::default();
        let dirty = Arc::new(AtomicBool::new(false));
        assert!(!work.tint_newer(1), "nothing made yet");
        assert!(work.submit(|| Some(Box::new(Shade)), &all, &shaded(10), id(1, 1), (4, 4), &dirty));
        wait_for("the first result", || work.tint_newer(1));
        assert_eq!(grey_of(&work.tinted(4, 4, 1).unwrap()), 10);
        assert!(!work.tint_newer(1), "what was handed over isn't newer");
        // The next frame is painted from that result while its own is made, and the stream then stops
        assert!(work.submit(|| None, &all, &shaded(20), id(1, 2), (4, 4), &dirty));
        wait_for("the second result is newer than the one shown", || work.tint_newer(1));
        assert_eq!(grey_of(&work.tinted(4, 4, 1).unwrap()), 20);
        assert!(!work.tint_newer(1));
        assert!(!work.tint_newer(2), "another stream's isn't asked about");
    }

    #[test]
    fn no_scaler_means_no_worker_and_the_window_paints_without_the_tint() {
        let all = passes(vec![("tint", Arc::new(colours(1, 0)) as Arc<dyn PreviewPass>)]);
        let mut work = PassWork::default();
        let source = Frame::new(2, 2, PixelFormat::Rgba, vec![0; 16]);
        assert!(!work.submit(|| None, &all, &source, id(1, 1), (2, 2), &Arc::new(AtomicBool::new(false))));
    }

    #[test]
    fn a_look_that_is_dropped_before_it_ran_frees_its_pass_for_the_next() {
        let (first, second) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(true)));
        let mailbox = Mailbox::new();
        mailbox.put(Busy(first.clone()));
        mailbox.put(Busy(second.clone()));
        assert!(!first.load(Ordering::SeqCst), "the older one was replaced");
        assert!(second.load(Ordering::SeqCst), "the newest waits for its turn");
        drop(mailbox.take());
        assert!(!second.load(Ordering::SeqCst));
    }

    #[test]
    fn the_scope_runs_on_a_thread_that_stays() {
        let watcher = Arc::new(Colours { fps: 1000.0, ..colours(0, 0) });
        let all = passes(vec![("scope", watcher.clone() as Arc<dyn PreviewPass>)]);
        let dirty = Arc::new(AtomicBool::new(false));
        let mut work = PassWork::default();
        for round in 0..3u8 {
            work.analyse(&all, &grid(round), &dirty);
            wait_for("the analysis ran", || watcher.analysed.lock().unwrap().len() == round as usize + 1 && !work.analysed["scope"].1.load(Ordering::SeqCst));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*watcher.analysed.lock().unwrap(), [0, 1, 2]);
    }

    #[test]
    fn no_analysis_while_no_layer_wants_it() {
        let all = passes(vec![("plain", Arc::new(colours(1, 0)) as Arc<dyn PreviewPass>)]);
        let mut work = PassWork::default();
        work.analyse(&all, &grid(0), &Arc::new(AtomicBool::new(false)));
        assert!(work.analysed.is_empty());
    }
}
