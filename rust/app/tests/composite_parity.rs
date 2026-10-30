//! Several cameras against what the Python app does. crates/effects/tests/fixtures holds what Python made
//! (make_composite_fixtures.py): the test streams, each layout the Compositor drew from them, and what the plugin saves after
//! loading odd settings. The two "through real workers" tests run the plugin's step in media's frame pipeline and slots, as
//! a stream's worker does.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::FrameStore;
use telescope_core::media::{FrameSlot, StagedStep, StreamFrames};
use telescope_core::media::FramePipeline;
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

/// A stream's worker: its frame steps, then its slot (StreamWorker._publish).
struct Worker {
    sid: &'static str,
    pipeline: Pipeline,
    slot: Arc<Slot>,
}

impl Worker {
    fn new(sid: &'static str, plugin: &mut CompositePlugin, cover: Option<Frame>) -> Worker {
        let mut steps = vec![StagedStep { owner: "composite".into(), stage: FrameStage::Combine, step: plugin.frame_step().unwrap() }];
        if let Some(c) = cover {
            steps.push(StagedStep { owner: "wait_screen".into(), stage: FrameStage::Cover, step: FrameStep::Cpu(Box::new(Cover(c))) });
        }
        let mut pipeline = Pipeline::new();
        pipeline.set_steps(steps);
        Worker { sid, pipeline, slot: Slot::new() }
    }

    fn publish(&mut self, frame: Frame) {
        let out = self.pipeline.process(frame, &Ctx(self.sid));
        self.slot.publish(out);
    }

    fn latest(&self) -> Frame {
        self.slot.latest().unwrap().0
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
    assert!(plugin.draw_stalled(now + SECOND));
    assert!(px(&a.latest(), 90, 280) == [0, 255, 255] && store.stream_frame(Some("a")).unwrap().frame.number == number);
    assert!(!store.replace_frame(Some("a"), out.clone(), number - 1) && !store.replace_frame(Some("gone"), out, number));
    // a long stall: b keeps moving on a's camera
    for (i, colour) in [[255, 0, 255], [255, 255, 0], [0, 0, 255]].into_iter().enumerate() {
        b.publish(solid(colour, (90, 160)));
        assert!(plugin.draw_stalled(now + SECOND + i as u64 * SECOND / 2));
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
