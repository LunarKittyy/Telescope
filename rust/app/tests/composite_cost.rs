// One picture under load through real stream workers, one thread each: `cargo test --release -p telescope-app --test composite_cost -- --ignored --nocapture --test-threads=1` (COMPOSITE_COST_CASE=text runs only the cases with it in their name).
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use telescope_core::balance::{Balance, Place};
use telescope_core::colour::{bake, lattice};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, Touch};
use telescope_core::host::ColourStage;
use telescope_core::media::{Codec, DecodeOptions, Decoded, Decoder, FeedEnd, FeedStopper, FramePipeline, FrameSlot, Media, Packet, Scaler, StagedStep, StreamFrames, VideoFeed};
use telescope_core::output::{Outputs, Slot as OutputSlot, VideoOutput};
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::testing::FakeHost;
use telescope_core::time::monotonic_ns;
use telescope_core::ui::UiEvent;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::composite::{Layout, Tile};
use telescope_effects::composite_plugin::CompositePlugin;
use telescope_effects::fun_pack::FunPackPlugin;
use telescope_media::frames::{Frames as StreamStore, Slot};
use telescope_media::pipeline::Pipeline;
use telescope_shell::worker::{Opener, StateOwners, StreamWorker, VisionFeed as WorkerVision, WorkerSetup};

const FPS: u64 = 30;
const WARM_UP: Duration = Duration::from_secs(3);
const FRAMES: usize = 300;
const TIMEOUT: Duration = Duration::from_secs(90);
const SWITCH_EVERY: Duration = Duration::from_millis(1500);

// The Fun pack effect of Load B-fun: COMPOSITE_COST_EFFECT, else this
fn fun_effect() -> String {
    std::env::var("COMPOSITE_COST_EFFECT").unwrap_or_else(|_| "crt".into())
}

#[derive(Clone, Copy, PartialEq)]
enum Load {
    // Background blur on every stream, Cut out on the last tile
    A,
    // Load A and a colour LUT on every stream
    BCheap,
    // Load A and a Fun pack effect on every stream
    BHeavy,
}

struct Case {
    name: String,
    canvas: (u32, u32),
    streams: usize,
    // only the first stream is canvas sized, the others are 1080p
    mixed: bool,
    load: Load,
    moving: bool,
    card: bool,
}

fn cases() -> Vec<Case> {
    let (hd, uhd) = ((1920, 1080), (3840, 2160));
    let mut all = Vec::new();
    let mut add = |canvas: (u32, u32), streams: usize, mixed: bool, load: Load, card: bool| {
        for moving in [false, true] {
            let name = format!("{}p {} streams {} {} {}{}", canvas.1, streams, if mixed { "mixed" } else { "same" },
                               match load { Load::A => "loadA".to_string(), Load::BCheap => "loadB-lut".to_string(), Load::BHeavy => format!("loadB-fun({})", fun_effect()) },
                               if moving { "moving" } else { "steady" }, if card { " card" } else { "" });
            all.push(Case { name, canvas, streams, mixed, load, moving, card });
        }
    };
    for (streams, mixed) in [(3, false), (4, false), (3, true), (4, true)] {
        add(uhd, streams, mixed, Load::A, false);
    }
    for load in [Load::BCheap, Load::BHeavy] {
        add(uhd, 3, false, load, false);
        add(uhd, 4, false, load, false);
    }
    add(hd, 3, false, Load::A, false);
    add(hd, 4, false, Load::A, false);
    add(hd, 4, false, Load::BHeavy, false);
    add(uhd, 4, false, Load::A, true);
    all
}

// ---- the fake source: a queue the worker's decoder takes the newest frame from

#[derive(Default)]
struct Queue {
    frames: Mutex<VecDeque<Frame>>,
    ready: Condvar,
}

struct NoStop;

impl FeedStopper for NoStop {
    fn stop(&self) {}
}

struct Feed(Arc<Queue>, Arc<AtomicBool>);

impl VideoFeed for Feed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        let mut frames = self.0.frames.lock().unwrap();
        while frames.is_empty() {
            if self.1.load(Ordering::SeqCst) {
                return Err(FeedEnd::Stopped);
            }
            frames = self.0.ready.wait_timeout(frames, Duration::from_millis(20)).unwrap().0;
        }
        Ok(Packet { data: vec![0], time_ns: 0 })
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(NoStop)
    }
}

struct Handing(Arc<Queue>);

impl Decoder for Handing {
    fn decode(&mut self, _packet: &Packet) -> Decoded {
        Decoded { newest: self.0.frames.lock().unwrap().pop_front(), count: 1 }
    }
    fn hardware(&self) -> bool {
        false
    }
}

