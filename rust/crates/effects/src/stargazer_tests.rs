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
    let saved = json!({"mode": "trails", "frames": 800, "keep": 25, "interval": 12, "camera": true, "use_zoom": false, "shot": {"shutter_s": 20.0, "iso": 1600}});
    p.set_config(&saved).unwrap();
    assert_eq!(p.get_config(), saved);
    assert!(p.shared.camera.load(Ordering::SeqCst));
    assert_eq!(p.shared.interval_ms.load(Ordering::SeqCst), 12_000);
    // Out of range or nonsense falls back or is clamped, and a run is never part of it
    p.set_config(&json!({"mode": "nonsense", "frames": 5, "keep": 500, "interval": "soon"})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "frames": 50, "keep": 100, "interval": 5, "camera": false, "use_zoom": false}));
    p.set_config(&json!({"frames": 99999})).unwrap();
    assert_eq!(p.get_config()["frames"], 2000);
    p.set_config(&json!({})).unwrap();
    assert_eq!(p.get_config(), json!({"mode": "lucky", "frames": 300, "keep": 10, "interval": 5, "camera": false, "use_zoom": false}));
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

fn night_host() -> FakeHost {
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

fn night_ui(plugin: &mut StargazerPlugin, host: &mut FakeHost, callback: &str, arg: Value) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
}

fn night_plugin(host: &mut FakeHost) -> StargazerPlugin {
    let mut plugin = StargazerPlugin::new();
    plugin.set_config(&json!({"mode": "night"})).unwrap();
    plugin.setup(host);
    plugin
}

#[test]
fn night_is_the_fourth_mode() {
    assert_eq!(Mode::ALL, [Mode::Lucky, Mode::Live, Mode::Trails, Mode::Night]);
    assert_eq!(Mode::Night.index(), 3);
    assert_eq!(Mode::from_key("night"), Some(Mode::Night));
    assert_eq!(StargazerPlugin::fixed_props()["mode-items"], json!(["Lucky", "Live", "Trails", "Night"]));
}

#[test]
fn the_first_night_pick_saves_defaults_and_never_replaces_saved_choices() {
    let mut host = night_host();
    let mut plugin = StargazerPlugin::new();
    plugin.set_config(&json!({})).unwrap();
    plugin.setup(&mut host);
    assert!(plugin.get_config().get("shot").is_none());
    host.calls.clear();
    night_ui(&mut plugin, &mut host, "mode-picked", json!(3));
    assert_eq!(plugin.get_config()["shot"], json!({"shutter_s": 10.0, "iso": 800, "focus_diopters": 0.0}));
    assert!(host.calls.iter().any(|c| c == "schedule_save"));
    assert!(plugin.worker.is_none());
    let saved = json!({"iso": 1600});
    plugin.set_config(&json!({"shot": saved})).unwrap();
    night_ui(&mut plugin, &mut host, "mode-picked", json!(3));
    assert_eq!(plugin.get_config()["shot"], saved);
    let mut restored = StargazerPlugin::new();
    restored.set_config(&plugin.get_config()).unwrap();
    assert_eq!(restored.get_config(), plugin.get_config());
    assert_eq!(restored.shot.shutter_s, None);
    assert_eq!(restored.shot.focus_diopters, None);
}

#[test]
fn night_takes_one_jpeg_from_the_focused_stream_without_the_worker() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    assert_eq!(plugin.state_props(&host)["status"], "Ready");
    assert!(plugin.frame_step().is_none());
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.len(), 1);
    assert_eq!(host.stills_taken[0], (json!({"format": "jpeg", "quality": 95, "rotation": 0, "use_zoom": false,
        "shot": {"shutter_s": 10.0, "iso": 800}, "skip_if_hidden": true}), plugin.folder.clone()));
    assert_eq!(host.still_sources, vec![Some("phone".to_string())]);
    assert_eq!(plugin.phase, Phase::Collecting);
    assert!(plugin.taking.is_some());
    assert!(plugin.worker.is_none() && plugin.feed.is_none());
    assert!(!host.frames_wanted.contains(NAME));
    assert_eq!(plugin.state_props(&host)["run-text"], "Stop");
    assert_eq!(plugin.state_props(&host)["run-enabled"], true);
    assert_eq!(plugin.state_props(&host)["status"], "Taking the photo");
    night_ui(&mut plugin, &mut host, "mode-picked", json!(0));
    assert_eq!(plugin.mode, Mode::Night);
}

