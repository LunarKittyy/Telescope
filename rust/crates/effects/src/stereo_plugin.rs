//! The 3D plugin: two phones streaming at once become one stereo picture on the Telescope camera.
//!
//! The picture is made in the frame step of the stream going to the Telescope camera (slot 0), like One picture's. Each of that step's frames
//! is paired with the other eye's frame taken at about the same moment (stereo::pair_frames), the pair is packed side by side or as red/cyan
//! and goes out in place of the stream's frame. The target stream's own frames are kept here, since what the host keeps of it afterwards is
//! the picture this step made. While it's switched off, or fewer than two streams run, the streams go out as they are.
//!
//! Match (stereo_match) works on two different phones: it sets both to the same shutter, nudges the right phone's ISO until the middle of its
//! picture is as bright as the left's, and fits colour curves that the packing applies to the right eye. The step reads the pictures for it
//! while it runs; the card's timer reads what the step saw and sends the ISO changes.

use crate::stereo::{self, Eye, Look, SyncStatus, Tune, DEFAULT_FRAME_NS, SHIFT_LIMIT, TILT_LIMIT, TIMING_LIMIT_MS};
use crate::stereo_match::{self, Act, Lut, Outcome, Run, Stats, SIZE_MAX, SIZE_MIN};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, StreamFrame, Touch};
use telescope_core::host::{FrameStore, Host};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stereo";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stereo.slint"));
/// What the Telescope camera opens at before any stream has said how big it is.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
/// How old a stream's newest picture can be and still be an eye.
const STALE_NS: u64 = 3_000_000_000;
/// How long a sync reading stays on the card once pictures stop coming.
const SEEN_NS: u64 = 2_000_000_000;
/// The timer that keeps the sync badge up to date while it's on.
const STATUS_TIMER: &str = "status";
const STATUS_MS: u64 = 1000;
/// The timer that drives Match while it runs.
const MATCH_TIMER: &str = "match";
const MATCH_MS: u64 = 400;
/// How long Match waits for both phones to answer, and for the whole run, nanoseconds.
const POLL_NS: u64 = 4_000_000_000;
const RUN_NS: u64 = 30_000_000_000;

const SWITCH_TIP: &str = n_("Turn two running streams into one 3D picture.");
const LOOK_TIP: &str = n_("Side by side suits a headset or a 3D screen and red and cyan suits glasses.");
const DEPTH_TIP: &str = n_("Slide the two pictures apart or together to change how far forward the 3D reaches.");
const HEIGHT_TIP: &str = n_("Lift or lower the right picture when the phones don't sit level.");
const SIZE_TIP: &str = n_("Make the right picture bigger or smaller when its lens sees more or less than the left one's.");
const TILT_TIP: &str = n_("Roll the right picture a little when one phone is tilted against the other.");
const TIMING_TIP: &str = n_("Move the right picture earlier or later in time when it moves ahead of or behind the left one.");
const FINE_TIP: &str = n_("Show the sliders that line the two pictures up by hand.");
const SWAP_TIP: &str = n_("Swap which phone is the left eye and which is the right if the 3D looks inside out.");
const MATCH_TIP: &str = n_("Even out two different phones with the same shutter, similar brightness and a colour curve that makes the right picture look like the left.");
const RESET_TIP: &str = n_("Drop the colour curve Match made and leave the phones' exposure as it is.");

/// What the frame step of every stream and the plugin share.
pub(crate) struct Hub {
    pub(crate) on: bool,
    /// The source id of the stream going to the Telescope camera, where the picture goes out.
    pub(crate) target: Option<String>,
    pub(crate) look: Look,
    /// The right eye's fixing: depth, height, size, tilt and the colour curves.
    pub(crate) tune: Tune,
    /// How much later the right eye's moment is than the left's, nanoseconds.
    pub(crate) timing_ns: i64,
    pub(crate) swap: bool,
    /// The picture's size.
    size: (u32, u32),
    /// How many frames of each eye are kept for pairing, which follows the timing.
    pub(crate) history: usize,
    /// The target stream's last few frames as they came in, before the picture replaced them.
    ring: VecDeque<Frame>,
    /// The other eye's last few frames by arrival, for pairing without capture times (the host only keeps history for streams with them).
    other: Option<(String, VecDeque<Frame>)>,
    /// How far apart the target stream's frames come, nanoseconds.
    pub(crate) frame_ns: u64,
    last_in: Option<u64>,
    /// How the last picture's pair lined up, and when it was made.
    pub(crate) seen: Option<(SyncStatus, u64)>,
    /// How many pictures have gone out, for the tests.
    pub(crate) pictures: u64,
    /// Whether the step reads the middle of both eyes (while Match runs), and what it read last.
    pub(crate) measure: bool,
    pub(crate) reading: Option<Reading>,
}