struct NoScaling;

impl Scaler for NoScaling {
    fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        vec![0; (width * height * 3) as usize]
    }
}

// The app's frame pipeline on a balance of the test's choosing, and a decoder that hands over the frames waiting
struct TestMedia(Arc<Queue>, Arc<Balance>);

impl Media for TestMedia {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Ok(Box::new(Handing(self.0.clone())))
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(NoScaling)
    }
    fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
        Ok(Box::new(Pipeline::with_balance(self.1.clone())))
    }
}

struct NoCameras;

impl Outputs for NoCameras {
    fn slots(&self) -> Vec<OutputSlot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no camera here".into())
    }
    fn locked_size(&self, _slot: u32) -> Option<(u32, u32)> {
        None
    }
}

// ---- the vision stub: a soft oval of person, a new Arc on every ask as a model that has a new answer every frame

#[derive(Default)]
struct MaskVision {
    masks: Mutex<HashMap<(usize, usize), Vec<u8>>>,
}

fn soft_oval(w: usize, h: usize) -> Vec<u8> {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    (0..h).flat_map(|y| (0..w).map(move |x| {
        let d = (((x as f32 - cx) / (cx * 0.7)).powi(2) + ((y as f32 - cy) / (cy * 0.9)).powi(2)).sqrt();
        let t = ((1.2 - d) / 0.4).clamp(0.0, 1.0);
        (t * t * (3.0 - 2.0 * t) * 255.0) as u8
    })).collect()
}

impl telescope_core::vision::VisionService for MaskVision {
    fn attach(&self, _bus: telescope_core::BusSender) {}
    fn state(&self) -> String {
        "ready".into()
    }
    fn load(&self) {}
    fn person_mask(&self, source_id: &str, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.person_mask_at(source_id, frame, frame.width as usize, frame.height as usize)
    }
    fn person_mask_at(&self, _source_id: &str, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let mut masks = self.masks.lock().unwrap();
        Some(Arc::new(masks.entry((w, h)).or_insert_with(|| soft_oval(w, h)).clone()))
    }
    fn want_people(&self, _owner: &str, _on: bool) {}
    fn wants_people(&self) -> bool {
        false
    }
    fn feed_raw(&self, _source_id: &str, _frame: &Frame, _upright: telescope_core::vision::Upright) {}
    fn tracked_people(&self, _source_id: &str) -> (Vec<serde_json::Value>, u32) {
        (Vec::new(), 0)
    }
    fn view_turned(&self, _old: telescope_core::vision::Turn, _new: telescope_core::vision::Turn) {}
    fn drop_stream(&self, _source_id: &str) {}
    fn set_power_saving(&self, _on: bool) {}
    fn diagnostics(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    fn shutdown(&self) {}
    fn pack(&self) -> telescope_core::vision::VisionPack {
        telescope_core::vision::VisionPack::default()
    }
    fn download(&self, _switch_on: &str) {}
    fn cancel_download(&self) {}
    fn remove_pack(&self) -> Result<(), String> {
        Ok(())
    }
}

// ---- the markers: three steps in each stream's chain that note when a frame passes

#[derive(Default)]
struct Samples {
    // start of the chain to just before One picture, ms
    chain: Vec<f64>,
    // start of the chain to its end, ms
    whole: Vec<f64>,
    // the One picture step alone (the target's), ms
    step: Vec<f64>,
    // the compositor's scale, paste and setup for that frame, ms, and whether a transition was playing
    parts: Vec<([f64; 3], bool)>,
}

struct Stream {
    t0: AtomicU64,
    t1: AtomicU64,
    recording: Arc<AtomicBool>,
    samples: Mutex<Samples>,
    times: Option<Box<dyn Fn() -> (telescope_effects::compositor::RenderTimes, bool) + Send + Sync>>,
}

#[derive(Clone, Copy)]
enum At {
    Start,
    BeforePicture,
    AfterPicture,
}

struct Mark(Arc<Stream>, At);

impl CpuStep for Mark {
    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        Touch::Reads
    }

    fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
        let now = monotonic_ns();
        let s = &self.0;
        let ms = |from: u64| now.saturating_sub(from) as f64 / 1e6;
        match self.1 {
            At::Start => s.t0.store(now, Ordering::SeqCst),
            At::BeforePicture => {
                s.t1.store(now, Ordering::SeqCst);
                if s.recording.load(Ordering::SeqCst) {
                    s.samples.lock().unwrap().chain.push(ms(s.t0.load(Ordering::SeqCst)));
                }
            }
            At::AfterPicture => {
                if s.recording.load(Ordering::SeqCst) {
                    let mut samples = s.samples.lock().unwrap();
                    samples.whole.push(ms(s.t0.load(Ordering::SeqCst)));
                    if let Some(times) = &s.times {
                        let (t, moving) = times();
                        samples.step.push(ms(s.t1.load(Ordering::SeqCst)));
                        samples.parts.push(([t.scale.as_secs_f64() * 1e3, t.paste.as_secs_f64() * 1e3, t.setup.as_secs_f64() * 1e3], moving));
                    }
                }
            }
        }
    }
}

// ---- one stream: its pacer, its real worker

struct Running {
    stream: Arc<Stream>,
    slot: Arc<Slot>,
    worker: StreamWorker,
    queue: Arc<Queue>,
    stopped: Arc<AtomicBool>,
    overruns: Arc<AtomicU64>,
    pacer: Option<JoinHandle<()>>,
}

fn pseudo_data(seed: u8, (w, h): (u32, u32)) -> Vec<u8> {
    (0..PixelFormat::I420.size(w, h)).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[2]).collect()
}

// A new frame every 1/30 s as a decoder makes them, taking the place of one the worker hasn't taken yet
fn pace(queue: Arc<Queue>, size: (u32, u32), seed: u8, stopped: Arc<AtomicBool>, overruns: Arc<AtomicU64>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let template = pseudo_data(seed, size);
        let period = Duration::from_nanos(1_000_000_000 / FPS);
        let mut next = Instant::now();
        while !stopped.load(Ordering::SeqCst) {
            next += period;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let frame = Frame::new(size.0, size.1, PixelFormat::I420, template.clone());
            let mut waiting = queue.frames.lock().unwrap();
            if !waiting.is_empty() {
                overruns.fetch_add(1, Ordering::SeqCst);
                waiting.clear();
            }
            waiting.push_back(frame);
            queue.ready.notify_one();
        }
    })
}

fn staged(owner: &str, stage: FrameStage, step: FrameStep) -> StagedStep {
    StagedStep { owner: owner.into(), stage, step }
}

fn start_worker(id: &str, steps: Vec<StagedStep>, stream: Arc<Stream>, balance: Arc<Balance>, vision: WorkerVision) -> Running {
    let (queue, stopped, slot) = (Arc::new(Queue::default()), Arc::new(AtomicBool::new(false)), Slot::new());
    let (q, s) = (queue.clone(), stopped.clone());
    let opener: Opener = Arc::new(move |_| Ok((Codec::Mjpeg, Box::new(Feed(q.clone(), s.clone())) as Box<dyn VideoFeed>).into()));
    let setup = WorkerSetup { url: "url".into(), opener, width: None, height: None, fps: FPS as u32, canvas: (None, None), slot: 0, source_id: id.into(),
                              hardware_decode: false, hardware_default: false, plugin_source: false, waiting_text: None, vision: Some(vision), steps, gate: None, idle: None };
    let worker = StreamWorker::start(setup, Arc::new(TestMedia(queue.clone(), balance)), Arc::new(NoCameras), slot.clone() as Arc<dyn FrameSlot>, Arc::new(|_| {})).unwrap();
    Running { stream, slot, worker, queue, stopped, overruns: Arc::new(AtomicU64::new(0)), pacer: None }
}

// ---- the layouts

fn boxes(streams: usize) -> Vec<(f64, f64, f64, f64)> {
    match streams {
        3 => vec![(0.0, 0.0, 0.66, 1.0), (0.68, 0.0, 0.32, 0.49), (0.68, 0.51, 0.32, 0.49)],
        _ => vec![(0.0, 0.0, 0.5, 0.5), (0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
    }
}

// The layout with every stream moved on by `turn` places; the last stream is cut out
fn layout(streams: usize, turn: usize) -> Layout {
    let places = boxes(streams);
    Layout::of((0..streams).map(|i| {
        let (x, y, w, h) = places[(i + turn) % streams];
        Tile { cut_out: i == streams - 1, ..Tile::boxed(&format!("s{i}"), "", x, y, w, h) }
    }).collect())
}

// ---- numbers

fn pct(values: &[f64], share: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * share).ceil() as usize).clamp(1, v.len()) - 1]
}

fn trio(values: &[f64]) -> String {
    format!("{:.1}/{:.1}/{:.1}", pct(values, 0.5), pct(values, 0.9), pct(values, 1.0))
}

fn card_name() -> Option<String> {
    telescope_media::gpu::Gpu::shared().map(|gpu| gpu.adapter_name.clone())
}

