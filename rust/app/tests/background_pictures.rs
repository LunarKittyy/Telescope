// test_background.py with the real picture reader on real files, and the real card: what can't be done from the effects crate,
// which mustn't know about media.
use i_slint_backend_testing::ElementHandle;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::path::Path;
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::plugin::Plugin;
use telescope_core::testing::{scratch_dir, FakeHost};
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_core::vision;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::shared::{picture_reader, PictureReader};
use telescope_shell::source::to_slint;

fn reader() -> PictureReader {
    picture_reader(Arc::new(telescope_media::FfmpegMedia))
}

fn write_png(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes).write_image(&rgb.repeat((w * h) as usize), w, h, ExtendedColorType::Rgb8).unwrap();
    std::fs::write(path, bytes).unwrap();
}

// test_background.py::test_a_picture_with_a_non_ascii_path_opens
#[test]
fn a_picture_with_a_non_ascii_path_opens() {
    let dir = scratch_dir("background-pictures");
    let path = dir.join("Bakgrund åäö.png");
    write_png(&path, 20, 10, [7, 7, 7]);
    let read = reader();
    let picture = read(path.to_str().unwrap()).expect("the picture");
    assert_eq!((picture.width, picture.height), (20, 10));
    assert!(read(dir.join("missing.png").to_str().unwrap()).is_err());
    std::fs::write(dir.join("broken.png"), b"not a picture").unwrap();
    assert!(read(dir.join("broken.png").to_str().unwrap()).is_err());
}

/// A person on the left half.
struct HalfMask;

impl FrameContext for HalfMask {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        Some(Arc::new((0..w * h).map(|i| if i % w < w / 2 { 255 } else { 0 }).collect()))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn grey_frame() -> Frame {
    Frame::new(64, 36, PixelFormat::Rgba, [200, 200, 200, 255].repeat(64 * 36))
}

fn process(plugin: &mut BackgroundPlugin, frame: &Frame) -> Frame {
    let mut out = frame.clone();
    if let Some(FrameStep::Cpu(mut step)) = plugin.frame_step() {
        step.process(&mut out, &HalfMask);
    }
    out
}

fn ready() -> (BackgroundPlugin, FakeHost) {
    let (mut plugin, mut host) = (BackgroundPlugin::with_reader(reader()), FakeHost::new());
    host.vision = Some(vision::READY.into());
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    (plugin, host)
}

fn pixels(frame: &Frame, x0: usize, x1: usize) -> Vec<[u8; 3]> {
    let w = frame.width as usize;
    frame.data.chunks(4).enumerate().filter(|(i, _)| (x0..x1).contains(&(i % w))).map(|(_, p)| [p[0], p[1], p[2]]).collect()
}

// test_background.py::test_picture_mode_puts_the_picture_behind_and_survives_a_bad_file
#[test]
fn picture_mode_puts_the_picture_behind_and_survives_a_bad_file() {
    let (mut plugin, mut host) = ready();
    let dir = scratch_dir("background-pictures");
    let path = dir.join("beach.png");
    write_png(&path, 20, 10, [0, 0, 255]);
    let path = path.to_str().unwrap().to_string();
    plugin.set_config(&json!({"mode": "picture", "picture": path, "edges": 0})).unwrap();
    let out = process(&mut plugin, &grey_frame());
    assert!(pixels(&out, 40, 64).iter().all(|p| *p == [0, 0, 255]), "the picture behind");
    assert!(pixels(&out, 0, 30).iter().all(|p| *p == [200, 200, 200]), "the person in front");
    assert_eq!(plugin.get_config()["picture"], json!(path));
    let gone = dir.join("gone.png");
    plugin.set_config(&json!({"mode": "picture", "picture": gone.to_str().unwrap()})).unwrap();
    let frame = grey_frame();
    assert_eq!(process(&mut plugin, &frame).data, frame.data);
    plugin.on_event(&mut host, &Event::StreamStopped);
    let last = host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok()).find_map(|v| v.get("vision-text").cloned());
    assert_eq!(last, Some(json!("Telescope can't open that picture.")));
}

// test_background.py::test_settings_round_trip_and_damaged_ones_fall_back
#[test]
fn settings_round_trip_and_damaged_ones_fall_back() {
    i_slint_backend_testing::init_no_event_loop();
    let (mut plugin, mut host) = ready();
    plugin.set_config(&json!({"mode": "blur", "blur": 9, "edges": 0})).unwrap();
    assert_eq!(plugin.get_config(), json!({"mode": "blur", "blur": 9, "edges": 0, "picture": ""}));
    let spec = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = &spec.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("background.slint", source, vec![], &spec.component).expect("the card compiles");
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(460.0, 400.0));
    ui.show().unwrap();
    let draw = |props: &Value| {
        for (name, value) in props.as_object().unwrap() {
            if let Ok(like) = ui.get_property(name) {
                ui.set_property(name, to_slint(value, &like)).unwrap();
            }
        }
    };
    // The mode's segments carry these names too, so each row is found by what only it has
    let blur_row = || ElementHandle::find_by_accessible_label(&ui, "Blur").any(|e| e.accessible_value_maximum().is_some());
    let picture_row = || ElementHandle::find_by_accessible_label(&ui, "Choose").count() > 0;
    draw(&spec.props);
    assert!(blur_row() && !picture_row(), "the blur row shows, the picture row doesn't");
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "mode-picked".into(), args: vec![json!(2)] });
    draw(&plugin.card().expect("a card").ui.props);
    assert!(!blur_row() && picture_row());
    plugin.set_config(&json!({"mode": "sparkles", "blur": "lots", "edges": 99, "picture": 3})).unwrap();
    assert_eq!(plugin.get_config(), json!({"mode": "off", "blur": 6, "edges": 10, "picture": ""}));
}
