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
        self.feed_from("phone", frame);
    }

    fn feed_from(&mut self, source: &'static str, frame: Frame) {
        let mut frame = frame;
        self.step.process(&mut frame, &Ctx(source));
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

    /// The card's status line as last set.
    fn status(&self) -> String {
        let line = self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).find(|l| l.contains("\"status\"")).expect("a status was set");
        serde_json::from_str::<Value>(line).unwrap()["status"].as_str().unwrap().to_string()
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
fn live_stack_counts_frames_and_stops_when_the_size_changes() {
    let mut rig = Rig::new("live", config("live"));
    rig.click("run-clicked", Value::Null);
    for _ in 0..6 {
        rig.feed(field(2.0));
    }
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    rig.feed(sky(80, 80, &SKY, 2.0));
    // The run ends with what it had, and says why
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert!(!rig.host.frames_wanted.contains(NAME));
    assert_eq!((rig.plugin.count, rig.plugin.refused), (6, 0));
    let props = rig.props();
    assert!(props["status"].as_str().unwrap().starts_with("Stopped because the picture size changed"), "{}", props["status"]);
    assert!(props["tally"].as_str().unwrap().contains("6"));
    assert_eq!(props["save-enabled"], true);
    assert!(rig.pngs().is_empty(), "a live stack is saved on Save");
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.pngs().len(), 1);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    // Starting again at the new size begins a fresh stack instead of refusing every frame
    rig.click("run-clicked", Value::Null);
    rig.feed(sky(80, 80, &SKY, 2.0));
    rig.feed(sky(80, 80, &SKY, 2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (2, 0));
    // At the same size it goes on with the same stack, and Reset starts over
    rig.click("run-clicked", Value::Null);
    rig.feed(sky(80, 80, &SKY, 2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 3);
    rig.click("reset-clicked", Value::Null);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (0, 0));
    assert_eq!(rig.props()["save-enabled"], false);
}

#[test]
fn lucky_and_trails_end_on_a_size_change_with_what_they_have() {
    let mut lucky = Rig::new("size-lucky", config("lucky"));
    lucky.click("run-clicked", Value::Null);
    for i in 0..10 {
        lucky.feed(field(if i % 3 == 0 { 1.2 } else { 3.5 }));
    }
    lucky.feed(sky(80, 80, &SKY, 2.0));
    assert_eq!(lucky.plugin.phase, Phase::Idle);
    assert_eq!(lucky.pngs().len(), 1, "stacked and saved from the frames of one size");
    assert!(lucky.props()["status"].as_str().unwrap().starts_with("Stopped because the picture size changed"));

    let mut trails = Rig::new("size-trails", config("trails"));
    trails.click("run-clicked", Value::Null);
    trails.plugin.shared.interval_ms.store(0, Ordering::SeqCst);
    for x in [20.0, 40.0, 60.0] {
        trails.feed(sky(SIZE, SIZE, &[(x, 48.0)], 1.5));
    }
    trails.feed(sky(80, 80, &SKY, 2.0));
    assert_eq!(trails.plugin.phase, Phase::Idle);
    assert_eq!((trails.plugin.count, trails.pngs().len()), (3, 1));
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
    *shared.source.lock().unwrap() = "phone".into();
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
fn switching_the_plugin_off_waits_for_the_stack_to_be_saved() {
    let mut rig = Rig::new("off", config("lucky"));
    rig.click("run-clicked", Value::Null);
    for i in 0..20 {
        rig.feed(field(if i % 4 == 0 { 1.2 } else { 3.5 }));
    }
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.host.frames_wanted.contains(NAME));
    // It's there when shutdown returns, whole, and nothing is left beside it
    let files = rig.pngs();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].extension().and_then(|e| e.to_str()), Some("png"));
    assert!(read_png(&files[0], &[(10, 10)]).len() == 1);
    assert!(rig.plugin.worker.is_none());
}

#[test]
fn a_picture_is_written_whole_or_not_at_all() {
    let dir = folder("atomic");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.png");
    write_atomic(&path, b"data").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"data");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left behind");
    // A name that can't be renamed onto fails and takes its temporary file with it
    let taken = dir.join("taken.png");
    std::fs::create_dir_all(taken.join("inside")).unwrap();
    assert!(write_atomic(&taken, b"data").is_err());
    assert!(!dir.join("taken.png.part").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_settings_round_trip() {
    let mut p = StargazerPlugin::new();
    let saved = json!({"mode": "trails", "input": "video", "frames": 800, "keep": 25, "interval": 12, "photo_count": 10, "photo_every": 0, "camera": true, "use_zoom": false,
        "shot": {"shutter_s": 20.0, "iso": 1600}});
    p.set_config(&saved).unwrap();
    assert_eq!(p.get_config(), saved);
    assert!(p.shared.camera.load(Ordering::SeqCst));
    assert_eq!(p.shared.interval_ms.load(Ordering::SeqCst), 12_000);
    // Out of range or nonsense falls back or is clamped, and a run is never part of it
    p.set_config(&json!({"mode": "nonsense", "frames": 5, "keep": 500, "interval": "soon"})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "input": "video", "frames": 50, "keep": 100, "interval": 5, "photo_count": 10, "photo_every": 0, "camera": false, "use_zoom": false}));
    p.set_config(&json!({"frames": 99999})).unwrap();
    assert_eq!(p.get_config()["frames"], 2000);
    p.set_config(&json!({})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "input": "video", "frames": 300, "keep": 10, "interval": 5, "photo_count": 10, "photo_every": 0, "camera": false, "use_zoom": false}));
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
    *shared.source.lock().unwrap() = "phone".into();
    let (feed, rx) = loose_feed(&shared);
    let mut step = Handoff { shared: shared.clone(), feed };
    let frame = field(2.0);
    for _ in 0..QUEUE + 5 {
        let mut shown = frame.clone();
        step.process(&mut shown, &Ctx("phone"));
        assert_eq!(shown, frame, "the picture goes on untouched");
    }
    assert_eq!(rx.try_iter().filter(|m| matches!(m, Msg::Frame { .. })).count(), QUEUE, "the rest were dropped");
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
    *shared.result.lock().unwrap() = Some(Stack { source: "phone".into(), frame: stack.clone() });
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

#[test]
fn a_lucky_run_holds_frames_by_bytes_not_by_count() {
    // Full-size frames: a 4K I420 picture is about 12 MB and stacking takes about 230 MB, so 1.5 GB (less the queue) holds about 95
    let uhd = Frame::new(3840, 2160, PixelFormat::I420, vec![0; PixelFormat::I420.size(3840, 2160)]);
    let fits = lucky_capacity(2000, &uhd, LUCKY_BUDGET);
    assert!((90..=100).contains(&fits), "{fits}");
    assert_eq!(lucky_capacity(30, &uhd, LUCKY_BUDGET), 30, "a small share is never raised");
    assert_eq!(lucky_capacity(2000, &field(2.0), LUCKY_BUDGET), 2000, "small pictures fit");
    assert_eq!(lucky_capacity(500, &uhd, 1000), 1, "at least one");
}

#[test]
fn the_card_says_when_memory_lowers_a_lucky_run() {
    let mut rig = Rig::new("budget", json!({"mode": "lucky", "frames": 50, "keep": 40}));
    // Room for stacking, the queue and three pictures, where the share asked for is twenty
    rig.plugin.budget = (QUEUE + 3) * field(2.0).format.size(SIZE as u32, SIZE as u32) + SIZE * SIZE * STACK_BYTES_PER_PIXEL;
    rig.click("run-clicked", Value::Null);
    for i in 0..50 {
        rig.feed(field(1.0 + (i % 7) as f32 * 0.4));
    }
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.count, 3);
    assert_eq!(rig.pngs().len(), 1);
    assert_eq!(rig.props()["limit"], "Memory limits this run to the sharpest 3 frames");
    // A run that fits says nothing, and a new run clears the note
    rig.plugin.budget = LUCKY_BUDGET;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.props()["limit"], "");
}

#[test]
fn a_stack_that_could_not_be_saved_can_be_saved_again() {
    for mode in ["lucky", "trails"] {
        let mut rig = Rig::new(&format!("retry-{mode}"), config(mode));
        // A file where the folder should be, so there's nowhere to save
        std::fs::write(&rig.folder, b"in the way").unwrap();
        rig.click("run-clicked", Value::Null);
        rig.plugin.shared.interval_ms.store(0, Ordering::SeqCst);
        for i in 0..50 {
            rig.feed(if mode == "lucky" { field(if i % 5 == 0 { 1.2 } else { 3.5 }) } else { sky(SIZE, SIZE, &[(20.0 + i as f32, 48.0)], 1.5) });
            if rig.plugin.phase == Phase::Idle {
                break;
            }
        }
        if mode == "trails" {
            rig.click("run-clicked", Value::Null);
        }
        assert_eq!(rig.plugin.phase, Phase::Idle, "{mode}");
        let props = rig.props();
        assert_eq!(props["status"], "Couldn't save the picture", "{mode}");
        assert_eq!(props["save-enabled"], true, "{mode}: Save is there in every mode");
        // Retrying while the drive is still full fails again and keeps the stack
        rig.click("save-clicked", Value::Null);
        assert_eq!(rig.props()["save-enabled"], true);
        std::fs::remove_file(&rig.folder).unwrap();
        rig.click("save-clicked", Value::Null);
        assert_eq!(rig.pngs().len(), 1, "{mode}");
        assert_eq!(rig.props()["save-enabled"], false, "{mode}");
        assert!(rig.props()["saved"].as_str().unwrap().starts_with("Saved stargazer_"));
    }
    // A new run drops the unsaved stack
    let mut rig = Rig::new("retry-new", config("lucky"));
    std::fs::write(&rig.folder, b"in the way").unwrap();
    rig.click("run-clicked", Value::Null);
    for i in 0..50 {
        rig.feed(field(if i % 5 == 0 { 1.2 } else { 3.5 }));
    }
    assert_eq!(rig.props()["save-enabled"], true);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.props()["save-enabled"], false);
    let _ = std::fs::remove_file(&rig.folder);
}

