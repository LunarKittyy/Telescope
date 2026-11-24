//! The Composite plugin's tests. Frames are BGRA, so a colour reads as the BGR Python's do.

use super::*;
use crate::composite::{bgr, Tile, DEFAULT_BACKGROUND};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

const SECOND: u64 = 1_000_000_000;

/// The stream frames the host hands out, changed as a test goes.
#[derive(Default)]
struct Store {
    frames: Mutex<Vec<StreamFrame>>,
    /// replace_frame finds the stream has sent a newer frame.
    newer: AtomicBool,
    /// (source id, frame, number) handed to replace_frame.
    replaced: Mutex<Vec<(String, Frame, u64)>>,
}

impl FrameStore for Store {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        self.frames.lock().unwrap().clone()
    }
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        self.frames.lock().unwrap().iter().find(|f| Some(f.source_id.as_str()) == source_id).cloned()
    }
    fn replace_frame(&self, source_id: Option<&str>, frame: Frame, number: u64) -> bool {
        // Like StreamWorker's: what goes out next changes, stream_frame() keeps the stream's own frame
        if self.newer.load(Ordering::SeqCst) {
            return false;
        }
        self.replaced.lock().unwrap().push((source_id.unwrap_or("").to_string(), frame, number));
        true
    }
}

impl Store {
    fn set(&self, i: usize, sf: StreamFrame) {
        self.frames.lock().unwrap()[i] = sf;
    }

    fn last(&self) -> (String, Frame, u64) {
        self.replaced.lock().unwrap().last().cloned().expect("a replaced frame")
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

fn solid(colour: [u8; 3], (h, w): (u32, u32)) -> Frame {
    Frame::new(w, h, PixelFormat::Bgra, [colour[0], colour[1], colour[2], 255].repeat((w * h) as usize))
}

fn numbered(mut frame: Frame, number: u64, time_ns: u64) -> Frame {
    (frame.number, frame.time_ns) = (number, time_ns);
    frame
}

fn stream(sid: &str, name: &str, frame: Frame) -> StreamFrame {
    StreamFrame { source_id: sid.into(), name: name.into(), frame }
}

fn px(frame: &Frame, y: usize, x: usize) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    [frame.data[at], frame.data[at + 1], frame.data[at + 2]]
}

struct Rig {
    plugin: CompositePlugin,
    host: FakeHost,
    store: Arc<Store>,
}

fn rig() -> Rig {
    let store = Arc::new(Store::default());
    let mut host = FakeHost::new();
    host.frame_store = Some(store.clone());
    // what goes to the Telescope camera
    host.cameras = vec![(0, "a".into(), None, None)];
    host.canvas = (Some(320), Some(180));
    let mut plugin = CompositePlugin::new();
    plugin.setup(&mut host);
    plugin.on_stream_start(&mut host, &StreamInfo { source_id: "u".into(), url: "u".into(), session: 0 });
    Rig { plugin, host, store }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.plugin.shutdown(&mut self.host);
    }
}

impl Rig {
    fn two_streams(&mut self, b_age: f64) {
        let now = monotonic_ns();
        *self.store.frames.lock().unwrap() = vec![
            stream("a", "Phone A", numbered(solid([0, 0, 0], (90, 160)), 1, now)),
            stream("b", "Phone B", numbered(solid([0, 255, 0], (90, 160)), 2, now - (b_age * 1e9) as u64)),
        ];
        self.host.live = vec!["a".into(), "b".into()];
        self.plugin.set_layout(&mut self.host, Layout::of(vec![Tile::boxed("a", "Phone A", 0.0, 0.0, 0.5, 1.0), Tile::boxed("b", "Phone B", 0.5, 0.0, 0.5, 1.0)]));
        self.switch(true);
    }

