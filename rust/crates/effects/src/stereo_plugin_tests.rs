//! The 3D plugin through a fake host: when it takes over, what it asks the host to keep, which frames it pairs and what the card says.

use super::*;
use crate::stereo::HISTORY_FRAMES;
use std::collections::BTreeMap;
use telescope_core::clock::ClockSync;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

const MS: u64 = 1_000_000;

/// The stream frames the host hands out, with the history it keeps for each and each phone's clock error.
#[derive(Default)]
struct Store {
    frames: Mutex<Vec<StreamFrame>>,
    history: Mutex<BTreeMap<String, Vec<Frame>>>,
    errors: Mutex<BTreeMap<String, u64>>,
    /// Every frame put out in place of a stream's, as (stream, frame, number it was for).
    replaced: Mutex<Vec<(String, Frame, u64)>>,
    /// The host's pipeline epoch, which goes up when any stream's steps are handed out again.
    epoch: std::sync::atomic::AtomicU64,
    /// What the camera of every stream sends when it isn't the stream's own newest frame (another plugin replaced it).
    outgoing: Mutex<Option<Frame>>,
    /// Run whenever the sync of a stream is asked, which a picture does while it's being made: the moment to change things under it.
    while_drawing: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl FrameStore for Store {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        self.frames.lock().unwrap().clone()
    }
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        self.frames.lock().unwrap().iter().find(|f| Some(f.source_id.as_str()) == source_id).cloned()
    }
    fn replace_frame(&self, source_id: Option<&str>, frame: Frame, number: u64) -> bool {
        self.replaced.lock().unwrap().push((source_id.unwrap_or("").to_string(), frame, number));
        true
    }
    fn stream_frame_near(&self, source_id: Option<&str>, capture_ns: u64) -> Option<StreamFrame> {
        let id = source_id?;
        let history = self.history.lock().unwrap();
        let list = history.get(id)?;
        let captures: Vec<Option<u64>> = list.iter().map(|f| f.capture_ns).collect();
        let frame = list[stereo::nearest_capture(&captures, capture_ns)?].clone();
        Some(StreamFrame { source_id: id.into(), name: id.into(), frame })
    }
    fn outgoing_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        match self.outgoing.lock().unwrap().clone() {
            Some(frame) => Some(StreamFrame { source_id: source_id?.into(), name: String::new(), frame }),
            None => self.stream_frame(source_id),
        }
    }
    fn pipeline_epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn clock_sync(&self, source_id: Option<&str>) -> Option<ClockSync> {
        let hook = self.while_drawing.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
        self.errors.lock().unwrap().get(source_id?).map(|e| ClockSync { error_ns: *e })
    }
}

struct Ctx(String);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        &self.0
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A 32x18 picture of one colour taken at `capture` (milliseconds on the computer's clock), published just now.
fn shot(rgb: [u8; 3], capture: Option<u64>) -> Frame {
    let mut f = Frame::new(32, 18, PixelFormat::Rgba, [rgb[0], rgb[1], rgb[2], 255].repeat(32 * 18));
    f.time_ns = monotonic_ns();
    f.capture_ns = capture.map(|ms| ms * MS);
    f
}

fn px(frame: &Frame, x: usize, y: usize) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    [frame.data[at], frame.data[at + 1], frame.data[at + 2]]
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const YELLOW: [u8; 3] = [255, 255, 0];
const CYAN: [u8; 3] = [0, 255, 255];

struct Rig {
    plugin: StereoPlugin,
    host: FakeHost,
    store: Arc<Store>,
    /// The step the pipeline holds: it stays the same until the steps are handed out again (new_step).
    step: Option<Box<dyn CpuStep>>,
}

fn rig() -> Rig {
    let store = Arc::new(Store::default());
    let mut host = FakeHost::new();
    host.frame_store = Some(store.clone());
    // the Telescope camera shows a, and two streams run
    host.cameras = vec![(0, "a".into(), None, None)];
    host.canvas = (Some(64), Some(36));
    host.streaming = true;
    host.streams = Some(2);
    host.live = vec!["a".into(), "b".into()];
    let mut plugin = StereoPlugin::new();
    plugin.setup(&mut host);
    plugin.on_stream_start(&mut host, &StreamInfo { source_id: "a".into(), url: "a".into(), session: 0 });
    Rig { plugin, host, store, step: None }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.plugin.shutdown(&mut self.host);
    }
}

impl Rig {
    /// Streams a and b show these frames now (and their history is just that frame).
    fn show_frames(&self, a: Frame, b: Frame) {
        *self.store.frames.lock().unwrap() = vec![
            StreamFrame { source_id: "a".into(), name: "Phone A".into(), frame: a.clone() },
            StreamFrame { source_id: "b".into(), name: "Phone B".into(), frame: b.clone() },
        ];
        *self.store.history.lock().unwrap() = BTreeMap::from([("a".to_string(), vec![a]), ("b".to_string(), vec![b])]);
    }

    fn switch(&mut self, on: bool) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(on)] });
    }

    fn card(&mut self, callback: &str, arg: Value) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
    }

    /// The frame as the plugin's step in source_id's stream hands it on.
    fn process(&mut self, frame: &Frame, source_id: &str) -> Frame {
        if self.step.is_none() {
            self.new_step();
        }
        let mut out = frame.clone();
        self.step.as_mut().expect("a step").process(&mut out, &Ctx(source_id.into()));
        out
    }

    /// The pipeline asking the plugin for its step again, as it does whenever any plugin changes its own (a Blur coming on).
    fn new_step(&mut self) {
        let Some(FrameStep::Cpu(step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        self.step = Some(step);
    }

    fn props(&self) -> Value {
        self.plugin.card_props(Some(&self.host))
    }

    fn status(&self) -> String {
        self.props()["status"].as_str().unwrap().to_string()
    }
}

fn same(a: &Frame, b: &Frame) -> bool {
    Arc::ptr_eq(&a.data, &b.data)
}

#[test]
fn it_is_optional_and_off_by_default_under_streaming_and_devices() {
    let plugin = StereoPlugin::new();
    let info = plugin.info();
    assert_eq!((info.name.as_str(), info.title.as_str(), info.category.as_str()), ("stereo", "3D", "streaming"));
    assert!(info.optional && !info.required);
    assert_eq!(plugin.get_config()["on"], json!(false));
}

#[test]
fn it_leaves_the_streams_alone_while_off() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    let frame = shot(RED, None);
    assert!(same(&r.process(&frame, "a"), &frame));
    assert!(same(&r.process(&frame, "b"), &frame));
}

#[test]
fn it_takes_over_the_telescope_camera_once_on_with_two_streams() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((out.width, out.height), (64, 36), "the canvas Setup asks for");
    assert_eq!(px(&out, 5, 10), RED, "the first stream is the left eye");
    assert_eq!(px(&out, 58, 10), BLUE, "the second the right");
    // only the stream on the Telescope camera gets the picture, the other goes to its own camera as it was
    let frame = shot(BLUE, None);
    assert!(same(&r.process(&frame, "b"), &frame));
}

#[test]
fn the_picture_keeps_the_frames_number_and_times() {
    let mut r = rig();
    r.show_frames(shot(RED, Some(100)), shot(BLUE, Some(100)));
    r.switch(true);
    let mut frame = shot(RED, Some(100));
    (frame.number, frame.time_ns) = (77, 12_345);
    let out = r.process(&frame, "a");
    assert_eq!((out.number, out.time_ns, out.capture_ns), (77, 12_345, Some(100 * MS)));
}

#[test]
fn a_stream_alone_goes_out_as_it_is() {
    let mut r = rig();
    r.switch(true);
    *r.store.frames.lock().unwrap() = vec![StreamFrame { source_id: "a".into(), name: "A".into(), frame: shot(RED, None) }];
    let frame = shot(RED, None);
    assert!(same(&r.process(&frame, "a"), &frame));
    assert_eq!(r.plugin.shared.hub().pictures, 0);
}