#[test]
fn a_run_belongs_to_the_phone_it_started_on() {
    let mut rig = Rig::new("phones", config("live"));
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    rig.feed(field(2.0));
    // Pictures of another phone are never handed over while the run is on
    rig.host.focused = Some("other".into());
    rig.event(Event::StreamConnected { source_id: "other".into() });
    rig.feed_from("other", field(2.0));
    // Moving off the phone ends the run like the stream stopping does, and the stack is kept to save
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.settle();
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.count, 2);
    assert!(!rig.host.frames_wanted.contains(NAME));
    // The other phone starts its own stack, not on top of the first one's
    rig.click("run-clicked", Value::Null);
    rig.feed_from("other", field(2.0));
    rig.feed_from("phone", field(2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 1, "the first phone's pictures are left alone now");

    // A picture of another phone that does get to the worker ends the run too
    let mut lucky = Rig::new("phones-lucky", config("lucky"));
    lucky.click("run-clicked", Value::Null);
    for i in 0..10 {
        lucky.feed(field(if i % 3 == 0 { 1.2 } else { 3.5 }));
    }
    let run = lucky.plugin.run;
    lucky.plugin.feed.as_ref().unwrap().hand(run, "other", field(2.0));
    lucky.settle();
    assert_eq!(lucky.plugin.phase, Phase::Idle);
    assert_eq!(lucky.pngs().len(), 1);
}

#[test]
fn the_stack_only_goes_back_onto_its_own_phone() {
    let shared = Arc::new(Shared::default());
    *shared.focused.lock().unwrap() = "other".into();
    shared.camera.store(true, Ordering::SeqCst);
    *shared.result.lock().unwrap() = Some(Stack { source: "phone".into(), frame: sky(SIZE, SIZE, &[(10.0, 10.0)], 1.5) });
    let (feed, _rx) = loose_feed(&shared);
    let mut step = Handoff { shared: shared.clone(), feed };
    let mut shown = field(2.0);
    let before = shown.clone();
    assert_eq!(step.touch(&Ctx("other")), Touch::Reads);
    step.process(&mut shown, &Ctx("other"));
    assert_eq!(shown.data, before.data, "another phone's stack isn't shown here");
}

#[test]
fn a_hidden_picture_pauses_collecting_and_keeps_the_stack_off_the_camera() {
    let mut rig = Rig::new("hidden", config("live"));
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    let tick = || Event::TimerFired { owner: NAME.into(), token: "tick".into() };
    rig.host.picture_hidden = true;
    rig.event(tick());
    assert!(rig.status().contains("hidden"), "{}", rig.status());
    assert_eq!(rig.plugin.phase, Phase::Collecting, "the run goes on");
    rig.feed(field(2.0));
    rig.feed(field(2.0));
    assert_eq!(rig.plugin.shared.queued.load(Ordering::SeqCst), 0);
    // The stack isn't sent while the picture is hidden
    rig.click("camera-toggled", Value::Bool(true));
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Nothing);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 1, "the frames that came while it was hidden were dropped");
    // Unhidden, the run goes on with the same stack and the camera gets it
    rig.host.picture_hidden = false;
    rig.event(tick());
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    assert!(rig.status().starts_with("Stacking"), "{}", rig.status());
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Changes);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 2);
}

#[test]
fn hidden_is_looked_at_about_once_a_second() {
    let mut rig = Rig::new("hidden-rate", config("live"));
    let now = Arc::new(Mutex::new(100.0f64));
    let clock = now.clone();
    rig.plugin.clock = Box::new(move || *clock.lock().unwrap());
    rig.event(Event::StreamConnected { source_id: "phone".into() });
    rig.host.picture_hidden = true;
    rig.event(Event::StreamConnected { source_id: "phone".into() });
    assert!(!rig.plugin.hidden, "too soon to look again");
    *now.lock().unwrap() += 1.1;
    rig.event(Event::StreamConnected { source_id: "phone".into() });
    assert!(rig.plugin.hidden);
}

#[test]
fn stop_stacks_the_frames_already_admitted() {
    let mut rig = Rig::new("drain", config("lucky"));
    rig.click("run-clicked", Value::Null);
    // Three frames handed over and Stop right behind them, with no wait for the worker between
    for i in 0..3 {
        rig.step.process(&mut field(1.2 + i as f32), &Ctx("phone"));
    }
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.pngs().len(), 1, "all three counted");
    assert_eq!(rig.plugin.count, 3);
}

#[test]
fn stopping_trails_reports_the_time_it_ran() {
    let mut rig = Rig::new("clock", config("trails"));
    let now = Arc::new(Mutex::new(100.0f64));
    let clock = now.clone();
    rig.plugin.clock = Box::new(move || *clock.lock().unwrap());
    rig.click("run-clicked", Value::Null);
    rig.plugin.shared.interval_ms.store(0, Ordering::SeqCst);
    rig.feed(sky(SIZE, SIZE, &[(20.0, 48.0)], 1.5));
    rig.feed(sky(SIZE, SIZE, &[(40.0, 48.0)], 1.5));
    *now.lock().unwrap() += 30.0;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    *now.lock().unwrap() += 60.0;
    assert_eq!(rig.plugin.elapsed(), 30.0);
    assert!(rig.props()["tally"].as_str().unwrap().contains("0:30"), "{}", rig.props()["tally"]);
}

#[test]
fn another_phone_starts_with_an_empty_tally() {
    let mut rig = Rig::new("tally", config("live"));
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    rig.feed(field(2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 2);
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("other".into());
    rig.plugin.on_stream_start(&mut rig.host, &StreamInfo { source_id: "other".into(), url: "u".into(), session: 0 });
    assert_eq!(rig.plugin.tally(), "");
    assert!(rig.plugin.can_save(), "the first phone's stack can still be saved");
    // Back on the same phone after a stop, the numbers stay
    let mut same = Rig::new("tally-same", config("live"));
    same.click("run-clicked", Value::Null);
    same.feed(field(2.0));
    same.click("run-clicked", Value::Null);
    same.plugin.on_stream_start(&mut same.host, &StreamInfo { source_id: "phone".into(), url: "u".into(), session: 0 });
    assert_eq!(same.plugin.count, 1);
}

#[test]
fn a_cover_between_looks_stops_frames_at_once() {
    let mut rig = Rig::new("hidden-flag", config("live"));
    let flag = Arc::new(AtomicBool::new(false));
    rig.host.hidden_flag = Some(flag.clone());
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    // The host's flag turns before the plugin's next look: the frame step reads it and takes nothing
    flag.store(true, Ordering::SeqCst);
    rig.feed(field(2.0));
    rig.feed(field(2.0));
    assert_eq!(rig.plugin.shared.queued.load(Ordering::SeqCst), 0);
    rig.click("camera-toggled", Value::Bool(true));
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Nothing);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 1);
}

#[test]
fn a_preset_brings_back_the_settings_and_never_begins_a_run() {
    let mut a = StargazerPlugin::new();
    a.set_config(&json!({"mode": "trails", "frames": 800, "keep": 25, "interval": 12, "camera": true})).unwrap();
    let saved = a.preset_config();
    assert!(saved.get("folder").is_none());
    let mut rig = Rig::new("preset", json!({}));
    rig.plugin.apply_preset(&mut rig.host, &saved).unwrap();
    assert_eq!(rig.plugin.get_config(), a.get_config());
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert!(rig.plugin.shared.camera.load(Ordering::SeqCst));
}

#[test]
fn an_older_preset_without_the_keys_changes_nothing_in_stargazer() {
    let mut rig = Rig::new("preset-old", json!({"mode": "live", "frames": 700, "keep": 20, "interval": 9, "camera": true}));
    let before = rig.plugin.get_config();
    rig.plugin.apply_preset(&mut rig.host, &json!({})).unwrap();
    assert_eq!(rig.plugin.get_config(), before);
    rig.plugin.apply_preset(&mut rig.host, &json!({"frames": 400})).unwrap();
    let after = rig.plugin.get_config();
    assert_eq!((&after["frames"], &after["mode"], &after["camera"]), (&json!(400), &json!("live"), &json!(true)));
}

#[test]
fn a_preset_mid_run_leaves_the_mode_alone() {
    let mut rig = Rig::new("preset-run", json!({"mode": "live"}));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    rig.plugin.apply_preset(&mut rig.host, &json!({"mode": "trails", "frames": 400})).unwrap();
    assert_eq!((rig.plugin.mode, rig.plugin.shared.mode(), rig.plugin.frames), (Mode::Live, Mode::Live, 400));
    assert_eq!(rig.plugin.phase, Phase::Collecting);
}