    fn switch(&mut self, on: bool) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(on)] });
    }

    /// The frame as the plugin's step in source_id's stream hands it on.
    fn process(&mut self, frame: &Frame, source_id: &str) -> Frame {
        let Some(FrameStep::Cpu(mut step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        let mut out = frame.clone();
        step.process(&mut out, &Ctx(source_id.into()));
        out
    }

    fn opened(&mut self, source_id: &str, slot: u32, width: u32, height: u32) {
        self.plugin.on_event(&mut self.host, &Event::CameraOpened { source_id: source_id.into(), slot, width, height });
    }

    fn set_main(&self, frame: &Frame, at: u64) {
        self.plugin.shared.hub().main_frame = Some((frame.clone(), at));
    }

    fn draw_stalled(&self, now: u64) -> bool {
        self.plugin.shared.draw_stalled(now)
    }

    fn current_text(&self) -> String {
        let props = self.plugin.card_props();
        let i = props["layout-current"].as_i64().unwrap();
        if i < 0 { String::new() } else { props["layout-items"][i as usize].as_str().unwrap().to_string() }
    }
}

fn same(a: &Frame, b: &Frame) -> bool {
    Arc::ptr_eq(&a.data, &b.data)
}

#[test]
fn other_streams_pass_while_the_picture_is_drawing() {
    let mut r = rig();
    r.two_streams(0.0);
    let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    let (tx, rx) = std::sync::mpsc::channel();
    // a drawing holds the hub for as long as it takes
    let held = r.plugin.shared.hub();
    std::thread::spawn(move || {
        let _ = tx.send((step.touch(&Ctx("a".into())), step.touch(&Ctx("b".into()))));
    });
    let (main, other) = rx.recv_timeout(Duration::from_secs(5)).expect("touch waited for the hub");
    drop(held);
    assert!(main == Touch::Changes && other == Touch::Nothing);
    r.switch(false);
    let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    assert!(step.touch(&Ctx("a".into())) == Touch::Nothing);
}

#[test]
fn the_picture_goes_out_on_the_main_stream_only() {
    let mut r = rig();
    r.two_streams(0.0);
    let frame = solid([255, 0, 0], (180, 320));
    assert!(same(&r.process(&frame, "b"), &frame));
    let out = r.process(&frame, "a");
    assert!((out.width, out.height) == (320, 180) && px(&out, 90, 40) == [255, 0, 0] && px(&out, 90, 280) == [0, 255, 0]);
    assert!(r.plugin.note.contains(&r.plugin.camera_label), "{}", r.plugin.note);
}

#[test]
fn the_picture_goes_out_on_the_telescope_camera_at_its_size() {
    let mut r = rig();
    r.two_streams(0.0);
    // Auto
    r.host.canvas = (None, None);
    // a phone held upright on Telescope
    r.host.cameras = vec![(0, "a".into(), Some(90), Some(160))];
    r.opened("a", 0, 90, 160);
    assert_eq!(r.plugin.size(), (90, 160));
    let out = r.process(&solid([255, 0, 0], (160, 90)), "a");
    assert_eq!((out.width, out.height), (90, 160));
    // b is made the main stream (the mic moves) but still goes to its own camera: the picture stays on a's
    r.opened("b", 1, 1920, 1080);
    assert_eq!(r.plugin.size(), (90, 160));
    // a stops and b is the only stream: nothing goes to Telescope, so nothing is drawn
    r.host.cameras.clear();
    r.plugin.on_stream_stop(&mut r.host);
    let frame = solid([0, 0, 255], (180, 320));
    assert!(same(&r.process(&frame, "b"), &frame) && r.plugin.note.contains("no stream is running there"));
    // b restarts onto Telescope, in landscape: the picture is drawn there at its new size
    r.host.cameras = vec![(0, "b".into(), Some(320), Some(180))];
    r.opened("b", 0, 320, 180);
    let out = r.process(&frame, "b");
    assert!(r.plugin.size() == (320, 180) && (out.width, out.height) == (320, 180));
}

#[test]
fn the_clock_rests_while_no_stream_goes_to_the_telescope_camera() {
    let mut r = rig();
    r.host.cameras.clear();
    r.plugin.on_stream_stop(&mut r.host);
    // switched on
    r.two_streams(0.0);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.plugin.shared.hub().ticks, 0);
    r.host.cameras = vec![(0, "a".into(), Some(160), Some(90))];
    r.opened("a", 0, 160, 90);
    let deadline = Instant::now() + Duration::from_secs(2);
    while r.plugin.shared.hub().ticks == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // a stream to keep moving: it ticks again
    assert!(r.plugin.shared.hub().ticks > 0);
}

#[test]
fn the_clock_leaves_a_frame_that_reached_the_step_alone() {
    let mut r = rig();
    r.plugin.shutdown(&mut r.host);
    r.two_streams(0.0);
    let main = solid([255, 0, 0], (90, 160));
    let t0 = monotonic_ns().saturating_sub(SECOND);
    let sent = numbered(r.process(&main, "a"), 1, t0);
    r.store.set(0, stream("a", "Phone A", sent));
    r.set_main(&main, t0);
    // the frame behind it is waiting for the lock: a picture drawn now would only make it wait longer
    let Some(FrameStep::Cpu(step)) = r.plugin.frame_step() else { panic!("a CPU step") };
    let seen = monotonic_ns();
    assert_eq!(step.touch(&Ctx("a".into())), Touch::Changes);
    assert!(!r.draw_stalled(seen + SECOND * 3 / 100));
    assert!(r.draw_stalled(seen + SECOND / 5));
}

#[test]
fn while_the_telescope_stream_stalls_the_others_keep_moving() {
    let mut r = rig();
    // the real clock would draw first on a slow machine; this test moves the time itself
    r.plugin.shutdown(&mut r.host);
    r.two_streams(0.0);
    let main = solid([255, 0, 0], (90, 160));
    let t0 = monotonic_ns();
    // the picture a sent out
    let sent = numbered(r.process(&main, "a"), 1, t0);
    r.store.set(0, stream("a", "Phone A", sent));
    r.set_main(&main, t0);
    // a is on time: its own frame steps draw the picture
    assert!(!r.draw_stalled(t0 + SECOND * 3 / 100));
    assert!(r.draw_stalled(t0 + SECOND / 10));
    let (sid, out, number) = r.store.last();
    assert!((sid.as_str(), number) == ("a", 1) && px(&out, 90, 40) == [255, 0, 0] && px(&out, 90, 280) == [0, 255, 0]);
    // b hasn't sent a new frame either: nothing to draw again
    assert!(!r.draw_stalled(t0 + SECOND * 15 / 100));
    r.store.set(1, stream("b", "Phone B", numbered(solid([0, 255, 255], (90, 160)), 3, t0 + SECOND * 15 / 100)));
    // a sent a frame while this was drawn: that one wins
    r.store.newer.store(true, Ordering::SeqCst);
    assert!(!r.draw_stalled(t0 + SECOND / 5));
    r.store.newer.store(false, Ordering::SeqCst);
    assert!(r.draw_stalled(t0 + SECOND / 5) && px(&r.store.last().1, 90, 280) == [0, 255, 255]);
    // a gone for longer than STALE_S leaves the picture, as any other stream does
    let later = t0 + STALE_NS + SECOND;
    r.store.set(1, stream("b", "Phone B", numbered(solid([0, 255, 0], (90, 160)), 4, later)));
    assert!(r.draw_stalled(later));
    assert_eq!(px(&r.store.last().1, 90, 40), bgr(DEFAULT_BACKGROUND));
}

