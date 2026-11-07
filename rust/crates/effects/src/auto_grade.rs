//! What Auto colour grading needs between the Colour grading card and the frames: the frame step that looks at the picture and what the people events tell it. The measuring is in `auto_measure`, the fit in `auto_fit`.

use crate::auto_fit::Basic;
use crate::auto_measure::{noise_level, Look, Measurement, Measurer};
use crate::auto_tuning::*;
use crate::cpu::Rgb;
use crate::shared::mask_of;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::{CpuStep, Frame, FrameContext, Touch};
use telescope_core::host::Poster;
use telescope_core::people::Box4;
use telescope_core::pose::Person;
use telescope_core::time::monotonic_ns;
use telescope_core::view::Geometry;

/// What the step posts once it has enough frames.
pub const POST_MEASURED: &str = "auto_measured";
/// The timer that ends a measurement that never got enough.
pub const T_TIMEOUT: &str = "auto_timeout";

/// What the vision service last said about the people on the stream being measured.
#[derive(Default)]
struct Seen {
    people: Vec<Person>,
    main: Option<Box4>,
    /// When the people came in, monotonic nanoseconds.
    at_ns: u64,
    view: Geometry,
}

#[derive(Default)]
struct Inner {
    source: Option<String>,
    measurer: Option<Measurer>,
    seen: Seen,
    done: Option<Measurement>,
    last_ns: Option<u64>,
}

/// Shared by the plugin (on the host thread) and the frame steps (on the streams' threads).
#[derive(Default)]
pub struct Shared {
    inner: Mutex<Inner>,
    /// Goes up whenever a measurement starts or ends, so a frame still being looked at for an old one can tell.
    generation: AtomicU64,
    /// When a frame was last let down from the card for a look.
    last_read: Mutex<Option<Instant>>,
}

impl Shared {
    /// Starts looking at this stream's frames.
    pub fn start(&self, source: &str, view: Geometry) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.last_read.lock().unwrap() = None;
        let mut i = self.inner.lock().unwrap();
        *i = Inner { source: Some(source.to_string()), measurer: Some(Measurer::new()), seen: Seen { view, ..Seen::default() }, ..Inner::default() };
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.inner.lock().unwrap() = Inner::default();
    }

    pub fn running(&self) -> bool {
        self.inner.lock().unwrap().source.is_some()
    }

    /// The source being measured, if any.
    pub fn source(&self) -> Option<String> {
        self.inner.lock().unwrap().source.clone()
    }

    pub fn hear_people(&self, people: Vec<Person>, at_ns: u64) {
        let mut i = self.inner.lock().unwrap();
        (i.seen.people, i.seen.at_ns) = (people, at_ns);
    }

    pub fn hear_main(&self, main: Option<Box4>) {
        self.inner.lock().unwrap().seen.main = main;
    }

    pub fn hear_view(&self, view: Geometry) {
        self.inner.lock().unwrap().seen.view = view;
    }

    /// What was measured: whatever the step finished, or (timing out, a stream that stopped) what it has so far. Ends the measurement either way.
    pub fn take(&self) -> Option<Measurement> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut i = self.inner.lock().unwrap();
        let done = i.done.take().or_else(|| i.measurer.take().map(Measurer::finish));
        *i = Inner::default();
        done
    }

    /// Whether this stream's frames are being looked at, and it's time for another: a frame is only let down from the card now and then.
    fn due_to_read(&self, source_id: &str) -> bool {
        if !self.wants(source_id) {
            return false;
        }
        let mut last = self.last_read.lock().unwrap();
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < Duration::from_millis(FRAME_EVERY_MS)) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// Whether this stream's frames are being looked at.
    fn wants(&self, source_id: &str) -> bool {
        let i = self.inner.lock().unwrap();
        i.measurer.is_some() && i.source.as_deref() == Some(source_id)
    }
}