#[test]
fn a_preset_that_changes_the_mode_starts_that_mode_clean() {
    let mut rig = Rig::new("preset-mode", json!({"mode": "live"}));
    rig.plugin.count = 5;
    rig.plugin.unsaved = true;
    rig.plugin.apply_preset(&mut rig.host, &json!({"mode": "lucky"})).unwrap();
    assert_eq!((rig.plugin.mode, rig.plugin.count, rig.plugin.unsaved, rig.plugin.can_save()), (Mode::Lucky, 0, false, false));
}

#[test]
fn a_preset_turning_the_camera_off_drops_the_stack_from_it() {
    let mut rig = Rig::new("preset-cam", json!({"mode": "live"}));
    *rig.plugin.shared.result.lock().unwrap() = Some(Stack { source: "phone".into(), frame: field(2.0) });
    rig.plugin.apply_preset(&mut rig.host, &json!({"camera": true})).unwrap();
    assert!(rig.plugin.shared.camera.load(Ordering::SeqCst));
    rig.plugin.apply_preset(&mut rig.host, &json!({"camera": false})).unwrap();
    assert!(!rig.plugin.shared.camera.load(Ordering::SeqCst));
    assert!(rig.plugin.shared.result.lock().unwrap().is_none());
}

#[test]
fn reset_puts_the_sliders_back_and_leaves_the_mode_and_the_camera() {
    let mut rig = Rig::new("reset", json!({"mode": "live", "frames": 900, "keep": 40, "interval": 20, "camera": true}));
    let shown = |rig: &Rig| rig.plugin.state_props(&rig.host)["settings-reset-shown"].clone();
    assert_eq!(shown(&rig), json!(true));
    rig.click("settings-reset-clicked", Value::Null);
    let cfg = rig.plugin.get_config();
    assert_eq!((cfg["frames"].as_u64(), cfg["keep"].as_u64(), cfg["interval"].as_u64()), (Some(DEFAULT_FRAMES.into()), Some(DEFAULT_KEEP.into()), Some(DEFAULT_TRAIL_S.into())));
    assert_eq!((cfg["mode"].as_str(), cfg["camera"].as_bool()), (Some("live"), Some(true)));
    assert_eq!(rig.plugin.shared.interval_ms.load(Ordering::SeqCst), u64::from(DEFAULT_TRAIL_S) * 1000);
    assert!(rig.plugin.shared.camera.load(Ordering::SeqCst));
    assert_eq!(shown(&rig), json!(false));
    assert!(rig.host.calls.iter().any(|c| c.starts_with("set_ui_props card") && c.contains("\"settings-reset-shown\":false")));
}

#[test]
fn reset_waits_for_a_run_to_end() {
    let mut rig = Rig::new("reset-run", json!({"mode": "live", "frames": 900}));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert_eq!(rig.plugin.state_props(&rig.host)["settings-reset-shown"], json!(false));
    rig.click("settings-reset-clicked", Value::Null);
    assert_eq!(rig.plugin.frames, 900, "a run keeps the settings it began with");
    assert_eq!(rig.plugin.phase, Phase::Collecting);
}

#[test]
fn idle_focus_analysis_only_reads_frames_with_the_card_visible() {
    let mut rig = Rig::new("focus-visible", config("live"));
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Reads);
    rig.event(Event::CardFolded { key: "other/Card".into(), folded: true });
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Reads);
    rig.event(Event::CardFolded { key: "stargazer/Stargazer".into(), folded: true });
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Nothing);
    rig.feed(field(2.0));
    assert!(rig.plugin.focus.is_none());
    rig.event(Event::WindowShown { shown: false });
    rig.event(Event::CardFolded { key: "stargazer/Stargazer".into(), folded: false });
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Nothing);
    rig.event(Event::WindowShown { shown: true });
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Reads);
    rig.feed(field(2.0));
    assert!(rig.plugin.focus.is_some());
}

#[test]
fn stacking_keeps_reading_frames_with_the_card_and_window_hidden() {
    let mut rig = Rig::new("stack-hidden", config("live"));
    rig.event(Event::WindowShown { shown: false });
    rig.event(Event::CardFolded { key: "stargazer/Stargazer".into(), folded: true });
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Reads);
    rig.feed(field(2.0));
    assert!(rig.plugin.focus.is_none());
    assert!(rig.host.frames_wanted.contains(NAME));
    rig.click("run-clicked", Value::Null);
    assert!(rig.plugin.count > 0);
}

fn photo_host() -> FakeHost {
    let mut host = FakeHost::new();
    host.streaming = true;
    host.focused = Some("phone".into());
    host.still_only_for = Some("phone".into());
    host.still_caps = Some(json!({"shot": {
        "shutter": {"min_ns": 250_000, "max_ns": 20_000_000_000i64},
        "iso": {"min": 50, "max": 3200}, "focus": {"near_diopters": 10.0}, "freezes_from_ns": 500_000_000,
    }}));
    host
}

fn photo_ui(plugin: &mut StargazerPlugin, host: &mut FakeHost, callback: &str, arg: Value) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
}

fn photo_plugin(host: &mut FakeHost, config: Value) -> StargazerPlugin {
    let mut plugin = StargazerPlugin::new();
    plugin.set_config(&config).unwrap();
    plugin.setup(host);
    plugin
}

fn live_photos() -> Value {
    json!({"mode": "live", "input": "photos"})
}

fn jpeg_of(frame: &Frame) -> Vec<u8> {
    let rgb: Vec<u8> = frame.data.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
    image::ImageEncoder::write_image(encoder, &rgb, frame.width, frame.height, image::ExtendedColorType::Rgb8).unwrap();
    out
}

impl Rig {
    /// A rig whose host can take photos, with its clock in the test's hands.
    fn photos(name: &str, config: Value) -> (Rig, Arc<Mutex<f64>>) {
        let mut rig = Rig::new(name, config);
        let host = photo_host();
        (rig.host.still_caps, rig.host.still_only_for) = (host.still_caps, host.still_only_for);
        let now = Arc::new(Mutex::new(100.0f64));
        let clock = now.clone();
        rig.plugin.clock = Box::new(move || *clock.lock().unwrap());
        (rig, now)
    }

    /// The photo on its way comes back as this picture, in a file of the run's folder.
    fn photo_done(&mut self, frame: &Frame) {
        let request = self.plugin.taking.expect("a photo is on its way");
        std::fs::create_dir_all(&self.folder).unwrap();
        let path = self.folder.join(format!("photo_{}.jpg", request.0));
        std::fs::write(&path, jpeg_of(frame)).unwrap();
        self.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"ok": true, "files": [path.display().to_string()]}) });
    }

    fn saved_pngs(&self) -> Vec<PathBuf> {
        self.pngs().into_iter().filter(|p| p.extension().is_some_and(|e| e == "png")).collect()
    }

    fn taken(&self) -> usize {
        self.host.stills_taken.len()
    }
}

#[test]
fn frames_come_from_the_video_or_photos_and_lucky_only_from_the_video() {
    assert_eq!(Mode::ALL, [Mode::Lucky, Mode::Live, Mode::Trails]);
    assert_eq!(Input::ALL, [Input::Video, Input::Photos]);
    assert_eq!(StargazerPlugin::fixed_props()["mode-items"], json!(["Lucky", "Live", "Trails"]));
    assert_eq!(StargazerPlugin::fixed_props()["input-items"], json!(["Video", "Photos"]));
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, json!({"mode": "lucky"}));
    // Photos can't be picked for lucky imaging
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!(plugin.input, Input::Video);
    photo_ui(&mut plugin, &mut host, "mode-picked", json!(1));
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!((plugin.mode, plugin.input), (Mode::Live, Input::Photos));
    assert_eq!(plugin.state_props(&host)["input-index"], 1);
    // And picking lucky brings the frames back to the video
    photo_ui(&mut plugin, &mut host, "mode-picked", json!(0));
    assert_eq!((plugin.mode, plugin.input), (Mode::Lucky, Input::Video));
    // A settings file that says otherwise is put right
    plugin.set_config(&json!({"mode": "lucky", "input": "photos"})).unwrap();
    assert_eq!(plugin.input, Input::Video);
    assert_eq!(plugin.get_config()["input"], "video");
    plugin.apply_preset(&mut host, &json!({"mode": "lucky", "input": "photos"})).unwrap();
    assert_eq!((plugin.mode, plugin.input), (Mode::Lucky, Input::Video));
}

#[test]
fn a_saved_night_becomes_the_live_stack_taking_one_photo_with_its_shot_kept() {
    let mut plugin = StargazerPlugin::new();
    plugin.set_config(&json!({"mode": "night", "frames": 700, "shot": {"shutter_s": 20.0, "iso": 1600}})).unwrap();
    assert_eq!((plugin.mode, plugin.input, plugin.photo_count), (Mode::Live, Input::Photos, 1));
    assert_eq!(plugin.shot, ShotChoice { shutter_s: Some(20.0), iso: Some(1600), focus_diopters: None });
    assert_eq!(plugin.frames, 700);
    let saved = plugin.get_config();
    assert_eq!((saved["mode"].as_str(), saved["input"].as_str(), saved["photo_count"].as_u64()), (Some("live"), Some("photos"), Some(1)));
    // A Night that never had a shot saved starts at the long exposure it always did
    let mut bare = StargazerPlugin::new();
    bare.set_config(&json!({"mode": "night"})).unwrap();
    assert_eq!(bare.shot, StargazerPlugin::default_shot());
    assert_eq!(bare.photo_count, 1);
}

