use super::*;
use std::sync::mpsc::Receiver;
use telescope_core::testing::FakeHost;

const SIZE: usize = 96;
const SKY: [(f32, f32); 5] = [(30.0, 34.0), (62.0, 28.0), (48.0, 60.0), (70.0, 66.0), (36.0, 70.0)];

struct Ctx(&'static str);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        self.0
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// Gaussian stars on a dark sky; a wider star is dimmer at its centre, as a blurred one is.
fn sky(w: usize, h: usize, stars: &[(f32, f32)], sigma: f32) -> Frame {
    let peak = (3.0 / (sigma * sigma)).min(1.0);
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let v = 0.03 + stars.iter().map(|(sx, sy)| {
                let (dx, dy) = (x as f32 - sx, y as f32 - sy);
                peak * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp()
            }).sum::<f32>();
            let byte = (v.min(1.0) * 255.0 + 0.5) as u8;
            data.extend([byte, byte, byte, 255]);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

fn field(sigma: f32) -> Frame {
    sky(SIZE, SIZE, &SKY, sigma)
}

fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("stargazer-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    folder
}

struct Rig {
    plugin: StargazerPlugin,
    host: FakeHost,
    step: Box<dyn CpuStep>,
    folder: PathBuf,
}

impl Rig {
    fn new(name: &str, config: Value) -> Rig {
        let mut host = FakeHost::new();
        host.streaming = true;
        host.focused = Some("phone".into());
        let mut plugin = StargazerPlugin::new();
        plugin.folder = folder(name).display().to_string();
        plugin.set_config(&config).unwrap();
        plugin.setup(&mut host);
        let Some(FrameStep::Cpu(step)) = plugin.frame_step() else { panic!("a Rust step") };
        let folder = PathBuf::from(&plugin.folder);
        Rig { plugin, host, step, folder }
    }

    fn click(&mut self, callback: &str, arg: Value) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
        self.settle();
    }

    /// Waits for the worker to deal with everything sent, then hears what it posted.
    fn settle(&mut self) {
        let pending = self.plugin.feed.as_ref().map(|f| f.pending.clone());
        let deadline = Instant::now() + Duration::from_secs(20);
        while pending.as_ref().is_some_and(|p| p.load(Ordering::SeqCst) > 0) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        for (token, data) in self.host.take_posted() {
            self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
        }
    }

    fn feed(&mut self, frame: Frame) {
        let mut frame = frame;
        self.step.process(&mut frame, &Ctx("phone"));
        self.settle();
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
        self.settle();
    }

    fn pngs(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&self.folder).map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
        found.sort();
        found
    }

    fn props(&self) -> Value {
        let line = self.host.calls.iter().rev().find_map(|c| c.strip_prefix("set_ui_props card ")).expect("props were set");
        serde_json::from_str(line).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

/// The saved picture's red channel, 0..1, at each point.
fn read_png(path: &Path, points: &[(usize, usize)]) -> Vec<f32> {
    let pixmap = tiny_skia::Pixmap::decode_png(&std::fs::read(path).unwrap()).unwrap();
    points.iter().map(|&(x, y)| pixmap.data()[(y * pixmap.width() as usize + x) * 4] as f32 / 255.0).collect()
}

fn red_at(frame: &Frame, x: usize, y: usize) -> f32 {
    frame.data[(y * frame.width as usize + x) * 4] as f32 / 255.0
}

fn config(mode: &str) -> Value {
    json!({"mode": mode, "frames": 50, "keep": 10})
}

#[test]
fn lucky_imaging_keeps_the_sharpest_share_and_saves_one_png() {
    let mut rig = Rig::new("lucky", config("lucky"));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert!(rig.host.frames_wanted.contains(NAME));
    // Every tenth frame is sharp, the rest are soft
    for i in 0..50 {
        rig.feed(field(if i % 10 == 0 { 1.2 } else { 4.0 }));
    }
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert!(!rig.host.frames_wanted.contains(NAME));
    // Ten per cent of fifty
    assert_eq!((rig.plugin.count, rig.plugin.refused), (5, 0));
    let files = rig.pngs();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].extension().and_then(|e| e.to_str()), Some("png"));
    assert!(files[0].file_name().unwrap().to_string_lossy().starts_with("stargazer_"));
    let (sharp, soft) = (field(1.2), field(4.0));
    let (x, y) = (SKY[0].0 as usize, SKY[0].1 as usize);
    assert!(red_at(&sharp, x, y) > 2.0 * red_at(&soft, x, y), "the sharp and soft frames differ enough to tell");
    let stacked = read_png(&files[0], &[(x, y)])[0];
    assert!((stacked - red_at(&sharp, x, y)).abs() < 0.02, "the stack is of the sharp frames only: {stacked}");
    assert!(rig.plugin.last.ends_with(".png"));
    assert!(rig.props()["saved"].as_str().unwrap().starts_with("Saved stargazer_"));
}