#[test]
fn a_stream_that_stopped_sending_isnt_an_eye() {
    let mut r = rig();
    r.switch(true);
    let mut old = shot(BLUE, None);
    old.time_ns = monotonic_ns() - 10_000 * MS;
    r.show_frames(shot(RED, None), old);
    let frame = shot(RED, None);
    assert!(same(&r.process(&frame, "a"), &frame));
}

#[test]
fn swap_eyes_exchanges_the_eyes() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    r.card("swap-toggled", json!(true));
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (BLUE, RED));
    r.card("swap-toggled", json!(false));
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (RED, BLUE));
}

#[test]
fn a_stream_that_isnt_on_the_telescope_camera_can_be_an_eye_while_the_picture_goes_there() {
    let mut r = rig();
    r.host.cameras = vec![(0, "c".into(), None, None)];
    r.host.streams = Some(3);
    r.plugin.on_stream_start(&mut r.host, &StreamInfo { source_id: "c".into(), url: "c".into(), session: 0 });
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.store.frames.lock().unwrap().push(StreamFrame { source_id: "c".into(), name: "C".into(), frame: shot(GREEN, None) });
    r.switch(true);
    let out = r.process(&shot(GREEN, None), "c");
    assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (RED, BLUE), "the first two that started");
}

#[test]
fn red_cyan_comes_out_when_asked_for() {
    let mut r = rig();
    let white = shot([255, 255, 255], None);
    r.show_frames(white.clone(), shot([0, 0, 0], None));
    r.switch(true);
    r.card("look-picked", json!(1));
    let out = r.process(&white, "a");
    assert_eq!(px(&out, 30, 10), [255, 0, 0], "the left eye reaches only the red channel");
    r.card("look-picked", json!(0));
    let out = r.process(&white, "a");
    assert_eq!(px(&out, 5, 10), [255, 255, 255], "back to side by side");
}

#[test]
fn the_sliders_move_the_eyes() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    // each eye is 32 wide: 5 % is 2 pixels, the left goes right by one and the right goes left by one
    r.card("depth-moved", json!(5.0));
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((px(&out, 0, 10), px(&out, 1, 10), px(&out, 31, 10)), ([0, 0, 0], RED, RED));
    assert_eq!((px(&out, 32, 10), px(&out, 62, 10), px(&out, 63, 10)), (BLUE, BLUE, [0, 0, 0]));
    r.card("depth-moved", json!(0.0));
    // 5 % of 36 rows is 2: the right eye goes up
    r.card("height-moved", json!(5.0));
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((px(&out, 40, 0), px(&out, 40, 34), px(&out, 40, 35)), (BLUE, [0, 0, 0], [0, 0, 0]));
    assert_eq!(px(&out, 10, 35), RED, "the left eye stays");
}

// ── What it asks the host to keep ─────────────────────────────────────────

#[test]
fn history_is_asked_for_only_while_on_with_two_streams() {
    let mut r = rig();
    assert!(r.host.history_wanted.is_empty(), "off asks for nothing");
    assert!(!r.host.frames_wanted.contains(NAME));
    r.switch(true);
    assert_eq!(r.host.history_wanted.get(NAME), Some(&HISTORY_FRAMES));
    assert!(r.host.frames_wanted.contains(NAME));
    // a stream stops
    r.host.streams = Some(1);
    r.plugin.on_stream_stop(&mut r.host);
    assert!(r.host.history_wanted.is_empty(), "released with one stream left");
    assert!(r.host.frames_wanted.contains(NAME), "the other streams' frames keep coming for when it's back");
    r.host.streams = Some(2);
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert_eq!(r.host.history_wanted.get(NAME), Some(&HISTORY_FRAMES));
    // switched off
    r.switch(false);
    assert!(r.host.history_wanted.is_empty());
    assert!(!r.host.frames_wanted.contains(NAME));
    // and again on quitting
    r.switch(true);
    r.plugin.shutdown(&mut r.host);
    assert!(r.host.history_wanted.is_empty() && r.host.frames_wanted.is_empty());
}

#[test]
fn a_saved_on_asks_for_history_once_the_host_is_there() {
    let mut r = rig();
    r.plugin.set_config(&json!({"on": true})).unwrap();
    assert!(r.host.history_wanted.is_empty(), "set_config has no host");
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert_eq!(r.host.history_wanted.get(NAME), Some(&HISTORY_FRAMES));
}

#[test]
fn the_status_timer_runs_only_while_on() {
    let mut r = rig();
    assert!(r.host.scheduled.is_empty());
    r.switch(true);
    assert_eq!(r.host.scheduled, vec![(STATUS_MS, STATUS_TIMER.to_string())]);
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: STATUS_TIMER.into() });
    assert_eq!(r.host.scheduled.len(), 1, "it asks again");
    r.switch(false);
    assert!(r.host.scheduled.is_empty());
}

// ── Pairing ───────────────────────────────────────────────────────────────

#[test]
fn the_stream_behind_keeps_its_frame_and_the_other_gives_its_nearest_from_the_hosts_history() {
    let mut r = rig();
    r.switch(true);
    // a (the target) is behind at 100 ms, b is ahead at 166 ms and has older frames in the host's history
    let (b_old, b_mid, b_new) = (shot(CYAN, Some(90)), shot(GREEN, Some(133)), shot(YELLOW, Some(166)));
    let a1 = shot(RED, Some(100));
    r.show_frames(a1.clone(), b_new.clone());
    r.store.history.lock().unwrap().insert("b".into(), vec![b_old, b_mid, b_new]);
    let out = r.process(&a1, "a");
    assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (RED, CYAN));
}

#[test]
fn when_the_target_runs_ahead_its_own_earlier_frame_is_used() {
    let mut r = rig();
    r.switch(true);
    // b is behind at 110 ms. What the host holds of a is the 3D picture, so a's own frames come from what the step saw
    let b1 = shot(YELLOW, Some(110));
    for (colour, at) in [(RED, 100), (GREEN, 133), (BLUE, 166)] {
        let frame = shot(colour, Some(at));
        r.show_frames(frame.clone(), b1.clone());
        // the host's history of a is whatever it published, which is never what the step needs
        r.store.history.lock().unwrap().insert("a".into(), vec![shot([9, 9, 9], Some(110))]);
        let out = r.process(&frame, "a");
        if at == 166 {
            assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (RED, YELLOW), "100 ms is nearest to 110");
        }
    }
}

#[test]
fn without_capture_times_the_newest_of_each_go_together() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(shot(RED, None), shot(BLUE, None));
    let out = r.process(&shot(GREEN, None), "a");
    assert_eq!((px(&out, 5, 10), px(&out, 58, 10)), (GREEN, BLUE), "the step's own frame is a's newest");
    assert_eq!(r.plugin.shared.hub().seen.map(|s| s.0), Some(SyncStatus::NotSynced));
    assert_eq!(r.status(), "Not synced");
}

// ── The badge ─────────────────────────────────────────────────────────────

#[test]
fn the_badge_says_how_well_the_pair_is_synced() {
    let mut r = rig();
    assert_eq!(r.status(), "", "nothing while off");
    r.switch(true);
    assert_eq!(r.status(), "Waiting");
    r.show_frames(shot(RED, Some(100)), shot(BLUE, Some(105)));
    r.store.errors.lock().unwrap().extend([("a".to_string(), 2 * MS), ("b".to_string(), 3 * MS)]);
    r.process(&shot(RED, Some(100)), "a");
    // worst error 3 ms and a gap of 5 ms are under half a frame
    assert_eq!(r.status(), "In sync");
    assert_eq!(r.props()["status-kind"], json!("status_ok"));
    // the worse of the two bounds counts
    r.store.errors.lock().unwrap().insert("b".into(), 14 * MS);
    r.process(&shot(RED, Some(100)), "a");
    assert_eq!(r.status(), "Rough sync");
    assert_eq!(r.props()["status-kind"], json!("status_warn"));
    // a phone with no clock offset yet
    r.store.errors.lock().unwrap().remove("b");
    r.process(&shot(RED, Some(100)), "a");
    assert_eq!(r.status(), "Not synced");
    // the details are sentences for the tooltip
    assert!(r.props()["status-detail"].as_str().unwrap().ends_with('.'));
}