#[test]
fn a_saved_night_preset_is_recalled_as_live_photos_and_never_changes_a_run() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, json!({"mode": "trails"}));
    plugin.apply_preset(&mut host, &json!({"mode": "night", "shot": {"iso": 800}})).unwrap();
    assert_eq!((plugin.mode, plugin.input, plugin.photo_count), (Mode::Live, Input::Photos, 1));
    assert_eq!(plugin.shot, ShotChoice { iso: Some(800), ..ShotChoice::default() });
    assert_eq!(plugin.preset_config()["mode"], "live");
    // During a run the mode and the source stay, whatever the preset says
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(plugin.phase, Phase::Collecting);
    plugin.apply_preset(&mut host, &json!({"mode": "trails", "input": "video", "photo_count": 9, "shot": {"iso": 100}})).unwrap();
    assert_eq!((plugin.mode, plugin.input), (Mode::Live, Input::Photos));
    assert_eq!(plugin.photo.as_ref().unwrap().options["shot"], json!({"iso": 800}), "the run keeps what it started with");
    assert_eq!(host.stills_taken.len(), 1);
    plugin.shutdown(&mut host);
}

#[test]
fn the_first_photos_pick_saves_defaults_and_never_replaces_saved_choices() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, json!({"mode": "live"}));
    assert!(plugin.get_config().get("shot").is_none());
    host.calls.clear();
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!(plugin.get_config()["shot"], json!({"shutter_s": 10.0, "iso": 800, "focus_diopters": 0.0}));
    assert!(host.calls.iter().any(|c| c == "schedule_save"));
    assert!(plugin.worker.is_none());
    let saved = json!({"iso": 1600});
    plugin.set_config(&json!({"mode": "live", "shot": saved})).unwrap();
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!(plugin.get_config()["shot"], saved);
    let mut restored = StargazerPlugin::new();
    restored.set_config(&plugin.get_config()).unwrap();
    assert_eq!(restored.get_config(), plugin.get_config());
    assert_eq!(restored.shot.shutter_s, None);
    assert_eq!(restored.shot.focus_diopters, None);
}

#[test]
fn photo_settings_are_saved_and_come_back() {
    let mut plugin = StargazerPlugin::new();
    plugin.set_config(&json!({"mode": "trails", "input": "photos", "photo_count": 25, "photo_every": 30, "shot": {"shutter_s": 15.0}})).unwrap();
    let cfg = plugin.get_config();
    assert_eq!((cfg["photo_count"].as_u64(), cfg["photo_every"].as_u64(), cfg["input"].as_str()), (Some(25), Some(30), Some("photos")));
    let mut again = StargazerPlugin::new();
    again.set_config(&cfg).unwrap();
    assert_eq!(again.get_config(), cfg);
    // Out of range numbers are held to the range
    again.set_config(&json!({"photo_count": 0, "photo_every": 100_000})).unwrap();
    assert_eq!((again.photo_count, again.photo_every), (1, MAX_PHOTO_EVERY));
    again.set_config(&json!({})).unwrap();
    assert_eq!((again.photo_count, again.photo_every, again.input), (DEFAULT_PHOTOS, 0, Input::Video));
}

#[test]
fn photos_take_one_jpeg_at_a_time_with_the_options_the_run_started_with() {
    let (mut rig, _) = Rig::photos("photos-options", json!({"mode": "live", "input": "photos", "photo_count": 3, "shot": {"shutter_s": 10.0, "iso": 800}}));
    assert_eq!(rig.plugin.state_props(&rig.host)["status"], "Ready");
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.taken(), 1);
    assert_eq!(rig.host.stills_taken[0], (json!({"format": "jpeg", "quality": 95, "rotation": 0, "use_zoom": false,
        "shot": {"shutter_s": 10.0, "iso": 800}, "skip_if_hidden": true}), rig.plugin.folder.clone()));
    assert_eq!(rig.host.still_sources, vec![Some("phone".to_string())]);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert!(rig.plugin.taking.is_some());
    assert!(!rig.host.frames_wanted.contains(NAME), "the video isn't needed for photos");
    assert_eq!(rig.plugin.shared.run.load(Ordering::SeqCst), 0, "the frame step hands nothing over for a photo run");
    assert_eq!(rig.plugin.state_props(&rig.host)["run-text"], "Stop");
    assert_eq!(rig.plugin.state_props(&rig.host)["status"], "Taking photo 1 of 3");
    // The card and a preset change nothing in the run
    rig.click("use-zoom-toggled", json!(true));
    assert!(!rig.plugin.use_zoom, "locked during the run");
    rig.plugin.use_zoom = true;
    rig.plugin.shot.iso = Some(100);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 2);
    assert_eq!(rig.host.stills_taken[1].0, rig.host.stills_taken[0].0);
}

#[test]
fn a_photo_run_stacks_every_photo_and_ends_with_the_last() {
    let (mut rig, _) = Rig::photos("photos-run", json!({"mode": "live", "input": "photos", "photo_count": 3}));
    rig.click("run-clicked", Value::Null);
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::RequestProgress { request, data: json!({"state": "capturing"}) });
    assert_eq!(rig.status(), "Video paused while exposing");
    rig.photo_done(&field(2.0));
    assert_eq!((rig.plugin.count, rig.plugin.refused, rig.taken()), (1, 0, 2));
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 3);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 3, "no photo after the last");
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!((rig.plugin.count, rig.plugin.refused), (3, 0));
    assert!(rig.plugin.taking.is_none() && rig.plugin.photo.is_none());
    assert_eq!(rig.status(), "Stacked 3 photos. Save keeps it, Reset starts a new stack");
    let props = rig.props();
    assert_eq!((props["save-enabled"].as_bool(), props["run-enabled"].as_bool(), props["open-enabled"].as_bool()), (Some(true), Some(true), Some(true)));
    assert!(rig.saved_pngs().is_empty());
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.saved_pngs().len(), 1);
    // The photos themselves stay where they were taken
    assert_eq!(rig.pngs().len() - 1, 3);
}

#[test]
fn photo_cadence_is_a_minimum_from_start_to_start_with_no_overlap_and_no_catching_up() {
    let (mut rig, now) = Rig::photos("photos-cadence", json!({"mode": "live", "input": "photos", "photo_count": 3, "photo_every": 10}));
    let timer = || Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() };
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.taken(), 1);
    // The first photo took 4 s, so the next starts 6 s later and not before
    *now.lock().unwrap() = 104.0;
    rig.host.scheduled.clear();
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 1);
    assert!(rig.host.scheduled.contains(&(6000, NEXT_PHOTO.to_string())), "{:?}", rig.host.scheduled);
    assert_eq!(rig.status(), "Waiting for the next photo");
    *now.lock().unwrap() = 110.0;
    rig.event(timer());
    assert_eq!(rig.taken(), 2);
    // A timer that fires while a photo is on its way starts nothing
    rig.event(timer());
    rig.event(timer());
    assert_eq!(rig.taken(), 2, "never two at once");
    // The second took 25 s, longer than the interval: one photo follows at once and none for the ones that were due
    *now.lock().unwrap() = 135.0;
    rig.host.scheduled.clear();
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 3);
    assert!(!rig.host.scheduled.iter().any(|(_, t)| t == NEXT_PHOTO));
    *now.lock().unwrap() = 136.0;
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 3);
    assert_eq!(rig.plugin.count, 3);
}

#[test]
fn with_no_wait_the_next_photo_follows_the_last_at_once() {
    let (mut rig, _) = Rig::photos("photos-nowait", json!({"mode": "live", "input": "photos", "photo_count": 3}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 2);
    assert!(!rig.host.scheduled.iter().any(|(_, t)| t == NEXT_PHOTO));
}

#[test]
fn stop_cancels_the_photo_and_ignores_every_late_reply_but_keeps_the_stack() {
    let (mut rig, _) = Rig::photos("photos-stop", json!({"mode": "live", "input": "photos", "photo_count": 5}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    rig.photo_done(&field(2.0));
    assert_eq!(rig.plugin.count, 2);
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::RequestProgress { request, data: json!({"state": "capturing"}) });
    let run = rig.plugin.run;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert!(rig.plugin.taking.is_none() && rig.plugin.photo.is_none() && !rig.plugin.exposing);
    assert!(rig.host.calls.contains(&format!("cancel_still {}", request.0)));
    assert_eq!(rig.plugin.state_props(&rig.host)["run-text"], "Start");
    // The late reply of the photo that was cancelled finds nothing to do
    let path = rig.folder.join("late.jpg");
    std::fs::write(&path, jpeg_of(&field(2.0))).unwrap();
    rig.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"ok": true, "files": [path.display().to_string()]}) });
    assert_eq!((rig.taken(), rig.plugin.count), (3, 2));
    assert!(rig.plugin.last.is_empty() || !rig.plugin.last.ends_with("late.jpg"));
    // And so does a file that was already with the worker for the run that ended
    rig.plugin.feed.as_ref().unwrap().send(Msg::Photo { run, source: "phone".into(), path });
    rig.settle();
    assert_eq!(rig.plugin.count, 2);
    // What was stacked is still there to save
    assert_eq!(rig.plugin.state_props(&rig.host)["save-enabled"], true);
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.saved_pngs().len(), 1);
    assert!(rig.plugin.note.starts_with("Saved a stack of 2 frames"), "{}", rig.plugin.note);
}

