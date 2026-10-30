//! Transforms and Colour grading against what the Python app does on the same pictures. crates/effects/tests/fixtures holds the
//! pictures Python made (make_fixtures.py): the same test frame through the window's view and through each grade, and what
//! each plugin saves after loading odd settings. The effects run on the graphics card through media's frame pipeline, so
//! they're checked without one only for what doesn't need it.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use telescope_core::colour::ColourPipeline;
use telescope_core::frame::{Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::View;
use telescope_core::media::{Media, StagedStep};
use telescope_core::plugin::Plugin;
use telescope_core::view;
use telescope_effects::chroma_key::{chroma_of, key_tables, keyable, ChromaKeyPlugin};
use telescope_effects::colour_grading::{ColourGradingPlugin, OWNER};
use telescope_effects::cpu::Rgb;
use telescope_effects::fun_effects::{Clones, Effect, Inputs, Look};
use telescope_effects::fun_pack::FunPackPlugin;
use telescope_effects::imgops::Gray;
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
        let mut pipeline = ColourPipeline::inline();
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
            "chroma_key" => Box::new(ChromaKeyPlugin::new()),
            "fun_pack" => Box::new(FunPackPlugin::new()),
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

/// How close the key's picture has to be: the matte is looked up in the very tables Python uses and only the scaling around it
/// differs (OpenCV's fixed-point resize against the card's float), which only shows where a table has a cliff.
const CHROMA_MAX: u32 = 16;
const CHROMA_MEAN: f64 = 0.1;
const CHROMA_OVER: f64 = 0.005;

#[test]
fn the_key_makes_the_same_picture_as_python() {
    if !gpu_here() {
        return;
    }
    let chroma = json_of("chroma.json");
    let media: Arc<dyn Media> = Arc::new(telescope_media::FfmpegMedia);
    let mut results = Vec::new();
    for case in chroma["cases"].as_array().unwrap() {
        let mut plugin = ChromaKeyPlugin::with_reader(telescope_effects::shared::picture_reader(media.clone()));
        let mut cfg = with_fixtures(&case["config"]);
        cfg["on"] = json!(true);
        plugin.set_config(&cfg).unwrap();
        let step = plugin.frame_step().expect("a key");
        let (w, h) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
        let scene = picture(case["scene"].as_str().unwrap(), w, h);
        let got = run(vec![StagedStep { owner: "chroma_key".into(), stage: FrameStage::Background, step }], scene);
        let want = picture(case["file"].as_str().unwrap(), w, h);
        let g = gap(&got, &want, 4);
        eprintln!("{:<40} max {:>3} mean {:.3} over {:.4}", case["name"].as_str().unwrap(), g.max, g.mean, g.over);
        results.push((case["name"].as_str().unwrap().to_string(), g));
    }
    for (name, g) in &results {
        assert!(g.max <= CHROMA_MAX && g.mean <= CHROMA_MEAN && g.over <= CHROMA_OVER, "{name}: {g:?}");
    }
}

#[test]
fn the_keys_tables_are_pythons_to_the_last_entry() {
    let chroma = json_of("chroma.json");
    for p in chroma["probes"].as_array().unwrap() {
        let bgr: Vec<u8> = p["bgr"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let bgr = [bgr[0], bgr[1], bgr[2]];
        let (cb, cr) = chroma_of(bgr);
        assert_eq!((json!(cb), json!(cr)), (p["chroma"][0].clone(), p["chroma"][1].clone()), "{bgr:?}");
        assert_eq!(keyable(bgr), p["keyable"].as_bool().unwrap(), "{bgr:?}");
    }
    for t in chroma["tables"].as_array().unwrap() {
        let key: Vec<u8> = t["key"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let n = |k: &str| t[k].as_i64().unwrap();
        let got = key_tables([key[0], key[1], key[2]], n("tolerance"), n("softness"), n("spill"));
        let differ = |a: &[u8], b: &str| a.iter().zip(b.as_bytes().chunks(2)).filter(|(x, y)| format!("{x:02x}").as_bytes() != *y).count();
        let (a, cr, cb) = (differ(&got.alpha, t["alpha"].as_str().unwrap()), differ(&got.cr, t["cr"].as_str().unwrap()), differ(&got.cb, t["cb"].as_str().unwrap()));
        assert_eq!((a, cr, cb), (0, 0, 0), "entries that differ for {} {} {} {}", t["key"], t["tolerance"], t["softness"], t["spill"]);
    }
}

fn rgb_picture(name: &str, w: usize, h: usize) -> Rgb {
    Rgb::from_frame(&picture(name, w as u32, h as u32)).unwrap()
}

fn frame_rgb(rgb: &Rgb) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, vec![0; rgb.w * rgb.h * 4]);
    rgb.write_to(&mut f);
    f
}

/// How far apart the CPU effects' pictures may be from OpenCV's, as (max, mean, share over 8 levels): the ones made of the same
/// operations are close; the stick figure's lines are drawn with a different anti-aliasing, and ASCII's glyphs are our own.
fn fun_limits(effect: &str) -> (u32, f64, f64) {
    match effect {
        "cloak" | "clones" => (24, 0.5, 0.01),
        "slit" => (4, 0.2, 0.0),
        "light" => (4, 0.2, 0.0),
        "crt" => (6, 0.3, 0.01),
        "stick" => (255, 1.5, 0.03),
        "ascii" => (255, 40.0, 0.6),
        _ => (255, 255.0, 1.0),
    }
}

#[test]
fn the_cpu_fun_effects_make_pythons_pictures() {
    let fun = json_of("fun.json");
    let (w, h) = (fun["width"].as_u64().unwrap() as usize, fun["height"].as_u64().unwrap() as usize);
    let plate = Arc::new(rgb_picture("fun_plate.rgba", w, h));
    let r: Vec<usize> = fun["clone_rect"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let mut mask = Gray::new(w, h);
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            mask.data[y * w + x] = 255;
        }
    }
    let clones = Arc::new(Clones::add(None, &rgb_picture("fun_clone.rgba", w, h), &mask));
    let mut seen = 0;
    for case in fun["cases"].as_array().unwrap() {
        let effect = case["effect"].as_str().unwrap();
        if effect == "game_boy" {
            continue;
        }
        let point = |v: &Value| (v[0].as_f64().unwrap(), v[1].as_f64().unwrap());
        let look = Look {
            effect: effect.into(),
            plate: case["plate"].as_bool().unwrap().then(|| plate.clone()).or_else(|| (effect == "cloak" || effect == "clones").then(|| plate.clone())),
            clones: case["clones"].as_bool().unwrap().then(|| clones.clone()),
            edges: case["edges"].as_i64().unwrap(),
            fade: case["fade"].as_i64().unwrap(),
            brightness: case["brightness"].as_i64().unwrap(),
            spread: case["spread"].as_i64().unwrap(),
            across: case["across"].as_bool().unwrap(),
            lines: case["lines"].as_array().unwrap().iter().map(|l| (point(&l[0]), point(&l[1]))).collect(),
            heads: case["heads"].as_array().unwrap().iter().map(|l| (point(&l[0]), l[1].as_f64().unwrap())).collect(),
            reset: 0,
        };
        let mut e = Effect::named(effect).unwrap();
        let mut shown = None;
        for f in case["frames"].as_array().unwrap() {
            shown = Some(e.run(&rgb_picture(f.as_str().unwrap(), w, h), &look, Inputs { mask: None }));
        }
        let got = frame_rgb(&shown.unwrap());
        let want = picture(case["file"].as_str().unwrap(), w as u32, h as u32);
        let g = gap(&got, &want, 8);
        eprintln!("{:<35} max {:>3} mean {:.3} over {:.4}", case["name"].as_str().unwrap(), g.max, g.mean, g.over);
        let (max, mean, over) = fun_limits(effect);
        assert!(g.max <= max && g.mean <= mean && g.over <= over, "{}: {g:?}", case["name"]);
        seen += 1;
    }
    assert!(seen >= 10);
}