#[test]
fn the_badge_asks_for_two_streams() {
    let mut r = rig();
    r.switch(true);
    r.host.streams = Some(1);
    r.plugin.on_stream_stop(&mut r.host);
    assert_eq!(r.status(), "Needs two streams");
    assert_eq!(r.props()["match-enabled"], json!(false));
    r.host.streams = Some(2);
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert_eq!(r.props()["match-enabled"], json!(true));
}

#[test]
fn the_badge_forgets_a_pair_once_the_pictures_stop() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.process(&shot(RED, None), "a");
    assert_eq!(r.status(), "Not synced");
    r.plugin.shared.hub().seen = Some((SyncStatus::InSync, monotonic_ns() - 5_000 * MS));
    assert_eq!(r.status(), "Waiting");
}

// ── Timing ────────────────────────────────────────────────────────────────

/// Frame number `n` of a stream that came in `ago` milliseconds before now.
fn arrived(mut frame: Frame, n: u64, ago: u64) -> Frame {
    frame.number = n;
    frame.time_ns = monotonic_ns() - ago * MS;
    frame
}

fn left_pixel(out: &Frame) -> [u8; 3] {
    px(out, 5, 10)
}

fn right_pixel(out: &Frame) -> [u8; 3] {
    px(out, 58, 10)
}

#[test]
fn timing_moves_the_pairing_target_and_the_sync_follows_it() {
    let mut r = rig();
    r.switch(true);
    r.store.errors.lock().unwrap().extend([("a".to_string(), 2 * MS), ("b".to_string(), 2 * MS)]);
    let (b90, b133, b166) = (shot(CYAN, Some(90)), shot(GREEN, Some(133)), shot(YELLOW, Some(166)));
    let a = shot(RED, Some(100));
    r.show_frames(a.clone(), b166.clone());
    r.store.history.lock().unwrap().insert("b".into(), vec![b90, b133, b166]);
    assert_eq!(right_pixel(&r.process(&a, "a")), CYAN, "nearest to 100 ms");
    // the right eye 33 ms later than the left: nearest to 133 ms, and that is in sync
    r.card("timing-moved", json!(33.0));
    assert_eq!(right_pixel(&r.process(&a, "a")), GREEN);
    assert_eq!(r.status(), "In sync", "the offset counts against the gap, so 33 ms apart is what was asked for");
    r.card("timing-moved", json!(66.0));
    assert_eq!(right_pixel(&r.process(&a, "a")), YELLOW);
    // earlier than the left eye: 100 less 33 is 67 ms
    r.card("timing-moved", json!(-33.0));
    assert_eq!(right_pixel(&r.process(&a, "a")), CYAN);
    // back to nothing, and the sync is measured without it
    r.card("timing-moved", json!(0.0));
    assert_eq!(right_pixel(&r.process(&a, "a")), CYAN);
    // and only the pair that is really 33 ms apart, with nothing asked for, is a rough sync
    r.store.history.lock().unwrap().insert("b".into(), vec![shot(GREEN, Some(133))]);
    r.show_frames(a.clone(), shot(GREEN, Some(133)));
    r.process(&a, "a");
    assert_eq!(r.status(), "Rough sync", "33 ms apart without asking for it");
}

#[test]
fn the_history_asked_for_follows_the_timing() {
    let mut r = rig();
    r.switch(true);
    assert_eq!(r.host.history_wanted.get(NAME), Some(&HISTORY_FRAMES));
    // 200 ms at the default 30 fps is 7 frames and 3 to spare
    r.card("timing-moved", json!(200.0));
    assert_eq!(r.host.history_wanted.get(NAME), Some(&10));
    r.card("timing-moved", json!(-200.0));
    assert_eq!(r.host.history_wanted.get(NAME), Some(&10));
    r.card("timing-moved", json!(0.0));
    assert_eq!(r.host.history_wanted.get(NAME), Some(&HISTORY_FRAMES));
    assert_eq!(r.plugin.shared.hub().history, HISTORY_FRAMES, "the frames kept here follow it too");
}

#[test]
fn without_capture_times_a_positive_timing_holds_the_left_eye_back_by_arrival() {
    let mut r = rig();
    r.switch(true);
    r.card("timing-moved", json!(66.0));
    // the target's frames arrive 33 ms apart; the newest is 66 ms after the one that goes out
    for (n, (colour, ago)) in [(RED, 99), (GREEN, 66), (YELLOW, 33), (CYAN, 0)].into_iter().enumerate() {
        let a = arrived(shot(colour, None), n as u64, ago);
        r.show_frames(a.clone(), arrived(shot(BLUE, None), n as u64, ago));
        let out = r.process(&a, "a");
        if n == 3 {
            assert_eq!((left_pixel(&out), right_pixel(&out)), (GREEN, BLUE));
        }
    }
    assert_eq!(r.status(), "Not synced");
}

#[test]
fn without_capture_times_a_negative_timing_holds_the_right_eye_back_by_arrival() {
    let mut r = rig();
    r.switch(true);
    r.card("timing-moved", json!(-66.0));
    // the other stream isn't the target, so its frames are kept here as they are seen
    for (n, (colour, ago)) in [(GREEN, 99), (YELLOW, 66), (CYAN, 33), (BLUE, 0)].into_iter().enumerate() {
        let a = arrived(shot(RED, None), n as u64, ago);
        r.show_frames(a.clone(), arrived(shot(colour, None), n as u64, ago));
        let out = r.process(&a, "a");
        if n == 3 {
            assert_eq!((left_pixel(&out), right_pixel(&out)), (RED, YELLOW));
        }
    }
}

#[test]
fn without_a_timing_the_newest_of_each_still_go_together() {
    let mut r = rig();
    r.switch(true);
    for (n, (colour, ago)) in [(GREEN, 66), (YELLOW, 33), (CYAN, 0)].into_iter().enumerate() {
        let a = arrived(shot(colour, None), n as u64, ago);
        r.show_frames(a.clone(), arrived(shot(BLUE, None), n as u64, ago));
        let out = r.process(&a, "a");
        if n == 2 {
            assert_eq!((left_pixel(&out), right_pixel(&out)), (CYAN, BLUE));
        }
    }
}

// ── Size from the lenses ──────────────────────────────────────────────────

/// A phone's state with one lens of this 35 mm equivalent focal length.
fn lens(auto: bool, iso: Option<i64>, shutter: Option<i64>, focal: i64) -> Value {
    json!({
        "cameras": [{
            "id": "0", "current": true, "focalMm": focal, "isoMin": 50, "isoMax": 12800, "shutterMinNs": 100_000, "shutterMaxNs": 500_000_000,
            "supportsManualSensor": true,
        }],
        "active_lens": "0", "auto": auto, "iso": iso, "shutter_ns": shutter, "phone_fps": 30,
    })
}

impl Rig {
    /// A phone's state arriving, as every stream's does.
    fn state(&mut self, id: &str, state: Value) {
        self.plugin.on_event(&mut self.host, &Event::StreamState { source_id: id.into(), state });
    }

    /// The status timer going off, which is when Size looks at the lenses.
    fn tick_status(&mut self) {
        self.plugin.on_event(&mut self.host, &Event::TimerFired { owner: NAME.into(), token: STATUS_TIMER.into() });
    }

    fn size(&self) -> f64 {
        self.plugin.get_config()["size"].as_f64().unwrap()
    }
}

#[test]
fn size_starts_from_the_two_lenses_once_for_each_pair() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    // one phone has said which lens it is on: not enough
    r.state("a", lens(true, None, None, 26));
    r.tick_status();
    assert_eq!(r.size(), 100.0);
    // the right lens (24 mm) sees more than the left one's (26 mm), so its picture is enlarged
    r.state("b", lens(true, None, None, 24));
    r.tick_status();
    assert_eq!(r.size(), 108.3);
    assert_eq!(r.props()["size-text"], json!("108.3%"));
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
    // what a person sets stays, and it isn't looked at again for this pair
    r.card("size-moved", json!(100.0));
    r.tick_status();
    assert_eq!(r.size(), 100.0);
    r.card("size-moved", json!(97.0));
    r.card("swap-toggled", json!(true));
    r.tick_status();
    assert_eq!(r.size(), 97.0, "a size somebody chose isn't touched");
}