/// What the step read from the two eyes of a picture, and when.
pub(crate) struct Reading {
    pub(crate) at: u64,
    /// Left, right.
    pub(crate) stats: [Stats; 2],
}

pub(crate) struct Shared {
    pub(crate) hub: Mutex<Hub>,
    store: Mutex<Arc<dyn FrameStore>>,
}

struct NoFrames;

impl FrameStore for NoFrames {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        Vec::new()
    }
    fn stream_frame(&self, _source_id: Option<&str>) -> Option<StreamFrame> {
        None
    }
    fn replace_frame(&self, _source_id: Option<&str>, _frame: Frame, _number: u64) -> bool {
        false
    }
}

/// The two eyes' newest frames and the streams they come from, left first.
pub(crate) struct Eyes {
    pub(crate) ids: [String; 2],
    pub(crate) frames: [Frame; 2],
}

/// The left and the right eye from the running streams: the first two that started (swapped on request), each with its newest frame; the target
/// stream's is `frame`, the one the step is handling, since its published frame is the picture itself.
pub(crate) fn eyes_of(store: &dyn FrameStore, frame: &Frame, target: &str, swap: bool, now: u64) -> Option<Eyes> {
    let mut eyes: Vec<(String, Frame)> = Vec::new();
    for sf in store.stream_frames() {
        if sf.source_id == target {
            eyes.push((sf.source_id, frame.clone()));
        } else if sf.age(now) < STALE_NS {
            eyes.push((sf.source_id, sf.frame));
        }
    }
    // it has no published frame yet, but this is one
    if !eyes.iter().any(|(id, _)| id == target) {
        eyes.push((target.to_string(), frame.clone()));
    }
    eyes.truncate(2);
    if eyes.len() < 2 {
        return None;
    }
    if swap {
        eyes.swap(0, 1);
    }
    let (b, a) = (eyes.pop()?, eyes.pop()?);
    Some(Eyes { ids: [a.0, b.0], frames: [a.1, b.1] })
}

/// The ids of the two eyes from the streams that have a picture, left first; what Match talks to.
fn eye_ids(store: &dyn FrameStore, swap: bool) -> Option<[String; 2]> {
    let mut ids: Vec<String> = store.stream_frames().into_iter().map(|s| s.source_id).take(2).collect();
    if ids.len() < 2 {
        return None;
    }
    if swap {
        ids.swap(0, 1);
    }
    let b = ids.pop()?;
    Some([ids.pop()?, b])
}

/// Which two pictures go together, and how well they line up.
pub(crate) struct Chosen {
    pub(crate) pair: stereo::Pair<Frame>,
    pub(crate) sync: SyncStatus,
}

/// The target stream's recent frames and the other eye's, both oldest first and by arrival.
pub(crate) struct Rings<'a> {
    pub(crate) target: &'a [Frame],
    pub(crate) other: &'a [Frame],
}

/// Pairs the eyes' frames. With capture times the frames come from the host's history (or `rings.target` for the target stream, whose published
/// frame is the picture itself), lined up `timing_ns` apart. Without them the newest of each go together, except that Timing holds one eye back by
/// arrival time: the left for a positive timing (the right eye is later), the right for a negative one.
pub(crate) fn choose(store: &dyn FrameStore, eyes: &Eyes, target: &str, rings: &Rings, timing_ns: i64, frame_ns: u64) -> Chosen {
    let near = |eye: Eye, at: u64| -> Option<Frame> {
        let at_eye = if eye == Eye::Left { 0 } else { 1 };
        let id = &eyes.ids[at_eye];
        if id == target {
            let captures: Vec<Option<u64>> = rings.target.iter().map(|f| f.capture_ns).collect();
            stereo::nearest_capture(&captures, at).map(|i| rings.target[i].clone())
        } else {
            store.stream_frame_near(Some(id), at).map(|s| s.frame)
        }
    };
    let mut pair = stereo::pair_frames(&eyes.frames[0], &eyes.frames[1], |f| f.capture_ns, near, timing_ns);
    if !pair.by_time && timing_ns != 0 {
        let held = if timing_ns > 0 { 0 } else { 1 };
        let list = if eyes.ids[held] == target { rings.target } else { rings.other };
        let arrivals: Vec<u64> = list.iter().map(|f| f.time_ns).collect();
        if let Some(i) = stereo::delayed_index(&arrivals, timing_ns.unsigned_abs()) {
            if held == 0 { pair.left = list[i].clone() } else { pair.right = list[i].clone() }
        }
    }
    // the right eye's time on the left's clock is its own less the timing
    let gap = pair.left.capture_ns.zip(pair.right.capture_ns).map(|(l, r)| l.saturating_add_signed(timing_ns).abs_diff(r));
    let error = |id: &str| store.clock_sync(Some(id)).map(|c| c.error_ns);
    let sync = stereo::sync_status([error(&eyes.ids[0]), error(&eyes.ids[1])], gap, frame_ns);
    Chosen { pair, sync }
}

