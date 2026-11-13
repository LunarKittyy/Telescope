//! Several cameras against what the Python app does. crates/effects/tests/fixtures holds what Python made
//! (recorded from the Python app): the test streams, each layout the Compositor drew from them, and what the plugin saves after
//! loading odd settings. The two "through real workers" tests run the plugin's step on the app's own stream worker, with a decoder
//! that hands it the test's frames. Background pictures here are real PNG files through the app's own picture reader.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::FrameStore;
use telescope_core::media::{Codec, DecodeOptions, Decoded, Decoder, FeedEnd, FeedStopper, FramePipeline, FrameSlot, Media, Packet, Scaler, StagedStep, StreamFrames, VideoFeed};
use telescope_core::output::{Outputs, Slot as OutputSlot, VideoOutput};
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::testing::FakeHost;
use telescope_core::time::monotonic_ns;
use telescope_core::ui::UiEvent;
use telescope_effects::composite::{clean_layout, Layout, Tile};
use telescope_effects::composite_plugin::CompositePlugin;
use telescope_effects::compositor::{Compositor, Frames};
use telescope_effects::pixels::{put_rgb, to_rgb};
use telescope_media::frames::{Frames as StreamStore, Slot};
use telescope_media::pipeline::Pipeline;
use telescope_shell::worker::{Opener, StreamWorker, WorkerSetup};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/effects/tests/fixtures")
}

fn json_of(name: &str) -> Value {
    telescope_core::json::loads(&std::fs::read_to_string(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))).unwrap()
}

fn rgba_file(name: &str, w: u32, h: u32) -> Frame {
    let data = std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    Frame::new(w, h, PixelFormat::Rgba, data)
}

fn dims(v: &Value) -> (u32, u32) {
    (v["width"].as_u64().unwrap() as u32, v["height"].as_u64().unwrap() as u32)
}

/// A saved number is the same number, whether it was written 100 or 100.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same(va, vb)),
        _ => a == b,
    }
}

#[test]
fn settings_load_and_save_as_python_does() {
    let cases = json_of("composite.json");
    for case in cases["configs"].as_array().unwrap() {
        let mut plugin = CompositePlugin::new();
        plugin.set_config(&case["input"]).unwrap();
        let got = plugin.get_config();
        assert!(same(&got, &case["output"]), "given {}:\n  rust   {got}\n  python {}", case["input"], case["output"]);
    }
}

/// The streams in format, through RGB for one that isn't RGBA.
fn sources(format: PixelFormat) -> Frames {
    let cases = json_of("composite.json");
    cases["sources"].as_array().unwrap().iter().map(|s| {
        let (w, h) = dims(s);
        let rgba = rgba_file(s["file"].as_str().unwrap(), w, h);
        let mut frame = Frame::new(w, h, format, vec![255; format.size(w, h)]);
        put_rgb(&mut frame, &to_rgb(&rgba).unwrap());
        (s["source_id"].as_str().unwrap().to_string(), (frame, None))
    }).collect()
}

/// (largest, mean) difference of two pictures' R, G and B, and the share of pixels more than tolerance off.
fn gap(got: &[u8], want: &[u8], tolerance: u8) -> (u8, f64, f64) {
    let (mut max, mut sum, mut over, mut n) = (0u8, 0u64, 0usize, 0usize);
    for (a, b) in got.chunks(3).zip(want.chunks(3)) {
        let worst = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
        max = max.max(worst);
        sum += (0..3).map(|c| a[c].abs_diff(b[c]) as u64).sum::<u64>();
        over += (worst > tolerance) as usize;
        n += 1;
    }
    (max, sum as f64 / (n * 3) as f64, over as f64 / n as f64)
}