#[test]
fn swapping_the_eyes_turns_the_size_it_chose_round() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    r.state("a", lens(true, None, None, 26));
    r.state("b", lens(true, None, None, 24));
    r.tick_status();
    assert_eq!(r.size(), 108.3);
    r.card("swap-toggled", json!(true));
    assert_eq!(r.size(), 92.3, "the other way round it is the reverse");
    r.card("swap-toggled", json!(false));
    assert_eq!(r.size(), 108.3);
}

#[test]
fn size_isnt_set_for_lenses_too_far_apart_or_phones_that_dont_say() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    // a wide lens against a long one is off the slider
    r.state("a", lens(true, None, None, 24));
    r.state("b", lens(true, None, None, 52));
    r.tick_status();
    assert_eq!(r.size(), 100.0);
    // a phone that doesn't report its lens
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    r.state("a", lens(true, None, None, 24));
    r.state("b", json!({"battery": 80}));
    r.tick_status();
    assert_eq!(r.size(), 100.0);
}

#[test]
fn size_waits_for_the_plugin_to_be_on() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.state("a", lens(true, None, None, 26));
    r.state("b", lens(true, None, None, 24));
    r.tick_status();
    assert_eq!(r.size(), 100.0);
    r.switch(true);
    r.tick_status();
    assert_eq!(r.size(), 108.3);
}

// ── Match ─────────────────────────────────────────────────────────────────

/// A 64x36 picture whose every channel runs from black to `top` across its width: the middle half averages about half of it.
fn gradient(top: u8) -> Frame {
    let mut f = Frame::new(64, 36, PixelFormat::Rgba, (0..64 * 36).flat_map(|i| {
        let v = ((i % 64) as f64 * top as f64 / 63.0).round() as u8;
        [v, v, v, 255]
    }).collect());
    f.time_ns = monotonic_ns();
    f
}

fn control(id: &str, body: Value) -> String {
    format!("phone_control {:?} {}", Some(id), body)
}

impl Rig {
    fn polled(&mut self, id: &str, state: Value) {
        self.plugin.on_event(&mut self.host, &Event::PhonePolled { source_id: id.into(), session: 1, shown: false, state });
    }

    fn controls(&self) -> Vec<String> {
        self.host.calls.iter().filter(|c| c.starts_with("phone_control")).cloned().collect()
    }

    /// The last ISO sent to a phone.
    fn iso_sent(&self, id: &str) -> Option<i64> {
        let prefix = format!("phone_control {:?} ", Some(id));
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix(&prefix)).filter_map(|b| serde_json::from_str::<Value>(b).ok()).find(|b| b["action"] == "iso").and_then(|b| b["value"].as_i64())
    }

    fn match_badge(&self) -> String {
        self.props()["match-status"].as_str().unwrap().to_string()
    }

    /// One look at the pictures `at` (the computer's clock) and one tick of Match's timer.
    fn match_tick(&mut self, left: &Frame, right: &Frame, at: u64) {
        self.show_frames(left.clone(), right.clone());
        self.process(left, "a");
        if let Some(reading) = self.plugin.shared.hub().reading.as_mut() {
            reading.at = at;
        }
        self.plugin.match_tick_at(&mut self.host, at);
    }
}

#[test]
fn match_sets_both_phones_nudges_the_right_iso_and_makes_the_colour_curves() {
    let mut r = rig();
    r.switch(true);
    // the right phone is a stop darker at the same ISO, and its picture follows the ISO it is sent
    let right = |iso: i64| gradient((120.0 * (iso as f64 / 400.0).powf(1.0 / 2.2)).min(255.0) as u8);
    let left = gradient(240);
    r.show_frames(left.clone(), right(400));
    assert_eq!(r.props()["match-enabled"], json!(true));
    r.card("match-clicked", json!(null));
    assert!(r.host.calls.contains(&"request_phone_state Some(\"a\")".to_string()));
    assert!(r.host.calls.contains(&"request_phone_state Some(\"b\")".to_string()));
    assert_eq!(r.match_badge(), "Matching");
    assert_eq!((r.props()["match-enabled"].clone(), r.props()["reset-enabled"].clone()), (json!(false), json!(true)));
    // nothing is sent until both phones have answered
    r.polled("a", lens(false, Some(400), Some(8_000_000), 24));
    assert!(r.controls().is_empty());
    r.polled("b", lens(true, None, None, 26));
    // both go to the left phone's shutter and ISO
    assert_eq!(r.controls(), vec![
        control("a", json!({"action": "iso", "value": 400})),
        control("a", json!({"action": "shutter", "value": 8_000_000})),
        control("b", json!({"action": "iso", "value": 400})),
        control("b", json!({"action": "shutter", "value": 8_000_000})),
    ]);
    assert!(r.plugin.shared.hub().measure, "the step now reads the pictures");
    // a few looks: only the right phone's ISO moves, and each look waits for the last change to show
    let (mut at, mut iso) = (monotonic_ns(), 400);
    let mut steps = 0;
    while r.match_badge() == "Matching" {
        at += 800 * MS;
        r.match_tick(&left, &right(iso), at);
        let sent = r.iso_sent("b").unwrap();
        if sent != iso {
            (iso, steps) = (sent, steps + 1);
        }
        assert!(steps <= 6, "it keeps nudging");
    }
    assert!((2..=4).contains(&steps), "a few steps: {steps}");
    assert!(iso > 400, "the darker phone went up");
    assert_eq!(r.iso_sent("a"), Some(400), "the left phone was left alone");
    assert_eq!(r.match_badge(), "Matched");
    assert_eq!(r.props()["match-status-kind"], json!("status_ok"));
    assert!(r.plugin.lut.is_some());
    assert!(!r.plugin.shared.hub().measure, "the step stops reading");
}

#[test]
fn the_colour_curves_are_applied_while_packing_and_only_to_the_right_eye() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    // phones that take no manual exposure only get their colour matched
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    assert!(r.controls().is_empty());
    let (left, dark) = (gradient(240), gradient(120));
    r.show_frames(left.clone(), dark.clone());
    let before = r.process(&left, "a");
    assert!(px(&before, 16, 18)[0].abs_diff(px(&before, 48, 18)[0]) > 40, "the right eye is much darker: {:?} {:?}", px(&before, 16, 18), px(&before, 48, 18));
    r.match_tick(&left, &dark, monotonic_ns() + 800 * MS);
    assert_eq!(r.match_badge(), "Matched");
    let after = r.process(&left, "a");
    assert_eq!(px(&after, 16, 18), px(&before, 16, 18), "the left eye is as it was");
    for x in [8, 16, 24] {
        let (l, rr) = (px(&after, x, 18), px(&after, 32 + x, 18));
        assert!(l[0].abs_diff(rr[0]) <= 8 && l[1].abs_diff(rr[1]) <= 8 && l[2].abs_diff(rr[2]) <= 8, "x {x}: {l:?} {rr:?}");
    }
}

#[test]
fn match_reads_the_pictures_only_while_it_runs() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(200), gradient(100));
    r.process(&gradient(200), "a");
    assert!(r.plugin.shared.hub().reading.is_none());
}

#[test]
fn reset_match_drops_the_curves_and_leaves_the_phones_alone() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    let (left, dark) = (gradient(240), gradient(120));
    r.match_tick(&left, &dark, monotonic_ns() + 800 * MS);
    assert!(r.plugin.lut.is_some());
    let sent = r.controls().len();
    r.card("reset-match-clicked", json!(null));
    assert!(r.plugin.lut.is_none());
    assert_eq!(r.match_badge(), "");
    assert_eq!(r.props()["reset-enabled"], json!(false));
    assert_eq!(r.controls().len(), sent, "the phones keep their exposure");
    let out = r.process(&left, "a");
    assert!(px(&out, 16, 18)[0].abs_diff(px(&out, 48, 18)[0]) > 40, "the right eye is as dark as it was");
    // and it stops a run that is going
    r.card("match-clicked", json!(null));
    r.card("reset-match-clicked", json!(null));
    assert_eq!(r.match_badge(), "");
    assert_eq!(r.props()["match-enabled"], json!(true));
}

