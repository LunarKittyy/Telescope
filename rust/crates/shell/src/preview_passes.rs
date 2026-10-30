//! What plugins do to the preview's copy of the picture (overlays.PreviewLayer's tint, analyse and paint): the preview takes a
//! CPU copy at the size it paints while some pass tints or wants a look, runs each pass's tint on it, hands the passes a copy
//! from before any tint on a thread of their own at the rate they ask, and lays the pictures they paint over the frame.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

#[derive(Default)]
pub struct PassWork {
    scaler: Option<Box<dyn Scaler>>,
    /// By owner: when its analysis last started, and whether one is still running.
    analysed: BTreeMap<String, (Instant, Arc<AtomicBool>)>,
    painted_at: Option<Instant>,
    /// The size the overlay was last made for.
    size: (u32, u32),
}

/// The frame as RGBA at this size.
fn copy_of(scaler: &mut dyn Scaler, frame: &Frame, w: u32, h: u32) -> Option<Frame> {
    let rgb = scaler.rgb(frame, w, h);
    if rgb.len() != (w * h * 3) as usize {
        return None;
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
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

    /// Starts each due pass's analysis on a thread of its own, from the picture before any tint. When one finishes and asks for
    /// another paint, `dirty` is set.
    pub fn analyse(&mut self, passes: &[(String, Arc<dyn PreviewPass>)], copy: &Frame, dirty: &Arc<AtomicBool>) {
        for (owner, pass) in passes {
            if !self.due(owner, pass) {
                continue;
            }
            let busy = Arc::new(AtomicBool::new(true));
            self.analysed.insert(owner.clone(), (Instant::now(), busy.clone()));
            let (owner, pass, picture, dirty) = (owner.clone(), pass.clone(), copy.clone(), dirty.clone());
            std::thread::spawn(move || {
                if guarded(&owner, "analyse", || pass.analyse(&picture)).unwrap_or(false) {
                    dirty.store(true, Ordering::SeqCst);
                }
                busy.store(false, Ordering::SeqCst);
            });
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

    /// What every pass paints over the frame, as one picture the frame's size (None when nothing shows).
    pub fn overlay(&mut self, passes: &[(String, Arc<dyn PreviewPass>)], w: u32, h: u32, scale: f32) -> Option<Image> {
        self.painted_at = Some(Instant::now());
        self.size = (w, h);
        let mut out: Option<Vec<u8>> = None;
        for (owner, pass) in passes {
            let Some(layer) = guarded(owner, "paint", || pass.paint(w, h, scale)).flatten().filter(|l| (l.width, l.height) == (w, h) && l.format == PixelFormat::Rgba) else { continue };
            match out.as_mut() {
                None => out = Some(layer.data.as_ref().clone()),
                Some(below) => over(below, &layer.data),
            }
        }
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&out?, w, h);
        Some(Image::from_rgba8(buffer))
    }
}

/// `top` laid over `below`, both RGBA with straight alpha.
fn over(below: &mut [u8], top: &[u8]) {
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

    #[test]
    fn no_analysis_while_no_layer_wants_it() {
        let all = passes(vec![("plain", Arc::new(colours(1, 0)) as Arc<dyn PreviewPass>)]);
        let mut work = PassWork::default();
        work.analyse(&all, &grid(0), &Arc::new(AtomicBool::new(false)));
        assert!(work.analysed.is_empty());
    }
}