impl Hub {
    /// The target stream's next frame: kept for pairing, and its spacing noted for the sync.
    fn take_in(&mut self, frame: &Frame, now: u64) {
        if let Some(last) = self.last_in {
            self.frame_ns = stereo::smooth_frame_ns(self.frame_ns, now.saturating_sub(last));
        }
        self.last_in = Some(now);
        self.ring.push_back(frame.clone());
        while self.ring.len() > self.history {
            self.ring.pop_front();
        }
    }

    /// The other eye's newest frame, kept if it's a new one; its stream's frames only, so a different stream starts over.
    fn take_other(&mut self, id: &str, frame: &Frame) {
        if self.other.as_ref().is_none_or(|(have, _)| have != id) {
            self.other = Some((id.to_string(), VecDeque::new()));
        }
        let Some((_, ring)) = &mut self.other else { return };
        if ring.back().map(|f| f.number) != Some(frame.number) {
            ring.push_back(frame.clone());
        }
        while ring.len() > self.history {
            ring.pop_front();
        }
    }

    fn forget(&mut self) {
        (self.ring, self.other, self.last_in, self.seen, self.reading) = (VecDeque::new(), None, None, None, None);
    }
}

impl Shared {
    pub(crate) fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store(&self) -> Arc<dyn FrameStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The frame step of whichever stream source_id is: the 3D picture in place of the target stream's frame.
    fn step(&self, frame: &mut Frame, source_id: &str) {
        let (look, tune, timing_ns, swap, size, ring, frame_ns, measure) = {
            let mut hub = self.hub();
            if !hub.on || hub.target.as_deref() != Some(source_id) {
                return;
            }
            hub.take_in(frame, monotonic_ns());
            (hub.look, hub.tune.clone(), hub.timing_ns, hub.swap, hub.size, hub.ring.iter().cloned().collect::<Vec<_>>(), hub.frame_ns, hub.measure)
        };
        // the pictures are worked on without holding the hub, so the card never waits for them
        let now = monotonic_ns();
        let store = self.store();
        let eyes = eyes_of(&*store, frame, source_id, swap, now);
        let other: Vec<Frame> = match &eyes {
            Some(eyes) => {
                let at = if eyes.ids[0] == source_id { 1 } else { 0 };
                let mut hub = self.hub();
                hub.take_other(&eyes.ids[at], &eyes.frames[at]);
                hub.other.iter().flat_map(|(_, r)| r.iter().cloned()).collect()
            }
            None => Vec::new(),
        };
        let chosen = eyes.as_ref().map(|e| choose(&*store, e, source_id, &Rings { target: &ring, other: &other }, timing_ns, frame_ns));
        let out = chosen.as_ref().and_then(|c| stereo::render_measured(&c.pair.left, &c.pair.right, look, size, &tune, frame, measure));
        let mut hub = self.hub();
        let (Some(chosen), Some(mut out)) = (chosen, out) else {
            hub.seen = None;
            return;
        };
        (out.frame.number, out.frame.time_ns, out.frame.capture_ns) = (frame.number, frame.time_ns, frame.capture_ns);
        hub.seen = Some((chosen.sync, now));
        hub.pictures += 1;
        if let Some(stats) = out.stats.filter(|_| hub.measure) {
            hub.reading = Some(Reading { at: now, stats });
        }
        *frame = out.frame;
    }
}

/// The step every stream gets; only the target stream's does anything.
struct StereoStep(Arc<Shared>);

impl CpuStep for StereoStep {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        let hub = self.0.hub();
        if hub.on && hub.target.as_deref() == Some(ctx.source_id()) { Touch::Changes } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        self.0.step(frame, ctx.source_id());
    }
}

/// Where Match is.
enum MatchState {
    Idle,
    /// Both phones have been asked for their state; their answers come in as they arrive.
    Polling { ids: [String; 2], states: [Option<Value>; 2], since: u64 },
    /// The phones are set and the ISO is being nudged, then the colour fitted.
    Running { ids: [String; 2], run: Run, since: u64 },
    /// It finished for these two streams, and the colour curves are on.
    Done(Outcome, [String; 2]),
    /// The phones didn't answer, or the pictures didn't come.
    Failed,
}