#[test]
fn match_fails_when_the_phones_dont_answer() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", lens(false, Some(400), Some(8_000_000), 24));
    r.plugin.match_tick_at(&mut r.host, monotonic_ns() + 1_000 * MS);
    assert_eq!(r.match_badge(), "Matching", "still waiting for b");
    r.plugin.match_tick_at(&mut r.host, monotonic_ns() + 6_000 * MS);
    assert_eq!(r.match_badge(), "Match failed");
    assert_eq!(r.props()["match-status-kind"], json!("status_warn"));
    assert!(r.controls().is_empty(), "nothing was changed");
    assert!(r.props()["match-enabled"].as_bool().unwrap(), "and it can be tried again");
}

#[test]
fn match_ends_as_partly_matched_when_the_pictures_cant_be_brought_together() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", lens(false, Some(400), Some(8_000_000), 24));
    r.polled("b", lens(false, Some(400), Some(8_000_000), 24));
    // the pictures don't change with the ISO, so the steps run out
    let (left, dark) = (gradient(240), gradient(120));
    let mut at = monotonic_ns();
    for _ in 0..30 {
        at += 800 * MS;
        r.match_tick(&left, &dark, at);
        if r.match_badge() != "Matching" {
            break;
        }
    }
    assert_eq!(r.match_badge(), "Partly matched");
    assert_eq!(r.props()["match-status-kind"], json!("status_warn"));
    assert!(r.plugin.lut.is_some(), "the colour is still matched");
}

#[test]
fn the_curves_belong_to_the_pair_so_swapping_the_eyes_drops_them() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    r.match_tick(&gradient(240), &gradient(120), monotonic_ns() + 800 * MS);
    assert!(r.plugin.lut.is_some());
    r.card("swap-toggled", json!(true));
    assert!(r.plugin.lut.is_none());
    assert_eq!(r.match_badge(), "");
}

#[test]
fn the_curves_go_when_a_stream_stops_and_match_needs_two() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    r.match_tick(&gradient(240), &gradient(120), monotonic_ns() + 800 * MS);
    r.host.streams = Some(1);
    r.plugin.on_stream_stop(&mut r.host);
    assert!(r.plugin.lut.is_none());
    assert_eq!(r.props()["match-enabled"], json!(false));
    r.card("match-clicked", json!(null));
    assert_eq!(r.match_badge(), "", "nothing to match");
}

#[test]
fn match_waits_for_the_plugin_to_be_on() {
    let mut r = rig();
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    assert!(!r.host.calls.iter().any(|c| c.starts_with("request_phone_state")));
    // switching it off ends a run too
    r.switch(true);
    r.card("match-clicked", json!(null));
    r.switch(false);
    assert_eq!(r.match_badge(), "");
}

#[test]
fn the_fine_tune_group_opens_and_shuts_without_touching_the_picture() {
    let mut r = rig();
    assert_eq!(r.props()["fine-open"], json!(false));
    r.card("fine-toggled", json!(true));
    assert_eq!(r.props()["fine-open"], json!(true));
    r.card("fine-toggled", json!(false));
    assert_eq!(r.props()["fine-open"], json!(false));
    assert!(!r.host.calls.iter().any(|c| c == "schedule_save"), "it's not a setting");
}

#[test]
fn every_new_row_has_a_one_sentence_tooltip() {
    let r = rig();
    let props = r.props();
    for key in ["size-tip", "tilt-tip", "timing-tip", "fine-tip", "match-tip", "reset-tip"] {
        let tip = props[key].as_str().unwrap();
        assert!(tip.ends_with('.') && tip.matches(". ").count() == 0 && !tip.contains('\u{2014}') && !tip.contains('\u{2013}'), "{key}: {tip}");
    }
}

// ── Settings ──────────────────────────────────────────────────────────────

#[test]
fn the_settings_come_back_and_are_held_to_the_sliders() {
    let mut r = rig();
    r.plugin.set_config(&json!({"on": true, "look": "red_cyan", "depth": 1.26, "height": -40.0, "size": 140.0, "tilt": 2.34, "timing": -250.4, "swap": true})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"on": true, "look": "red_cyan", "depth": 1.3, "height": -5.0, "size": 125.0, "tilt": 2.3, "timing": -200.0, "swap": true}));
    r.plugin.set_config(&json!({"look": "nonsense", "depth": "far"})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"on": false, "look": "side_by_side", "depth": 0.0, "height": 0.0, "size": 100.0, "tilt": 0.0, "timing": 0.0, "swap": false}));
    // a preset brings them back too
    r.plugin.apply_preset(&mut r.host, &json!({"on": true, "look": "red_cyan"})).unwrap();
    assert_eq!(r.plugin.get_config()["look"], json!("red_cyan"));
    assert!(r.host.history_wanted.contains_key(NAME));
}

#[test]
fn the_card_callbacks_change_the_settings_and_save_them() {
    let mut r = rig();
    r.card("look-picked", json!(1));
    r.card("depth-moved", json!(9.0));
    r.card("height-moved", json!(-1.5));
    r.card("size-moved", json!(92.46));
    r.card("tilt-moved", json!(-9.0));
    r.card("timing-moved", json!(35.4));
    r.card("swap-toggled", json!(true));
    assert_eq!(
        r.plugin.get_config(),
        json!({"on": false, "look": "red_cyan", "depth": 5.0, "height": -1.5, "size": 92.5, "tilt": -5.0, "timing": 35.0, "swap": true})
    );
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
    let props = r.props();
    assert_eq!((props["look-current"].clone(), props["depth-text"].clone(), props["height-text"].clone()), (json!(1), json!("+5.0%"), json!("-1.5%")));
    assert_eq!((props["size-text"].clone(), props["tilt-text"].clone(), props["timing-text"].clone()), (json!("92.5%"), json!("-5.0°"), json!("+35 ms")));
    // the sliders sit at their defaults at first
    let fresh = rig().props();
    assert_eq!((fresh["size-text"].clone(), fresh["tilt-text"].clone(), fresh["timing-text"].clone()), (json!("100.0%"), json!("0.0°"), json!("0 ms")));
    // a look that isn't there is ignored
    r.card("look-picked", json!(7));
    assert_eq!(r.plugin.get_config()["look"], json!("red_cyan"));
}

#[test]
fn the_toggle_action_switches_it() {
    let mut r = rig();
    let actions = r.plugin.actions();
    assert_eq!(actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["stereo.on"]);
    assert_eq!(r.plugin.run_action(&mut r.host, "stereo.on", &json!({})).as_deref(), Some("3D: on"));
    assert_eq!(r.plugin.get_config()["on"], json!(true));
    r.plugin.run_action(&mut r.host, "stereo.on", &json!({"mode": "off"}));
    assert_eq!(r.plugin.get_config()["on"], json!(false));
}

#[test]
fn nothing_goes_out_after_quitting() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.switch(true);
    r.plugin.shutdown(&mut r.host);
    let frame = shot(RED, None);
    assert!(same(&r.process(&frame, "a"), &frame));
    assert_eq!(r.plugin.get_config()["on"], json!(true), "but what's saved stays on");
}

// ── Review fixes: pipeline changes, stalls, who the eyes are, Match's clock, Reset ───

impl Rig {
    /// Streams a, b and c show these frames now (and their history is just that frame).
    fn show_three(&self, a: Frame, b: Frame, c: Frame) {
        let list = [("a", "Phone A", a), ("b", "Phone B", b), ("c", "Phone C", c)];
        *self.store.frames.lock().unwrap() = list.iter().map(|(id, name, f)| StreamFrame { source_id: id.to_string(), name: name.to_string(), frame: f.clone() }).collect();
        *self.store.history.lock().unwrap() = list.iter().map(|(id, _, f)| (id.to_string(), vec![f.clone()])).collect();
    }

    /// What stream a goes out with now: the picture the step made, as the host holds it.
    fn publish_picture(&self, out: &Frame) {
        let mut frames = self.store.frames.lock().unwrap();
        if let Some(a) = frames.iter_mut().find(|f| f.source_id == "a") {
            a.frame = out.clone();
        }
    }