#[test]
fn a_covered_stream_stays_covered_while_it_stalls() {
    let mut r = rig();
    r.two_streams(0.0);
    let main = solid([255, 0, 0], (90, 160));
    let t0 = monotonic_ns();
    let picture = r.process(&main, "a");
    r.set_main(&main, t0);
    // what a sent out: the picture, uncovered
    r.store.set(0, stream("a", "Phone A", numbered(picture.clone(), 1, t0)));
    assert!(r.draw_stalled(t0 + SECOND / 10));
    r.store.replaced.lock().unwrap().clear();
    r.process(&main, "a");
    r.set_main(&main, t0);
    // the wait screen covered it
    let covered = Frame::new(picture.width, picture.height, picture.format, vec![0; picture.data.len()]);
    r.store.set(0, stream("a", "Phone A", numbered(covered, 1, t0)));
    r.store.set(1, stream("b", "Phone B", numbered(solid([0, 255, 255], (90, 160)), 3, t0 + SECOND / 10)));
    assert!(!r.draw_stalled(t0 + SECOND / 5) && r.store.replaced.lock().unwrap().is_empty());
}

#[test]
fn the_stall_clock_draws_nothing_without_another_stream_or_a_frame() {
    let mut r = rig();
    r.two_streams(STALE_S + 1.0);
    let t0 = monotonic_ns();
    r.set_main(&solid([255, 0, 0], (90, 160)), t0);
    // b dropped: a alone goes through as it is, frozen or not
    assert!(!r.draw_stalled(t0 + SECOND / 2));
    r.two_streams(0.0);
    // a hasn't sent a frame since the picture was switched on
    r.plugin.shared.hub().main_frame = None;
    assert!(!r.draw_stalled(t0 + SECOND / 2));
    r.set_main(&solid([255, 0, 0], (90, 160)), t0);
    r.switch(false);
    assert!(!r.draw_stalled(t0 + SECOND / 2) && r.store.replaced.lock().unwrap().is_empty());
}

#[test]
fn with_one_stream_running_or_switched_off_frames_go_through() {
    let mut r = rig();
    // b dropped a while ago
    r.two_streams(STALE_S + 1.0);
    let frame = solid([255, 0, 0], (180, 320));
    assert!(same(&r.process(&frame, "a"), &frame));
    r.two_streams(0.0);
    r.switch(false);
    assert!(same(&r.process(&frame, "a"), &frame));
}

#[test]
fn layouts_are_saved_picked_renamed_and_deleted() {
    let mut r = rig();
    r.two_streams(0.0);
    assert_eq!(r.plugin.save_layout(&mut r.host, "  Side by side "), "Side by side");
    assert!(r.plugin.saved_match() == "Side by side" && r.current_text() == "Side by side");
    r.plugin.set_layout(&mut r.host, Layout::named("Side by side", vec![Tile::new("a")]));
    // changed since
    assert!(r.plugin.saved_match().is_empty() && r.plugin.card_props()["layout-current"] == -1);
    r.plugin.save_layout(&mut r.host, "Just one");
    assert!(r.plugin.pick(&mut r.host, "Side by side") && r.plugin.layout().tiles.len() == 2);
    r.plugin.rename_layout(&mut r.host, "Side by side", "Two");
    assert!(r.plugin.names() == ["Two", "Just one"] && r.plugin.layout().name == "Two");
    r.plugin.delete_layout(&mut r.host, "Just one");
    assert!(r.plugin.names() == ["Two"] && !r.plugin.pick(&mut r.host, "Just one"));
}

#[test]
fn settings_and_presets_bring_the_picture_back() {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.save_layout(&mut r.host, "Two");
    let saved = r.plugin.get_config();
    let mut other = rig();
    other.plugin.set_config(&saved).unwrap();
    other.plugin.on_event(&mut other.host, &Event::StreamsChanged { count: 2 });
    assert!(other.plugin.get_config() == saved && other.plugin.card_props()["on"] == true);
    // what a preset saved, and a layout list that's changed since
    let mut preset = saved.clone();
    preset["layouts"] = json!([]);
    r.plugin.set_layout(&mut r.host, Layout::default());
    r.switch(false);
    r.plugin.apply_preset(&mut r.host, &preset).unwrap();
    assert!(r.plugin.layout() == other.plugin.layout() && r.plugin.card_props()["on"] == true);
    // a preset leaves the saved layouts alone
    assert_eq!(r.plugin.names(), ["Two"]);
}

#[test]
fn a_key_picks_a_saved_layout() {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.save_layout(&mut r.host, "Two");
    r.plugin.set_layout(&mut r.host, Layout::named("One", vec![Tile::new("a")]));
    r.plugin.save_layout(&mut r.host, "One");
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["composite.on", "composite.layout"]);
    let said = r.plugin.run_action(&mut r.host, "composite.layout", &json!({"op": "set", "option": "Two"}));
    assert!(said.is_some() && r.plugin.layout().name == "Two");
    r.plugin.run_action(&mut r.host, "composite.on", &json!({"op": "toggle"}));
    assert_eq!(r.plugin.get_config()["on"], false);
}