/// The frame step: while a measurement runs, shrinks the stream's frames now and then and hands them to the measurer. The frame goes on untouched.
pub struct MeasureStep {
    pub shared: Arc<Shared>,
    pub poster: Arc<dyn Poster>,
}

impl CpuStep for MeasureStep {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        if self.shared.due_to_read(ctx.source_id()) { Touch::Reads } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        if !self.shared.wants(ctx.source_id()) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        {
            let mut i = self.shared.inner.lock().unwrap();
            if i.last_ns.is_some_and(|last| frame.time_ns.saturating_sub(last) < FRAME_EVERY_MS * 1_000_000) {
                return;
            }
            i.last_ns = Some(frame.time_ns);
        }
        let Some(rgb) = Rgb::from_frame(frame) else { return };
        let noise = noise_level(&rgb);
        let (w, h) = (rgb.w, rgb.h);
        let small = if w > SMALL_WIDTH { rgb.shrink(SMALL_WIDTH, ((h * SMALL_WIDTH) as f64 / w as f64).round().max(1.0) as usize) } else { rgb.clone() };
        let mask = mask_of(ctx, frame).map(|m| m.shrink(small.w, small.h));
        let (people, main, view, mut measurer) = {
            let mut i = self.shared.inner.lock().unwrap();
            if self.shared.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let fresh = frame.time_ns.abs_diff(i.seen.at_ns) <= FRESH_NS;
            let people = if fresh { i.seen.people.clone() } else { Vec::new() };
            let Some(measurer) = i.measurer.take() else { return };
            (people, i.seen.main, i.seen.view, measurer)
        };
        let finished = measurer.look(&Look { small: &small, mask: mask.as_ref(), people: &people, main, view: &view, noise, time_ns: frame.time_ns });
        let mut i = self.shared.inner.lock().unwrap();
        if self.shared.generation.load(Ordering::SeqCst) != generation || i.source.is_none() {
            // stopped, or stopped and started again, while this frame was being looked at: it belongs to a measurement that's over
            return;
        }
        if finished {
            i.done = Some(measurer.finish());
            drop(i);
            self.poster.post(POST_MEASURED, Value::Null);
        } else {
            i.measurer = Some(measurer);
        }
    }
}

/// What Undo puts back: the sliders Auto replaced and, once the phone's exposure and white balance can be locked for a measurement, the camera's own state.
#[derive(Clone, Debug)]
pub struct Undo {
    pub basic: Basic,
    pub camera: Option<Value>,
}

/// The camera as measuring found it. Where the phone's auto exposure and white balance locks plug in.
#[derive(Clone, Debug)]
pub struct CameraLock {
    /// What Undo gives back to the camera.
    pub restore: Option<Value>,
    /// The source can't be locked, so the result may drift as it adjusts itself.
    pub drifts: bool,
}

impl Default for CameraLock {
    fn default() -> CameraLock {
        CameraLock { restore: None, drifts: true }
    }
}

/// What the card shows about Auto.
pub struct AutoState {
    /// Monotonic nanoseconds, which a test sets by hand.
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub shared: Arc<Shared>,
    pub poster: Option<Arc<dyn Poster>>,
    pub busy: bool,
    pub lock: Option<CameraLock>,
    pub undo: Option<Undo>,
    /// A press finished, so the brighter and darker buttons make sense.
    pub nudge: bool,
    /// Auto's values are in the sliders now, so a later press that finds nothing to change takes them out.
    pub wrote: bool,
    pub note: (String, &'static str),
}

impl Default for AutoState {
    fn default() -> AutoState {
        AutoState { now: Arc::new(monotonic_ns), shared: Arc::default(), poster: None, busy: false, lock: None, undo: None, nudge: false, wrote: false, note: (String::new(), "dim") }
    }
}

impl AutoState {
    pub fn props(&self) -> Value {
        json!({"auto-busy": self.busy, "auto-undo": self.undo.is_some(), "auto-nudge": self.nudge, "auto-note": self.note.0, "auto-kind": self.note.1})
    }
}