    fn replaced(&self) -> Vec<(String, Frame, u64)> {
        self.store.replaced.lock().unwrap().clone()
    }
}

/// A (the target) kept frames at 100, 133 and 166 ms, and b is behind at 110 ms.
fn kept_three(r: &mut Rig) -> Frame {
    let b1 = shot(YELLOW, Some(110));
    for (colour, at) in [(RED, 100), (GREEN, 133), (BLUE, 166)] {
        let frame = shot(colour, Some(at));
        r.show_frames(frame.clone(), b1.clone());
        let out = r.process(&frame, "a");
        if at == 166 {
            // b is behind at 110 ms, so a's own earlier frame (100 ms) is the one that goes with it
            assert_eq!(left_pixel(&out), RED, "before anything changed");
        }
    }
    b1
}

#[test]
fn frames_kept_before_the_epoch_moved_are_not_paired_even_when_this_streams_step_stayed() {
    let mut r = rig();
    r.switch(true);
    let b1 = kept_three(&mut r);
    // a Blur comes on for b only: a's own step is the same one, but the red frame was a clear one
    r.store.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let frame = shot(BLUE, Some(200));
    r.show_frames(frame.clone(), b1.clone());
    let out = r.process(&frame, "a");
    assert_eq!((left_pixel(&out), right_pixel(&out)), (BLUE, YELLOW), "only the frame that came after the change");
    let hub = r.plugin.shared.hub();
    assert_eq!(hub.current_ring(r.store.pipeline_epoch()).len(), 1);
}

#[test]
fn a_new_step_alone_does_not_drop_what_the_epoch_still_allows() {
    let mut r = rig();
    r.switch(true);
    let b1 = kept_three(&mut r);
    // the host bumps the epoch whenever it hands steps out, so a step handed out without it is nothing to forget
    r.new_step();
    let frame = shot(BLUE, Some(200));
    r.show_frames(frame.clone(), b1.clone());
    let out = r.process(&frame, "a");
    assert_eq!((left_pixel(&out), right_pixel(&out)), (RED, YELLOW));
}

#[test]
fn the_other_streams_ring_is_stamped_and_older_frames_are_left_out() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.process(&shot(RED, None), "a");
    assert!(r.plugin.shared.hub().other.is_some());
    r.store.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut b2 = shot(GREEN, None);
    b2.number = 9;
    r.show_frames(shot(RED, None), b2);
    r.process(&shot(RED, None), "a");
    let hub = r.plugin.shared.hub();
    assert_eq!(hub.other.as_ref().map(|(_, ring)| ring.len()), Some(2), "both are kept");
    assert_eq!(hub.current_other(r.store.pipeline_epoch()).len(), 1, "only the one after the change is paired from");
}

#[test]
fn the_rings_go_when_the_pair_breaks() {
    let mut r = rig();
    r.switch(true);
    let a = shot(RED, Some(100));
    r.show_frames(a.clone(), shot(BLUE, Some(100)));
    r.process(&a, "a");
    r.process(&a, "a");
    assert!(r.plugin.shared.hub().ring.len() >= 2 && r.plugin.shared.hub().other.is_some());
    // b stops: what a's step sees next has one stream, and neither eye's frames are kept
    *r.store.frames.lock().unwrap() = vec![StreamFrame { source_id: "a".into(), name: "Phone A".into(), frame: a.clone() }];
    assert!(same(&r.process(&a, "a"), &a));
    {
        let hub = r.plugin.shared.hub();
        assert!(hub.ring.is_empty() && hub.other.is_none());
    }
    // and with no pictures coming at all, the plugin hearing that a stream stopped lets go too
    r.show_frames(a.clone(), shot(BLUE, Some(100)));
    r.process(&a, "a");
    assert!(!r.plugin.shared.hub().ring.is_empty());
    r.host.streams = Some(1);
    r.plugin.on_stream_stop(&mut r.host);
    assert!(r.plugin.shared.hub().ring.is_empty());
}

#[test]
fn a_stalled_stream_isnt_an_eye_for_the_step_or_for_match() {
    let mut r = rig();
    r.switch(true);
    let mut stale_b = shot(GREEN, None);
    stale_b.time_ns = monotonic_ns() - 10_000 * MS;
    r.show_three(shot(RED, None), stale_b, shot(BLUE, None));
    let out = r.process(&shot(RED, None), "a");
    assert_eq!((left_pixel(&out), right_pixel(&out)), (RED, BLUE), "a and c, since b stopped sending");
    assert_eq!(r.plugin.eyes(&r.host), Some(["a".to_string(), "c".to_string()]), "and that is who Match talks to");
    r.card("match-clicked", json!(null));
    let asked: Vec<&String> = r.host.calls.iter().filter(|c| c.starts_with("request_phone_state")).collect();
    assert_eq!(asked, ["request_phone_state Some(\"a\")", "request_phone_state Some(\"c\")"]);
}

#[test]
fn a_match_is_dropped_when_the_eye_pair_changes_without_the_streams_changing() {
    let mut r = rig();
    r.switch(true);
    let mut stale_b = shot(GREEN, None);
    stale_b.time_ns = monotonic_ns() - 10_000 * MS;
    r.show_three(gradient(240), stale_b, gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("c", json!({"cameras": []}));
    r.process(&gradient(240), "a");
    r.plugin.shared.hub().reading.as_mut().unwrap().at = monotonic_ns() + 800 * MS;
    r.plugin.match_tick_at(&mut r.host, monotonic_ns() + 800 * MS);
    assert!(r.plugin.lut.is_some(), "matched for a and c");
    assert!(r.plugin.shared.hub().tune.lut.is_some(), "and the step is applying them");
    // b starts sending again: it is the second stream, so the pair is a and b now and the curves were for another
    r.show_three(gradient(240), gradient(100), gradient(120));
    r.tick_status();
    assert!(r.plugin.lut.is_none());
    assert!(r.plugin.shared.hub().tune.lut.is_none(), "the step stops applying them at once, not with the next slider move");
    assert_eq!(r.match_badge(), "");
}

#[test]
fn match_only_takes_a_look_from_pictures_taken_after_the_change_and_its_settling() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    let now = monotonic_ns();
    // a right eye that was cached: it was taken long before the change, whenever the pair was put together
    let (left, mut right) = (gradient(240), gradient(120));
    right.capture_ns = Some(now - 5_000 * MS);
    r.show_frames(left.clone(), right.clone());
    let mut left_new = left.clone();
    left_new.capture_ns = Some(now + 5_000 * MS);
    r.process(&left_new, "a");
    assert_eq!(r.plugin.shared.hub().reading.as_ref().map(|x| x.at), Some(now - 5_000 * MS), "the older of the two pictures");
    r.plugin.match_tick_at(&mut r.host, now + 2_000 * MS);
    assert_eq!(r.match_badge(), "Matching", "a picture from before the change tells nothing");
    // both taken after the settling time
    right.capture_ns = Some(now + 5_000 * MS);
    r.show_frames(left_new.clone(), right.clone());
    r.process(&left_new, "a");
    r.plugin.match_tick_at(&mut r.host, now + 5_100 * MS);
    assert_eq!(r.match_badge(), "Matched");
}

#[test]
fn match_without_capture_times_goes_by_arrival() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    let (mut left, mut right) = (gradient(240), gradient(120));
    right.time_ns = monotonic_ns() - 2_000 * MS;
    left.time_ns = monotonic_ns() + 1_500 * MS;
    r.show_frames(left.clone(), right.clone());
    r.process(&left, "a");
    r.plugin.match_tick_at(&mut r.host, monotonic_ns() + 2_000 * MS);
    assert_eq!(r.match_badge(), "Matching", "the right picture arrived before the change");
}

#[test]
fn match_asks_the_phones_again_when_it_is_done_so_the_camera_card_hears_the_exposure() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    let asked = |r: &Rig, id: &str| r.host.calls.iter().filter(|c| **c == format!("request_phone_state Some(\"{id}\")")).count();
    assert_eq!((asked(&r, "a"), asked(&r, "b")), (1, 1));
    r.match_tick(&gradient(240), &gradient(120), monotonic_ns() + 800 * MS);
    assert_eq!(r.match_badge(), "Matched");
    assert_eq!((asked(&r, "a"), asked(&r, "b")), (2, 2));
}