#[test]
fn the_game_boy_makes_pythons_four_shades() {
    if !gpu_here() {
        return;
    }
    let fun = json_of("fun.json");
    let (w, h) = (fun["width"].as_u64().unwrap() as u32, fun["height"].as_u64().unwrap() as u32);
    let case = fun["cases"].as_array().unwrap().iter().find(|c| c["effect"] == "game_boy").unwrap();
    let mut plugin = FunPackPlugin::new();
    plugin.set_config(&json!({"effect": "game_boy"})).unwrap();
    let step = plugin.frame_step().expect("a step");
    let got = run(vec![StagedStep { owner: "fun_pack".into(), stage: FrameStage::Effects, step }], picture(case["frames"][0].as_str().unwrap(), w, h));
    let want = picture(case["file"].as_str().unwrap(), w, h);
    let g = gap(&got, &want, 8);
    eprintln!("game boy max {} mean {:.3} over {:.4}", g.max, g.mean, g.over);
    let shades: std::collections::HashSet<[u8; 3]> = got.data.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
    assert_eq!(shades.len(), 4);
    // a pixel on the edge of a dither threshold can land on the next shade: those are the only ones that differ
    assert!(g.over <= 0.02, "{g:?}");
}

#[test]
fn the_effects_shaders_compile() {
    for (name, plugin) in [("chroma_key", Box::new({ let mut p = ChromaKeyPlugin::new(); p.set_config(&json!({"on": true})).unwrap(); p }) as Box<dyn Plugin>), ("game_boy", Box::new({ let mut p = FunPackPlugin::new(); p.set_config(&json!({"effect": "game_boy"})).unwrap(); p }))] {
        let mut plugin = plugin;
        let Some(FrameStep::Shader(step)) = plugin.frame_step() else { panic!("{name}: a shader") };
        telescope_plugin_host::testing::validate_shader(&step, name).unwrap();
    }
}