#[test]
fn the_card_says_where_the_picture_goes() {
    let mut r = rig();
    r.switch(true);
    assert!(r.plugin.note.starts_with("Nothing's in the picture yet"));
    r.two_streams(0.0);
    assert_eq!(r.plugin.note, format!("Outputting on {}.", r.plugin.camera_label));
    r.host.live = vec!["a".into()];
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 1 });
    assert_eq!(r.plugin.note, "Active once two streams are running.");
    r.switch(false);
    assert!(r.plugin.note.is_empty());
    assert_eq!(r.plugin.diagnostics(), [("One picture".to_string(), "off".to_string())]);
}

#[path = "composite_editor_tests.rs"]
mod editor_tests;

#[test]
fn the_clock_sleeps_until_the_newest_frame_would_stall() {
    let period = SECOND / 30;
    // no frame yet or a stalled one: a frame time
    assert_eq!(Shared::clock_wait(None, 10 * period, period), period);
    assert_eq!(Shared::clock_wait(Some(0), 3 * period, period), period);
    // a fresh one: until it is two frame times old, when draw_stalled takes over
    assert_eq!(Shared::clock_wait(Some(0), 0, period), 2 * period);
    assert_eq!(Shared::clock_wait(Some(0), period, period), period);
    // just before it counts as stalled: a nanosecond more
    assert_eq!(Shared::clock_wait(Some(0), 2 * period - 1, period), 1);
    // a frame stamped in the future is new, and never makes the clock sleep for long
    assert_eq!(Shared::clock_wait(Some(10 * period), 0, period), 2 * period);
}

#[test]
fn switching_the_target_stream_forgets_when_the_old_one_was_fed() {
    let mut r = rig();
    r.two_streams(0.0);
    let frame = solid([255, 0, 0], (90, 160));
    r.process(&frame, "a");
    assert!(r.plugin.shared.fed_at.load(Ordering::Relaxed) > 0, "a step stamps it");
    r.host.cameras = vec![(0, "b".into(), None, None)];
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert_eq!(r.plugin.shared.hub().target.as_deref(), Some("b"));
    assert_eq!(r.plugin.shared.fed_at.load(Ordering::Relaxed), 0, "the new target starts unstamped");
}

#[test]
fn a_slow_main_stream_does_not_hold_back_a_faster_one() {
    let mut r = rig();
    r.plugin.shutdown(&mut r.host);
    r.two_streams(0.0);
    let main = solid([255, 0, 0], (90, 160));
    let period = SECOND / 30;
    let handed = r.process(&main, "a");
    // the step stamps the time it handed the frame on, so the clock counts from there
    let t0 = monotonic_ns();
    let sent = numbered(handed, 1, t0);
    r.store.set(0, stream("a", "Phone A", sent));
    r.set_main(&main, t0);
    // 12 fps main: the clock wakes when its frame is two periods old, before the next one is due
    let wake = t0 + Shared::clock_wait(Some(t0), t0, period);
    r.store.set(1, stream("b", "Phone B", numbered(solid([0, 255, 255], (90, 160)), 3, wake)));
    assert!(r.draw_stalled(wake) && px(&r.store.last().1, 90, 280) == [0, 255, 255]);
    // and it keeps waking each period until the main's next frame arrives
    assert_eq!(Shared::clock_wait(Some(t0), wake, period), period);
    r.store.set(1, stream("b", "Phone B", numbered(solid([255, 255, 0], (90, 160)), 4, wake + period)));
    assert!(r.draw_stalled(wake + period) && px(&r.store.last().1, 90, 280) == [255, 255, 0]);
}

#[test]
#[ignore = "timing: how often the clock wakes while frames arrive"]
fn bench_clock_wakes_while_frames_arrive() {
    let mut r = rig();
    r.two_streams(0.0);
    let frame = solid([255, 0, 0], (90, 160));
    let before = r.plugin.shared.hub().ticks;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        r.process(&frame, "a");
        std::thread::sleep(Duration::from_millis(33));
    }
    let wakes = r.plugin.shared.hub().ticks - before;
    println!("clock wakes per second at 30 fps: {:.1}", wakes as f64 / start.elapsed().as_secs_f64());
}

#[test]
fn the_one_picture_needs_frames_only_while_it_is_on() {
    let mut rig = rig();
    assert!(rig.host.frames_wanted.is_empty());
    rig.two_streams(0.0);
    assert!(rig.host.frames_wanted.contains(NAME), "it draws from other streams' frames");
    rig.switch(false);
    assert!(rig.host.frames_wanted.is_empty());
    rig.switch(true);
    rig.plugin.shutdown(&mut rig.host);
    assert!(rig.host.frames_wanted.is_empty());
}

#[test]
fn reset_clears_the_picture_and_keeps_the_switch_and_saved_layouts() {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.save_layout(&mut r.host, "Two");
    assert!(r.plugin.reset_shown());
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "reset-clicked".into(), args: vec![] });
    assert!(r.plugin.layout().tiles.is_empty());
    assert_eq!(r.plugin.get_config()["on"], true);
    assert_eq!(r.plugin.names(), ["Two"]);
    assert!(!r.plugin.reset_shown());
    assert!(r.host.calls.iter().any(|c| c.starts_with("set_ui_props card") && c.contains("\"reset-shown\":false")));
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
}