#[test]
fn phones_with_no_shutter_in_common_are_left_alone_and_the_match_is_partly_done() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", lens(false, Some(400), Some(8_000_000), 24));
    let mut other = lens(true, None, None, 26);
    other["cameras"][0]["shutterMinNs"] = json!(600_000_000);
    other["cameras"][0]["shutterMaxNs"] = json!(900_000_000);
    r.polled("b", other);
    assert!(r.controls().is_empty(), "nothing out of a lens's range is sent");
    r.match_tick(&gradient(240), &gradient(120), monotonic_ns() + 800 * MS);
    assert_eq!(r.match_badge(), "Partly matched");
    assert!(r.plugin.lut.is_some(), "the colours are still matched");
}

/// The card with Reset's things moved: look, swap, a slider and a match.
fn moved_rig() -> Rig {
    let mut r = rig();
    r.switch(true);
    r.card("look-picked", json!(1));
    r.card("swap-toggled", json!(true));
    r.card("depth-moved", json!(1.5));
    r.card("timing-moved", json!(40.0));
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    r.polled("a", json!({"cameras": []}));
    r.polled("b", json!({"cameras": []}));
    r.match_tick(&gradient(240), &gradient(120), monotonic_ns() + 800 * MS);
    r
}

#[test]
fn reset_is_lit_for_a_moved_look_swap_slider_or_a_match_and_not_otherwise() {
    let mut r = rig();
    assert_eq!(r.props()["reset-shown"], json!(false));
    r.switch(true);
    assert_eq!(r.props()["reset-shown"], json!(false), "the switch isn't part of it");
    for (callback, arg) in [("look-picked", json!(1)), ("swap-toggled", json!(true)), ("depth-moved", json!(1.0)), ("height-moved", json!(1.0)), ("size-moved", json!(110.0)), ("tilt-moved", json!(1.0)), ("timing-moved", json!(10.0))] {
        let back = match callback { "look-picked" => json!(0), "swap-toggled" => json!(false), "size-moved" => json!(100.0), _ => json!(0.0) };
        r.card(callback, arg);
        assert_eq!(r.props()["reset-shown"], json!(true), "{callback}");
        r.card(callback, back);
        assert_eq!(r.props()["reset-shown"], json!(false), "{callback} back");
    }
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    assert_eq!(r.props()["reset-shown"], json!(true), "a match is under way");
}

#[test]
fn reset_puts_everything_back_drops_the_match_and_leaves_the_phones_and_the_switch() {
    let mut r = moved_rig();
    assert_eq!(r.props()["reset-shown"], json!(true));
    assert!(r.plugin.lut.is_some());
    let sent = r.controls();
    r.host.calls.clear();
    r.card("reset-clicked", json!(null));
    let cfg = r.plugin.get_config();
    assert_eq!(cfg, json!({"on": true, "look": "side_by_side", "depth": 0.0, "height": 0.0, "size": 100.0, "tilt": 0.0, "timing": 0.0, "swap": false}));
    assert!(r.plugin.lut.is_none());
    assert_eq!(r.match_badge(), "");
    assert_eq!(r.props()["reset-shown"], json!(false));
    assert!(r.controls().is_empty() && sent.is_empty(), "no word to the phones");
    assert!(r.host.calls.contains(&"schedule_save".to_string()));
}

#[test]
fn reset_keeps_size_at_a_hundred_even_with_two_different_lenses() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.state("a", lens(true, None, None, 24));
    r.state("b", lens(true, None, None, 28));
    r.tick_status();
    assert!(r.size() != 100.0, "chosen from the lenses");
    r.card("reset-clicked", json!(null));
    assert_eq!(r.size(), 100.0);
    r.tick_status();
    assert_eq!(r.size(), 100.0, "the lenses don't take it back for the same two phones");
}

// ── A stalled target stream ───────────────────────────────────────────────

/// A running pair whose a stream (the target) made `out` and then stopped sending `quiet` milliseconds ago.
fn stalled_rig(quiet: u64) -> (Rig, Frame) {
    let mut r = rig();
    r.switch(true);
    let a = shot(RED, Some(100));
    r.show_frames(a.clone(), shot(BLUE, Some(100)));
    let out = r.process(&a, "a");
    r.publish_picture(&out);
    r.plugin.shared.hub().last_in = Some(monotonic_ns() - quiet * MS);
    (r, out)
}

#[test]
fn while_the_target_stalls_the_picture_goes_on_from_its_last_frame_and_the_other_eye() {
    let (r, _) = stalled_rig(500);
    assert!(r.plugin.draw_stalled(monotonic_ns()), "drawn from a's last frame and b's newest");
    let (id, frame, number) = r.replaced().pop().expect("a picture went out");
    assert_eq!(id, "a");
    assert_eq!(number, r.store.frames.lock().unwrap()[0].frame.number);
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (RED, BLUE));
    // nothing moved since: no new picture
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    // the other eye sent a new one
    let mut b = shot(GREEN, Some(133));
    b.number = 77;
    let a = r.store.frames.lock().unwrap()[0].frame.clone();
    *r.store.frames.lock().unwrap() = vec![
        StreamFrame { source_id: "a".into(), name: "Phone A".into(), frame: a },
        StreamFrame { source_id: "b".into(), name: "Phone B".into(), frame: b.clone() },
    ];
    *r.store.history.lock().unwrap().get_mut("b").unwrap() = vec![b];
    assert!(r.plugin.draw_stalled(monotonic_ns()));
    let frame = r.replaced().pop().unwrap().1;
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (RED, GREEN));
}

#[test]
fn a_target_that_is_still_sending_isnt_drawn_over() {
    let (r, _) = stalled_rig(5);
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    assert!(r.replaced().is_empty());
}

#[test]
fn after_three_seconds_the_stalled_eye_is_dropped_and_the_live_one_goes_on_both_sides() {
    let (r, _) = stalled_rig(4_000);
    // what the host holds of a is as old as the stream's last frame
    r.store.frames.lock().unwrap()[0].frame.time_ns = monotonic_ns() - 4_000 * MS;
    assert!(r.plugin.draw_stalled(monotonic_ns()));
    let frame = r.replaced().pop().expect("a picture went out").1;
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (BLUE, BLUE), "the picture is moving, not frozen on the red eye");
    assert_eq!(r.status(), "Waiting", "and it isn't called 3D");
}

#[test]
fn nothing_is_drawn_when_nothing_else_is_moving_or_a_later_step_covered_the_picture() {
    // the other stream is gone too
    let (r, _) = stalled_rig(4_000);
    r.store.frames.lock().unwrap().truncate(1);
    r.store.frames.lock().unwrap()[0].frame.time_ns = monotonic_ns() - 4_000 * MS;
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    // a wait screen (or anything after the step) changed what a sends
    let (r, _) = stalled_rig(500);
    r.store.frames.lock().unwrap()[0].frame = shot([1, 2, 3], Some(100));
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    assert!(r.replaced().is_empty());
}

#[test]
fn the_clock_draws_on_its_own_while_the_stream_stalls_and_stops_with_the_plugin() {
    let (mut r, _) = stalled_rig(500);
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    let start = std::time::Instant::now();
    while r.replaced().is_empty() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!r.replaced().is_empty(), "the clock drew without being asked");
    r.plugin.shutdown(&mut r.host);
    assert!(r.plugin.clock.is_none());
}

// ── Second review: Privacy on the other stream, a render in flight, whose frame the clock may replace, late answers, Match's curves ──

/// Like stalled_rig, without the clock thread, so a test is the only one drawing.
fn quiet_rig(quiet: u64) -> (Rig, Frame) {
    let mut r = rig();
    r.switch(true);
    r.plugin.shared.hub().shut = true;
    let a = shot(RED, Some(100));
    r.show_frames(a.clone(), shot(BLUE, Some(100)));
    let out = r.process(&a, "a");
    r.publish_picture(&out);
    r.plugin.shared.hub().last_in = Some(monotonic_ns() - quiet * MS);
    (r, out)
}