#[test]
fn live_stack_counts_frames_and_refuses_another_size() {
    let mut rig = Rig::new("live", config("live"));
    rig.click("run-clicked", Value::Null);
    for _ in 0..6 {
        rig.feed(field(2.0));
    }
    rig.feed(sky(80, 80, &SKY, 2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (6, 1));
    assert!(rig.props()["tally"].as_str().unwrap().contains("6"));
    assert!(rig.pngs().is_empty(), "a live stack is saved on Save");
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.pngs().len(), 1);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    // Starting again goes on with the same stack, and Reset starts over
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 7);
    rig.click("reset-clicked", Value::Null);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (0, 0));
    assert_eq!(rig.props()["save-enabled"], false);
}

#[test]
fn star_trails_do_not_align_and_save_when_stopped() {
    let mut rig = Rig::new("trails", config("trails"));
    rig.click("run-clicked", Value::Null);
    rig.plugin.shared.interval_ms.store(0, Ordering::SeqCst);
    // The same sky each time with one star moving along; lining the frames up would pull the trail back into one dot
    let trail = [20usize, 40, 60, 80];
    for x in trail {
        rig.feed(sky(SIZE, SIZE, &[(x as f32, 48.0)], 1.5));
    }
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (4, 0));
    let files = rig.pngs();
    assert_eq!(files.len(), 1);
    for lit in read_png(&files[0], &trail.map(|x| (x, 48))) {
        assert!(lit > 0.9, "every position of the star is still lit: {lit}");
    }
}

#[test]
fn trails_take_one_frame_per_interval() {
    let shared = Arc::new(Shared::default());
    shared.run.store(1, Ordering::SeqCst);
    shared.mode.store(Mode::Trails.index() as u8, Ordering::SeqCst);
    shared.interval_ms.store(5000, Ordering::SeqCst);
    *shared.focused.lock().unwrap() = "phone".into();
    let (feed, rx) = loose_feed(&shared);
    let mut step = Handoff { shared, feed };
    for _ in 0..4 {
        step.process(&mut field(2.0), &Ctx("phone"));
    }
    assert_eq!(rx.try_iter().count(), 1);
}

#[test]
fn stopping_the_stream_ends_a_run() {
    let mut live = Rig::new("stream-live", config("live"));
    live.click("run-clicked", Value::Null);
    live.feed(field(2.0));
    live.feed(field(2.0));
    live.event(Event::StreamStopped);
    assert_eq!(live.plugin.phase, Phase::Idle);
    assert!(!live.host.frames_wanted.contains(NAME));
    assert_eq!(live.props()["run-enabled"], false, "nothing starts without a stream");
    assert_eq!(live.plugin.count, 2, "the stack is kept to save");

    // A lucky run with enough frames stacks what it has
    let mut lucky = Rig::new("stream-lucky", config("lucky"));
    lucky.click("run-clicked", Value::Null);
    for i in 0..20 {
        lucky.feed(field(if i % 4 == 0 { 1.2 } else { 3.5 }));
    }
    lucky.event(Event::StreamStopped);
    assert_eq!(lucky.plugin.phase, Phase::Idle);
    assert_eq!(lucky.pngs().len(), 1);
    assert!(lucky.props()["status"].as_str().unwrap().starts_with("Stopped early"), "{}", lucky.props()["status"]);

    // ...and one with too few saves nothing and says so
    let mut few = Rig::new("stream-few", config("lucky"));
    few.click("run-clicked", Value::Null);
    few.feed(field(2.0));
    few.feed(field(2.0));
    few.event(Event::StreamStopped);
    assert_eq!(few.plugin.phase, Phase::Idle);
    assert!(few.pngs().is_empty());
    assert!(few.props()["status"].as_str().unwrap().contains("Nothing was saved"));
}

#[test]
fn switching_the_plugin_off_still_stacks_what_a_lucky_run_has() {
    let mut rig = Rig::new("off", config("lucky"));
    rig.click("run-clicked", Value::Null);
    for i in 0..20 {
        rig.feed(field(if i % 4 == 0 { 1.2 } else { 3.5 }));
    }
    let pending = rig.plugin.feed.as_ref().unwrap().pending.clone();
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.host.frames_wanted.contains(NAME));
    let deadline = Instant::now() + Duration::from_secs(20);
    while (pending.load(Ordering::SeqCst) > 0 || rig.pngs().is_empty()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.pngs().len(), 1);
}