#[test]
fn the_auto_director_works_only_while_on_and_switches_layouts_on_hands() {
    let mut r = rig();
    r.two_streams(0.0);
    let two = |big: &str, small: &str| Layout::of(vec![Tile::boxed(big, "", 0.0, 0.0, 0.7, 1.0), Tile::boxed(small, "", 0.7, 0.0, 0.3, 0.4)]);
    r.plugin.set_layout(&mut r.host, two("a", "b"));
    r.plugin.save_layout(&mut r.host, "A big");
    r.plugin.set_layout(&mut r.host, two("b", "a"));
    r.plugin.save_layout(&mut r.host, "B big");
    r.plugin.pick(&mut r.host, "A big");
    assert_eq!(r.plugin.card_props()["director"], false, "off by default");
    assert!(!r.host.calls.iter().any(|c| c == "want_people composite true"));
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "director-toggled".into(), args: vec![json!(true)] });
    assert!(r.host.calls.iter().any(|c| c == "want_people composite true"), "asks for people on every stream");
    assert_eq!(r.plugin.get_config()["director"], true);
    // hands on b for long enough: B big goes on; the pick at the start is long past
    r.plugin.director.picked(0.0, "A big");
    for t in 0..8 {
        r.plugin.director.hear(10.0 + t as f64 * 0.5, "b", true);
        r.plugin.direct(&mut r.host, 10.0 + t as f64 * 0.5);
    }
    assert_eq!(r.plugin.layout().name, "B big");
    // the director's own switch is not a hand pick: A big stays home
    assert_eq!(r.plugin.director.home(), "A big");
    // off: nothing asked for any more
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "director-toggled".into(), args: vec![json!(false)] });
    assert_eq!(r.host.calls.iter().rev().find(|c| c.starts_with("want_people composite")).map(String::as_str), Some("want_people composite false"));
    r.plugin.direct(&mut r.host, 100.0);
    assert_eq!(r.plugin.layout().name, "A big", "off puts the layout picked by hand back, and it changes nothing after that");
}

/// A stand-in picture: a flat colour with a few filled shapes, (x, y, w, h, colour) as fractions.
fn scene(background: [u8; 3], shapes: &[(f64, f64, f64, f64, [u8; 3])]) -> Frame {
    let (w, h) = (640usize, 360usize);
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            let c = shapes.iter().rev().find(|(sx, sy, sw, sh, _)| fx >= *sx && fx < sx + sw && fy >= *sy && fy < sy + sh).map_or(background, |s| s.4);
            data.extend([c[0], c[1], c[2], 255]);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Bgra, data)
}

/// Not part of the suite: writes the layouts the director picks over a scripted minute as PPM files into TELESCOPE_DIRECTOR_OUT (a folder),
/// drawn by the real compositor on stand-in pictures, with nothing on screen. `cargo nextest run -p telescope-effects --run-ignored only render_the_director`.
#[test]
#[ignore]
fn render_the_director_over_a_scripted_minute() {
    let Ok(out) = std::env::var("TELESCOPE_DIRECTOR_OUT") else { return };
    let mut r = rig();
    r.two_streams(0.0);
    let face = scene([40, 60, 90], &[(0.38, 0.12, 0.24, 0.42, [150, 190, 230]), (0.22, 0.58, 0.56, 0.42, [200, 90, 60])]);
    let desk = scene([90, 70, 50], &[(0.15, 0.2, 0.7, 0.6, [60, 140, 90]), (0.3, 0.35, 0.12, 0.2, [240, 230, 220])]);
    let desk_hands = scene([90, 70, 50], &[(0.15, 0.2, 0.7, 0.6, [60, 140, 90]), (0.3, 0.35, 0.12, 0.2, [240, 230, 220]), (0.45, 0.4, 0.25, 0.3, [190, 150, 130])]);
    let now = monotonic_ns();
    let two = |big: &str, small: &str| Layout::of(vec![Tile::boxed(big, "", 0.0, 0.0, 0.72, 1.0), Tile::boxed(small, "", 0.72, 0.0, 0.28, 0.4)]);
    r.plugin.set_layout(&mut r.host, two("a", "b"));
    r.plugin.save_layout(&mut r.host, "Face big");
    r.plugin.set_layout(&mut r.host, two("b", "a"));
    r.plugin.save_layout(&mut r.host, "Desk big");
    r.plugin.pick(&mut r.host, "Face big");
    r.plugin.director.picked(0.0, "Face big");
    r.plugin.director_on = true;
    std::fs::create_dir_all(&out).unwrap();
    let mut log = String::new();
    // hands on the desk from 10 s to 25 s, then gone
    for step in 0..=70 {
        let t = 8.0 + step as f64 * 0.5;
        let hands = (10.0..25.0).contains(&t);
        r.plugin.director.hear(t, "b", hands);
        let before = r.plugin.layout().name.clone();
        r.plugin.direct(&mut r.host, t);
        let name = r.plugin.layout().name.clone();
        if name != before || step == 0 {
            log += &format!("{t:5.1} s: {name}\n");
            let mut frames = crate::compositor::Frames::new();
            frames.insert("a".into(), (numbered(face.clone(), 1, now), Some(1)));
            frames.insert("b".into(), (numbered(if hands { desk_hands.clone() } else { desk.clone() }, 2, now), Some(2)));
            let mut compositor = crate::compositor::Compositor::new(no_pictures());
            compositor.prepare(r.plugin.layout(), (960, 540));
            let picture = compositor.render(&frames, PixelFormat::Bgra).expect("a picture");
            let mut ppm = format!("P6\n{} {}\n255\n", picture.width, picture.height).into_bytes();
            picture.data.chunks(4).for_each(|p| ppm.extend([p[2], p[1], p[0]]));
            std::fs::write(format!("{out}/{:04.1}-{}.ppm", t, name.replace(' ', "-")), ppm).unwrap();
        }
    }
    std::fs::write(format!("{out}/log.txt"), log).unwrap();
}

