//! Transforms and Colour grading against what the Python app does on the same pictures. crates/effects/tests/fixtures holds the
//! pictures Python made (make_fixtures.py): the same test frame through the window's view and through each grade, and what
//! each plugin saves after loading odd settings. The effects run on the graphics card through media's frame pipeline, so
//! they're checked without one only for what doesn't need it.

use serde_json::{json, Value};
use std::path::PathBuf;
use telescope_core::colour::ColourPipeline;
use telescope_core::frame::{Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::View;
use telescope_core::media::{Media, StagedStep};
use telescope_core::plugin::Plugin;
use telescope_core::view;
use telescope_effects::colour_grading::{ColourGradingPlugin, OWNER};
use telescope_effects::transforms::TransformsPlugin;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/effects/tests/fixtures")
}

fn json_of(name: &str) -> Value {
    telescope_core::json::loads(&std::fs::read_to_string(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))).unwrap()
}

fn picture(name: &str, w: u32, h: u32) -> Frame {
    let data = std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    Frame::new(w, h, PixelFormat::Rgba, data)
}

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// Whether this machine has a graphics card the shaders can run on.
fn gpu_here() -> bool {
    let here = telescope_media::gpu::Gpu::shared().is_some();
    if !here {
        eprintln!("no graphics card here: the shader comparisons are skipped");
    }
    here
}

fn run(steps: Vec<StagedStep>, frame: Frame) -> Frame {
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(steps);
    pipeline.process(frame, &Ctx)
}

/// How far two RGBA pictures of the same size are apart, in levels of the colour channels.
#[derive(Debug)]
struct Gap {
    max: u32,
    mean: f64,
    /// The share of pixels with a channel more than `tolerance` levels off.
    over: f64,
}

fn gap(got: &Frame, want: &Frame, tolerance: u32) -> Gap {
    assert_eq!((got.width, got.height), (want.width, want.height), "the picture's size");
    let (mut max, mut sum, mut over) = (0u32, 0u64, 0usize);
    for (a, b) in got.data.chunks(4).zip(want.data.chunks(4)) {
        let worst = (0..3).map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs()).max().unwrap_or(0);
        max = max.max(worst);
        sum += (0..3).map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs() as u64).sum::<u64>();
        over += (worst > tolerance) as usize;
    }
    let pixels = (got.width * got.height) as f64;
    Gap { max, mean: sum as f64 / (pixels * 3.0), over: over as f64 / pixels }
}

fn view_of(case: &Value) -> View {
    let v = &case["view"];
    let f = |k: &str, d: f64| v.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let rotation = v.get("rotation").and_then(Value::as_str).and_then(view::rotation_named).flatten();
    View { zoom: f("zoom", 1.0), pan_x: f("pan_x", 0.0), pan_y: f("pan_y", 0.0), flip_h: b("flip_h"), flip_v: b("flip_v"), rotation,
           stretch_x: f("stretch_x", 1.0), stretch_y: f("stretch_y", 1.0) }
}

/// How close Rust's view has to be to OpenCV's: warpAffine interpolates in fixed point (a 32nd of a pixel), the card in float.
const VIEW_MAX: u32 = 6;
const VIEW_MEAN: f64 = 0.9;
const VIEW_OVER: f64 = 0.02;