/// Each case drawn in format against Python's picture put into the same format, so what's measured is the drawing and not
/// what the format itself loses. In I420 the brightness plane is checked on its own too.
fn compare(format: PixelFormat, allowed: (u8, f64, f64), luma_allowed: (u8, f64)) {
    let cases = json_of("composite.json");
    let frames = sources(format);
    for case in cases["cases"].as_array().unwrap() {
        let (w, h) = dims(case);
        let layout = clean_layout(&case["layout"]).unwrap();
        let mut comp = Compositor::default();
        comp.prepare(&layout, (w, h));
        let got = comp.render(&frames, format).unwrap();
        let mut want = Frame::new(w, h, format, vec![255; format.size(w, h)]);
        put_rgb(&mut want, &to_rgb(&rgba_file(case["file"].as_str().unwrap(), w, h)).unwrap());
        let name = case["name"].as_str().unwrap();
        let (max, mean, over) = gap(&to_rgb(&got).unwrap(), &to_rgb(&want).unwrap(), 4);
        eprintln!("{format:?} {name:<45} max {max:>3} mean {mean:.3} over {over:.4}");
        assert!(max <= allowed.0 && mean <= allowed.1 && over <= allowed.2, "{name}: max {max} mean {mean} over {over}");
        if format == PixelFormat::I420 {
            let n = (w * h) as usize;
            let worst = got.data[..n].iter().zip(&want.data[..n]).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            let mean = got.data[..n].iter().zip(&want.data[..n]).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / n as f64;
            eprintln!("     brightness max {worst:>3} mean {mean:.3}");
            assert!(worst <= luma_allowed.0 && mean <= luma_allowed.1, "{name}: brightness max {worst} mean {mean}");
        }
    }
}

#[test]
fn the_compositor_draws_the_same_picture_as_python() {
    // OpenCV's fixed-point INTER_LINEAR and blendLinear, so a level at most
    compare(PixelFormat::Rgba, (1, 0.05, 0.0), (0, 0.0));
}

#[test]
fn in_i420_the_picture_is_python_s_but_for_the_shared_colour_samples() {
    // Brightness within 2 levels; the half-size colour planes are scaled as they are, so colour differs at sharp edges
    compare(PixelFormat::I420, (255, 4.0, 0.4), (2, 0.3));
}

fn solid(bgr: [u8; 3], (h, w): (u32, u32)) -> Frame {
    Frame::new(w, h, PixelFormat::Bgra, [bgr[0], bgr[1], bgr[2], 255].repeat((w * h) as usize))
}

fn px(frame: &Frame, y: usize, x: usize) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    [frame.data[at], frame.data[at + 1], frame.data[at + 2]]
}

/// The wait screen in the cover stage, after the picture.
struct Cover(Frame);

impl CpuStep for Cover {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        *frame = self.0.clone();
    }
}

/// The frames a worker has still to decode.
type Waiting = Arc<Mutex<VecDeque<Frame>>>;

struct NoStop;

impl FeedStopper for NoStop {
    fn stop(&self) {}
}

/// A packet for each frame waiting.
struct Feed(Waiting, Arc<AtomicBool>);

impl VideoFeed for Feed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        while self.0.lock().unwrap().is_empty() {
            if self.1.load(Ordering::SeqCst) {
                return Err(FeedEnd::Stopped);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(Packet { data: vec![0], time_ns: 0 })
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(NoStop)
    }
}

struct Handing(Waiting);

impl Decoder for Handing {
    fn decode(&mut self, _packet: &Packet) -> Decoded {
        Decoded { newest: self.0.lock().unwrap().pop_front(), count: 1 }
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

/// The app's frame pipeline, and a decoder that hands over the frames waiting.
struct TestMedia(Waiting);

impl Media for TestMedia {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Ok(Box::new(Handing(self.0.clone())))
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(NoScaling)
    }
    fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
        Ok(Box::new(Pipeline::new()))
    }
}

/// No camera to send to: what's checked is what the worker makes of frames.
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

/// A stream's real worker (StreamWorker), whose decoder hands over the test's frames.
struct Worker {
    worker: StreamWorker,
    waiting: Waiting,
    stopped: Arc<AtomicBool>,
    slot: Arc<Slot>,
}

impl Worker {
    fn new(sid: &'static str, plugin: &mut CompositePlugin, cover: Option<Frame>) -> Worker {
        let mut steps = vec![StagedStep { owner: "composite".into(), stage: FrameStage::Combine, step: plugin.frame_step().unwrap() }];
        if let Some(c) = cover {
            steps.push(StagedStep { owner: "wait_screen".into(), stage: FrameStage::Cover, step: FrameStep::Cpu(Box::new(Cover(c))) });
        }
        let (waiting, stopped, slot) = (Waiting::default(), Arc::new(AtomicBool::new(false)), Slot::new());
        let (w, s) = (waiting.clone(), stopped.clone());
        let opener: Opener = Arc::new(move |_| Ok((Codec::Mjpeg, Box::new(Feed(w.clone(), s.clone())) as Box<dyn VideoFeed>).into()));
        let setup = WorkerSetup { url: "url".into(), opener, width: None, height: None, fps: 30, canvas: (None, None), slot: 0, source_id: sid.into(),
                                  hardware_decode: false, hardware_default: false, plugin_source: false, waiting_text: None, vision: None, steps, gate: None, idle: None };
        let worker = StreamWorker::start(setup, Arc::new(TestMedia(waiting.clone())), Arc::new(NoCameras), slot.clone() as Arc<dyn FrameSlot>, Arc::new(|_| {})).unwrap();
        Worker { worker, waiting, stopped, slot }
    }