#[test]
fn a_picture_far_bigger_than_the_frame_is_averaged_over_everything_each_pixel_covers() {
    if !gpu_here() {
        return;
    }
    // 2400 x 1350 shown at 160 x 90 is 15 x 15 pixels to the one; stripes that repeat every 15 show up any dropped column
    let (pw, ph) = (2400usize, 1350usize);
    let mut source = Rgb::new(pw, ph);
    for y in 0..ph {
        for x in 0..pw {
            let on = |v: bool| if v { 255 } else { 0 };
            source.set(x, y, [on(x % 15 < 8), on(y % 15 < 5), on((x + y) % 15 < 8)]);
        }
    }
    let picture_frame = frame_rgb(&source);
    let reader: telescope_effects::shared::PictureReader = Arc::new(move |_| Ok(picture_frame.clone()));
    let mut plugin = ChromaKeyPlugin::with_reader(reader);
    plugin.set_config(&json!({"on": true, "screen": "green", "background": "picture", "picture": "big.png"})).unwrap();
    let (w, h) = (160usize, 90usize);
    // all screen: the whole frame is the picture behind
    let scene = frame_rgb(&Rgb::filled(w, h, [50, 180, 60]));
    let got = run(vec![StagedStep { owner: "chroma_key".into(), stage: FrameStage::Background, step: plugin.frame_step().expect("a key") }], scene);
    let want = Rgb { w, h, data: telescope_effects::imgops::resize_area(&source.data, pw, ph, 3, w, h) };
    let g = gap(&got, &frame_rgb(&want), 1);
    eprintln!("a picture 15 times the frame: max {} mean {:.3} over {:.4}", g.max, g.mean, g.over);
    assert!(g.max <= 2 && g.mean <= 0.1 && g.over <= 0.005, "{g:?}");
}

/// The picture through the window's view steps, as the stream sees it.
fn through_the_view(view: &View, frame: Frame) -> Frame {
    let steps: Vec<StagedStep> = telescope_core::view_step::steps(view).into_iter()
        .map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect();
    run(steps, frame)
}

/// Where the lit pixels of a picture are on average, as (u, v) of its size: pixels with red above 20, like the Python test's.
fn lit_centre(out: &Frame) -> (f64, f64) {
    let (mut xs, mut ys, mut n) = (0.0, 0.0, 0.0);
    for (i, p) in out.data.chunks(4).enumerate() {
        if p[0] > 20 {
            xs += (i % out.width as usize) as f64;
            ys += (i / out.width as usize) as f64;
            n += 1.0;
        }
    }
    assert!(n > 0.0, "something is lit");
    ((xs / n + 0.5) / out.width as f64, (ys / n + 0.5) / out.height as f64)
}

fn marked(w: u32, h: u32, spots: &[(u32, u32)]) -> Frame {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for p in data.chunks_mut(4) {
        p[3] = 255;
    }
    for &(x, y) in spots {
        let i = ((y * w + x) * 4) as usize;
        data[i..i + 3].fill(255);
    }
    Frame::new(w, h, PixelFormat::Rgba, data)
}