impl MatchState {
    fn busy(&self) -> bool {
        matches!(self, MatchState::Polling { .. } | MatchState::Running { .. })
    }
}

pub struct StereoPlugin {
    on: bool,
    look: Look,
    depth: f64,
    height: f64,
    /// The right eye's size in percent, and its roll in degrees.
    size_percent: f64,
    tilt: f64,
    /// Milliseconds the right eye's moment is later than the left's.
    timing: f64,
    swap: bool,
    target: Option<String>,
    /// What the Telescope camera opened at, for the size of the picture.
    camera_size: Option<(u32, u32)>,
    /// Setup's canvas as last read, for a prepare without a host.
    canvas: (Option<u32>, Option<u32>),
    pub(crate) shared: Arc<Shared>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// Where the switch was when a held key began.
    held: Option<bool>,
    /// Whether Fine tune is open on the card.
    fine: bool,
    /// Where Match is, and the colour curves it made (applied by the step).
    matching: MatchState,
    lut: Option<Arc<Lut>>,
    /// Each stream's lens, as the 35 mm equivalent focal length its phone last reported.
    focal: BTreeMap<String, f64>,
    /// The pair of streams Size was last decided for, and the size that came from the lenses (to tell it from one a person set).
    sized_for: Option<[String; 2]>,
    auto_size: Option<f64>,
}

impl Default for StereoPlugin {
    fn default() -> Self {
        StereoPlugin::new()
    }
}

/// A slider's value as the card reads it out: 0.0%, +1.5%, -2.0%.
fn percent_text(v: f64) -> String {
    if v.abs() < 0.05 { "0.0%".to_string() } else { format!("{v:+.1}%") }
}

/// The size as the card reads it out: 100.0%, 92.5%.
fn size_text(v: f64) -> String {
    format!("{v:.1}%")
}

/// The roll as the card reads it out: 0.0°, +1.5°, -2.0°.
fn tilt_text(v: f64) -> String {
    if v.abs() < 0.05 { "0.0°".to_string() } else { format!("{v:+.1}°") }
}

/// The timing as the card reads it out: 0 ms, +25 ms, -120 ms.
fn timing_text(v: f64) -> String {
    if v.abs() < 0.5 { "0 ms".to_string() } else { format!("{v:+.0} ms") }
}

/// A value held to a slider's range and its steps of a tenth.
fn clean_tenths(v: f64, limit: f64) -> f64 {
    if v.is_finite() { (v.clamp(-limit, limit) * 10.0).round() / 10.0 } else { 0.0 }
}

fn clean_shift(v: f64) -> f64 {
    clean_tenths(v, SHIFT_LIMIT)
}

fn clean_tilt(v: f64) -> f64 {
    clean_tenths(v, TILT_LIMIT)
}

/// Whole milliseconds inside the Timing slider.
fn clean_timing(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-TIMING_LIMIT_MS, TIMING_LIMIT_MS).round() } else { 0.0 }
}

/// A size in percent inside the Size slider, in tenths; anything else is 100.
fn clean_size(v: f64) -> f64 {
    if v.is_finite() { (v.clamp(SIZE_MIN, SIZE_MAX) * 10.0).round() / 10.0 } else { 100.0 }
}

impl StereoPlugin {
    pub fn new() -> StereoPlugin {
        let hub = Hub {
            on: false, target: None, look: Look::default(), tune: Tune::default(), timing_ns: 0, swap: false, size: DEFAULT_SIZE,
            history: stereo::HISTORY_FRAMES, ring: VecDeque::new(), other: None, frame_ns: DEFAULT_FRAME_NS, last_in: None, seen: None, pictures: 0,
            measure: false, reading: None,
        };
        let shared = Arc::new(Shared { hub: Mutex::new(hub), store: Mutex::new(Arc::new(NoFrames)) });
        StereoPlugin {
            on: false, look: Look::default(), depth: 0.0, height: 0.0, size_percent: 100.0, tilt: 0.0, timing: 0.0, swap: false, target: None,
            camera_size: None, canvas: (None, None), shared, pending: false, held: None, fine: false, matching: MatchState::Idle, lut: None,
            focal: BTreeMap::new(), sized_for: None, auto_size: None,
        }
    }

    // ── The picture ───────────────────────────────────────────────────

    /// The picture's size: the canvas size Setup asks for, else what the Telescope camera opened at, else 1920x1080.
    pub fn size(&self) -> (u32, u32) {
        match self.canvas {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => self.camera_size.unwrap_or(DEFAULT_SIZE),
        }
    }