#[test]
fn night_follows_its_own_progress_and_saved_file() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    let request = plugin.taking.unwrap();
    plugin.on_event(&mut host, &Event::RequestProgress { request: RequestId(request.0 + 1), data: json!({"state": "capturing"}) });
    assert_eq!(plugin.state_props(&host)["status"], "Taking the photo");
    plugin.on_event(&mut host, &Event::RequestProgress { request, data: json!({"state": "capturing"}) });
    assert_eq!(plugin.state_props(&host)["status"], "Video paused while exposing");
    plugin.on_event(&mut host, &Event::RequestDone { request: RequestId(request.0 + 1), ok: false, message: String::new(), data: json!({}) });
    assert_eq!(plugin.taking, Some(request));
    plugin.on_event(&mut host, &Event::RequestDone { request, ok: true, message: String::new(), data: json!({"ok": true, "files": ["sky.jpg"], "applied": {"shutter_s": 10.0, "iso": 800}}) });
    assert_eq!(plugin.last, "sky.jpg");
    assert_eq!(plugin.note, "Saved the photo");
    assert_eq!(plugin.phase, Phase::Idle);
    assert!(plugin.taking.is_none());
    assert_eq!(plugin.state_props(&host)["run-enabled"], true);
    assert_eq!(plugin.state_props(&host)["open-enabled"], true);
    assert_eq!(plugin.tally(), "");
    assert!(!host.frames_wanted.contains(NAME));
}

#[test]
fn night_shows_the_failure_and_can_try_again() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    for (data, expected) in [(json!({"error": "The phone disconnected"}), "The phone disconnected"), (json!({}), "Couldn't take the photo")] {
        night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
        let request = plugin.taking.unwrap();
        plugin.on_event(&mut host, &Event::RequestDone { request, ok: false, message: String::new(), data });
        assert_eq!(plugin.problem.as_deref(), Some(expected));
        assert_eq!(plugin.phase, Phase::Idle);
        assert!(plugin.taking.is_none());
        assert_eq!(plugin.state_props(&host)["run-enabled"], true);
    }
    host.still_busy = true;
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(plugin.phase, Phase::Idle);
    assert!(plugin.taking.is_none());
    assert_eq!(plugin.problem.as_deref(), Some("Couldn't take the photo"));
}

#[test]
fn night_keeps_requested_overrides_for_validation_and_never_focus() {
    for caps in [
        json!({}),
        json!({"shot": {"iso": {"min": 50, "max": 3200}}}),
        json!({"shot": {"shutter": {"min_ns": 250_000, "max_ns": 10_000_000_000i64}}}),
    ] {
        let mut host = night_host();
        host.still_caps = Some(caps);
        let mut plugin = night_plugin(&mut host);
        night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
        assert_eq!(host.stills_taken[0].0["shot"], json!({"shutter_s": 10.0, "iso": 800}));
        assert!(host.stills_taken[0].0["shot"].get("focus_diopters").is_none());
    }
}

#[test]
fn missing_caps_disable_controls_and_a_missing_shutter_has_its_reason() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    let props = plugin.state_props(&host);
    assert_eq!(props["shot-summary"], "10 s · ISO 800 · focus ∞");
    assert_eq!(props["focus-readout"], "∞");
    assert_eq!(props["focus-enabled"], false);
    assert_eq!(props["shutter-enabled"], true);
    assert_eq!(props["iso-enabled"], true);
    host.still_caps = Some(json!({"shot": {"iso": {"min": 50, "max": 3200}}}));
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-enabled"], false);
    assert_eq!(props["shutter-tip"], "This camera can't set the shutter speed.");
    assert_eq!(props["iso-enabled"], true);
    host.still_caps = Some(json!({}));
    assert_eq!(plugin.state_props(&host)["iso-tip"], "This camera can't set the ISO.");
    host.still_caps = None;
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-tip"], "Waiting for the picture");
    assert_eq!(props["iso-tip"], "Waiting for the picture");
    assert_eq!(props["run-enabled"], false);
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert!(host.stills_taken.is_empty());
}

#[test]
fn camera_choices_save_but_the_disclosure_is_temporary_and_focus_is_locked() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    host.calls.clear();
    night_ui(&mut plugin, &mut host, "shutter-picked", json!(0));
    night_ui(&mut plugin, &mut host, "iso-picked", json!(0));
    assert_eq!(plugin.shot.shutter_s, None);
    assert_eq!(plugin.shot.iso, None);
    assert_eq!(host.calls.iter().filter(|c| c.as_str() == "schedule_save").count(), 2);
    night_ui(&mut plugin, &mut host, "customize-toggled", json!(true));
    assert_eq!(plugin.state_props(&host)["customize-open"], true);
    assert!(plugin.get_config().get("customize-open").is_none());
    let mut restored = StargazerPlugin::new();
    restored.set_config(&plugin.get_config()).unwrap();
    assert!(!restored.customize_open);
    night_ui(&mut plugin, &mut host, "settings-reset-clicked", Value::Null);
    assert_eq!(plugin.shot, StargazerPlugin::default_shot());
    assert_eq!(plugin.mode, Mode::Night);
}

#[test]
fn presets_carry_shot_choices_and_older_presets_keep_them() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    plugin.apply_preset(&mut host, &json!({"shot": {"iso": 1600}})).unwrap();
    assert_eq!(plugin.preset_config()["shot"], json!({"iso": 1600}));
    plugin.apply_preset(&mut host, &json!({"frames": 700})).unwrap();
    assert_eq!(plugin.shot, ShotChoice { iso: Some(1600), ..ShotChoice::default() });
    assert_eq!(plugin.frames, 700);
    assert_eq!(plugin.phase, Phase::Idle);
}