#[test]
fn a_picture_in_flight_is_not_sent_after_3d_went_off_was_reset_or_moved_to_another_stream() {
    for what in ["off", "reset", "target"] {
        let (r, _) = quiet_rig(500);
        let shared = r.plugin.shared.clone();
        *r.store.while_drawing.lock().unwrap() = Some(Arc::new(move || {
            let mut hub = shared.hub();
            match what {
                "off" => {
                    hub.on = false;
                    hub.forget();
                }
                "reset" => hub.generation += 1,
                _ => {
                    hub.target = Some("b".into());
                    hub.forget();
                }
            }
        }));
        assert!(!r.plugin.draw_stalled(monotonic_ns()), "{what}");
        assert!(r.replaced().is_empty(), "{what}: it went out anyway");
    }
}

#[test]
fn the_clock_never_overwrites_a_frame_another_plugin_put_out_in_its_place() {
    let (r, _) = quiet_rig(500);
    // what the camera sends isn't the picture the step made, and isn't the stream's own newest frame either
    *r.store.outgoing.lock().unwrap() = Some(shot([1, 2, 3], Some(100)));
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    assert!(r.replaced().is_empty());
    // nobody replaced it: the clock draws, and its own picture on the camera is still its own for the next one
    *r.store.outgoing.lock().unwrap() = None;
    assert!(r.plugin.draw_stalled(monotonic_ns()));
    *r.store.outgoing.lock().unwrap() = Some(r.replaced().pop().unwrap().1);
    let mut b = shot(GREEN, Some(133));
    b.number = 77;
    r.store.frames.lock().unwrap()[1].frame = b;
    assert!(r.plugin.draw_stalled(monotonic_ns()), "its own earlier picture is not someone else's");
}

#[test]
fn a_late_answer_for_phones_that_are_not_the_eyes_any_more_sends_them_nothing() {
    let mut r = rig();
    r.switch(true);
    r.show_frames(gradient(240), gradient(120));
    r.card("match-clicked", json!(null));
    // b stops sending while the phones are being asked: the eyes are a and c now
    let mut stale_b = gradient(120);
    stale_b.time_ns = monotonic_ns() - 10_000 * MS;
    r.show_three(gradient(240), stale_b, gradient(120));
    r.polled("a", lens(false, Some(400), Some(8_000_000), 24));
    r.polled("b", lens(true, None, None, 26));
    assert!(r.controls().is_empty(), "nothing went to a or b: {:?}", r.controls());
    assert_eq!(r.match_badge(), "", "and the match is dropped, not left running");
    assert!(!r.plugin.shared.hub().measure);
}

#[test]
fn a_stalled_redraw_without_capture_times_still_takes_in_the_other_eyes_new_frames() {
    let mut r = rig();
    r.switch(true);
    r.plugin.shared.hub().shut = true;
    r.card("timing-moved", json!(-66.0));
    let mut last = None;
    for (n, (colour, ago)) in [(GREEN, 99), (YELLOW, 66), (CYAN, 33), (BLUE, 0)].into_iter().enumerate() {
        let a = arrived(shot(RED, None), n as u64, ago);
        let b = arrived(shot(colour, None), n as u64, ago);
        r.show_frames(a.clone(), b.clone());
        let out = r.process(&a, "a");
        r.publish_picture(&out);
        last = Some(b);
    }
    // the target stalls and the other eye sends one more, a frame time after the last
    r.plugin.shared.hub().last_in = Some(monotonic_ns() - 500 * MS);
    let mut next = arrived(shot(GREEN, None), 4, 0);
    next.time_ns = last.unwrap().time_ns + 33 * MS;
    r.store.frames.lock().unwrap()[1].frame = next;
    assert!(r.plugin.draw_stalled(monotonic_ns()));
    let frame = r.replaced().pop().expect("a picture went out").1;
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (RED, CYAN), "held back 66 ms from the new frame, which only works if it was added to the ring");
}

#[test]
fn when_both_feeds_stall_the_rings_are_dropped_after_three_seconds() {
    let (r, _) = quiet_rig(4_000);
    {
        let hub = r.plugin.shared.hub();
        assert!(!hub.ring.is_empty() && hub.other.is_some());
    }
    let old = monotonic_ns() - 4_000 * MS;
    for f in r.store.frames.lock().unwrap().iter_mut() {
        f.frame.time_ns = old;
    }
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    let hub = r.plugin.shared.hub();
    assert!(hub.ring.is_empty() && hub.other.is_none(), "neither eye's old frames wait for the next pair");
}

// ── Third review: the epoch a frame was made under, a fallback from the other eye alone, whose picture the clock replaces ──

#[test]
fn a_target_frame_from_before_the_epoch_changed_is_not_drawn_next_to_the_other_eye() {
    let (r, _) = quiet_rig(500);
    // a Blur came on for the other stream after a's last frame was kept
    r.store.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    assert!(r.plugin.draw_stalled(monotonic_ns()));
    let frame = r.replaced().pop().expect("a picture went out").1;
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (BLUE, BLUE), "the clear red eye isn't on show, the other eye alone is");
}

#[test]
fn the_other_eye_alone_goes_on_both_sides_after_both_feeds_stalled_and_only_it_came_back() {
    let (r, _) = quiet_rig(4_000);
    let old = monotonic_ns() - 4_000 * MS;
    for f in r.store.frames.lock().unwrap().iter_mut() {
        f.frame.time_ns = old;
    }
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    assert!(r.plugin.shared.hub().ring.is_empty(), "the rings were let go");
    // b comes back, a doesn't
    let mut b = shot(GREEN, Some(500));
    b.number = 88;
    r.store.frames.lock().unwrap()[1].frame = b;
    assert!(r.plugin.draw_stalled(monotonic_ns()), "the camera doesn't stay frozen");
    let frame = r.replaced().pop().expect("a picture went out").1;
    assert_eq!((left_pixel(&frame), right_pixel(&frame)), (GREEN, GREEN));
}

#[test]
fn the_clock_does_not_overwrite_a_picture_put_out_between_its_check_and_its_write() {
    let (r, _) = quiet_rig(500);
    let store = r.store.clone();
    // while the picture is made, another plugin replaces what the camera sends
    *r.store.while_drawing.lock().unwrap() = Some(Arc::new(move || {
        *store.outgoing.lock().unwrap() = Some(shot([1, 2, 3], Some(100)));
    }));
    assert!(!r.plugin.draw_stalled(monotonic_ns()));
    assert!(r.replaced().is_empty());
}

#[test]
fn a_step_that_was_rendering_while_3d_was_switched_off_or_reset_leaves_its_frame_alone() {
    for what in ["off", "reset"] {
        let mut r = rig();
        r.show_frames(shot(RED, Some(100)), shot(BLUE, Some(100)));
        r.switch(true);
        let shared = r.plugin.shared.clone();
        *r.store.while_drawing.lock().unwrap() = Some(Arc::new(move || {
            let mut hub = shared.hub();
            if what == "off" {
                hub.on = false;
            }
            hub.generation += 1;
        }));
        r.store.errors.lock().unwrap().insert("a".into(), MS);
        let frame = shot(RED, Some(100));
        assert!(same(&r.process(&frame, "a"), &frame), "{what}: the stale picture was installed");
    }
}

#[test]
fn the_ring_marks_a_frame_with_the_epoch_the_pipeline_gave_it() {
    let mut r = rig();
    r.show_frames(shot(RED, Some(100)), shot(BLUE, Some(100)));
    r.switch(true);
    let frame = shot(RED, Some(100));
    r.store.epoch.fetch_add(4, std::sync::atomic::Ordering::SeqCst);
    struct Entered(String, u64);
    impl FrameContext for Entered {
        fn source_id(&self) -> &str {
            &self.0
        }
        fn entry_epoch(&self) -> Option<u64> {
            Some(self.1)
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }
    r.new_step();
    let mut out = frame.clone();
    // the steps began under epoch 1, and the host is at 4 by the time they get here
    r.step.as_mut().unwrap().process(&mut out, &Entered("a".into(), 1));
    assert_eq!(r.plugin.shared.hub().ring.back().map(|(_, e)| *e), Some(1));
}