    /// Hands what the step needs to it.
    fn prepare(&mut self) {
        let size = self.size();
        let mut hub = self.shared.hub();
        hub.on = self.on;
        hub.tune = Tune { depth: self.depth, height: self.height, size: self.size_percent, tilt: self.tilt, lut: self.lut.clone() };
        (hub.look, hub.swap, hub.size, hub.timing_ns) = (self.look, self.swap, size, (self.timing * 1e6) as i64);
        hub.measure = self.on && matches!(self.matching, MatchState::Running { .. });
        if !self.on {
            hub.forget();
        }
    }

    fn follow_camera(&mut self, host: &mut dyn Host) {
        self.canvas = host.canvas_size();
        let on_camera = host.stream_on_camera(0);
        let target = on_camera.as_ref().map(|c| c.0.clone());
        let size = on_camera.as_ref().and_then(|c| match (c.1, c.2) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        });
        if target != self.target {
            self.target = target.clone();
            let mut hub = self.shared.hub();
            hub.target = target;
            // another stream's frames aren't this one's
            hub.forget();
        }
        if size.is_some() && size != self.camera_size {
            self.camera_size = size;
        }
        self.check_match(host);
        self.prepare();
        self.show(host);
    }

    // ── Card ──────────────────────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("3D"), "switch-help": tr(SWITCH_TIP),
            "look-items": [tr("Side by side"), tr("Red/cyan")], "look-tip": tr(LOOK_TIP),
            "depth-tip": tr(DEPTH_TIP), "height-tip": tr(HEIGHT_TIP), "size-tip": tr(SIZE_TIP), "tilt-tip": tr(TILT_TIP), "timing-tip": tr(TIMING_TIP),
            "fine-tip": tr(FINE_TIP), "swap-tip": tr(SWAP_TIP), "match-tip": tr(MATCH_TIP), "reset-tip": tr(RESET_TIP),
        })
    }

    /// What the sync badge says: (word, kind, sentence), blank while it's off.
    fn status(&self, host: &dyn Host) -> (String, &'static str, String) {
        let none = (String::new(), "status_dim", String::new());
        if !self.on {
            return none;
        }
        if host.stream_count() < 2 {
            return (tr("Needs two streams"), "status_dim", tr("Start a second stream to get the 3D picture."));
        }
        let seen = self.shared.hub().seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS);
        match seen {
            None => (tr("Waiting"), "status_dim", tr("Both streams have to send a picture, and one of them has to be on the Telescope camera.")),
            Some((SyncStatus::InSync, _)) => (tr("In sync"), "status_ok", tr("The two pictures were taken within half a frame of each other.")),
            Some((SyncStatus::Rough, _)) => (tr("Rough sync"), "status_warn", tr("The two pictures were taken a little apart, so fast movement can look doubled.")),
            Some((SyncStatus::NotSynced, _)) => (
                tr("Not synced"),
                "status_warn",
                tr("Telescope can't match the pictures by when they were taken (both phones need the newer phone app), so it uses the newest of each."),
            ),
        }
    }

    /// What the Match badge says: (word, kind, sentence), blank until Match has been used.
    fn match_status(&self) -> (String, &'static str, String) {
        match &self.matching {
            MatchState::Idle => (String::new(), "status_dim", String::new()),
            MatchState::Polling { .. } | MatchState::Running { .. } => (
                tr("Matching"),
                "status_dim",
                tr("Telescope is evening out the two phones' brightness and colour, which takes a few seconds."),
            ),
            MatchState::Done(Outcome::Matched, _) => (tr("Matched"), "status_ok", tr("The right picture's brightness and colour now follow the left one's.")),
            MatchState::Done(Outcome::Limited, _) => (
                tr("Partly matched"),
                "status_warn",
                tr("A phone ran out of ISO range or the pictures kept changing, so the match is as close as it got."),
            ),
            MatchState::Failed => (tr("Match failed"), "status_warn", tr("Telescope couldn't read both phones and their pictures, so nothing was matched.")),
        }
    }

    fn card_props(&self, host: Option<&dyn Host>) -> Value {
        let mut props = Self::fixed_props();
        let (status, kind, detail) = host.map_or((String::new(), "status_dim", String::new()), |h| self.status(h));
        let (matching, match_kind, match_detail) = if self.on { self.match_status() } else { (String::new(), "status_dim", String::new()) };
        let two = host.is_some_and(|h| h.stream_count() >= 2);
        let reset = self.lut.is_some() || !matches!(self.matching, MatchState::Idle);
        if let Value::Object(o) = &mut props {
            o.extend([
                ("on".to_string(), json!(self.on)),
                ("look-current".to_string(), json!(self.look.index())),
                ("depth".to_string(), json!(self.depth)),
                ("depth-text".to_string(), json!(percent_text(self.depth))),
                ("height-shift".to_string(), json!(self.height)),
                ("height-text".to_string(), json!(percent_text(self.height))),
                ("size-percent".to_string(), json!(self.size_percent)),
                ("size-text".to_string(), json!(size_text(self.size_percent))),
                ("tilt".to_string(), json!(self.tilt)),
                ("tilt-text".to_string(), json!(tilt_text(self.tilt))),
                ("timing".to_string(), json!(self.timing)),
                ("timing-text".to_string(), json!(timing_text(self.timing))),
                ("fine-open".to_string(), json!(self.fine)),
                ("swap".to_string(), json!(self.swap)),
                ("match-enabled".to_string(), json!(two && !self.matching.busy())),
                ("reset-enabled".to_string(), json!(reset)),
                ("status".to_string(), json!(status)),
                ("status-kind".to_string(), json!(kind)),
                ("status-detail".to_string(), json!(detail)),
                ("match-status".to_string(), json!(matching)),
                ("match-status-kind".to_string(), json!(match_kind)),
                ("match-status-detail".to_string(), json!(match_detail)),
            ]);
        }
        props
    }

    fn show(&mut self, host: &mut dyn Host) {
        // While it's on it pairs other streams' frames, which only keep coming when the steps run, and needs a few of each held
        let two = host.stream_count() >= 2;
        let frames = {
            let mut hub = self.shared.hub();
            hub.history = stereo::history_frames(self.timing, hub.frame_ns);
            hub.history
        };
        host.want_frames(NAME, self.on);
        host.want_frame_history(NAME, if self.on && two { frames } else { 0 });
        if self.on {
            host.schedule(STATUS_MS, STATUS_TIMER);
        } else {
            host.cancel_schedule(STATUS_TIMER);
        }
        if self.on && self.matching.busy() {
            host.schedule(MATCH_MS, MATCH_TIMER);
        } else {
            host.cancel_schedule(MATCH_TIMER);
        }
        let props = self.card_props(Some(host));
        host.set_ui_props("card", &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.canvas = host.canvas_size();
            self.prepare();
            self.show(host);
            host.refresh_actions();
        }
    }

    fn changed(&mut self, host: &mut dyn Host) {
        self.prepare();
        self.show(host);
        host.schedule_save();
    }

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        if !on {
            self.drop_match();
        }
        self.changed(host);
    }

    // ── Size from the lenses ──────────────────────────────────────────

    /// The ids of the two eyes, left first.
    fn eyes(&self, host: &dyn Host) -> Option<[String; 2]> {
        eye_ids(&*host.frames(), self.swap)
    }

    /// Once for each pair of streams, sets Size from the two lenses' fields of view when it is still at 100 % (or at what this set for the other
    /// way round after Swap eyes). Waits for both phones to have reported their lens.
    fn size_from_lenses(&mut self, host: &mut dyn Host) {
        if !self.on {
            return;
        }
        let Some(ids) = self.eyes(host) else { return };
        if self.sized_for.as_ref() == Some(&ids) {
            return;
        }
        let (Some(left), Some(right)) = (self.focal.get(&ids[0]).copied(), self.focal.get(&ids[1]).copied()) else { return };
        let untouched = (self.size_percent - 100.0).abs() < 0.05 || self.auto_size == Some(self.size_percent);
        self.sized_for = Some(ids);
        self.auto_size = None;
        if !untouched {
            return;
        }
        if let Some(size) = stereo_match::size_from_focal(left, right).map(clean_size) {
            self.size_percent = size;
            self.auto_size = Some(size);
            self.changed(host);
        } else if self.size_percent != 100.0 {
            self.size_percent = 100.0;
            self.changed(host);
        }
    }

    /// A phone's state as it came in: its lens is kept for Size.
    fn note_state(&mut self, host: &mut dyn Host, source_id: &str, state: &Value) {
        match stereo_match::focal_mm(state) {
            Some(mm) => self.focal.insert(source_id.to_string(), mm),
            None => self.focal.remove(source_id),
        };
        self.size_from_lenses(host);
    }

    // ── Match ─────────────────────────────────────────────────────────

    /// Drops what Match did to the picture and stops a run; the phones keep their exposure.
    fn drop_match(&mut self) {
        self.lut = None;
        self.matching = MatchState::Idle;
    }

    /// The colour curves belong to a pair of streams: they go when the eyes change, and a run stops.
    fn check_match(&mut self, host: &dyn Host) {
        let ids = self.eyes(host);
        let stale = match &self.matching {
            MatchState::Idle | MatchState::Failed => false,
            MatchState::Polling { ids: mine, .. } | MatchState::Running { ids: mine, .. } | MatchState::Done(_, mine) => ids.as_ref() != Some(mine),
        };
        if stale || host.stream_count() < 2 {
            self.drop_match();
        }
    }

    /// Match: both phones are asked for their state, then it goes on with the answers (see `polled`).
    fn start_match(&mut self, host: &mut dyn Host) {
        let Some(ids) = self.eyes(host).filter(|_| self.on && host.stream_count() >= 2 && !self.matching.busy()) else { return };
        for id in &ids {
            host.request_phone_state(Some(id));
        }
        self.lut = None;
        self.matching = MatchState::Polling { ids, states: [None, None], since: monotonic_ns() };
        self.changed_without_save(host);
    }

    /// A phone's answer to the state Match asked for. With both there the phones are set and the run begins.
    fn polled(&mut self, host: &mut dyn Host, source_id: &str, state: &Value) {
        let MatchState::Polling { ids, states, since } = &mut self.matching else { return };
        let Some(at) = ids.iter().position(|id| id == source_id) else { return };
        states[at] = Some(state.clone());
        if states.iter().any(Option::is_none) {
            return;
        }
        let (ids, since) = (ids.clone(), *since);
        let [Some(left), Some(right)] = std::mem::take(states) else { return };
        let now = monotonic_ns();
        let frame_ns = self.shared.hub().frame_ns;
        let run = match stereo_match::plan(&left, &right, frame_ns) {
            Some(start) => {
                for (id, iso) in ids.iter().zip(start.iso) {
                    for body in stereo_match::exposure_bodies(iso, start.shutter) {
                        host.phone_control(Some(id), &body);
                    }
                }
                Run::new(start.iso, start.limits, start.tune_left, true, now)
            }
            // phones that don't take a manual exposure still get their colours matched
            None => Run::new([0, 0], [(0, 0); 2], false, false, now),
        };
        self.matching = MatchState::Running { ids, run, since };
        self.shared.hub().reading = None;
        self.changed_without_save(host);
    }

    fn changed_without_save(&mut self, host: &mut dyn Host) {
        self.prepare();
        self.show(host);
    }

    /// The timer of a run: waits for the answers, then reads what the step saw and does what the run says.
    fn match_tick(&mut self, host: &mut dyn Host) {
        self.match_tick_at(host, monotonic_ns());
    }

    fn match_tick_at(&mut self, host: &mut dyn Host, now: u64) {
        match std::mem::replace(&mut self.matching, MatchState::Idle) {
            MatchState::Polling { ids, states, since } => {
                self.matching = if now.saturating_sub(since) > POLL_NS { MatchState::Failed } else { MatchState::Polling { ids, states, since } };
            }
            MatchState::Running { ids, mut run, since } => {
                let reading = self.shared.hub().reading.as_ref().map(|r| (r.at, [r.stats[0].luma, r.stats[1].luma]));
                self.matching = match run.tick(now, reading) {
                    Act::Wait if now.saturating_sub(since) > RUN_NS => MatchState::Failed,
                    Act::Wait => MatchState::Running { ids, run, since },
                    Act::SetIso { eye, iso } => {
                        host.phone_control(Some(&ids[if eye == Eye::Left { 0 } else { 1 }]), &stereo_match::iso_body(iso));
                        MatchState::Running { ids, run, since }
                    }
                    Act::Fit(outcome) => {
                        let lut = self.shared.hub().reading.as_ref().map(|r| stereo_match::fit_lut(&r.stats[1].hist, &r.stats[0].hist));
                        match lut {
                            Some(lut) => {
                                self.lut = Some(Arc::new(lut));
                                MatchState::Done(outcome, ids)
                            }
                            None => MatchState::Failed,
                        }
                    }
                };
            }
            other => self.matching = other,
        }
        self.changed_without_save(host);
    }

    /// Reset match: the colour curves go and a run stops. The phones keep the exposure Match gave them.
    fn reset_match(&mut self, host: &mut dyn Host) {
        self.drop_match();
        self.changed_without_save(host);
    }

    /// Whether it's on and how it looks, from settings or a preset.
    fn apply(&mut self, cfg: &Value) {
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.look = cfg.get("look").and_then(Value::as_str).and_then(Look::from_name).unwrap_or_default();
        let number = |key: &str| cfg.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        (self.depth, self.height, self.tilt) = (clean_shift(number("depth")), clean_shift(number("height")), clean_tilt(number("tilt")));
        self.size_percent = clean_size(cfg.get("size").and_then(Value::as_f64).unwrap_or(100.0));
        self.timing = clean_timing(number("timing"));
        self.swap = cfg.get("swap") == Some(&Value::Bool(true));
        // what was matched or sized for another set of settings no longer holds
        self.drop_match();
        (self.sized_for, self.auto_size) = (None, None);
        self.prepare();
    }
}