#[test]
fn a_late_answer_for_an_older_run_is_not_this_runs() {
    let (mut rig, _) = Rig::photos("photos-older", json!({"mode": "live", "input": "photos", "photo_count": 4}));
    rig.click("run-clicked", Value::Null);
    let old = rig.plugin.run;
    rig.click("run-clicked", Value::Null);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.run, old + 1);
    let taken = rig.taken();
    rig.plugin.on_event(&mut rig.host, &Event::Posted { owner: NAME.into(), token: "photo".into(), data: json!({"run": old, "accepted": true}) });
    assert_eq!(rig.taken(), taken);
    assert_eq!(rig.plugin.photo.as_ref().unwrap().finished, 0);
}

#[test]
fn stopping_star_trails_of_photos_saves_what_is_there() {
    let (mut rig, _) = Rig::photos("photos-trails", json!({"mode": "trails", "input": "photos", "photo_count": 5}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&sky(SIZE, SIZE, &[(20.0, 48.0)], 1.5));
    rig.photo_done(&sky(SIZE, SIZE, &[(40.0, 48.0)], 1.5));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!((rig.plugin.count, rig.saved_pngs().len()), (2, 1));
    assert_eq!(rig.plugin.note, "Saved the trails");
    // The brightest of the photos: both stars are in it
    let trail = read_png(&rig.saved_pngs()[0], &[(20, 48), (40, 48)]);
    assert!(trail.iter().all(|v| *v > 0.5), "{trail:?}");
}

#[test]
fn a_photo_that_can_t_be_read_stops_the_run_with_the_stack_to_save() {
    let (mut rig, _) = Rig::photos("photos-broken", json!({"mode": "live", "input": "photos", "photo_count": 5}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    rig.photo_done(&field(2.0));
    let request = rig.plugin.taking.unwrap();
    let path = rig.folder.join("broken.jpg");
    std::fs::write(&path, b"no picture in here").unwrap();
    rig.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"ok": true, "files": [path.display().to_string()]}) });
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.problem.as_deref(), Some("Couldn't read the photo"));
    assert_eq!(rig.taken(), 3, "no photo after that");
    assert_eq!(rig.plugin.count, 2);
    assert_eq!(rig.props()["save-enabled"], true);
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.saved_pngs().len(), 1);
    assert_eq!(rig.plugin.problem.as_deref(), Some("Couldn't read the photo"), "saving doesn't hide why the run stopped");
    // The next run starts clean
    rig.click("run-clicked", Value::Null);
    assert!(rig.plugin.problem.is_none());
}

#[test]
fn a_photo_the_camera_fails_to_take_stops_the_run_and_a_busy_camera_is_waited_for() {
    let (mut rig, _) = Rig::photos("photos-failed", json!({"mode": "live", "input": "photos", "photo_count": 5}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::RequestDone { request, ok: false, message: String::new(), data: json!({"ok": false, "files": [], "error": "The phone disconnected"}) });
    assert_eq!(rig.plugin.problem.as_deref(), Some("The phone disconnected"));
    assert_eq!((rig.plugin.phase, rig.plugin.count), (Phase::Idle, 1));
    // A camera that is busy at the start leaves the card as it was
    rig.host.still_busy = true;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.problem.as_deref(), Some("Couldn't take the photo"));
    // One that gets busy between photos is tried again a second later
    rig.host.still_busy = false;
    rig.click("run-clicked", Value::Null);
    rig.host.still_busy = true;
    rig.host.scheduled.clear();
    rig.photo_done(&field(2.0));
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert!(rig.host.scheduled.contains(&(RETRY_MS, NEXT_PHOTO.to_string())));
    rig.host.still_busy = false;
    let before = rig.taken();
    rig.event(Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() });
    assert_eq!(rig.taken(), before + 1);
}

#[test]
fn a_hidden_picture_is_not_photographed_and_the_run_waits_for_it() {
    let (mut rig, _) = Rig::photos("photos-hidden", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.host.picture_hidden = true;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.taken(), 0, "nothing asked for while it is hidden");
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert!(rig.status().contains("hidden"), "{}", rig.status());
    assert!(rig.host.scheduled.contains(&(RETRY_MS, NEXT_PHOTO.to_string())));
    let timer = || Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() };
    rig.event(timer());
    assert_eq!(rig.taken(), 0);
    rig.host.picture_hidden = false;
    rig.event(timer());
    assert_eq!(rig.taken(), 1);
    // The host refusing a photo because the picture got hidden is no failure and no photo
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::RequestDone { request, ok: false, message: String::new(), data: json!({"ok": false, "files": [], "code": "hidden", "error": "hidden"}) });
    assert!(rig.plugin.problem.is_none());
    assert_eq!((rig.plugin.phase, rig.plugin.photo.as_ref().unwrap().finished), (Phase::Collecting, 0));
    rig.host.scheduled.clear();
    rig.event(timer());
    assert_eq!(rig.taken(), 2);
    // A picture covered while the photo exposed is dropped without being read
    rig.host.picture_hidden = true;
    let covered = rig.plugin.taking.map(|r| rig.folder.join(format!("photo_{}.jpg", r.0))).unwrap();
    rig.photo_done(&field(2.0));
    assert!(!covered.exists(), "a photo of a hidden picture doesn't stay on disk");
    assert_eq!(rig.plugin.count, 0);
    assert!(!rig.plugin.photo.as_ref().unwrap().handed);
    assert!(rig.host.scheduled.contains(&(RETRY_MS, NEXT_PHOTO.to_string())));
    assert!(rig.plugin.last.is_empty());
}