#[test]
fn the_settings_round_trip() {
    let mut p = StargazerPlugin::new();
    let saved = json!({"mode": "trails", "frames": 800, "keep": 25, "interval": 12, "camera": true});
    p.set_config(&saved).unwrap();
    assert_eq!(p.get_config(), saved);
    assert!(p.shared.camera.load(Ordering::SeqCst));
    assert_eq!(p.shared.interval_ms.load(Ordering::SeqCst), 12_000);
    // Out of range or nonsense falls back or is clamped, and a run is never part of it
    p.set_config(&json!({"mode": "nonsense", "frames": 5, "keep": 500, "interval": "soon"})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "frames": 50, "keep": 100, "interval": 5, "camera": false}));
    p.set_config(&json!({"frames": 99999})).unwrap();
    assert_eq!(p.get_config()["frames"], 2000);
    p.set_config(&json!({})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "frames": 300, "keep": 10, "interval": 5, "camera": false}));
    assert_eq!(p.phase, Phase::Idle);
}

/// A feed whose worker never takes anything, so the queue fills.
fn loose_feed(shared: &Arc<Shared>) -> (Feed, Receiver<Msg>) {
    let (tx, rx) = channel();
    (Feed { tx, shared: shared.clone(), pending: Arc::default() }, rx)
}

#[test]
fn the_frame_step_hands_frames_over_and_never_blocks() {
    let shared = Arc::new(Shared::default());
    shared.run.store(1, Ordering::SeqCst);
    shared.mode.store(Mode::Live.index() as u8, Ordering::SeqCst);
    *shared.focused.lock().unwrap() = "phone".into();
    let (feed, rx) = loose_feed(&shared);
    let mut step = Handoff { shared: shared.clone(), feed };
    let frame = field(2.0);
    for _ in 0..QUEUE + 5 {
        let mut shown = frame.clone();
        step.process(&mut shown, &Ctx("phone"));
        assert_eq!(shown, frame, "the picture goes on untouched");
    }
    assert_eq!(rx.try_iter().filter(|m| matches!(m, Msg::Frame(_))).count(), QUEUE, "the rest were dropped");
    assert_eq!(shared.queued.load(Ordering::SeqCst), QUEUE);
    // A stream the panels don't show is left alone
    shared.queued.store(0, Ordering::SeqCst);
    step.process(&mut field(2.0), &Ctx("other"));
    assert_eq!(rx.try_iter().count(), 0);
    assert_eq!(step.touch(&Ctx("other")), Touch::Nothing);
    assert_eq!(step.touch(&Ctx("phone")), Touch::Reads);
}

#[test]
fn the_stack_goes_on_the_camera_only_when_asked() {
    let shared = Arc::new(Shared::default());
    *shared.focused.lock().unwrap() = "phone".into();
    let (feed, _rx) = loose_feed(&shared);
    let mut step = Handoff { shared: shared.clone(), feed };
    let stack = sky(SIZE, SIZE, &[(10.0, 10.0)], 1.5);
    *shared.result.lock().unwrap() = Some(stack.clone());
    let live = field(2.0);
    // The switch is off by default
    let mut shown = live.clone();
    step.process(&mut shown, &Ctx("phone"));
    assert_eq!(shown.data, live.data);
    shared.camera.store(true, Ordering::SeqCst);
    assert_eq!(step.touch(&Ctx("phone")), Touch::Changes);
    let mut shown = field(2.0);
    shown.number = 77;
    step.process(&mut shown, &Ctx("phone"));
    assert_eq!(shown.data, stack.data);
    assert_eq!(shown.number, 77, "it's still that frame's place in the stream");
    // A picture of another size is left as it is
    let mut small = sky(64, 64, &SKY, 2.0);
    let before = small.clone();
    step.process(&mut small, &Ctx("phone"));
    assert_eq!(small.data, before.data);
}

#[test]
fn the_focus_readout_follows_the_picture_and_keeps_the_best() {
    let mut rig = Rig::new("focus", config("lucky"));
    rig.feed(field(4.0));
    let first = rig.plugin.focus.expect("a reading");
    assert!(first > 0.0);
    std::thread::sleep(FOCUS_EVERY + Duration::from_millis(50));
    rig.feed(field(1.2));
    let sharp = rig.plugin.focus.unwrap();
    assert!(sharp > first);
    std::thread::sleep(FOCUS_EVERY + Duration::from_millis(50));
    rig.feed(field(4.0));
    assert!(rig.plugin.focus.unwrap() < sharp);
    assert_eq!(rig.plugin.best, sharp);
    assert!(rig.props()["focus-text"].as_str().unwrap().starts_with("Now "));
}

#[test]
fn the_folder_opens_on_the_last_saved_file() {
    let mut rig = Rig::new("open", config("lucky"));
    rig.plugin.last = rig.folder.join("stargazer_x.png").display().to_string();
    rig.click("open-clicked", Value::Null);
    assert!(rig.host.calls.iter().any(|c| c.starts_with("open_url file://") && c.contains("stargazer-test-open")), "{:?}", rig.host.calls);
}