#[test]
fn leaving_night_refreshes_the_frame_step_for_video_modes() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    assert!(plugin.frame_step().is_none());
    host.calls.clear();
    night_ui(&mut plugin, &mut host, "mode-picked", json!(0));
    assert!(host.calls.iter().any(|c| c == "refresh_frame_step"));
    assert!(plugin.frame_step().is_some());
    plugin.shutdown(&mut host);
}

#[test]
fn the_dials_pick_their_own_values_and_lock_during_the_photo() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    let props = plugin.state_props(&host);
    let shutter_index = props["shutter-stops"].as_array().unwrap().iter().position(|v| v == "20 s").unwrap();
    let iso_index = props["iso-stops"].as_array().unwrap().iter().position(|v| v == "1600").unwrap();
    night_ui(&mut plugin, &mut host, "shutter-picked", json!(shutter_index));
    night_ui(&mut plugin, &mut host, "iso-picked", json!(iso_index));
    assert_eq!(plugin.shot.shutter_s, Some(20.0));
    assert_eq!(plugin.shot.iso, Some(1600));
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    night_ui(&mut plugin, &mut host, "shutter-picked", json!(0));
    night_ui(&mut plugin, &mut host, "iso-picked", json!(0));
    assert_eq!(plugin.shot.shutter_s, Some(20.0));
    assert_eq!(plugin.shot.iso, Some(1600));
    let props = plugin.state_props(&host);
    assert_eq!(props["shutter-enabled"], false);
    assert_eq!(props["iso-enabled"], false);
}

#[test]
fn dslr_night_takes_one_photo_with_discrete_shot_caps() {
    let mut host = night_host();
    host.still_caps = Some(json!({"camera": true, "formats": ["jpeg"], "shot": {
        "shutter_values_s": [0.004, 10.0, 30.0], "iso_values": [100, 800, 1600]
    }}));
    let mut plugin = StargazerPlugin::new();
    plugin.setup(&mut host);
    night_ui(&mut plugin, &mut host, "mode-picked", json!(3));
    assert_eq!(plugin.mode, Mode::Night);
    assert_eq!(plugin.state_props(&host)["run-enabled"], true);
    let props = plugin.shot_props(&host);
    assert_eq!(props["shutter-enabled"], true);
    assert_eq!(props["iso-enabled"], true);
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.len(), 1);
    assert_eq!(host.stills_taken[0].0, json!({"format": "jpeg", "rotation": 0, "use_zoom": false, "shot": {"shutter_s": 10.0, "iso": 800}, "skip_if_hidden": true}));
    assert_eq!(plugin.phase, Phase::Collecting);
    assert!(plugin.worker.is_none());
}

#[test]
fn stopping_a_night_shot_cancels_it_and_ignores_its_late_reply() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    let request = plugin.taking.unwrap();
    plugin.on_event(&mut host, &Event::RequestProgress { request, data: json!({"state": "capturing"}) });
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    assert_eq!(plugin.phase, Phase::Idle);
    assert!(plugin.taking.is_none());
    assert!(!plugin.exposing);
    assert!(host.calls.contains(&format!("cancel_still {}", request.0)));
    assert_eq!(plugin.state_props(&host)["run-text"], "Start");
    assert!(plugin.worker.is_none());
    night_ui(&mut plugin, &mut host, "run-clicked", Value::Null);
    let next = plugin.taking;
    plugin.on_event(&mut host, &Event::RequestDone { request, ok: true, message: String::new(), data: json!({"ok": true, "files": ["cancelled.jpg"]}) });
    assert_eq!(plugin.taking, next);
    assert_eq!(plugin.phase, Phase::Collecting);
    assert!(plugin.last.is_empty());
    plugin.shutdown(&mut host);
    assert!(host.calls.contains(&format!("cancel_still {}", next.unwrap().0)));
}

#[test]
fn a_late_night_restore_failure_is_visible_after_stop() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    plugin.on_event(&mut host, &Event::RequestProgress { request: RequestId(99), data: json!({"state": "late", "code": "restore", "error": "Restore failed"}) });
    assert_eq!(plugin.problem.as_deref(), Some("Restore failed"));
    assert_eq!(plugin.state_props(&host)["status-kind"], "status_err");
}

#[test]
fn night_use_zoom_defaults_off_and_survives_a_preset() {
    let mut host = night_host();
    let mut plugin = night_plugin(&mut host);
    assert_eq!(plugin.get_config()["use_zoom"], false);
    assert_eq!(plugin.state_props(&host)["use-zoom"], false);
    night_ui(&mut plugin, &mut host, "use-zoom-toggled", json!(true));
    let preset = plugin.preset_config();
    assert_eq!(preset["use_zoom"], true);
    let mut restored = StargazerPlugin::new();
    restored.apply_preset(&mut host, &preset).unwrap();
    night_ui(&mut restored, &mut host, "run-clicked", Value::Null);
    assert_eq!(host.stills_taken.last().unwrap().0["use_zoom"], true);
    night_ui(&mut restored, &mut host, "use-zoom-toggled", json!(false));
    assert_eq!(restored.get_config()["use_zoom"], true, "a running photo keeps its settings");
}