#[test]
fn the_worker_leaves_out_a_photo_that_arrives_while_the_picture_is_hidden() {
    let (mut rig, _) = Rig::photos("photos-worker-hidden", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.click("run-clicked", Value::Null);
    let path = rig.folder.join("covered.jpg");
    std::fs::create_dir_all(&rig.folder).unwrap();
    std::fs::write(&path, jpeg_of(&field(2.0))).unwrap();
    // Hidden after the plugin looked, which the host's own flag says first
    rig.plugin.shared.hidden.store(true, Ordering::SeqCst);
    let run = rig.plugin.run;
    rig.plugin.feed.as_ref().unwrap().send(Msg::Photo { run, source: "phone".into(), path: path.clone() });
    rig.settle();
    assert_eq!(rig.plugin.count, 0);
    assert!(!path.exists(), "the worker deletes what it leaves out for being hidden");
    assert_eq!(rig.plugin.photo.as_ref().unwrap().finished, 0);
    std::fs::write(&path, jpeg_of(&field(2.0))).unwrap();
    rig.plugin.shared.hidden.store(false, Ordering::SeqCst);
    let flag = Arc::new(AtomicBool::new(true));
    *rig.plugin.shared.host_hidden.lock().unwrap() = Some(flag.clone());
    rig.plugin.feed.as_ref().unwrap().send(Msg::Photo { run, source: "phone".into(), path: path.clone() });
    rig.settle();
    assert_eq!(rig.plugin.count, 0);
    assert!(!path.exists());
}

#[test]
fn a_stack_refuses_a_photo_it_cannot_line_up_and_the_run_goes_on() {
    let (mut rig, _) = Rig::photos("photos-refused", json!({"mode": "live", "input": "photos", "photo_count": 3}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    // Cloud: no stars to line up on
    rig.photo_done(&sky(SIZE, SIZE, &[], 2.0));
    assert_eq!((rig.plugin.count, rig.plugin.refused), (1, 1));
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    rig.photo_done(&field(2.0));
    assert_eq!((rig.plugin.count, rig.plugin.refused, rig.plugin.phase), (2, 1, Phase::Idle));
}

#[test]
fn a_big_photo_is_stacked_smaller_to_keep_to_the_byte_budget_and_the_card_says_the_size() {
    let (mut rig, _) = Rig::photos("photos-budget", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.plugin.shared.video_size.lock().unwrap().insert("phone".into(), (100, 100));
    rig.plugin.budget = 1_000_000;
    let stars: Vec<(f32, f32)> = SKY.iter().map(|(x, y)| (x * 2.0, y * 2.0)).collect();
    let big = sky(192, 192, &stars, 4.0);
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&big);
    assert_eq!(rig.plugin.limit, "Stacking at 96 x 96 to save memory");
    assert_eq!(rig.props()["limit"], rig.plugin.limit.as_str());
    rig.photo_done(&big);
    assert_eq!(rig.plugin.count, 2);
    rig.click("save-clicked", Value::Null);
    let png = tiny_skia::Pixmap::decode_png(&std::fs::read(&rig.saved_pngs()[0]).unwrap()).unwrap();
    assert_eq!((png.width(), png.height()), (96, 96));
}

#[test]
fn a_photo_that_fits_is_stacked_whole_and_says_so() {
    let (mut rig, _) = Rig::photos("photos-whole", json!({"mode": "trails", "input": "photos", "photo_count": 2}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.plugin.limit, "Stacking at 96 x 96");
    rig.click("run-clicked", Value::Null);
}

#[test]
fn a_photo_no_shrinking_makes_fit_stops_the_run_before_it_is_decoded() {
    let (mut rig, _) = Rig::photos("photos-too-big", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.plugin.shared.video_size.lock().unwrap().insert("phone".into(), (100, 100));
    rig.plugin.budget = 50_000;
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.problem.as_deref(), Some("The photo is too big to stack in the memory there is"));
    assert_eq!(rig.plugin.count, 0);
    assert_eq!(rig.taken(), 1);
}

#[test]
fn a_photo_of_another_size_ends_the_run_with_the_stack_it_has() {
    let (mut rig, _) = Rig::photos("photos-resized", json!({"mode": "live", "input": "photos", "photo_count": 4}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    rig.photo_done(&sky(80, 80, &SKY, 2.0));
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.count, 1);
    assert!(rig.status().starts_with("Stopped because the picture size changed"), "{}", rig.status());
}

#[test]
fn the_cameras_copy_of_a_photo_stack_is_the_size_of_the_video_and_goes_on_it() {
    let (mut rig, _) = Rig::photos("photos-preview", json!({"mode": "live", "input": "photos", "photo_count": 2, "camera": true}));
    rig.click("run-clicked", Value::Null);
    // The video's frames tell the worker the size, once
    let mut video = sky(160, 90, &SKY, 2.0);
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Reads);
    rig.step.process(&mut video, &Ctx("phone"));
    rig.settle();
    rig.photo_done(&field(2.0));
    {
        let result = rig.plugin.shared.result.lock().unwrap();
        let stack = &result.as_ref().expect("a stack for the camera").frame;
        assert_eq!((stack.width, stack.height, stack.format), (160, 90, PixelFormat::Rgba));
    }
    assert_eq!(rig.step.touch(&Ctx("phone")), Touch::Changes);
    let mut frame = sky(160, 90, &[], 2.0);
    rig.step.process(&mut frame, &Ctx("phone"));
    // The square stack sits in the middle with black either side
    let at = |x: usize, y: usize| frame.data[(y * 160 + x) * 4..(y * 160 + x) * 4 + 3].to_vec();
    assert_eq!(at(2, 45), vec![0, 0, 0]);
    assert!(at(80, 45).iter().any(|v| *v > 0), "{:?}", at(80, 45));
    // A video of another size tells the worker, which makes the copy again
    let mut other = sky(128, 72, &SKY, 2.0);
    rig.step.process(&mut other, &Ctx("phone"));
    rig.settle();
    let result = rig.plugin.shared.result.lock().unwrap();
    assert_eq!((result.as_ref().unwrap().frame.width, result.as_ref().unwrap().frame.height), (128, 72));
}

#[test]
fn a_photo_stack_isnt_put_on_the_camera_while_the_picture_is_hidden() {
    let (mut rig, _) = Rig::photos("photos-preview-hidden", json!({"mode": "live", "input": "photos", "photo_count": 2, "camera": true}));
    rig.plugin.shared.video_size.lock().unwrap().insert("phone".into(), (160, 90));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    assert!(rig.plugin.shared.result.lock().unwrap().is_some());
    *rig.plugin.shared.result.lock().unwrap() = None;
    rig.plugin.shared.hidden.store(true, Ordering::SeqCst);
    rig.plugin.feed.as_ref().unwrap().send(Msg::Publish);
    rig.settle();
    assert!(rig.plugin.shared.result.lock().unwrap().is_none());
    rig.plugin.shared.hidden.store(false, Ordering::SeqCst);
    rig.plugin.feed.as_ref().unwrap().send(Msg::Publish);
    rig.settle();
    assert!(rig.plugin.shared.result.lock().unwrap().is_some());
}

#[test]
fn missing_caps_disable_controls_and_a_missing_shutter_has_its_reason() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    let props = plugin.state_props(&host);
    assert_eq!(props["shot-summary"], "10 s · ISO 800 · focus ∞");
    assert_eq!(props["focus-readout"], "∞");
    assert_eq!(props["focus-enabled"], true);
    assert_eq!(props["shutter-enabled"], true);
    assert_eq!(props["iso-enabled"], true);
    host.still_caps = Some(json!({"shot": {"iso": {"min": 50, "max": 3200}}}));
    let props = plugin.state_props(&host);
    assert_eq!(props["focus-enabled"], false);
    assert_eq!(props["focus-tip"], "This camera can't set the focus for a photo.");
    // The unsupported override stays clearable
    assert_eq!(props["shutter-enabled"], true);
    assert_eq!(props["shutter-tip"], "Use the camera's own shutter speed.");
    assert_eq!(props["iso-enabled"], true);
    plugin.shot = ShotChoice::default();
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-enabled"], false);
    assert_eq!(props["shutter-tip"], "This camera can't set the shutter speed.");
    host.still_caps = Some(json!({}));
    assert_eq!(plugin.state_props(&host)["iso-tip"], "This camera can't set the ISO.");
    host.still_caps = None;
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-tip"], "Waiting for the picture");
    assert_eq!(props["iso-tip"], "Waiting for the picture");
    assert_eq!(props["run-enabled"], false);
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert!(host.stills_taken.is_empty());
}

#[test]
fn photos_keep_requested_overrides_for_validation_and_never_focus() {
    for caps in [
        json!({}),
        json!({"shot": {"iso": {"min": 50, "max": 3200}}}),
        json!({"shot": {"shutter": {"min_ns": 250_000, "max_ns": 10_000_000_000i64}}}),
    ] {
        let mut host = photo_host();
        host.still_caps = Some(caps);
        // A shot saved earlier is a pick by hand, so the camera is asked for it and may refuse
        let mut plugin = photo_plugin(&mut host, json!({"mode": "live", "input": "photos", "shot": {"shutter_s": 10.0, "iso": 800}}));
        photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
        assert_eq!(host.stills_taken[0].0["shot"], json!({"shutter_s": 10.0, "iso": 800}));
        assert!(host.stills_taken[0].0["shot"].get("focus_diopters").is_none());
        plugin.shutdown(&mut host);
    }
}

#[test]
fn camera_choices_save_but_the_disclosure_is_temporary_and_focus_is_locked() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    host.calls.clear();
    photo_ui(&mut plugin, &mut host, "shutter-picked", json!(0));
    photo_ui(&mut plugin, &mut host, "iso-picked", json!(0));
    assert_eq!(plugin.shot.shutter_s, None);
    assert_eq!(plugin.shot.iso, None);
    assert_eq!(host.calls.iter().filter(|c| c.as_str() == "schedule_save").count(), 2);
    photo_ui(&mut plugin, &mut host, "customize-toggled", json!(true));
    assert_eq!(plugin.state_props(&host)["customize-open"], true);
    assert!(plugin.get_config().get("customize-open").is_none());
    let mut restored = StargazerPlugin::new();
    restored.set_config(&plugin.get_config()).unwrap();
    assert!(!restored.customize_open);
    photo_ui(&mut plugin, &mut host, "settings-reset-clicked", Value::Null);
    assert_eq!(plugin.shot, StargazerPlugin::default_shot());
    assert_eq!((plugin.mode, plugin.input), (Mode::Live, Input::Photos), "the mode and where the frames come from stay");
}

#[test]
fn the_video_has_no_camera_choices() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, json!({"mode": "live"}));
    photo_ui(&mut plugin, &mut host, "iso-picked", json!(3));
    assert_eq!(plugin.shot, ShotChoice::default());
}

#[test]
fn photo_counts_and_waits_move_while_idle_only() {
    let (mut rig, _) = Rig::photos("photos-sliders", json!({"mode": "live", "input": "photos", "photo_count": 5}));
    rig.click("photo-count-moved", json!(40.0));
    rig.click("photo-every-moved", json!(12.0));
    assert_eq!((rig.plugin.photo_count, rig.plugin.photo_every), (40, 12));
    let props = rig.props();
    assert_eq!((props["photo-count-text"].as_str(), props["photo-every-text"].as_str()), (Some("40 photos"), Some("12 s")));
    rig.click("photo-count-moved", json!(1.0));
    rig.click("photo-every-moved", json!(0.0));
    assert_eq!(rig.props()["photo-count-text"], "1 photo");
    assert_eq!(rig.props()["photo-every-text"], "No wait");
    rig.click("photo-count-moved", json!(9999.0));
    assert_eq!(rig.plugin.photo_count, MAX_PHOTOS);
    rig.click("run-clicked", Value::Null);
    rig.click("photo-count-moved", json!(3.0));
    rig.click("photo-every-moved", json!(99.0));
    assert_eq!((rig.plugin.photo_count, rig.plugin.photo_every), (MAX_PHOTOS, 0));
}

#[test]
fn switching_the_source_starts_from_an_empty_stack() {
    let mut rig = Rig::new("switch-source", config("live"));
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    rig.feed(field(2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 2);
    rig.click("input-picked", json!(0));
    assert_eq!(rig.plugin.count, 2, "picking what is picked changes nothing");
    rig.host.still_caps = photo_host().still_caps;
    rig.host.still_only_for = Some("phone".into());
    rig.click("input-picked", json!(1));
    assert_eq!((rig.plugin.input, rig.plugin.count), (Input::Photos, 0));
    assert!(rig.plugin.get_config().get("shot").is_some());
}

#[test]
fn a_live_stack_of_video_frames_isnt_carried_into_photos() {
    let mut rig = Rig::new("stack-input", config("live"));
    rig.click("run-clicked", Value::Null);
    rig.feed(field(2.0));
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 1);
    rig.host.still_caps = photo_host().still_caps;
    rig.host.still_only_for = Some("phone".into());
    // Not through the card, which would empty it: a preset that was recalled while the stack stood
    rig.plugin.input = Input::Photos;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.count, 0, "a stack of video frames doesn't go on with photos");
    rig.click("run-clicked", Value::Null);
}