#[test]
fn the_auto_director_saves_the_layout_picked_by_hand_follows_a_rename_and_puts_it_back_when_off() {
    let mut r = rig();
    r.two_streams(0.0);
    let two = |big: &str, small: &str| Layout::of(vec![Tile::boxed(big, "", 0.0, 0.0, 0.7, 1.0), Tile::boxed(small, "", 0.7, 0.0, 0.3, 0.4)]);
    r.plugin.set_layout(&mut r.host, two("a", "b"));
    r.plugin.save_layout(&mut r.host, "A big");
    r.plugin.set_layout(&mut r.host, two("b", "a"));
    r.plugin.save_layout(&mut r.host, "B big");
    r.plugin.pick(&mut r.host, "A big");
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "director-toggled".into(), args: vec![json!(true)] });
    r.plugin.director.picked(0.0, "A big");
    for t in 0..8 {
        r.plugin.director.hear(10.0 + t as f64 * 0.5, "b", true);
        r.plugin.direct(&mut r.host, 10.0 + t as f64 * 0.5);
    }
    assert_eq!(r.plugin.layout().name, "B big");
    assert_eq!(r.plugin.get_config()["layout"]["name"], "A big", "a cut isn't what is saved");
    // renaming home keeps the director working
    r.plugin.rename_layout(&mut r.host, "A big", "Talking");
    assert_eq!(r.plugin.director.home(), "Talking");
    // off while cut: the picture is back on the one picked by hand
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "director-toggled".into(), args: vec![json!(false)] });
    assert_eq!(r.plugin.layout().name, "Talking");
    // a card with one layout left can still switch it off
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "director-toggled".into(), args: vec![json!(true)] });
    r.plugin.delete_layout(&mut r.host, "B big");
    assert_eq!(r.plugin.card_props()["director-enabled"], true);
    assert_eq!(r.host.calls.iter().rev().find(|c| c.starts_with("want_people composite")).map(String::as_str), Some("want_people composite false"), "no pose work without two layouts");
}

#[test]
fn the_declared_settings_round_trip() {
    telescope_core::testing::settings_round_trip(&mut CompositePlugin::new(), &mut telescope_core::testing::FakeHost::new());
}

#[test]
fn a_preset_saves_the_picture_but_not_the_saved_layouts() {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.save_layout(&mut r.host, "Two");
    let saved = r.plugin.preset_config();
    assert!(saved.get("layouts").is_none() && saved["on"] == true && saved["layout"]["name"] == "Two", "{saved}");
    // settings still carry the layouts: they are what the app keeps between runs
    assert_eq!(r.plugin.get_config()["layouts"].as_array().map(Vec::len), Some(1));
    // bringing a preset back leaves the layouts as they are
    let mut other = rig();
    other.plugin.save_layout(&mut other.host, "Mine");
    other.plugin.apply_preset(&mut other.host, &saved).unwrap();
    assert_eq!(other.plugin.names(), ["Mine"]);
}

// ── Transitions ───────────────────────────────────────────────────────────

/// Whether the transition is still going `after_ms` from now. It starts with the first picture drawn, so one is drawn first.
fn goes(r: &Rig, after_ms: u64) -> bool {
    let mut hub = r.plugin.shared.hub();
    hub.compositor.render_at(&Frames::new(), PixelFormat::Bgra, STARTED);
    hub.compositor.transitioning(STARTED + after_ms * 1_000_000)
}

/// When the pictures of these tests are drawn.
const STARTED: u64 = 1_000_000_000;

/// A rig with a saved layout "Big" that differs from the one on.
fn with_saved_layout() -> Rig {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile::boxed("a", "Phone A", 0.0, 0.0, 1.0, 1.0), Tile::boxed("b", "Phone B", 0.7, 0.7, 0.25, 0.25)]));
    r.plugin.save_layout(&mut r.host, "Big");
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile::boxed("a", "Phone A", 0.0, 0.0, 0.5, 1.0), Tile::boxed("b", "Phone B", 0.5, 0.0, 0.5, 1.0)]));
    r
}

#[test]
fn a_picked_layout_goes_there_the_way_the_transition_says_and_an_edit_takes_effect_at_once() {
    let mut r = with_saved_layout();
    assert!(!goes(&r, 0), "an edit in the editor is not a transition");
    assert!(r.plugin.pick(&mut r.host, "Big"));
    assert!(goes(&r, 0) && goes(&r, 400) && !goes(&r, 1500));
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile::boxed("a", "Phone A", 0.0, 0.0, 0.5, 1.0), Tile::boxed("b", "Phone B", 0.5, 0.0, 0.5, 1.0)]));
    assert!(goes(&r, 400), "an edit mid-way is gone to on the way, not jumped to");
}

#[test]
fn a_stream_joining_in_the_middle_of_a_transition_does_not_end_it() {
    let mut r = with_saved_layout();
    r.plugin.pick(&mut r.host, "Big");
    assert!(goes(&r, 100));
    // what follow_camera does when the streams change
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 2 });
    assert!(goes(&r, 200), "it goes on");
}