#[test]
fn inverse_map_undoes_what_the_preview_shows() {
    if !gpu_here() {
        return;
    }
    // Mark one pixel, run the real view, then map the marked pixel's spot back
    let (w, h, src) = (160u32, 90u32, (97u32, 41u32));
    for rotation in [None, Some(90), Some(180), Some(270)] {
        for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
            for (zoom, pan) in [(1.0f32, (0.0f32, 0.0f32)), (2.0, (0.0, 0.0)), (2.5, (0.6, -0.4))] {
                let view = View { zoom, pan_x: pan.0, pan_y: pan.1, flip_h, flip_v, rotation, ..View::default() };
                let out = through_the_view(&view, marked(w, h, &[src]));
                let (u, v) = lit_centre(&out);
                let (x, y) = view::inverse_map(u, v, w as f64, h as f64, &view::Geometry::from(&view));
                let what = format!("{view:?}");
                assert!((x * w as f64 - (src.0 as f64 + 0.5)).abs() < 1.5, "{what}: x {}", x * w as f64);
                assert!((y * h as f64 - (src.1 as f64 + 0.5)).abs() < 1.5, "{what}: y {}", y * h as f64);
            }
        }
    }
}

#[test]
fn inverse_map_undoes_a_stretch() {
    if !gpu_here() {
        return;
    }
    // A marked spot through the real crop, rotation and stretch, then the shown spot mapped back
    let (w, h, src) = (160u32, 90u32, (84u32, 47u32));
    let spots: Vec<(u32, u32)> = (0..3).flat_map(|dy| (0..3).map(move |dx| (src.0 - 1 + dx, src.1 - 1 + dy))).collect();
    for (stretch_x, stretch_y) in [(1.0f32, 1.0f32), (1.5, 1.0), (1.0, 0.6), (0.5, 2.0), (2.0, 0.5)] {
        for rotation in [None, Some(90)] {
            for (zoom, pan) in [(1.0f32, (0.0f32, 0.0f32)), (2.0, (0.2, -0.1))] {
                let view = View { zoom, pan_x: pan.0, pan_y: pan.1, flip_h: true, rotation, stretch_x, stretch_y, ..View::default() };
                let out = through_the_view(&view, marked(w, h, &spots));
                let (u, v) = lit_centre(&out);
                let (x, y) = view::inverse_map(u, v, w as f64, h as f64, &view::Geometry::from(&view));
                let what = format!("{view:?}");
                assert!((x * w as f64 - (src.0 as f64 + 0.5)).abs() < 1.5, "{what}: x {}", x * w as f64);
                assert!((y * h as f64 - (src.1 as f64 + 0.5)).abs() < 1.5, "{what}: y {}", y * h as f64);
            }
        }
    }
}

#[test]
fn a_slow_pan_moves_the_picture_evenly_instead_of_in_whole_pixels() {
    if !gpu_here() {
        return;
    }
    // A soft blob, so its centre says how far the picture moved to a fraction of a pixel. Each frame the window moves about a
    // quarter of an output pixel: whole-pixel crops made that 0, 0, 1.4, 0...
    let (w, h) = (640u32, 360u32);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let d2 = (x as f64 - 320.0).powi(2) + (y as f64 - 180.0).powi(2);
            let v = (230.0 * (-d2 / (2.0 * 30.0 * 30.0)).exp()).round() as u8;
            data.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let frame = Frame::new(w, h, PixelFormat::Rgba, data);
    let centres: Vec<f64> = (0..12).map(|i| {
        let view = View { zoom: 1.4, pan_x: -0.5 + i as f32 * 0.002, ..View::default() };
        let out = through_the_view(&view, frame.clone());
        let (mut sum, mut total) = (0.0, 0.0);
        for (j, p) in out.data.chunks(4).enumerate() {
            sum += (j % w as usize) as f64 * p[0] as f64;
            total += p[0] as f64;
        }
        sum / total
    }).collect();
    let steps: Vec<f64> = centres.windows(2).map(|c| c[1] - c[0]).collect();
    let (lo, hi) = steps.iter().fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)));
    // The blob's centre is read off 8-bit pixels, so it wobbles a few hundredths; whole-pixel crops would be a pixel and more
    assert!(steps.iter().all(|s| *s < -0.05) && hi - lo < 0.1, "{steps:?}");
}