#[test]
fn dslr_photos_are_asked_for_with_discrete_shot_caps_and_no_quality() {
    let mut host = photo_host();
    host.still_caps = Some(json!({"camera": true, "formats": ["jpeg"], "shot": {
        "shutter_values_s": [0.004, 10.0, 30.0], "iso_values": [100, 800, 1600]
    }}));
    let mut plugin = StargazerPlugin::new();
    plugin.setup(&mut host);
    photo_ui(&mut plugin, &mut host, "mode-picked", json!(1));
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!(plugin.input, Input::Photos);
    assert_eq!(plugin.state_props(&host)["run-enabled"], true);
    let props = plugin.shot_props(&host);
    assert_eq!(props["shutter-enabled"], true);
    assert_eq!(props["iso-enabled"], true);
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.len(), 1);
    assert_eq!(host.stills_taken[0].0, json!({"format": "jpeg", "rotation": 0, "use_zoom": false, "shot": {"shutter_s": 10.0, "iso": 800}, "skip_if_hidden": true}));
    assert_eq!(plugin.phase, Phase::Collecting);
    plugin.shutdown(&mut host);
}

#[test]
fn the_dials_pick_their_own_values_and_lock_during_the_run() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    let props = plugin.state_props(&host);
    let shutter_index = props["shutter-stops"].as_array().unwrap().iter().position(|v| v == "20 s").unwrap();
    let iso_index = props["iso-stops"].as_array().unwrap().iter().position(|v| v == "1600").unwrap();
    photo_ui(&mut plugin, &mut host, "shutter-picked", json!(shutter_index));
    photo_ui(&mut plugin, &mut host, "iso-picked", json!(iso_index));
    assert_eq!(plugin.shot.shutter_s, Some(20.0));
    assert_eq!(plugin.shot.iso, Some(1600));
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    photo_ui(&mut plugin, &mut host, "shutter-picked", json!(0));
    photo_ui(&mut plugin, &mut host, "iso-picked", json!(0));
    assert_eq!(plugin.shot.shutter_s, Some(20.0));
    assert_eq!(plugin.shot.iso, Some(1600));
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-enabled"], false);
    assert_eq!(props["iso-enabled"], false);
    plugin.shutdown(&mut host);
}

#[test]
fn the_focus_slider_sets_the_photos_focus_where_the_camera_takes_one() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    assert_eq!(plugin.shot_props(&host)["focus-enabled"], true);
    photo_ui(&mut plugin, &mut host, "focus-moved", json!(250.0));
    assert_eq!(plugin.shot.focus_diopters, Some(2.5));
    assert_eq!(plugin.shot_props(&host)["focus-value"], json!(250.0));
    photo_ui(&mut plugin, &mut host, "focus-reset", Value::Null);
    assert_eq!(plugin.shot.focus_diopters, None);
    assert_eq!(plugin.shot_props(&host)["focus-readout"], "Camera");
    photo_ui(&mut plugin, &mut host, "focus-moved", json!(0.0));
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.last().unwrap().0["shot"]["focus_diopters"], json!(0.0));
    photo_ui(&mut plugin, &mut host, "focus-moved", json!(500.0));
    assert_eq!(plugin.shot.focus_diopters, Some(0.0), "the focus is held during a run");
    assert_eq!(plugin.shot_props(&host)["focus-enabled"], false);
    plugin.shutdown(&mut host);
}

#[test]
fn a_camera_without_a_focus_for_one_photo_keeps_its_own() {
    let mut host = photo_host();
    host.still_caps = Some(json!({"shot": {"shutter": {"min_ns": 250_000, "max_ns": 20_000_000_000i64}, "iso": {"min": 50, "max": 3200}}}));
    let mut plugin = photo_plugin(&mut host, live_photos());
    assert_eq!(plugin.shot.focus_diopters, Some(0.0), "the saved choice stays for a phone that takes it");
    let props = plugin.shot_props(&host);
    assert_eq!((props["focus-enabled"].clone(), props["focus-readout"].clone()), (json!(false), json!("Camera")));
    photo_ui(&mut plugin, &mut host, "focus-moved", json!(500.0));
    assert_eq!(plugin.shot.focus_diopters, Some(0.0));
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert!(host.stills_taken.last().unwrap().0["shot"].get("focus_diopters").is_none(), "an older phone would refuse the photo");
    plugin.shutdown(&mut host);
}

#[test]
fn a_late_restore_failure_is_visible_after_stop() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    plugin.on_event(&mut host, &Event::RequestProgress { request: RequestId(99), data: json!({"state": "late", "code": "restore", "error": "Restore failed"}) });
    assert_eq!(plugin.problem.as_deref(), Some("Restore failed"));
    assert_eq!(plugin.state_props(&host)["status-kind"], "status_err");
}

#[test]
fn use_zoom_defaults_off_survives_a_preset_and_is_pinned_by_the_run() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    assert_eq!(plugin.get_config()["use_zoom"], false);
    assert_eq!(plugin.state_props(&host)["use-zoom"], false);
    photo_ui(&mut plugin, &mut host, "use-zoom-toggled", json!(true));
    let preset = plugin.preset_config();
    assert_eq!(preset["use_zoom"], true);
    let mut restored = StargazerPlugin::new();
    restored.setup(&mut host);
    restored.apply_preset(&mut host, &preset).unwrap();
    photo_ui(&mut restored, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.last().unwrap().0["use_zoom"], true);
    photo_ui(&mut restored, &mut host, "use-zoom-toggled", json!(false));
    assert_eq!(restored.get_config()["use_zoom"], true, "a running photo keeps its settings");
    restored.shutdown(&mut host);
}

#[test]
fn shutting_the_plugin_down_during_a_photo_run_cancels_the_photo() {
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, live_photos());
    photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    let request = plugin.taking.unwrap();
    plugin.shutdown(&mut host);
    assert!(host.calls.contains(&format!("cancel_still {}", request.0)));
    assert!(plugin.photo.is_none() && plugin.taking.is_none());
    assert!(!host.scheduled.iter().any(|(_, t)| t == NEXT_PHOTO));
}

#[test]
fn the_video_stream_stopping_ends_a_photo_run_the_way_stop_does() {
    let (mut rig, _) = Rig::photos("photos-stream-stop", json!({"mode": "live", "input": "photos", "photo_count": 5}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::StreamStopped);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert!(rig.host.calls.contains(&format!("cancel_still {}", request.0)));
    assert_eq!(rig.plugin.count, 1);
}

#[test]
fn a_dslr_photo_whose_settings_didnt_come_back_is_stacked_and_the_run_goes_on() {
    let (mut rig, _) = Rig::photos("photos-restore", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.click("run-clicked", Value::Null);
    let request = rig.plugin.taking.unwrap();
    std::fs::create_dir_all(&rig.folder).unwrap();
    let path = rig.folder.join("restore.jpg");
    std::fs::write(&path, jpeg_of(&field(2.0))).unwrap();
    let data = json!({"ok": false, "code": "restore", "files": [path.display().to_string()], "error": "The camera's photo settings could not be restored. Check its ISO and shutter speed."});
    rig.event(Event::RequestDone { request, ok: false, message: String::new(), data });
    assert_eq!((rig.plugin.count, rig.plugin.phase), (1, Phase::Collecting));
    assert!(rig.plugin.problem.is_none());
    assert_eq!(rig.taken(), 2, "the run goes on");
    assert!(path.exists());
    assert_eq!(rig.status(), "The camera's photo settings could not be restored. Check its ISO and shutter speed.");
    // The next photo's clean answer ends the run, and a new run starts without the warning
    rig.photo_done(&field(2.0));
    assert_eq!((rig.plugin.count, rig.plugin.phase), (2, Phase::Idle));
    rig.click("run-clicked", Value::Null);
    assert!(rig.plugin.warning.is_none());
    assert_eq!(rig.status(), "Taking photo 1 of 2");
}

#[test]
fn a_run_stops_when_the_disk_is_low_and_a_start_is_refused() {
    let (mut rig, _) = Rig::photos("photos-disk", json!({"mode": "live", "input": "photos", "photo_count": 4}));
    let free = Arc::new(AtomicU64::new(50 << 30));
    let reading = free.clone();
    rig.plugin.free = Box::new(move |_| Some(reading.load(Ordering::SeqCst)));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 2);
    free.store(100 << 20, Ordering::SeqCst);
    rig.photo_done(&field(2.0));
    assert_eq!(rig.taken(), 2, "no photo with no room");
    assert_eq!((rig.plugin.phase, rig.plugin.count), (Phase::Idle, 2));
    assert_eq!(rig.plugin.problem.as_deref(), Some("Stopped because there's less than 1 GB free on the drive"));
    assert_eq!(rig.props()["save-enabled"], true);
    // And nothing starts while it is still low
    rig.click("run-clicked", Value::Null);
    assert_eq!((rig.plugin.phase, rig.taken()), (Phase::Idle, 2));
    assert_eq!(rig.plugin.problem.as_deref(), Some("Stopped because there's less than 1 GB free on the drive"));
    free.store(50 << 30, Ordering::SeqCst);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
}

#[test]
fn a_photo_nothing_can_line_up_on_is_the_base_of_a_single_photo_stack() {
    // A migrated Night shot: one dark photo with nothing in it to line up on
    let (mut rig, _) = Rig::photos("photos-night", json!({"mode": "night"}));
    rig.click("run-clicked", Value::Null);
    rig.photo_done(&sky(SIZE, SIZE, &[], 2.0));
    assert_eq!((rig.plugin.count, rig.plugin.refused, rig.plugin.phase), (1, 0, Phase::Idle));
    assert_eq!(rig.status(), "Stacked 1 photo. Save keeps it, Reset starts a new stack");
    assert_eq!(rig.props()["save-enabled"], true);
    rig.click("save-clicked", Value::Null);
    assert_eq!(rig.saved_pngs().len(), 1);
    // The video's stack still wants a first frame it can line up on
    let mut video = Rig::new("photos-night-video", json!({"mode": "live"}));
    video.click("run-clicked", Value::Null);
    video.feed(sky(SIZE, SIZE, &[], 2.0));
    assert_eq!(video.plugin.count, 0);
}

#[test]
fn a_hidden_photo_is_tried_again_no_sooner_than_every_allows() {
    let (mut rig, now) = Rig::photos("photos-hidden-every", json!({"mode": "live", "input": "photos", "photo_count": 2, "photo_every": 20}));
    rig.click("run-clicked", Value::Null);
    *now.lock().unwrap() = 105.0;
    rig.host.scheduled.clear();
    let request = rig.plugin.taking.unwrap();
    rig.event(Event::RequestDone { request, ok: false, message: String::new(), data: json!({"ok": false, "files": [], "code": "hidden", "error": "hidden"}) });
    assert!(rig.host.scheduled.contains(&(15_000, NEXT_PHOTO.to_string())), "{:?}", rig.host.scheduled);
    // The picture still hidden then: another look a second later, never before the gap is up
    *now.lock().unwrap() = 125.0;
    rig.host.picture_hidden = true;
    rig.host.scheduled.clear();
    rig.event(Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() });
    assert_eq!(rig.taken(), 1);
    assert!(rig.host.scheduled.contains(&(RETRY_MS, NEXT_PHOTO.to_string())), "{:?}", rig.host.scheduled);
    // And a photo covered while it exposed waits out the rest of the gap too
    rig.host.picture_hidden = false;
    rig.event(Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() });
    assert_eq!(rig.taken(), 2);
    *now.lock().unwrap() = 130.0;
    rig.host.picture_hidden = true;
    rig.host.scheduled.clear();
    rig.photo_done(&field(2.0));
    assert!(rig.host.scheduled.contains(&(15_000, NEXT_PHOTO.to_string())), "{:?}", rig.host.scheduled);
}

