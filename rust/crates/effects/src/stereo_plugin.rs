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
//!
//! The frames this plugin keeps itself (the target stream's last few, the other eye's by arrival) are stamped with the host's pipeline epoch
//! (FrameStore::pipeline_epoch), which goes up whenever any stream's frame steps are handed out again: a Blur coming on, or any other step
//! changing. Frames from an older epoch are never paired, so a picture is never made from a frame that went out before that, whichever stream
//! the change was for. The host does the same with the history it keeps for the other stream. When the stream on the Telescope camera stalls, a clock of its own keeps the
//! picture going from its last frame for a few seconds, then from the other eye alone, so the camera never sits on one frozen picture.

use crate::stereo::{self, Eye, Look, SyncStatus, Tune, DEFAULT_FRAME_NS, SHIFT_LIMIT, TILT_LIMIT, TIMING_LIMIT_MS};
use crate::shared::{new_token, token_ok};
use crate::stereo_headset::{self, Feed, Headset, Job};
use crate::stereo_match::{self, Act, Lut, Outcome, Run, Stats, BRIGHTNESS_LIMIT, OWN_LOOK_NS, SIZE_MAX, SIZE_MIN};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, StreamFrame, Touch};
use telescope_core::host::{FrameStore, Host};
use telescope_core::i18n::{n_, tr, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::web::{Serve, Web, WebServer};

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
/// The timer that sends the Brightness slider's last value, and the least time between two sends, milliseconds.
const BRIGHTNESS_TIMER: &str = "brightness";
const BRIGHTNESS_MS: u64 = 150;

const SWITCH_TIP: &str = n_("Turn two running streams into one 3D picture.");
const LOOK_TIP: &str = n_("Side by side suits a headset or a 3D screen and red and cyan suits glasses.");
const DEPTH_TIP: &str = n_("Slide the two pictures apart or together to change how far forward the 3D reaches.");
const HEIGHT_TIP: &str = n_("Lift or lower the right picture when the phones don't sit level.");
const SIZE_TIP: &str = n_("Zoom in on the phone that sees the wider view (the right one over 100%, the left one under it) to make both show the same thing.");
const TILT_LEFT_TIP: &str = n_("Turn the left eye's picture to line it up with the right.");
const TILT_RIGHT_TIP: &str = n_("Turn the right eye's picture to line it up with the left.");
const TIMING_TIP: &str = n_("Move the right picture earlier or later in time when it moves ahead of or behind the left one.");
const FINE_TIP: &str = n_("Show the sliders that line the two pictures up by hand.");
const SWAP_TIP: &str = n_("Swap which phone is the left eye and which is the right if the 3D looks inside out.");
const MATCH_TIP: &str = n_("Even out two different phones with the same shutter, similar brightness and a colour curve that makes the right picture look like the left.");
const BRIGHTNESS_TIP: &str = n_("Make both phones brighter or darker together after Match, keeping them matched.");
const HEADSET_TIP: &str = n_("Watch the 3D picture in a headset's browser on the same Wi-Fi.");
const HEADSET_QR_TIP: &str = n_("Scan the code with a phone, or type the link into the headset's browser.");
const HEADSET_ADDRESS_TIP: &str = n_("The address of this computer that the link uses.");
const HEADSET_LINK_TIP: &str = n_("Open the link in the headset's browser on the same Wi-Fi and accept the certificate warning once, then press Enter VR.");
const HEADSET_COPY_TIP: &str = n_("Copy the link to open it in a browser that can't scan the code.");
const HEADSET_NEW_LINK_TIP: &str = n_("Make a new link. The old link stops working and headsets on it are disconnected.");
const RESET_TIP: &str = n_("Drop the colour curve Match made and leave the phones' exposure as it is.");
/// How often the picture is drawn while the stream it goes out on stalls.
const FALLBACK_FPS: u64 = 30;
/// How many of its last pictures the plugin remembers having put out.
const RECENT_SENT: usize = 6;

/// What the frame step of every stream and the plugin share.
pub(crate) struct Hub {
    pub(crate) on: bool,
    /// The source id of the stream going to the Telescope camera, where the picture goes out.
    pub(crate) target: Option<String>,
    pub(crate) look: Look,
    /// The right eye's fixing: depth, height, size, the tilts and the colour curves.
    pub(crate) tune: Tune,
    /// How much later the right eye's moment is than the left's, nanoseconds.
    pub(crate) timing_ns: i64,
    pub(crate) swap: bool,
    /// The picture's size.
    size: (u32, u32),
    /// How many frames of each eye are kept for pairing, which follows the timing.
    pub(crate) history: usize,
    /// The target stream's last few frames as they came in, before the picture replaced them, each with the pipeline epoch it was kept under.
    ring: VecDeque<(Frame, u64)>,
    /// The other eye's last few frames by arrival, for pairing without capture times (the host only keeps history for streams with them).
    other: Option<(String, VecDeque<(Frame, u64)>)>,
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
    /// Goes up whenever the clock's picture in flight must not go out any more: off, a reset, shutdown or another target stream.
    generation: u64,
    /// What the step last handed on in the target stream, what the clock last put out in its place, and what the clock last drew from (so it
    /// doesn't draw the same picture again).
    handed: Option<Frame>,
    drawn: Option<Frame>,
    stalled_key: Option<Vec<(String, u64)>>,
    /// The pixels of the last few pictures this plugin put out, so the headset can tell them from one something later put in their place.
    recent: VecDeque<Arc<Vec<u8>>>,
    shut: bool,
}

/// What the step read from the two eyes of a picture, and when.
pub(crate) struct Reading {
    /// When the two pictures were taken (the later of the two's capture time, or arrival where there is none), computer clock nanoseconds: a cached
    /// picture is as old as it is, however recently it was put in a pair.
    pub(crate) at: u64,
    /// Left, right.
    pub(crate) stats: [Stats; 2],
}

pub(crate) struct Shared {
    pub(crate) hub: Mutex<Hub>,
    wake: Condvar,
    store: Mutex<Arc<dyn FrameStore>>,
    /// The pictures for the headsets watching.
    pub(crate) headset: Arc<Feed>,
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

/// The streams that count as eyes: the first two that have a picture less than three seconds old, in the order they started. The target
/// stream's own frame is never in the list (its published frame is the 3D picture), so it comes back as None for the caller to fill in; `in_step`
/// says its step is running, which is a frame of its own now whatever it published.
fn eye_list(store: &dyn FrameStore, target: Option<&str>, in_step: bool, now: u64) -> Vec<(String, Option<Frame>)> {
    let mut eyes: Vec<(String, Option<Frame>)> = Vec::new();
    for sf in store.stream_frames() {
        let own = Some(sf.source_id.as_str()) == target;
        if sf.age(now) < STALE_NS || (own && in_step) {
            eyes.push((sf.source_id, if own { None } else { Some(sf.frame) }));
        }
    }
    // it has no published frame yet, but this is one
    if let Some(t) = target.filter(|t| in_step && !eyes.iter().any(|(id, _)| id == t)) {
        eyes.push((t.to_string(), None));
    }
    eyes.truncate(2);
    eyes
}

/// Two streams' frames as the left and the right eye (swapped on request).
fn into_eyes(mut list: Vec<(String, Frame)>, swap: bool) -> Option<Eyes> {
    if list.len() < 2 {
        return None;
    }
    list.truncate(2);
    if swap {
        list.swap(0, 1);
    }
    let (b, a) = (list.pop()?, list.pop()?);
    Some(Eyes { ids: [a.0, b.0], frames: [a.1, b.1] })
}

/// The left and the right eye from the running streams: the first two that started and are still sending (swapped on request), each with its
/// newest frame; the target stream's is `frame`, the one the step is handling.
pub(crate) fn eyes_of(store: &dyn FrameStore, frame: &Frame, target: &str, swap: bool, now: u64) -> Option<Eyes> {
    into_eyes(eye_list(store, Some(target), true, now).into_iter().map(|(id, f)| (id, f.unwrap_or_else(|| frame.clone()))).collect(), swap)
}

/// The ids of the two eyes, left first: the same two `eyes_of` picks, seen from outside the step, which is what Match talks to.
fn eye_ids(store: &dyn FrameStore, target: Option<&str>, swap: bool, now: u64) -> Option<[String; 2]> {
    let mut ids: Vec<String> = eye_list(store, target, false, now).into_iter().map(|(id, _)| id).collect();
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
    fn take_in(&mut self, frame: &Frame, now: u64, epoch: u64) {
        if let Some(last) = self.last_in {
            self.frame_ns = stereo::smooth_frame_ns(self.frame_ns, now.saturating_sub(last));
        }
        self.last_in = Some(now);
        self.ring.push_back((frame.clone(), epoch));
        while self.ring.len() > self.history {
            self.ring.pop_front();
        }
    }

    /// The other eye's newest frame, kept if it's a new one; its stream's frames only, so a different stream starts over.
    fn take_other(&mut self, id: &str, frame: &Frame, epoch: u64) {
        if self.other.as_ref().is_none_or(|(have, _)| have != id) {
            self.other = Some((id.to_string(), VecDeque::new()));
        }
        let Some((_, ring)) = &mut self.other else { return };
        if ring.back().map(|(f, _)| f.number) != Some(frame.number) {
            ring.push_back((frame.clone(), epoch));
        }
        while ring.len() > self.history {
            ring.pop_front();
        }
    }

    /// Lets go of the frames kept for pairing.
    fn drop_frames(&mut self) {
        (self.ring, self.other, self.stalled_key) = (VecDeque::new(), None, None);
    }

    fn forget(&mut self) {
        self.recent.clear();
        self.drop_frames();
        (self.last_in, self.seen, self.reading, self.handed, self.drawn) = (None, None, None, None, None);
        self.generation += 1;
    }

    /// Whether a frame is one this plugin put out: what its step handed on or what the clock drew. Anything else on the camera was changed by
    /// something after it.
    fn is_ours(&self, frame: &Frame) -> bool {
        [&self.handed, &self.drawn].into_iter().flatten().any(|f| Arc::ptr_eq(&f.data, &frame.data))
    }

    fn note_sent(&mut self, data: &Arc<Vec<u8>>) {
        self.recent.push_back(data.clone());
        while self.recent.len() > RECENT_SENT {
            self.recent.pop_front();
        }
    }

    /// Whether a frame is one of the last few this plugin put out.
    pub(crate) fn was_ours(&self, frame: &Frame) -> bool {
        self.recent.iter().any(|d| Arc::ptr_eq(d, &frame.data))
    }

    /// The target stream's frames kept under this epoch or a later one, oldest first.
    fn current_ring(&self, epoch: u64) -> Vec<Frame> {
        self.ring.iter().filter(|(_, e)| *e >= epoch).map(|(f, _)| f.clone()).collect()
    }

    /// The other eye's frames kept under this epoch or a later one, oldest first.
    fn current_other(&self, epoch: u64) -> Vec<Frame> {
        self.other.iter().flat_map(|(_, r)| r.iter().filter(|(_, e)| *e >= epoch).map(|(f, _)| f.clone())).collect()
    }
}

/// What a picture is made from besides the two eyes' newest frames.
struct Make {
    look: Look,
    tune: Tune,
    timing_ns: i64,
    size: (u32, u32),
    ring: Vec<Frame>,
    other: Vec<Frame>,
    frame_ns: u64,
    measure: bool,
}

/// How long the target stream has to be quiet before it counts as stalled: two frame times, never under 2/30 s.
fn stall_ns(frame_ns: u64) -> u64 {
    (2 * frame_ns).max(2_000_000_000 / FALLBACK_FPS)
}

impl Shared {
    pub(crate) fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store(&self) -> Arc<dyn FrameStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn make(hub: &Hub, epoch: u64) -> Make {
        Make {
            look: hub.look, tune: hub.tune.clone(), timing_ns: hub.timing_ns, size: hub.size, ring: hub.current_ring(epoch),
            other: hub.current_other(epoch), frame_ns: hub.frame_ns, measure: hub.measure,
        }
    }

    /// Keeps the other eye's newest frame in its ring and gives back the frames of it a picture is paired from. Nothing when the target stream
    /// isn't one of the two eyes: the ring is for the eye that is paired with it.
    fn feed_other(&self, eyes: &Eyes, target: &str, epoch: u64) -> Vec<Frame> {
        let Some(at) = eyes.ids.iter().position(|id| id != target).filter(|_| eyes.ids.iter().any(|id| id == target)) else { return Vec::new() };
        let mut hub = self.hub();
        hub.take_other(&eyes.ids[at], &eyes.frames[at], epoch);
        hub.current_other(epoch)
    }

    /// The picture of a pair of eyes in the format of `like`, how well they line up, and when the older of the two pictures was taken.
    fn picture(store: &dyn FrameStore, eyes: &Eyes, target: &str, make: &Make, like: &Frame) -> Option<(stereo::Rendered, SyncStatus, u64, stereo::Pair<Frame>)> {
        let chosen = choose(store, eyes, target, &Rings { target: &make.ring, other: &make.other }, make.timing_ns, make.frame_ns);
        let out = stereo::render_measured(&chosen.pair.left, &chosen.pair.right, make.look, make.size, &make.tune, like, make.measure)?;
        let taken = [&chosen.pair.left, &chosen.pair.right].iter().map(|f| f.capture_ns.unwrap_or(f.time_ns)).min().unwrap_or(0);
        Some((out, chosen.sync, taken, chosen.pair))
    }

    /// The frame step of whichever stream source_id is: the 3D picture in place of the target stream's frame.
    fn step(&self, frame: &mut Frame, source_id: &str, entered: Option<u64>) {
        let now = monotonic_ns();
        let store = self.store();
        // the epoch the steps began on this frame under (the pipeline's), else read before anything is kept: either way a change of steps during
        // this frame leaves it marked as from before
        let epoch = entered.unwrap_or_else(|| store.pipeline_epoch());
        let (swap, mut make, generation) = {
            let mut hub = self.hub();
            if !hub.on || hub.target.as_deref() != Some(source_id) {
                return;
            }
            hub.take_in(frame, now, epoch);
            (hub.swap, Self::make(&hub, epoch), hub.generation)
        };
        // the pictures are worked on without holding the hub, so the card never waits for them
        let Some(eyes) = eyes_of(&*store, frame, source_id, swap, now) else {
            // the pair is gone: neither eye's frames are kept for the next one
            let mut hub = self.hub();
            hub.drop_frames();
            hub.seen = None;
            return;
        };
        make.other = self.feed_other(&eyes, source_id, epoch);
        let made = Self::picture(&*store, &eyes, source_id, &make, frame);
        let mut hub = self.hub();
        // the render took a while: if 3D was switched off, reset or moved to another stream meanwhile, this picture isn't for the camera any more
        if !hub.on || hub.target.as_deref() != Some(source_id) || hub.generation != generation {
            return;
        }
        let Some((mut out, sync, taken, pair)) = made else {
            hub.seen = None;
            return;
        };
        (out.frame.number, out.frame.time_ns, out.frame.capture_ns) = (frame.number, frame.time_ns, frame.capture_ns);
        hub.seen = Some((sync, now));
        hub.pictures += 1;
        hub.stalled_key = None;
        if let Some(stats) = out.stats.filter(|_| hub.measure) {
            hub.reading = Some(Reading { at: taken, stats });
        }
        hub.handed = Some(out.frame.clone());
        if self.headset.wanted() {
            hub.note_sent(&out.frame.data);
            self.headset.submit(Job {
                target: source_id.to_string(), left: pair.left, right: pair.right, tune: make.tune, number: frame.number, handed: out.frame.data.clone(),
                epoch, at: std::time::Instant::now(),
            });
        }
        *frame = out.frame;
    }

    /// Draws the picture while the stream on the Telescope camera stalls, if the other eye moved since. For three seconds the stalled stream's last
    /// frame is its eye; after that only the other eye is left, and it goes on both sides, so the camera keeps moving instead of sitting on a frozen
    /// picture. True if a picture went out.
    pub(crate) fn draw_stalled(&self, now: u64) -> bool {
        let store = self.store();
        let epoch = store.pipeline_epoch();
        let (target, kept, swap, generation) = {
            let hub = self.hub();
            let (Some(target), Some(at)) = (hub.target.clone(), hub.last_in) else { return false };
            if !hub.on || now.saturating_sub(at) < stall_ns(hub.frame_ns) {
                return false;
            }
            (target, hub.ring.back().cloned(), hub.swap, hub.generation)
        };
        // the target's last frame is its eye only if it was made under the current epoch: else it may be clear where a step now hides things
        let mine = kept.filter(|(_, e)| *e >= epoch).map(|(f, _)| f);
        // its camera is off, or it hasn't sent a frame yet
        let Some(sent) = store.outgoing_frame(Some(&target)) else { return false };
        // a later step or another plugin changed what it sends (the wait screen covers it): drawing would undo that
        if !self.hub().is_ours(&sent.frame) {
            return false;
        }
        // the format the picture is made in: the target's last frame, or what it sends now when none of its frames can be used or are left
        let like = mine.clone().unwrap_or_else(|| sent.frame.clone());
        let list: Vec<(String, Frame)> = eye_list(&*store, Some(&target), false, now).into_iter().filter_map(|(id, f)| Some((id, f.or_else(|| mine.clone())?))).collect();
        if list.is_empty() {
            // neither feed has sent for as long as a stream stays an eye: what was kept of them is too old to pair with anything that comes next
            self.hub().drop_frames();
            return false;
        }
        // only the stalled stream is left, or nothing: nothing else is moving
        let (eyes, flat) = match list.as_slice() {
            [(id, f)] if *id != target => (Eyes { ids: [id.clone(), id.clone()], frames: [f.clone(), f.clone()] }, true),
            _ => match into_eyes(list, swap) {
                Some(eyes) => (eyes, false),
                None => return false,
            },
        };
        let key: Vec<(String, u64)> = eyes.ids.iter().cloned().zip(eyes.frames.iter().map(|f| f.number)).collect();
        if self.hub().stalled_key.as_ref() == Some(&key) {
            return false;
        }
        let made = if flat {
            let make = Self::make(&self.hub(), epoch);
            stereo::render_measured(&eyes.frames[0], &eyes.frames[1], make.look, make.size, &make.tune, &like, false)
                .map(|r| (r, None, stereo::Pair { left: eyes.frames[0].clone(), right: eyes.frames[1].clone(), by_time: false }, make.tune.clone()))
        } else {
            // the other eye's new frames go in the ring here too, or without capture times both eyes would stay as they were until the three seconds are up
            let other = self.feed_other(&eyes, &target, epoch);
            let make = Make { other, ..Self::make(&self.hub(), epoch) };
            Self::picture(&*store, &eyes, &target, &make, &like).map(|(r, sync, _, pair)| (r, Some(sync), pair, make.tune.clone()))
        };
        let Some((out, sync, pair, tune)) = made else { return false };
        // the render took a while: if it was switched off, reset or moved to another stream meanwhile, it must not go out, and the check and the
        // send are one step under the hub so nothing slips in between
        let mut hub = self.hub();
        if !hub.on || hub.target.as_deref() != Some(target.as_str()) || hub.generation != generation {
            return false;
        }
        // not if the stream sent a frame in the meantime, that one has the newer picture, nor if another plugin put its own on the camera since
        if !store.replace_frame_if(Some(&target), &sent.frame.data, out.frame.clone(), sent.frame.number) {
            return false;
        }
        if self.headset.wanted() {
            hub.note_sent(&out.frame.data);
            self.headset.submit(Job {
                target: target.clone(), left: pair.left, right: pair.right, tune, number: sent.frame.number, handed: out.frame.data.clone(), epoch,
                at: std::time::Instant::now(),
            });
        }
        hub.drawn = Some(out.frame);
        hub.stalled_key = Some(key);
        hub.seen = sync.map(|s| (s, now));
        true
    }

    /// The clock: rests until there's a stream on the camera to keep moving, then draws while that stream stalls. While frames arrive it sleeps
    /// until the newest one would count as stalled, instead of looking every frame time.
    fn run_clock(&self) {
        let period = 1_000_000_000 / FALLBACK_FPS;
        loop {
            {
                let hub = self.hub();
                if hub.shut {
                    return;
                }
                if !hub.on || hub.target.is_none() {
                    let _rested = self.wake.wait(hub).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                let wait = Self::clock_wait(hub.last_in, monotonic_ns(), stall_ns(hub.frame_ns), period);
                // the wake lets shutdown cut the sleep short
                let (hub, _) = self.wake.wait_timeout(hub, Duration::from_nanos(wait)).unwrap_or_else(|e| e.into_inner());
                if hub.shut {
                    return;
                }
            }
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.draw_stalled(monotonic_ns()))).is_err() {
                log::error!("Drawing the 3D picture while its stream stalls failed");
            }
        }
    }

    /// How long the clock sleeps (ns): a frame time while the stream is stalled or unheard from, else until its newest frame counts as stalled.
    fn clock_wait(last_frame: Option<u64>, now: u64, stall: u64, period: u64) -> u64 {
        match last_frame {
            Some(at) if now.saturating_sub(at) < stall => stall - now.saturating_sub(at),
            _ => period,
        }
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
        self.0.step(frame, ctx.source_id(), ctx.entry_epoch());
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
    tilt_left: f64,
    tilt_right: f64,
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
    /// What Match left the phones at, while Brightness can move them together.
    exposure: Option<Exposure>,
    /// The Brightness slider in stops, when its ISOs were last sent and what was sent, and whether a later value waits for the timer.
    brightness: f64,
    brightness_sent: Option<(u64, [i64; 2])>,
    brightness_waits: bool,
    /// The size a swap turned round and the one it came from, so swapping back is exact.
    size_swapped: Option<(f64, f64)>,
    /// The clock that draws while the target stream stalls.
    clock: Option<JoinHandle<()>>,
    /// The headset viewer: what serves it (None without one, which leaves the Headset switch doing nothing), where the certificate is kept (the
    /// settings folder when None) and the thread that makes its pictures.
    web: Option<Arc<dyn Web>>,
    cert_folder: Option<PathBuf>,
    headset: bool,
    headset_token: String,
    headset_address: String,
    headset_server: Option<Box<dyn WebServer>>,
    headset_worker: Option<JoinHandle<()>>,
    /// Why the headset page isn't up.
    headset_problem: String,
    /// The link the QR code on the card was drawn for.
    headset_qr_url: String,
    /// Whether the link kept in its own file has been read.
    link_read: bool,
}

/// The two phones' ISOs as Match left them, which Brightness scales.
struct Exposure {
    ids: [String; 2],
    base: [i64; 2],
    limits: [(i64, i64); 2],
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

/// The brightness as the card reads it out: 0.0 EV, +1.0 EV, -0.5 EV.
fn brightness_text(v: f64) -> String {
    if v.abs() < 0.05 { "0.0 EV".to_string() } else { format!("{v:+.1} EV") }
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

/// The file in the settings folder that keeps the headset's link and address.
const LINK_FILE: &str = "3d-headset-link.json";

/// Written beside the file and moved over it, readable by this user only: the link lets anyone on the network watch.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(data)?;
    std::fs::rename(&tmp, path)
}

impl StereoPlugin {
    pub fn new() -> StereoPlugin {
        let hub = Hub {
            on: false, target: None, look: Look::default(), tune: Tune::default(), timing_ns: 0, swap: false, size: DEFAULT_SIZE,
            history: stereo::HISTORY_FRAMES, ring: VecDeque::new(), other: None, frame_ns: DEFAULT_FRAME_NS, last_in: None, seen: None, pictures: 0,
            measure: false, reading: None, generation: 0, handed: None, drawn: None, stalled_key: None, recent: VecDeque::new(), shut: false,
        };
        let token = new_token();
        let shared = Arc::new(Shared { hub: Mutex::new(hub), wake: Condvar::new(), store: Mutex::new(Arc::new(NoFrames)), headset: Arc::new(Feed::new(&token)) });
        StereoPlugin {
            on: false, look: Look::default(), depth: 0.0, height: 0.0, size_percent: 100.0, tilt_left: 0.0, tilt_right: 0.0, timing: 0.0, swap: false, target: None,
            camera_size: None, canvas: (None, None), shared, pending: false, held: None, fine: false, matching: MatchState::Idle, lut: None,
            focal: BTreeMap::new(), sized_for: None, auto_size: None, exposure: None, brightness: 0.0, brightness_sent: None, brightness_waits: false, size_swapped: None,
            clock: None, web: None, cert_folder: None, headset: false, headset_token: token, headset_address: String::new(), headset_server: None,
            headset_worker: None, headset_problem: String::new(), headset_qr_url: String::new(), link_read: false,
        }
    }

    /// With a web to serve the headset viewer through.
    pub fn with_web(web: Arc<dyn Web>) -> StereoPlugin {
        StereoPlugin { web: Some(web), ..StereoPlugin::new() }
    }

    /// For tests: a scratch folder for the certificate.
    pub fn for_test(web: Arc<dyn Web>, folder: PathBuf) -> StereoPlugin {
        StereoPlugin { cert_folder: Some(folder), ..StereoPlugin::with_web(web) }
    }

    /// draw_stalled at now (monotonic nanoseconds): what the clock does while the target stream stalls.
    pub fn draw_stalled(&self, now: u64) -> bool {
        self.shared.draw_stalled(now)
    }

    fn wake_clock(&mut self) {
        if !self.on || self.shared.hub().shut {
            return;
        }
        if self.clock.is_none() {
            let shared = self.shared.clone();
            match std::thread::Builder::new().name("stereo-clock".into()).spawn(move || shared.run_clock()) {
                Ok(handle) => self.clock = Some(handle),
                Err(e) => log::error!("The clock that draws the 3D picture while its stream stalls didn't start: {e}"),
            }
        }
        self.shared.wake.notify_all();
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
        hub.tune = Tune { depth: self.depth, height: self.height, size: self.size_percent, tilt_left: self.tilt_left, tilt_right: self.tilt_right, lut: self.lut.clone() };
        (hub.look, hub.swap, hub.size, hub.timing_ns) = (self.look, self.swap, size, (self.timing * 1e6) as i64);
        // the left phone's own picture is read while Match asks for the phones' states, before it changes anything
        hub.measure = self.on && self.matching.busy();
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
        // with fewer than two streams there's no pair to keep frames for
        if host.stream_count() < 2 {
            self.shared.hub().drop_frames();
        }
        self.check_match(host);
        self.prepare();
        self.show(host);
    }

    // ── Headset ───────────────────────────────────────────────────────

    fn headset_addresses(&self) -> Vec<String> {
        self.web.as_ref().map(|w| w.addresses()).unwrap_or_default()
    }

    fn fill_headset_address(&mut self) {
        let candidates = self.headset_addresses();
        if !candidates.contains(&self.headset_address) {
            self.headset_address = candidates.first().cloned().unwrap_or_default();
            self.save_link();
        }
    }

    /// Where the settings and the certificate are kept.
    fn folder(&self) -> PathBuf {
        self.cert_folder.clone().or_else(|| telescope_core::config::config_path().parent().map(PathBuf::from)).unwrap_or_default()
    }

    /// The link and address live in a file of their own and never in the settings, which Presets copy and share.
    fn link_file(&self) -> Option<PathBuf> {
        self.web.as_ref().map(|_| self.folder().join(LINK_FILE))
    }

    /// Takes the saved link on first use; one that's missing or isn't a link is kept as the one made at start and saved.
    fn read_link(&mut self) {
        if std::mem::replace(&mut self.link_read, true) {
            return;
        }
        let Some(path) = self.link_file() else { return };
        let saved: Value = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        match saved.get("token").and_then(Value::as_str).filter(|t| token_ok(t)) {
            Some(token) => {
                self.headset_token = token.into();
                self.headset_address = saved.get("address").and_then(Value::as_str).unwrap_or("").into();
                if self.shared.headset.token() != self.headset_token {
                    self.shared.headset.set_token(&self.headset_token);
                }
            }
            None => self.save_link(),
        }
    }

    fn save_link(&self) {
        let Some(path) = self.link_file() else { return };
        let text = json!({"token": self.headset_token, "address": self.headset_address}).to_string();
        if let Err(e) = write_private(&path, text.as_bytes()) {
            log::warn!("The 3D headset's link couldn't be saved: {e}");
        }
    }

    /// The link a headset opens; empty while the page isn't up.
    pub fn headset_url(&self) -> String {
        match &self.headset_server {
            Some(server) if !self.headset_address.is_empty() => format!("https://{}:{}/#{}", self.headset_address, server.port(), self.headset_token),
            _ => String::new(),
        }
    }

    /// Serves the viewer while 3D is on, the Headset switch is on and the plugin runs, and stops serving otherwise.
    fn sync_headset(&mut self) {
        let want = self.on && self.headset && self.web.is_some() && !self.shared.hub().shut;
        if want && self.headset_server.is_none() && self.headset_problem.is_empty() {
            self.start_headset();
        } else if !want {
            self.stop_headset();
            self.headset_problem.clear();
        }
    }

    fn start_headset(&mut self) {
        let Some(web) = self.web.clone() else { return };
        self.read_link();
        self.fill_headset_address();
        let folder = self.folder();
        let feed = self.shared.headset.clone();
        feed.open();
        let started = web.certificate(&folder, &web.addresses()).and_then(|certificate| {
            let serve = Serve { port: stereo_headset::HEADSET_PORT, host: String::new(), label: "3D headset".into() };
            web.serve(&certificate, &serve, Arc::new(Headset(feed.clone())))
        });
        match started {
            Ok(server) => {
                self.headset_server = Some(server);
                let (shared, store) = (self.shared.clone(), self.shared.store());
                feed.watch_epochs(store.clone());
                let worker = std::thread::Builder::new().name("stereo-headset".into()).spawn(move || feed.run(&*store, &|f| shared.hub().was_ours(f)));
                match worker {
                    Ok(handle) => self.headset_worker = Some(handle),
                    Err(e) => log::error!("The thread that makes the headset's pictures didn't start: {e}"),
                }
            }
            Err(error) => {
                feed.close();
                log::error!("The 3D headset page didn't start: {error}");
                self.headset_problem = tr("Couldn't start: {error}").field("error", &error);
            }
        }
    }

    fn stop_headset(&mut self) {
        if let Some(server) = self.headset_server.take() {
            server.stop();
        }
        self.shared.headset.close();
        if let Some(worker) = self.headset_worker.take() {
            let _ = worker.join();
        }
    }

    /// What follows the headset: the switch, its link and the code for it.
    fn headset_props(&mut self) -> Value {
        let url = self.headset_url();
        let addresses = if url.is_empty() { Vec::new() } else { self.headset_addresses() };
        let mut props = json!({
            "headset": self.headset, "headset-problem": if self.headset { self.headset_problem.clone() } else { String::new() },
            "headset-url-shown": !url.is_empty(), "headset-url-text": url,
            "headset-address-shown": addresses.len() > 1, "headset-address-current": addresses.iter().position(|a| *a == self.headset_address).unwrap_or(0),
            "headset-addresses": addresses,
        });
        if url != self.headset_qr_url {
            let (cells, size) = if url.is_empty() { (Vec::new(), 0) } else { telescope_core::qr::encode(&url) };
            props["headset-qr-cells"] = json!(cells);
            props["headset-qr-size"] = json!(size);
            self.headset_qr_url = url;
        }
        props
    }

    fn set_headset(&mut self, host: &mut dyn Host, on: bool) {
        self.headset = on;
        self.headset_problem.clear();
        self.show(host);
    }

    /// The old link stops working: headsets on it are disconnected.
    fn new_headset_link(&mut self, host: &mut dyn Host) {
        self.read_link();
        self.headset_token = new_token();
        self.shared.headset.set_token(&self.headset_token);
        self.save_link();
        self.show(host);
    }

    // ── Card ──────────────────────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("3D"), "switch-help": tr(SWITCH_TIP),
            "look-items": [tr("Side by side"), tr("Red/cyan")], "look-tip": tr(LOOK_TIP),
            "depth-tip": tr(DEPTH_TIP), "height-tip": tr(HEIGHT_TIP), "size-tip": tr(SIZE_TIP), "tilt-left-tip": tr(TILT_LEFT_TIP), "tilt-right-tip": tr(TILT_RIGHT_TIP), "timing-tip": tr(TIMING_TIP),
            "fine-tip": tr(FINE_TIP), "swap-tip": tr(SWAP_TIP), "match-tip": tr(MATCH_TIP), "reset-tip": tr(RESET_TIP), "brightness-tip": tr(BRIGHTNESS_TIP),
            "headset-name": tr("Headset"), "headset-tip": tr(HEADSET_TIP), "headset-qr-label": tr("Scan"), "headset-qr-tip": tr(HEADSET_QR_TIP),
            "headset-address-tip": tr(HEADSET_ADDRESS_TIP), "headset-link-tip": tr(HEADSET_LINK_TIP), "headset-copy-text": tr("Copy"),
            "headset-copy-tip": tr(HEADSET_COPY_TIP), "headset-new-link-text": tr("New link"), "headset-new-link-tip": tr(HEADSET_NEW_LINK_TIP),
        })
    }

    /// What the sync badge says: (word, kind, sentence), blank while it's off.
    fn status(&self, host: &dyn Host) -> (String, &'static str, String) {
        let none = (String::new(), "status_dim", String::new());
        if !self.on {
            return none;
        }
        if host.stream_count() < 2 {
            return (tr("Two streams required"), "status_dim", tr("Start a second stream to get the 3D picture."));
        }
        let seen = self.shared.hub().seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS);
        match seen {
            None => (tr("Waiting"), "status_dim", tr("Both streams have to send a picture and one of them has to be on the Telescope camera.")),
            Some((SyncStatus::InSync, _)) => (tr("In sync"), "status_ok", tr("The two pictures were taken within half a frame of each other.")),
            Some((SyncStatus::Rough, _)) => (tr("Rough sync"), "status_warn", tr("Fast movement can look doubled because the two pictures were taken a little apart.")),
            Some((SyncStatus::NotSynced, _)) => (
                tr("Not synced"),
                "status_warn",
                tr("Telescope can't match the pictures by when they were taken (both phones need the newer phone app). The newest of each is used."),
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
                tr("A phone ran out of ISO range or the two phones have no shutter speed in common. The match is as close as it got."),
            ),
            MatchState::Failed => (tr("Match failed"), "status_warn", tr("Telescope couldn't read both phones and their pictures. Nothing was matched.")),
        }
    }

    fn card_props(&self, host: Option<&dyn Host>) -> Value {
        let mut props = Self::fixed_props();
        let (status, kind, detail) = host.map_or((String::new(), "status_dim", String::new()), |h| self.status(h));
        let (matching, match_kind, match_detail) = if self.on { self.match_status() } else { (String::new(), "status_dim", String::new()) };
        // the header has room for one badge: what needs a look comes first, then what Match did, then that the pictures are in sync
        let (badge, badge_kind, badge_detail) = if matching.is_empty() || (match_kind == "status_ok" && kind != "status_ok") {
            (status, kind, detail)
        } else {
            (matching, match_kind, match_detail)
        };
        let two = host.is_some_and(|h| h.stream_count() >= 2);
        let reset = self.lut.is_some() || !matches!(self.matching, MatchState::Idle);
        if let Value::Object(o) = &mut props {
            o.extend([
                ("reset-shown".to_string(), json!(self.reset_shown())),
                ("on".to_string(), json!(self.on)),
                ("look-current".to_string(), json!(self.look.index())),
                ("depth".to_string(), json!(self.depth)),
                ("depth-text".to_string(), json!(percent_text(self.depth))),
                ("height-shift".to_string(), json!(self.height)),
                ("height-text".to_string(), json!(percent_text(self.height))),
                ("size-percent".to_string(), json!(self.size_percent)),
                ("size-text".to_string(), json!(size_text(self.size_percent))),
                ("tilt-left".to_string(), json!(self.tilt_left)),
                ("tilt-left-text".to_string(), json!(tilt_text(self.tilt_left))),
                ("tilt-right".to_string(), json!(self.tilt_right)),
                ("tilt-right-text".to_string(), json!(tilt_text(self.tilt_right))),
                ("timing".to_string(), json!(self.timing)),
                ("timing-text".to_string(), json!(timing_text(self.timing))),
                ("fine-open".to_string(), json!(self.fine)),
                ("swap".to_string(), json!(self.swap)),
                ("match-enabled".to_string(), json!(two && !self.matching.busy())),
                ("reset-enabled".to_string(), json!(reset)),
                ("badge".to_string(), json!(badge)),
                ("badge-kind".to_string(), json!(badge_kind)),
                ("badge-detail".to_string(), json!(badge_detail)),
                ("brightness".to_string(), json!(self.brightness)),
                ("brightness-text".to_string(), json!(brightness_text(self.brightness))),
                ("brightness-enabled".to_string(), json!(self.on && self.exposure.is_some() && matches!(self.matching, MatchState::Done(..)))),
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
        self.wake_clock();
        self.follow_preview(host);
        self.sync_headset();
        let mut props = self.card_props(Some(host));
        if let (Value::Object(all), Value::Object(headset)) = (&mut props, self.headset_props()) {
            all.extend(headset);
        }
        host.set_ui_props("card", &props);
    }

    /// While 3D makes a picture, picking the other eye's tile still shows it on the preview; with no pair, or off, the preview is as it was.
    fn follow_preview(&self, host: &mut dyn Host) {
        let follows = self.target.clone().filter(|_| self.on).and_then(|t| {
            let [a, b] = self.eyes(host)?;
            let other = if a == t { b } else if b == t { a } else { return None };
            Some((other, t))
        });
        host.preview_follows(NAME, follows.as_ref().map(|f| f.0.as_str()), follows.as_ref().map(|f| f.1.as_str()));
        // with a pair the picture that stream sends is the 3D one, not a camera's view
        let making = self.target.clone().filter(|_| self.on && self.eyes(host).is_some());
        host.preview_composited(NAME, making.as_deref());
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

    /// The ids of the two eyes, left first: what the step makes the picture from, as it sees the streams.
    fn eyes(&self, host: &dyn Host) -> Option<[String; 2]> {
        eye_ids(&*host.frames(), self.target.as_deref(), self.swap, monotonic_ns())
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

    /// Swap eyes: Size is the right eye against the left, so with the phones the other way round it is the reverse (125 % becomes 80 %) and each
    /// phone keeps the zoom it had. The way back gives the exact size it came from, however often the eyes swap. The lenses aren't asked again for
    /// the pair once they had decided.
    fn turn_size_round(&mut self, host: &dyn Host) {
        if (self.size_percent - 100.0).abs() >= 0.05 {
            let auto = self.auto_size == Some(self.size_percent);
            let turned = match self.size_swapped {
                Some((now, before)) if now == self.size_percent => before,
                _ => clean_size(10_000.0 / self.size_percent),
            };
            self.size_swapped = Some((turned, self.size_percent));
            self.size_percent = turned;
            self.auto_size = auto.then_some(turned);
        }
        if self.sized_for.is_some() {
            self.sized_for = self.eyes(host);
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
        // with no match there is no exposure to move together
        (self.exposure, self.brightness, self.brightness_sent, self.brightness_waits) = (None, 0.0, None, false);
        // the step applies what it was handed, so the curves go from there too, whoever dropped the match and whether or not a prepare follows
        let mut hub = self.shared.hub();
        (hub.tune.lut, hub.measure, hub.reading) = (None, false, None);
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
        self.exposure = None;
        self.brightness = 0.0;
        self.matching = MatchState::Polling { ids, states: [None, None], since: monotonic_ns() };
        // what the left phone looks like on its own is read from now until it is changed
        self.shared.hub().reading = None;
        self.changed_without_save(host);
    }

    /// A phone's answer to the state Match asked for. With both there the phones are set and the run begins.
    fn polled(&mut self, host: &mut dyn Host, source_id: &str, state: &Value) {
        let MatchState::Polling { ids, .. } = &self.matching else { return };
        if !ids.iter().any(|id| id == source_id) {
            return;
        }
        // a late answer for a pair that is no longer the eyes (a stream stalled, Swap eyes): nothing is sent to phones that aren't the eyes now
        if self.eyes(host).as_ref() != Some(ids) {
            self.drop_match();
            self.changed_without_save(host);
            return;
        }
        let MatchState::Polling { ids, states, .. } = &mut self.matching else { return };
        let Some(at) = ids.iter().position(|id| id == source_id) else { return };
        states[at] = Some(state.clone());
        self.begin(host);
    }

    /// With both phones' states in: sets them to the same exposure and starts the run. A left phone on automatic is brought back to the brightness
    /// it had before, read from its own picture, so that waits (for a moment) until there is such a reading.
    fn begin(&mut self, host: &mut dyn Host) {
        let MatchState::Polling { ids, states, since } = &self.matching else { return };
        let [Some(left), Some(right)] = states else { return };
        let (ids, since, now) = (ids.clone(), *since, monotonic_ns());
        let (left, right) = (left.clone(), right.clone());
        let (frame_ns, own) = {
            let hub = self.shared.hub();
            (hub.frame_ns, hub.reading.as_ref().filter(|r| now.saturating_sub(r.at) < OWN_LOOK_NS).map(|r| r.stats[0].luma))
        };
        let start = stereo_match::plan(&left, &right, frame_ns);
        if start.as_ref().is_some_and(|s| s.tune_left && s.shutter.is_some()) && own.is_none() && now.saturating_sub(since) < POLL_NS / 2 {
            return;
        }
        let run = match start {
            Some(start) => match start.shutter {
                Some(shutter) => {
                    for (id, iso) in ids.iter().zip(start.iso) {
                        for body in stereo_match::exposure_bodies(iso, shutter) {
                            host.phone_control(Some(id), &body);
                        }
                    }
                    let run = Run::new(start.iso, start.limits, start.tune_left, true, now);
                    match own.filter(|_| start.tune_left) {
                        Some(luma) => run.with_target(luma),
                        None => run,
                    }
                }
                // no shutter both lenses and both frame rates allow: nothing out of a lens's range is sent, and the colours are matched as far as they go
                None => Run::new([0, 0], [(0, 0); 2], false, false, now).limited(),
            },
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
        // a stream that stalled changes who the eyes are, and what was matched is for the pair it was matched for
        self.check_match(host);
        match std::mem::replace(&mut self.matching, MatchState::Idle) {
            MatchState::Polling { ids, states, since } => {
                self.matching = if now.saturating_sub(since) > POLL_NS { MatchState::Failed } else { MatchState::Polling { ids, states, since } };
                // both phones answered but the left one's own picture wasn't read yet
                self.begin(host);
            }
            MatchState::Running { ids, mut run, since } => {
                let reading = self.shared.hub().reading.as_ref().map(|r| (r.at, [r.stats[0].luma, r.stats[1].luma]));
                self.matching = match run.tick(now, reading) {
                    Act::Wait if now.saturating_sub(since) > RUN_NS => {
                        Self::refresh_phones(host, &ids);
                        MatchState::Failed
                    }
                    Act::Wait => MatchState::Running { ids, run, since },
                    Act::SetIso { eye, iso } => {
                        host.phone_control(Some(&ids[if eye == Eye::Left { 0 } else { 1 }]), &stereo_match::iso_body(iso));
                        MatchState::Running { ids, run, since }
                    }
                    Act::Fit(outcome) => {
                        Self::refresh_phones(host, &ids);
                        let lut = self.shared.hub().reading.as_ref().map(|r| stereo_match::fit_lut(&r.stats[1].hist, &r.stats[0].hist));
                        match lut {
                            Some(lut) => {
                                self.lut = Some(Arc::new(lut));
                                self.exposure = run.manual().then(|| Exposure { ids: ids.clone(), base: run.iso(), limits: run.limits() });
                                self.brightness = 0.0;
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

    /// Asks both phones for their state again, so whoever shows their exposure (the Camera card) hears what Match left it at.
    fn refresh_phones(host: &mut dyn Host, ids: &[String; 2]) {
        for id in ids {
            host.request_phone_state(Some(id));
        }
    }

    /// The Brightness slider: both phones' ISOs move by the same number of stops from what Match left them at, no further than either lens takes. It
    /// goes out at most every 150 ms while it is dragged, and the last value always goes out.
    fn move_brightness(&mut self, host: &mut dyn Host, ev: f64) {
        let Some(x) = &self.exposure else { return };
        let (_, ev) = stereo_match::brightness_isos(x.base, x.limits, clean_tenths(ev, BRIGHTNESS_LIMIT));
        self.brightness = (ev * 10.0).round() / 10.0;
        let (now, sent) = (monotonic_ns(), self.brightness_sent.map_or(0, |s| s.0));
        let due = sent.saturating_add(BRIGHTNESS_MS * 1_000_000);
        if self.brightness_sent.is_none() || now >= due {
            self.push_brightness(host, now);
        } else {
            self.brightness_waits = true;
            host.schedule((due - now).div_ceil(1_000_000), BRIGHTNESS_TIMER);
        }
        self.changed_without_save(host);
    }

    /// Sends both phones the ISO the Brightness slider stands for, when it is not the one they have.
    fn push_brightness(&mut self, host: &mut dyn Host, now: u64) {
        self.brightness_waits = false;
        let Some(x) = &self.exposure else { return };
        let (isos, _) = stereo_match::brightness_isos(x.base, x.limits, self.brightness);
        if self.brightness_sent.is_some_and(|s| s.1 == isos) || (self.brightness_sent.is_none() && isos == x.base) {
            return;
        }
        for (id, iso) in x.ids.iter().zip(isos) {
            host.phone_control(Some(id), &stereo_match::iso_body(iso));
        }
        self.brightness_sent = Some((now, isos));
    }

    /// Reset match: the colour curves go and a run stops. The phones keep the exposure Match gave them.
    fn reset_match(&mut self, host: &mut dyn Host) {
        self.drop_match();
        self.changed_without_save(host);
    }

    /// The card's own switch is not part of its Reset.
    const RESET_KEPT: &'static [&'static str] = &["on"];

    /// Whether the card's Reset would change anything: the look, the eyes or a slider is off its default, or a match is there.
    fn reset_shown(&self) -> bool {
        off_default(&self.get_config(), &StereoPlugin::new().get_config(), Self::RESET_KEPT) || self.lut.is_some() || !matches!(self.matching, MatchState::Idle)
    }

    /// The card's Reset: look, eye order and sliders back to their defaults and the match dropped. The phones keep the exposure Match gave them.
    fn reset(&mut self, host: &mut dyn Host) {
        // Brightness goes back to what Match gave the phones before the match is dropped
        if self.exposure.is_some() && self.brightness != 0.0 {
            self.brightness = 0.0;
            self.push_brightness(host, monotonic_ns());
        }
        let cfg = reset_config(&self.get_config(), &StereoPlugin::new().get_config(), Self::RESET_KEPT);
        if self.apply_preset(host, &cfg).is_ok() {
            // Size was put back by hand, so the lenses don't pick it again for this pair
            self.sized_for = self.eyes(host);
            host.schedule_save();
        }
    }

    /// Whether it's on and how it looks, from settings or a preset.
    fn apply(&mut self, cfg: &Value) {
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.look = cfg.get("look").and_then(Value::as_str).and_then(Look::from_name).unwrap_or_default();
        let number = |key: &str| cfg.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        (self.depth, self.height) = (clean_shift(number("depth")), clean_shift(number("height")));
        // a setup saved with the one Tilt, which only ever turned the right eye, keeps that turn on the right eye
        let right = cfg.get("tilt-right").or_else(|| cfg.get("tilt")).and_then(Value::as_f64).unwrap_or(0.0);
        (self.tilt_left, self.tilt_right) = (clean_tilt(number("tilt-left")), clean_tilt(right));
        self.size_percent = clean_size(cfg.get("size").and_then(Value::as_f64).unwrap_or(100.0));
        self.timing = clean_timing(number("timing"));
        self.swap = cfg.get("swap") == Some(&Value::Bool(true));
        // what was matched or sized for another set of settings no longer holds
        self.drop_match();
        // and a picture the clock is drawing with the old ones must not go out
        self.shared.hub().generation += 1;
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
            lab: true,
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
        host.preview_follows(NAME, None, None);
        host.want_frames(NAME, false);
        host.want_frame_history(NAME, 0);
        host.cancel_schedule(STATUS_TIMER);
        host.cancel_schedule(MATCH_TIMER);
        {
            // so no picture goes out after quitting, and the settings stay as they were
            let mut hub = self.shared.hub();
            hub.on = false;
            hub.measure = false;
            hub.shut = true;
            hub.forget();
        }
        self.stop_headset();
        self.shared.wake.notify_all();
        if let Some(clock) = self.clock.take() {
            let _ = clock.join();
        }
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
                // a stream that stalled or came back may have changed who the eyes are
                self.check_match(host);
                self.size_from_lenses(host);
                self.show(host);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == MATCH_TIMER => self.match_tick(host),
            Event::TimerFired { owner, token } if owner == NAME && token == BRIGHTNESS_TIMER
                && self.brightness_waits => {
                    self.push_brightness(host, monotonic_ns());
                }
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
            "tilt-left-moved" => {
                self.tilt_left = clean_tilt(number.unwrap_or(0.0));
                self.changed(host);
            }
            "tilt-right-moved" => {
                self.tilt_right = clean_tilt(number.unwrap_or(0.0));
                self.changed(host);
            }
            "brightness-moved" => self.move_brightness(host, number.unwrap_or(0.0)),
            "timing-moved" => {
                self.timing = clean_timing(number.unwrap_or(0.0));
                self.changed(host);
            }
            "fine-toggled" => {
                self.fine = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.show(host);
            }
            "swap-toggled" => {
                let before = self.swap;
                self.swap = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                if self.swap != before {
                    // each phone keeps the roll it had
                    std::mem::swap(&mut self.tilt_left, &mut self.tilt_right);
                    self.turn_size_round(host);
                }
                // the colour curves were made for the other way round
                self.check_match(host);
                self.changed(host);
                self.size_from_lenses(host);
            }
            "headset-toggled" => self.set_headset(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "headset-address-picked" => {
                let at = number.filter(|i| *i >= 0.0).map_or(0, |i| i as usize);
                if let Some(ip) = self.headset_addresses().get(at).cloned().filter(|ip| *ip != self.headset_address) {
                    self.read_link();
                    self.headset_address = ip;
                    self.save_link();
                    self.show(host);
                }
            }
            "headset-copy-clicked" => {
                let url = self.headset_url();
                host.copy_text(&url);
            }
            "headset-new-link-clicked" => self.new_headset_link(host),
            "reset-clicked" => self.reset(host),
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

    fn settings(&self) -> Vec<telescope_core::settings::SettingSpec> {
        use telescope_core::settings::SettingSpec;
        use crate::stereo_match::{SIZE_MAX, SIZE_MIN};
        vec![
            SettingSpec::switch("on", "Whether two streams are put into one 3D picture", false),
            SettingSpec::choice("look", "How the 3D picture is laid out", &[("side_by_side", "Side by side, for a headset or a 3D screen"), ("red_cyan", "Red and cyan, for glasses")], "side_by_side"),
            SettingSpec::number("depth", "Slides the two pictures apart or together to change how far forward the 3D reaches", -SHIFT_LIMIT, SHIFT_LIMIT, 1, 0.0),
            SettingSpec::number("height", "Lifts or lowers the right picture", -SHIFT_LIMIT, SHIFT_LIMIT, 1, 0.0),
            SettingSpec::number("size", "Zooms the right picture over 100 or the left one under it", SIZE_MIN, SIZE_MAX, 1, 100.0).unit("%"),
            SettingSpec::number("tilt-left", "Rolls the left picture", -TILT_LIMIT, TILT_LIMIT, 1, 0.0),
            SettingSpec::number("tilt-right", "Rolls the right picture", -TILT_LIMIT, TILT_LIMIT, 1, 0.0),
            SettingSpec::number("timing", "Moves the right picture earlier or later in time", -TIMING_LIMIT_MS, TIMING_LIMIT_MS, 0, 0.0).unit("ms"),
            SettingSpec::switch("swap", "Swaps which phone is the left eye", false),
        ]
    }

    fn get_config(&self) -> Value {
        json!({
            "on": self.on, "look": self.look.name(), "depth": self.depth, "height": self.height, "size": self.size_percent, "tilt-left": self.tilt_left, "tilt-right": self.tilt_right,
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
        let headset = match &self.headset_server {
            Some(server) => format!("on, port {}, {} watching", server.port(), self.shared.headset.viewers()),
            None if !self.headset => "off".to_string(),
            None => "not running".to_string(),
        };
        if !self.on {
            return vec![("3D".into(), "off".into()), ("3D headset".into(), headset)];
        }
        let hub = self.shared.hub();
        let sync = match hub.seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS) {
            Some((SyncStatus::InSync, _)) => "in sync",
            Some((SyncStatus::Rough, _)) => "rough sync",
            Some((SyncStatus::NotSynced, _)) => "not synced",
            None => "waiting",
        };
        vec![("3D".into(), format!("on, {}, {sync}", self.look.name())), ("3D headset".into(), headset)]
    }
}

#[cfg(test)]
#[path = "stereo_plugin_tests.rs"]
mod tests;