impl Plugin for StereoPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("3D").into(),
            about: n_("Turn two phones into one 3D picture, side by side for a headset or red and cyan for glasses.").into(),
            when_off: n_("Without the 3D plugin there's no 3D card and two streams only go to their own cameras.").into(),
            category: "streaming".into(),
            icon: "cube".into(),
            frame_stage: FrameStage::Combine,
            optional: true,
            in_presets: true,
            // it's about the Telescope camera's stream, whichever one the panels show
            follows_focus: false,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        *self.shared.store.lock().unwrap_or_else(|e| e.into_inner()) = host.frames();
        self.canvas = host.canvas_size();
        self.prepare();
        self.show(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        host.want_frames(NAME, false);
        host.want_frame_history(NAME, 0);
        host.cancel_schedule(STATUS_TIMER);
        host.cancel_schedule(MATCH_TIMER);
        // so no picture goes out after quitting, and the settings stay as they were
        let mut hub = self.shared.hub();
        hub.on = false;
        hub.measure = false;
        hub.forget();
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::StreamsChanged { .. } => self.follow_camera(host),
            Event::CameraOpened { slot: 0, .. } => self.follow_camera(host),
            Event::StreamState { source_id, state } => self.note_state(host, source_id, state),
            Event::PhonePolled { source_id, state, .. } => {
                self.note_state(host, source_id, state);
                self.polled(host, source_id, state);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == STATUS_TIMER => {
                self.size_from_lenses(host);
                self.show(host);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == MATCH_TIMER => self.match_tick(host),
            _ => {}
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        self.follow_camera(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.follow_camera(host);
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StereoCard".into(), props: self.card_props(None) }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let number = event.args.first().and_then(Value::as_f64);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_on(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "look-picked" => {
                if let Some(look) = number.filter(|i| *i >= 0.0).and_then(|i| Look::ALL.get(i as usize)) {
                    self.look = *look;
                    self.changed(host);
                }
            }
            "depth-moved" => {
                self.depth = clean_shift(number.unwrap_or(0.0));
                self.changed(host);
            }
            "height-moved" => {
                self.height = clean_shift(number.unwrap_or(0.0));
                self.changed(host);
            }
            "size-moved" => {
                self.size_percent = clean_size(number.unwrap_or(100.0));
                self.changed(host);
            }
            "tilt-moved" => {
                self.tilt = clean_tilt(number.unwrap_or(0.0));
                self.changed(host);
            }
            "timing-moved" => {
                self.timing = clean_timing(number.unwrap_or(0.0));
                self.changed(host);
            }
            "fine-toggled" => {
                self.fine = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.show(host);
            }
            "swap-toggled" => {
                self.swap = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                // the colour curves were made for the other way round
                self.check_match(host);
                self.changed(host);
                self.size_from_lenses(host);
            }
            "match-clicked" => self.start_match(host),
            "reset-match-clicked" => self.reset_match(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        vec![actions::toggle_action("stereo.on", &tr("3D"), &tr("3D"), "", &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if id != "stereo.on" {
            return None;
        }
        let want = actions::toggle_wanted(params, self.on, &mut self.held);
        if want != self.on {
            self.set_on(host, want);
        }
        Some(actions::toggle_text(&tr("3D"), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "stereo.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on {
                self.set_on(host, before);
            }
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        // the settings are the app's, not the stream's, so it stays live behind the panels too
        Some(FrameStep::Cpu(Box::new(StereoStep(self.shared.clone()))))
    }

    fn get_config(&self) -> Value {
        json!({
            "on": self.on, "look": self.look.name(), "depth": self.depth, "height": self.height, "size": self.size_percent, "tilt": self.tilt,
            "timing": self.timing, "swap": self.swap,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.apply(cfg);
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.catch_up(host);
        self.canvas = host.canvas_size();
        self.apply(cfg);
        self.show(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        if !self.on {
            return vec![("3D".into(), "off".into())];
        }
        let hub = self.shared.hub();
        let sync = match hub.seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS) {
            Some((SyncStatus::InSync, _)) => "in sync",
            Some((SyncStatus::Rough, _)) => "rough sync",
            Some((SyncStatus::NotSynced, _)) => "not synced",
            None => "waiting",
        };
        vec![("3D".into(), format!("on, {}, {sync}", self.look.name()))]
    }
}

#[cfg(test)]
#[path = "stereo_plugin_tests.rs"]
mod tests;
