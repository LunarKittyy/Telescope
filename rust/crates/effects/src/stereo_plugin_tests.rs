//! The 3D plugin through a fake host: when it takes over, what it asks the host to keep, which frames it pairs and what the card says.

use super::*;
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
}

impl FrameStore for Store {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        self.frames.lock().unwrap().clone()
    }
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        self.frames.lock().unwrap().iter().find(|f| Some(f.source_id.as_str()) == source_id).cloned()
    }
    fn replace_frame(&self, _source_id: Option<&str>, _frame: Frame, _number: u64) -> bool {
        false
    }
    fn stream_frame_near(&self, source_id: Option<&str>, capture_ns: u64) -> Option<StreamFrame> {
        let id = source_id?;
        let history = self.history.lock().unwrap();
        let list = history.get(id)?;
        let captures: Vec<Option<u64>> = list.iter().map(|f| f.capture_ns).collect();
        let frame = list[stereo::nearest_capture(&captures, capture_ns)?].clone();
        Some(StreamFrame { source_id: id.into(), name: id.into(), frame })
    }
    fn clock_sync(&self, source_id: Option<&str>) -> Option<ClockSync> {
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
    Rig { plugin, host, store }
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
        let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        let mut out = frame.clone();
        step.process(&mut out, &Ctx(source_id.into()));
        out
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
    assert_eq!(r.props()["link-enabled"], json!(false));
    r.host.streams = Some(2);
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert_eq!(r.props()["link-enabled"], json!(true));
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

// ── Link exposure ─────────────────────────────────────────────────────────

fn control(id: &str, body: Value) -> String {
    format!("phone_control {:?} {}", Some(id), body)
}

#[test]
fn link_exposure_sends_the_shown_phones_values_to_both_phones() {
    let mut r = rig();
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.host.phone_state = Some(json!({
        "auto": false, "iso": 400, "shutter_ns": 8_000_000, "wb_manual": true, "wb_r": 2.0, "wb_ge": 1.0, "wb_go": 1.0, "wb_b": 1.5,
        "focus_mode": "manual", "focus_distance": 2.5,
    }));
    r.card("link-clicked", json!(null));
    let sent: Vec<&String> = r.host.calls.iter().filter(|c| c.starts_with("phone_control")).collect();
    let bodies = [
        json!({"action": "iso", "value": 400}),
        json!({"action": "shutter", "value": 8_000_000}),
        json!({"action": "wb_gains", "r": 2.0, "ge": 1.0, "go": 1.0, "b": 1.5}),
        json!({"action": "focus_mode", "value": "manual"}),
        json!({"action": "focus_distance", "value": 2.5}),
    ];
    let want: Vec<String> = bodies.iter().flat_map(|b| [control("a", b.clone()), control("b", b.clone())]).collect();
    assert_eq!(sent, want.iter().collect::<Vec<_>>());
}

#[test]
fn link_exposure_does_nothing_without_two_phones_or_a_state() {
    let mut r = rig();
    r.host.phone_state = Some(json!({"iso": 400, "shutter_ns": 8_000_000}));
    *r.store.frames.lock().unwrap() = vec![StreamFrame { source_id: "a".into(), name: "A".into(), frame: shot(RED, None) }];
    r.card("link-clicked", json!(null));
    assert!(!r.host.calls.iter().any(|c| c.starts_with("phone_control")), "one phone");
    r.show_frames(shot(RED, None), shot(BLUE, None));
    r.host.phone_state = None;
    r.card("link-clicked", json!(null));
    assert!(!r.host.calls.iter().any(|c| c.starts_with("phone_control")), "no state yet");
}

// ── Settings ──────────────────────────────────────────────────────────────

#[test]
fn the_settings_come_back_and_are_held_to_the_sliders() {
    let mut r = rig();
    r.plugin.set_config(&json!({"on": true, "look": "red_cyan", "depth": 1.26, "height": -40.0, "swap": true})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"on": true, "look": "red_cyan", "depth": 1.3, "height": -5.0, "swap": true}));
    r.plugin.set_config(&json!({"look": "nonsense", "depth": "far"})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"on": false, "look": "side_by_side", "depth": 0.0, "height": 0.0, "swap": false}));
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
    r.card("swap-toggled", json!(true));
    assert_eq!(r.plugin.get_config(), json!({"on": false, "look": "red_cyan", "depth": 5.0, "height": -1.5, "swap": true}));
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
    let props = r.props();
    assert_eq!((props["look-current"].clone(), props["depth-text"].clone(), props["height-text"].clone()), (json!(1), json!("+5.0%"), json!("-1.5%")));
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