    /// The frame through the worker, as it comes back from the decoder.
    fn publish(&mut self, frame: Frame) {
        let before = self.slot.published().map(|f| f.number);
        self.waiting.lock().unwrap().push_back(frame);
        let until = Instant::now() + Duration::from_secs(20);
        while self.slot.published().map(|f| f.number) == before {
            assert!(Instant::now() < until, "the worker never published");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn latest(&self) -> Frame {
        self.slot.latest().unwrap().0
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.worker.request_stop();
    }
}

fn workers_rig(cover: Option<Frame>) -> (CompositePlugin, FakeHost, Arc<StreamStore>, Worker, Worker) {
    let store = StreamStore::new();
    let mut host = FakeHost::new();
    host.frame_store = Some(store.clone());
    host.cameras = vec![(0, "a".into(), None, None)];
    host.canvas = (Some(320), Some(180));
    host.live = vec!["a".into(), "b".into()];
    let mut plugin = CompositePlugin::new();
    plugin.setup(&mut host);
    plugin.on_stream_start(&mut host, &StreamInfo { source_id: "a".into(), url: "u".into(), session: 0 });
    let a = Worker::new("a", &mut plugin, cover);
    let b = Worker::new("b", &mut plugin, None);
    store.set_streams(vec![("a".into(), "Phone A".into(), a.slot.clone() as Arc<dyn FrameSlot>), ("b".into(), "Phone B".into(), b.slot.clone() as Arc<dyn FrameSlot>)]);
    plugin.set_layout(&mut host, Layout::of(vec![Tile::boxed("a", "", 0.0, 0.0, 0.5, 1.0), Tile::boxed("b", "", 0.5, 0.0, 0.5, 1.0)]));
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    plugin.on_event(&mut host, &Event::StreamsChanged { count: 2 });
    (plugin, host, store, a, b)
}

const SECOND: u64 = 1_000_000_000;

// np.full((40, 30, 3), (10, 20, 30)) and np.full((8, 8, 3), (10, 20, 30)) as cv2 writes them
const PNG_30_40: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x1e\x00\x00\x00\x28\x08\x02\x00\x00\x00\x66\x71\xdd\x45\x00\x00\x00\x86\x49\x44\x41\x54\x48\x0d\xb5\xc1\x21\x01\x00\x00\x00\xc2\x30\xb0\xe8\x6b\xfa\xc7\x24\x05\x9b\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\x87\x4b\xf4\xe1\x12\x7d\xb8\x44\x1f\x2e\xd1\xc7\x00\x77\x2f\x09\x89\x2e\x78\x89\xc1\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
const PNG_8_8: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x08\x00\x00\x00\x08\x08\x02\x00\x00\x00\x4b\x6d\x29\xdc\x00\x00\x00\x2c\x49\x44\x41\x54\x08\x1d\x6d\xc1\x21\x01\x00\x00\x00\xc2\x30\xb0\xe8\x6b\xfa\xc7\xa4\x00\x9b\x4b\xf4\xb8\x44\x8f\x4b\xf4\xb8\x44\x8f\x4b\xf4\xb8\x44\x8f\x4b\xf4\xb8\x44\xcf\x00\xd4\x1c\x01\xe9\xc0\x10\x35\xd7\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";

/// A compositor reading pictures the way the app does; None in a build without FFmpeg, which only reads JPEGs.
fn compositor() -> Option<Compositor> {
    use telescope_core::media::Media as _;
    if telescope_media::FfmpegMedia.decode_picture(PNG_8_8).is_err() {
        eprintln!("this build can't read a PNG: the picture isn't checked");
        return None;
    }
    Some(Compositor::new(telescope_effects::shared::picture_reader(Arc::new(telescope_media::FfmpegMedia))))
}

fn picture_layout(path: &std::path::Path, background: &str) -> Layout {
    Layout { picture: path.to_string_lossy().into(), background: background.into(), ..Layout::default() }
}

#[test]
fn the_background_picture_covers_the_picture() {
    let dir = telescope_core::testing::scratch_dir("composite-bg");
    let path = dir.join("bg.png");
    std::fs::write(&path, PNG_30_40).unwrap();
    let Some(mut comp) = compositor() else { return };
    comp.prepare(&picture_layout(&path, "#000000"), (64, 36));
    let out = comp.render(&Frames::new(), PixelFormat::Bgra).unwrap();
    assert!((out.width, out.height) == (64, 36) && px(&out, 18, 32) == [10, 20, 30], "{:?}", px(&out, 18, 32));
    comp.prepare(&picture_layout(&dir.join("missing.png"), "#ffffff"), (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [255, 255, 255]);
}

#[test]
fn a_missing_background_picture_is_read_again_once_its_there() {
    let path = telescope_core::testing::scratch_dir("composite-later").join("later.png");
    let layout = picture_layout(&path, "#ffffff");
    let Some(mut comp) = compositor() else { return };
    comp.prepare(&layout, (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [255, 255, 255]);
    std::fs::write(&path, PNG_8_8).unwrap();
    comp.prepare(&layout, (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [10, 20, 30]);
}

#[test]
fn moving_a_tile_doesnt_scale_the_background_picture_again() {
    let path = telescope_core::testing::scratch_dir("composite-move").join("bg.png");
    std::fs::write(&path, PNG_30_40).unwrap();
    let picture: String = path.to_string_lossy().into();
    let Some(mut comp) = compositor() else { return };
    for x in [0.0, 0.1, 0.2] {
        comp.prepare(&Layout { picture: picture.clone(), ..Layout::of(vec![Tile::boxed("a", "", x, 0.0, 0.5, 0.5)]) }, (64, 36));
    }
    assert_eq!(comp.covered, [(64, 36)]);
    comp.prepare(&Layout { picture, ..Layout::default() }, (32, 18));
    assert_eq!(comp.covered, [(64, 36), (32, 18)]);
}

#[test]
fn through_real_workers_the_main_stream_sends_the_picture_and_the_other_its_own() {
    let (mut plugin, mut host, store, mut a, mut b) = workers_rig(None);
    b.publish(solid([0, 255, 0], (90, 160)));
    a.publish(solid([255, 0, 0], (90, 160)));
    // its own camera still gets its own picture
    assert_eq!((b.latest().width, b.latest().height), (160, 90));
    let out = a.latest();
    assert!((out.width, out.height) == (320, 180) && px(&out, 90, 40) == [255, 0, 0] && px(&out, 90, 280) == [0, 255, 0]);
    // a stalls and b moves on: the clock's picture goes out on a's camera, but not over a frame a sent since
    let number = store.stream_frame(Some("a")).unwrap().frame.number;
    b.publish(solid([0, 255, 255], (90, 160)));
    let now = monotonic_ns();
    // The plugin's own clock may get there first on a slow machine, so what counts is the picture that goes out
    plugin.draw_stalled(now + SECOND);
    assert!(px(&a.latest(), 90, 280) == [0, 255, 255] && store.stream_frame(Some("a")).unwrap().frame.number == number);
    assert!(!store.replace_frame(Some("a"), out.clone(), number - 1) && !store.replace_frame(Some("gone"), out, number));
    // a long stall: b keeps moving on a's camera
    for (i, colour) in [[255, 0, 255], [255, 255, 0], [0, 0, 255]].into_iter().enumerate() {
        b.publish(solid(colour, (90, 160)));
        plugin.draw_stalled(now + SECOND + i as u64 * SECOND / 2);
        assert!(px(&a.latest(), 90, 280) == colour && store.stream_frame(Some("a")).unwrap().frame.number == number);
    }
    plugin.shutdown(&mut host);
}

#[test]
fn through_real_workers_a_covered_stream_stays_covered_while_it_stalls() {
    let wait_screen = Frame::new(320, 180, PixelFormat::Bgra, vec![77; 320 * 180 * 4]);
    let (mut plugin, mut host, _store, mut a, mut b) = workers_rig(Some(wait_screen.clone()));
    b.publish(solid([0, 255, 0], (90, 160)));
    a.publish(solid([255, 0, 0], (90, 160)));
    assert!(Arc::ptr_eq(&a.latest().data, &wait_screen.data));
    let now = monotonic_ns();
    for i in 0..3u64 {
        b.publish(solid([0, 255, 255], (90, 160)));
        assert!(!plugin.draw_stalled(now + SECOND + i * SECOND / 2));
        assert!(Arc::ptr_eq(&a.latest().data, &wait_screen.data));
    }
    plugin.shutdown(&mut host);
}