fn real_card() -> bool {
    card_name().is_some_and(|name| !["Basic Render", "llvmpipe", "Software"].iter().any(|soft| name.contains(soft)))
}

fn run(case: &Case) {
    let n = case.streams;
    let ids: Vec<String> = (0..n).map(|i| format!("s{i}")).collect();
    let sizes: Vec<(u32, u32)> = (0..n).map(|i| if case.mixed && i > 0 { (1920, 1080) } else { case.canvas }).collect();
    let balance = Arc::new(Balance::pinned_to(if case.card { Place::Card } else { Place::Cpu }));
    let recording = Arc::new(AtomicBool::new(false));

    // One picture
    let store = StreamStore::new();
    let mut host = FakeHost::new();
    host.frame_store = Some(store.clone());
    host.cameras = vec![(0, "s0".into(), None, None)];
    host.canvas = (Some(case.canvas.0), Some(case.canvas.1));
    host.live = ids.clone();
    let mut plugin = CompositePlugin::new();
    plugin.set_config(&json!({"transition": {"kind": "glide", "ms": 3000, "ease": "even"}})).unwrap();
    plugin.setup(&mut host);
    plugin.on_stream_start(&mut host, &StreamInfo { source_id: "s0".into(), url: "u".into(), session: 0 });

    // Background (blur, defaults) on every stream, with Cut out wanted where One picture says
    let mut background = BackgroundPlugin::new();
    background.set_config(&json!({"mode": "blur"})).unwrap();
    let mut other = FakeHost::new();
    other.focused = Some("s0".into());

    // the other effect of Load B
    let cube = bake(&[&ColourStage::new(33, Arc::new({
        let mut rgb = lattice(33);
        for px in rgb.as_chunks_mut::<3>().0 {
            *px = [px[0].sqrt(), 0.3 * px[1] + 0.7 * px[2], 1.0 - px[0] * px[2]];
        }
        rgb
    }), None)], None).unwrap();
    let mut fun = FunPackPlugin::new();
    if case.load == Load::BHeavy {
        fun.setup(&mut other);
        fun.on_event(&mut other, &Event::PhonesChanged { count: 1 });
        fun.set_config(&json!({"effect": fun_effect()})).unwrap();
        fun.on_event(&mut other, &Event::PhonesChanged { count: 1 });
        assert_eq!(fun.effect(), fun_effect());
    }

    let vision = WorkerVision { service: Arc::new(MaskVision::default()), upright: Arc::default(), frame_sizes: Arc::default(), motion: None, owners: Arc::new(StateOwners::default()), owner: 0 };
    let mut running: Vec<Running> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let target = i == 0;
        let stream = Arc::new(Stream {
            t0: AtomicU64::new(0), t1: AtomicU64::new(0), recording: recording.clone(), samples: Mutex::default(),
            times: target.then(|| Box::new(plugin.render_times()) as Box<dyn Fn() -> (telescope_effects::compositor::RenderTimes, bool) + Send + Sync>),
        });
        let mut steps = vec![staged("mark_start", FrameStage::View, FrameStep::Cpu(Box::new(Mark(stream.clone(), At::Start))))];
        steps.push(staged("background", FrameStage::Background, background.frame_step().expect("Background is on")));
        match case.load {
            Load::A => {}
            Load::BCheap => steps.push(staged("colour", FrameStage::Colour, cube.frame_step())),
            Load::BHeavy => steps.push(staged("fun_pack", FrameStage::Effects, fun.frame_step().expect("the effect is on"))),
        }
        steps.push(staged("mark_before", FrameStage::Overlay, FrameStep::Cpu(Box::new(Mark(stream.clone(), At::BeforePicture)))));
        steps.push(staged("composite", FrameStage::Combine, plugin.frame_step().unwrap()));
        steps.push(staged("mark_after", FrameStage::Cover, FrameStep::Cpu(Box::new(Mark(stream.clone(), At::AfterPicture)))));
        running.push(start_worker(id, steps, stream, balance.clone(), vision.clone()));
    }
    store.set_streams(ids.iter().zip(&running).map(|(id, r)| (id.clone(), id.clone(), r.slot.clone() as Arc<dyn FrameSlot>)).collect());
    plugin.set_layout(&mut host, layout(n, 0));
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    plugin.on_event(&mut host, &Event::StreamsChanged { count: n as u32 });
    for event in std::mem::take(&mut host.events) {
        if matches!(event, Event::CutOutWanted { .. }) {
            background.on_event(&mut other, &event);
        }
    }
    for (i, r) in running.iter_mut().enumerate() {
        r.pacer = Some(pace(r.queue.clone(), sizes[i], i as u8 + 1, r.stopped.clone(), r.overruns.clone()));
    }

    // the host thread: while the streams run, a moving case switches the layout back and forth before a transition can end
    let mut flips = 0usize;
    let mut last_switch = Instant::now().checked_sub(SWITCH_EVERY).unwrap_or_else(Instant::now);
    let mut drive = |until: &dyn Fn() -> bool| loop {
        if until() {
            return;
        }
        if case.moving && last_switch.elapsed() >= SWITCH_EVERY {
            flips += 1;
            plugin.switch_layout(&mut host, layout(n, flips % 2));
            last_switch = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let warm_end = Instant::now() + WARM_UP;
    drive(&|| Instant::now() >= warm_end);
    for r in &running {
        *r.stream.samples.lock().unwrap() = Samples::default();
    }
    let (wall0, cpu0, overruns0, transfers0) = (Instant::now(), telescope_core::stats::process_reading().0, running.iter().map(|r| r.overruns.load(Ordering::SeqCst)).sum::<u64>(), telescope_media::transfers::totals());
    recording.store(true, Ordering::SeqCst);
    drive(&|| running[0].stream.samples.lock().unwrap().whole.len() >= FRAMES || wall0.elapsed() > TIMEOUT);
    recording.store(false, Ordering::SeqCst);
    let (wall, cpu1, transfers1) = (wall0.elapsed().as_secs_f64(), telescope_core::stats::process_reading().0, telescope_media::transfers::totals());
    let overruns = running.iter().map(|r| r.overruns.load(Ordering::SeqCst)).sum::<u64>() - overruns0;
    let cut_out_seen = running[n - 1].slot.latest().is_some_and(|(frame, ..)| frame.alpha().is_some());

    // let the workers finish before the numbers are read, so no late sample lands
    for r in running.iter_mut() {
        r.stopped.store(true, Ordering::SeqCst);
        if let Some(p) = r.pacer.take() {
            let _ = p.join();
        }
        r.worker.stop();
    }
    plugin.shutdown(&mut host);
    store.set_streams(Vec::new());

    let target = running[0].stream.samples.lock().unwrap();
    let others: Vec<f64> = running[1..].iter().flat_map(|r| r.stream.samples.lock().unwrap().chain.clone()).collect();
    let part = |i: usize| target.parts.iter().map(|(p, _)| p[i]).collect::<Vec<f64>>();
    let in_transition = target.parts.iter().filter(|(_, moving)| *moving).count() as f64 / target.parts.len().max(1) as f64;
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get()) as f64;
    let used = cpu0.zip(cpu1).map_or(f64::NAN, |(a, b)| (b - a) as f64 / 1e9 / wall);
    let fps = |s: &Stream| s.samples.lock().unwrap().whole.len() as f64 / wall;
    let slowest_other = running[1..].iter().map(|r| fps(&r.stream)).fold(f64::MAX, f64::min);
    let frames = target.whole.len().max(1) as f64;
    println!("{} | target chain {} | other chains {} | One picture {} | scale/paste/setup p50 {:.1}/{:.1}/{:.1} | target end to end {} | CPU {:.1} cores ({:.0}%) | fps target {:.1}, slowest other {:.1}, pacer overruns {} | in transition {:.0}% | cut-out alpha {} | card ups/downs per target frame {:.1}/{:.1}",
             case.name, trio(&target.chain), trio(&others), trio(&target.step), pct(&part(0), 0.5), pct(&part(1), 0.5), pct(&part(2), 0.5), trio(&target.whole),
             used, used / cores * 100.0, target.whole.len() as f64 / wall, slowest_other, overruns, in_transition * 100.0, cut_out_seen,
             (transfers1.up - transfers0.up) as f64 / frames, (transfers1.down - transfers0.down) as f64 / frames);
}

#[test]
#[ignore]
fn one_picture_under_load() {
    let filter = std::env::var("COMPOSITE_COST_CASE").unwrap_or_default();
    let card = real_card();
    println!("machine: {} threads; graphics card: {}; ms, p50/p90/max over {FRAMES} target frames after {} s warm-up; Background and the other effects run on the CPU unless the name says card",
             std::thread::available_parallelism().map_or(1, |c| c.get()), card_name().unwrap_or_else(|| "none".into()), WARM_UP.as_secs());
    for case in cases().iter().filter(|c| c.name.contains(&filter)) {
        if case.card && !card {
            println!("{} | skipped, no graphics card here", case.name);
            continue;
        }
        run(case);
    }
}