#[test]
fn a_preset_without_a_transition_leaves_the_cards_alone_and_one_with_its_own_uses_it() {
    let mut r = with_saved_layout();
    let ui = |r: &mut Rig, callback: &str, arg: f64| r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![json!(arg)] });
    ui(&mut r, "transition-picked", TransitionKind::ALL.iter().position(|k| *k == TransitionKind::Slide).unwrap() as f64);
    let old = json!({"on": true, "layout": {"name": "Big", "tiles": [{"source_id": "a", "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0}]}});
    r.plugin.apply_preset(&mut r.host, &old).unwrap();
    assert_eq!(r.plugin.get_config()["transition"]["kind"], "slide", "an older preset has none");
    let mut own = old.clone();
    own["transition"] = json!({"kind": "wipe", "ms": 400, "ease": "even"});
    r.plugin.apply_preset(&mut r.host, &own).unwrap();
    assert_eq!(r.plugin.get_config()["transition"], json!({"kind": "wipe", "ms": 400, "ease": "even"}));
}

#[test]
fn reduced_motion_fades_briefly_and_off_cuts() {
    let mut r = with_saved_layout();
    r.host.reduced_motion = (true, false);
    r.plugin.pick(&mut r.host, "Big");
    assert!(goes(&r, 100) && !goes(&r, 400), "a short fade");
    let mut r = with_saved_layout();
    r.host.reduced_motion = (false, true);
    r.plugin.pick(&mut r.host, "Big");
    assert!(!goes(&r, 0), "a cut");
}

#[test]
fn the_transition_is_set_from_the_card_saved_and_clamped() {
    let mut r = rig();
    let ui = |r: &mut Rig, callback: &str, arg: f64| r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![json!(arg)] });
    assert_eq!(r.plugin.get_config()["transition"], json!({"kind": "glide", "ms": 600, "ease": "springy"}));
    ui(&mut r, "transition-picked", TransitionKind::ALL.iter().position(|k| *k == TransitionKind::Slide).unwrap() as f64);
    ui(&mut r, "ease-picked", 2.0);
    ui(&mut r, "transition-length-moved", 99999.0);
    assert_eq!(r.plugin.get_config()["transition"], json!({"kind": "slide", "ms": 3000, "ease": "bouncy"}));
    ui(&mut r, "transition-picked", 99.0);
    assert_eq!(r.plugin.get_config()["transition"]["kind"], "slide", "an unknown pick changes nothing");
    let saved = r.plugin.preset_config();
    let mut other = rig();
    other.plugin.apply_preset(&mut other.host, &saved).unwrap();
    assert_eq!(other.plugin.get_config()["transition"], saved["transition"], "a preset carries the transition");
}

#[test]
fn a_transition_keeps_being_drawn_while_the_telescope_stream_stalls() {
    let mut r = with_saved_layout();
    r.plugin.shutdown(&mut r.host);
    let main = solid([255, 0, 0], (90, 160));
    let t0 = monotonic_ns();
    let sent = numbered(r.process(&main, "a"), 1, t0);
    r.store.set(0, stream("a", "Phone A", sent));
    r.set_main(&main, t0);
    r.plugin.pick(&mut r.host, "Big");
    // no new frame from anyone, yet each tick draws the next picture of the transition
    assert!(r.draw_stalled(t0 + SECOND / 5));
    assert!(r.draw_stalled(t0 + SECOND * 3 / 10), "the same frames, drawn again for the transition");
}

#[test]
fn a_preset_saved_with_keep_what_is_on_leaves_the_current_layout_alone() {
    let mut r = rig();
    r.two_streams(0.0);
    r.plugin.save_layout(&mut r.host, "Two");
    let options = r.plugin.preset_options();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].items[0].0, "layout", "a new preset brings its layout, as before");
    assert!(r.plugin.preset_config_with(&[("layout".into(), "layout".into())])["layout"]["name"] == "Two");
    let kept = r.plugin.preset_config_with(&[("layout".into(), "keep".into())]);
    assert!(kept.get("layout").is_none() && kept["on"] == true, "{kept}");
    let mut other = rig();
    other.two_streams(0.0);
    other.plugin.save_layout(&mut other.host, "Mine");
    let before = other.plugin.get_config()["layout"].clone();
    other.plugin.apply_preset(&mut other.host, &kept).unwrap();
    assert_eq!(other.plugin.get_config()["layout"], before);
    assert_eq!(other.plugin.get_config()["on"], true);
}

// ── Cut out: telling the plugins that cut a person out ──

fn cut_out_events(r: &mut Rig) -> Vec<(String, bool)> {
    let told = r.host.events.iter().filter_map(|e| match e {
        Event::CutOutWantedBy { owner, source_id, on } if owner == NAME => Some((source_id.clone(), *on)),
        _ => None,
    }).collect();
    r.host.events.clear();
    told
}

fn with_cut_out(r: &mut Rig, a: bool, b: bool) {
    let tiles = vec![Tile { cut_out: a, ..Tile::boxed("a", "Phone A", 0.0, 0.0, 0.5, 1.0) }, Tile { cut_out: b, ..Tile::boxed("b", "Phone B", 0.5, 0.0, 0.5, 1.0) }];
    r.plugin.set_layout(&mut r.host, Layout::of(tiles));
}

fn told(list: &[(&str, bool)]) -> Vec<(String, bool)> {
    list.iter().map(|(s, on)| (s.to_string(), *on)).collect()
}