#[test]
fn a_start_right_after_stop_waits_for_the_camera_the_cancelled_photo_still_holds() {
    let (mut rig, now) = Rig::photos("photos-restart", json!({"mode": "live", "input": "photos", "photo_count": 2}));
    rig.click("run-clicked", Value::Null);
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    // The camera is still on the cancelled photo
    rig.host.still_busy = true;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Collecting);
    assert!(rig.plugin.problem.is_none());
    assert!(rig.host.scheduled.contains(&(RETRY_MS, NEXT_PHOTO.to_string())));
    let before = rig.taken();
    rig.host.still_busy = false;
    *now.lock().unwrap() = 102.0;
    rig.event(Event::TimerFired { owner: NAME.into(), token: NEXT_PHOTO.into() });
    assert_eq!(rig.taken(), before + 1);
    assert!(rig.plugin.taking.is_some());
    // A camera that stays busy long after is a failure again
    rig.click("run-clicked", Value::Null);
    *now.lock().unwrap() = 500.0;
    rig.host.still_busy = true;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    assert_eq!(rig.plugin.problem.as_deref(), Some("Couldn't take the photo"));
}

#[test]
fn progress_from_before_an_input_change_or_a_reset_is_ignored() {
    let mut rig = Rig::new("stale", config("live"));
    rig.click("run-clicked", Value::Null);
    for _ in 0..6 {
        rig.feed(field(2.0));
    }
    let old = rig.plugin.run;
    rig.click("run-clicked", Value::Null);
    assert_eq!(rig.plugin.phase, Phase::Idle);
    let stale = |rig: &mut Rig, run: u64| rig.event(Event::Posted { owner: NAME.into(), token: "progress".into(), data: json!({"run": run, "collected": 6, "target": 50, "count": 6, "refused": 0}) });
    rig.click("input-picked", json!(1));
    assert_eq!(rig.plugin.count, 0);
    stale(&mut rig, old);
    assert_eq!(rig.plugin.count, 0);
    assert_eq!(rig.props()["save-enabled"], false);
    let now = rig.plugin.run;
    rig.click("reset-clicked", Value::Null);
    assert!(rig.plugin.run > now);
    stale(&mut rig, now);
    assert_eq!(rig.plugin.count, 0);
    assert_eq!(rig.props()["save-enabled"], false);
}

#[test]
fn unsupported_shutter_and_iso_overrides_can_be_set_back_to_the_camera() {
    let mut plugin = StargazerPlugin::new();
    plugin.input = Input::Photos;
    plugin.shot = ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: None };
    let shutter = plugin.shutter_props(None);
    let iso = plugin.iso_props(None);
    assert_eq!((shutter["shutter-enabled"].clone(), iso["iso-enabled"].clone()), (json!(true), json!(true)));
    assert_eq!((shutter["shutter-index"].clone(), iso["iso-index"].clone()), (json!(1), json!(1)));
    let mut host = FakeHost::new();
    plugin.pick_shot(&mut host, "shutter-picked", 0);
    plugin.pick_shot(&mut host, "iso-picked", 0);
    assert_eq!(plugin.shot, ShotChoice::default());
    assert_eq!(plugin.shutter_props(None)["shutter-enabled"], false);
}

fn clean_calls(host: &FakeHost) -> Vec<String> {
    host.calls.iter().filter(|c| c.contains("clean_video")).cloned().collect()
}

#[test]
fn a_video_run_asks_for_cleaner_video_and_gives_it_back() {
    let mut rig = Rig::new("clean", config("live"));
    rig.host.phone_state = Some(json!({"clean_video_toggle": true}));
    rig.click("run-clicked", Value::Null);
    assert_eq!(clean_calls(&rig.host), [r#"phone_control Some("phone") {"action":"clean_video","value":1}"#]);
    rig.click("run-clicked", Value::Null);
    assert_eq!(clean_calls(&rig.host).len(), 2);
    assert!(clean_calls(&rig.host)[1].ends_with(r#"{"action":"clean_video","value":0}"#));
}

#[test]
fn an_older_phone_isnt_asked_for_cleaner_video() {
    let mut rig = Rig::new("clean-old", config("live"));
    rig.host.phone_state = Some(json!({}));
    rig.click("run-clicked", Value::Null);
    rig.click("run-clicked", Value::Null);
    assert!(clean_calls(&rig.host).is_empty());
}

#[test]
fn a_stream_that_stops_ends_cleaner_video_with_the_run() {
    let mut rig = Rig::new("clean-stop", config("trails"));
    rig.host.phone_state = Some(json!({"clean_video_toggle": true}));
    rig.click("run-clicked", Value::Null);
    rig.plugin.on_event(&mut rig.host, &Event::StreamStopped);
    assert!(clean_calls(&rig.host).last().unwrap().ends_with(r#"{"action":"clean_video","value":0}"#));
    rig.plugin.shutdown(&mut rig.host);
    assert_eq!(clean_calls(&rig.host).len(), 2, "nothing more to give back");
}

#[test]
fn the_first_photos_default_gives_way_to_camera_on_a_dial_the_camera_cant_set() {
    for (caps, shutter, iso) in [
        (json!({}), None, None),
        (json!({"shot": {"iso": {"min": 50, "max": 3200}}}), None, Some(800)),
        (json!({"shot": {"shutter": {"min_ns": 250_000, "max_ns": 20_000_000_000i64}}}), Some(10.0), None),
    ] {
        let mut host = photo_host();
        host.still_caps = Some(caps);
        let mut plugin = photo_plugin(&mut host, json!({"mode": "live"}));
        photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
        assert_eq!((plugin.shot.shutter_s, plugin.shot.iso), (shutter, iso));
        photo_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
        assert_eq!(plugin.phase, Phase::Collecting);
        let options = &host.stills_taken[0].0;
        assert_eq!(options["shot"].get("shutter_s").and_then(Value::as_f64), shutter);
        assert_eq!(options["shot"].get("iso").and_then(Value::as_i64), iso);
    }
    // A camera that can set both keeps the long exposure
    let mut host = photo_host();
    let mut plugin = photo_plugin(&mut host, json!({"mode": "live"}));
    photo_ui(&mut plugin, &mut host, "input-picked", json!(1));
    assert_eq!((plugin.shot.shutter_s, plugin.shot.iso), (Some(10.0), Some(800)));
}

#[test]
fn stacks_saved_at_the_same_moment_never_replace_each_other() {
    let dir = telescope_core::testing::scratch_dir("stargazer-same-moment");
    let frame = Frame::new(4, 4, PixelFormat::Rgba, vec![200; 4 * 4 * 4]);
    let threads = 16;
    for _ in 0..20 {
        let gate = std::sync::Barrier::new(threads);
        let before = std::fs::read_dir(&dir).unwrap().count();
        let paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..threads).map(|_| scope.spawn(|| {
                gate.wait();
                save_png(&frame, &dir).unwrap()
            })).collect();
            jobs.into_iter().map(|j| j.join().unwrap()).collect()
        });
        let distinct: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(distinct.len(), threads, "two saves got the same file");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count() - before, threads, "a save replaced another");
    }
}