#[test]
fn the_view_makes_the_same_picture_as_python() {
    if !gpu_here() {
        return;
    }
    let views = json_of("views.json");
    let frame = picture("frame.rgba", views["width"].as_u64().unwrap() as u32, views["height"].as_u64().unwrap() as u32);
    let mut worst = Vec::new();
    for case in views["cases"].as_array().unwrap() {
        let view = view_of(case);
        let steps: Vec<StagedStep> = telescope_core::view_step::steps(&view).into_iter()
            .map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect();
        let got = run(steps, frame.clone());
        let want = picture(case["file"].as_str().unwrap(), case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
        let g = gap(&got, &want, 4);
        eprintln!("{:<45} max {:>3} mean {:.3} over {:.4}", case["name"].as_str().unwrap(), g.max, g.mean, g.over);
        worst.push((case["name"].as_str().unwrap().to_string(), g));
    }
    for (name, g) in &worst {
        assert!(g.max <= VIEW_MAX && g.mean <= VIEW_MEAN && g.over <= VIEW_OVER, "{name}: {g:?}");
    }
}

/// A LUT in a grade's settings by the folder the fixtures are in.
fn with_fixtures(cfg: &Value) -> Value {
    serde_json::from_str(&cfg.to_string().replace("@fixtures", &fixtures().display().to_string().replace('\\', "\\\\"))).unwrap()
}

const COLOUR_MAX: u32 = 3;
const COLOUR_MEAN: f64 = 0.2;
const COLOUR_OVER: f64 = 0.01;

#[test]
fn the_grade_makes_the_same_picture_as_python() {
    if !gpu_here() {
        return;
    }
    let colours = json_of("colours.json");
    let frame = picture("frame.rgba", colours["width"].as_u64().unwrap() as u32, colours["height"].as_u64().unwrap() as u32);
    let mut results = Vec::new();
    for case in colours["cases"].as_array().unwrap() {
        let mut plugin = ColourGradingPlugin::new();
        plugin.set_config(&with_fixtures(&case["config"])).unwrap();
        let mut pipeline = ColourPipeline::new();
        pipeline.set_stage(OWNER, plugin.stage(), 0, None);
        let steps: Vec<StagedStep> = pipeline.step(None).map(|s| StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step: FrameStep::Shader(s) }).into_iter().collect();
        let kind = pipeline.lut(None).map(|l| l.kind()).unwrap_or_else(|| "none".into());
        let got = run(steps, frame.clone());
        let want = picture(case["file"].as_str().unwrap(), frame.width, frame.height);
        let g = gap(&got, &want, 2);
        eprintln!("{:<35} {:>5} (python {:>5}) max {:>3} mean {:.3} over {:.4}", case["name"].as_str().unwrap(), kind, case["kind"].as_str().unwrap(), g.max, g.mean, g.over);
        results.push((case["name"].as_str().unwrap().to_string(), g, kind, case["kind"].as_str().unwrap().to_string()));
    }
    for (name, g, kind, python_kind) in &results {
        assert!(g.max <= COLOUR_MAX && g.mean <= COLOUR_MEAN && g.over <= COLOUR_OVER, "{name}: {g:?}");
        assert_eq!(kind.replace("3D 33", "3D"), python_kind.replace("3D 33", "3D"), "{name}: the same kind of LUT");
    }
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
    let mut checked = 0;
    for case in json_of("configs.json").as_array().unwrap() {
        let mut plugin: Box<dyn Plugin> = match case["plugin"].as_str().unwrap() {
            "colour_grading" => Box::new(ColourGradingPlugin::new()),
            "transforms" => Box::new(TransformsPlugin::new()),
            other => panic!("{other}"),
        };
        plugin.set_config(&case["input"]).unwrap();
        let got = plugin.get_config();
        assert!(same(&got, &case["output"]), "{} given {}:\n  rust   {got}\n  python {}", case["plugin"], case["input"], case["output"]);
        checked += 1;
    }
    assert!(checked > 20);
}

#[test]
fn the_cards_say_they_are_what_python_says() {
    let g = ColourGradingPlugin::new().info();
    let t = TransformsPlugin::new().info();
    assert_eq!((g.name.as_str(), g.category.as_str(), g.icon.as_str(), g.optional, g.in_presets, g.device_local), ("colour_grading", "look", "colour", true, true, true));
    assert_eq!((t.name.as_str(), t.category.as_str(), t.icon.as_str(), t.pinned, t.in_presets, t.device_local), ("transforms", "camera", "crop", true, true, true));
    assert_eq!(json!(g.frame_stage), json!("effects"));
}