#[test]
fn a_tiles_cut_out_turning_on_and_off_is_told_once_to_its_stream() {
    let mut r = rig();
    r.two_streams(0.0);
    cut_out_events(&mut r);
    with_cut_out(&mut r, false, true);
    assert_eq!(cut_out_events(&mut r), told(&[("b", true)]));
    // the same picture again, or another change to it, says nothing new
    with_cut_out(&mut r, false, true);
    r.plugin.set_layout(&mut r.host, Layout { background: "#ff0000".into(), ..r.plugin.layout().clone() });
    assert!(cut_out_events(&mut r).is_empty());
    with_cut_out(&mut r, true, false);
    assert_eq!(cut_out_events(&mut r), told(&[("b", false), ("a", true)]));
    with_cut_out(&mut r, false, false);
    assert_eq!(cut_out_events(&mut r), told(&[("a", false)]));
}

#[test]
fn removing_a_tile_or_switching_the_picture_off_lets_go_of_its_cut_out() {
    let mut r = rig();
    r.two_streams(0.0);
    with_cut_out(&mut r, true, true);
    cut_out_events(&mut r);
    r.plugin.set_layout(&mut r.host, Layout::of(vec![Tile { cut_out: true, ..Tile::boxed("a", "Phone A", 0.0, 0.0, 1.0, 1.0) }]));
    assert_eq!(cut_out_events(&mut r), told(&[("b", false)]));
    with_cut_out(&mut r, true, true);
    cut_out_events(&mut r);
    r.switch(false);
    assert_eq!(cut_out_events(&mut r), told(&[("a", false), ("b", false)]));
    // while it is off a tile asking for a cut-out is nobody's business
    with_cut_out(&mut r, true, false);
    assert!(cut_out_events(&mut r).is_empty());
    r.switch(true);
    assert_eq!(cut_out_events(&mut r), told(&[("a", true)]));
    // and a plugin that stops lets go of what it asked for
    r.plugin.shutdown(&mut r.host);
    assert_eq!(cut_out_events(&mut r), told(&[("a", false)]));
}

#[test]
fn a_fading_out_tile_keeps_its_cut_out_until_the_transition_has_ended() {
    let mut r = with_saved_layout();
    with_cut_out(&mut r, true, false);
    cut_out_events(&mut r);
    // the picked layout has no cut-out and its picture is drawn from the old one for a while
    assert!(r.plugin.pick(&mut r.host, "Big"));
    assert!(cut_out_events(&mut r).is_empty(), "not let go while the old picture is still drawn");
    assert!(r.host.scheduled.iter().any(|(_, t)| t == "cut-outs"));
    let timer = Event::TimerFired { owner: NAME.into(), token: "cut-outs".into() };
    assert!(goes(&r, 100));
    r.plugin.on_event(&mut r.host, &timer);
    assert!(cut_out_events(&mut r).is_empty(), "still going at the next look");
    // drawn to its end
    r.plugin.shared.hub().compositor.render_at(&Frames::new(), PixelFormat::Bgra, STARTED + 5 * SECOND);
    r.plugin.on_event(&mut r.host, &timer);
    assert_eq!(cut_out_events(&mut r), told(&[("a", false)]));
    assert!(!r.host.scheduled.iter().any(|(_, t)| t == "cut-outs"), "nothing more to look for");
}

#[test]
fn every_cut_out_is_told_again_when_the_plugins_are_ready_and_when_the_shown_stream_changes() {
    let mut r = rig();
    r.two_streams(0.0);
    with_cut_out(&mut r, true, true);
    cut_out_events(&mut r);
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(cut_out_events(&mut r), told(&[("a", true), ("b", true)]));
    r.plugin.on_event(&mut r.host, &Event::SourceShown { source_id: "b".into() });
    assert_eq!(cut_out_events(&mut r), told(&[("a", true), ("b", true)]));
    // with nothing wanted there is nothing to say
    with_cut_out(&mut r, false, false);
    cut_out_events(&mut r);
    r.plugin.on_plugins_ready(&mut r.host);
    r.plugin.on_event(&mut r.host, &Event::SourceShown { source_id: "a".into() });
    assert!(cut_out_events(&mut r).is_empty());
}

#[test]
fn a_layout_from_settings_or_a_preset_with_cut_outs_asks_for_them() {
    let mut r = rig();
    r.two_streams(0.0);
    with_cut_out(&mut r, false, true);
    let saved = r.plugin.get_config();
    let preset = r.plugin.preset_config();
    assert_eq!(preset["layout"]["tiles"][1]["cut_out"], json!(true));
    assert!(preset["layout"]["tiles"][0].get("cut_out").is_none());
    // a start that reads its settings: the plugins hear it once they are all there
    let mut other = rig();
    other.plugin.set_config(&saved).unwrap();
    other.plugin.on_plugins_ready(&mut other.host);
    assert_eq!(cut_out_events(&mut other), told(&[("b", true)]));
    // a preset brings the tile's cut-out back, and a layout without any lets go
    with_cut_out(&mut r, false, false);
    cut_out_events(&mut r);
    r.plugin.apply_preset(&mut r.host, &preset).unwrap();
    assert!(r.plugin.layout().tiles[1].cut_out && !r.plugin.layout().tiles[0].cut_out);
    assert_eq!(cut_out_events(&mut r), told(&[("b", true)]));
    // "Leave the layout" keeps the current tiles, cut-outs included
    let keep = r.plugin.preset_config_with(&[("layout".into(), "keep".into())]);
    assert!(keep.get("layout").is_none());
    r.plugin.apply_preset(&mut r.host, &keep).unwrap();
    assert!(r.plugin.layout().tiles[1].cut_out);
}
