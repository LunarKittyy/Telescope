//! test_composite.py's plugin tests, by the same names. Frames are BGRA, so a colour reads as the BGR Python's do.

use super::*;
use crate::composite::{bgr, Tile, DEFAULT_BACKGROUND};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

const SECOND: u64 = 1_000_000_000;

/// The stream frames the host hands out, changed as a test goes (the _Host of test_composite.py).
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
    plugin.on_stream_start(&mut host, &StreamInfo { source_id: "u".into(), url: "u".into() });
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
    assert!(same(&r.process(&frame, "b"), &frame) && r.plugin.note.contains("no stream goes there"));
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
fn while_the_telescope_stream_stalls_the_others_keep_moving() {
    let mut r = rig();
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
    assert_eq!(r.plugin.note, format!("Goes out on {}.", r.plugin.camera_label));
    r.host.live = vec!["a".into()];
    r.plugin.on_event(&mut r.host, &Event::StreamsChanged { count: 1 });
    assert_eq!(r.plugin.note, "It takes over once two of its streams are running.");
    r.switch(false);
    assert!(r.plugin.note.is_empty());
    assert_eq!(r.plugin.diagnostics(), [("One picture".to_string(), "off".to_string())]);
}
