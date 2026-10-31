//! Transforms from the card to the picture, through the window's view controller and the frame steps it makes, on the graphics card
//! (test_transforms.py's tests of what the view does to frames, on its 6 x 4 grid). Without a graphics card the pixels aren't checked.

use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::frame::{Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::View;
use telescope_core::media::{Media, StagedStep};
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_core::view_step::steps;
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::view_control::{Said, ViewController};

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

fn gpu_here() -> bool {
    let here = telescope_media::gpu::Gpu::shared().is_some();
    if !here {
        eprintln!("no graphics card here: the pictures aren't checked");
    }
    here
}

fn run(view_steps: Vec<FrameStep>, frame: Frame) -> Frame {
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(view_steps.into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect());
    pipeline.process(frame, &Ctx)
}

/// test_transforms.py's _grid: 6 wide, 4 tall, 0 to 23.
fn grid() -> Frame {
    Frame::new(6, 4, PixelFormat::Rgba, (0..24u8).flat_map(|v| [v, v, v, 255]).collect())
}

/// A picture's rows, with each pixel grey as the grid's are.
fn rows(frame: &Frame) -> Vec<Vec<u8>> {
    let pixels: Vec<u8> = frame.data.chunks(4).map(|p| {
        assert!(p[0] == p[1] && p[1] == p[2], "{p:?}");
        p[0]
    }).collect();
    pixels.chunks(frame.width as usize).map(<[u8]>::to_vec).collect()
}

fn flipped(rows: Vec<Vec<u8>>, h: bool, v: bool) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = rows.into_iter().map(|mut r| {
        if h {
            r.reverse();
        }
        r
    }).collect();
    if v {
        out.reverse();
    }
    out
}

/// cv2.rotate by a quarter clockwise.
fn clockwise(rows: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    (0..rows[0].len()).map(|x| rows.iter().rev().map(|r| r[x]).collect()).collect()
}

/// The card, the window's view controller as the window joins them, and the view they keep.
struct Rig {
    plugin: TransformsPlugin,
    host: FakeHost,
    vc: ViewController,
    view: View,
}

impl Rig {
    fn new() -> Rig {
        let mut rig = Rig { plugin: TransformsPlugin::new(), host: FakeHost::new(), vc: ViewController::new(), view: View::default() };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.set_config(&json!({})).unwrap();
        rig.plugin.on_event(&mut rig.host, &Event::PhonesChanged { count: 1 });
        rig.hear_view();
        rig
    }

    fn said(&mut self, said: Vec<Said>) {
        for s in said {
            if let Said::Emit(event) = s {
                self.plugin.on_event(&mut self.host, &event);
            }
        }
        self.hear_view();
    }

    /// What the plugin asked of the window's view reaches it.
    fn hear_view(&mut self) {
        for change in std::mem::take(&mut self.host.view_changes) {
            let said = self.vc.set_view(&mut self.view, &change, 0.0);
            self.said(said);
        }
    }

    fn ui(&mut self, callback: &str, args: Value) {
        let args = args.as_array().cloned().unwrap_or_default();
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
        self.hear_view();
    }

    fn frame_step(&self) -> Vec<FrameStep> {
        steps(&self.vc.frame_view(&self.view))
    }

    fn process(&self, frame: Frame) -> Frame {
        run(self.frame_step(), frame)
    }

    /// The newest value the card was given for a property.
    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }
}

#[test]
fn the_view_crops_flips_and_rotates_as_the_card_says() {
    let mut r = Rig::new();
    r.ui("flip-toggled", json!([0, true]));
    r.ui("rotation-picked", json!([1]));
    if gpu_here() {
        let want = clockwise(flipped(rows(&grid()), true, false));
        assert_eq!(rows(&r.process(grid())), want);
    }
}

#[test]
fn a_frozen_frame_step_keeps_the_settings_it_was_taken_with() {
    let mut r = Rig::new();
    r.ui("flip-toggled", json!([0, true]));
    let step = r.frame_step();
    r.ui("flip-toggled", json!([0, false]));
    r.ui("rotation-picked", json!([1]));
    assert_eq!((r.view.flip_h, r.view.rotation), (false, Some(90)));
    if gpu_here() {
        assert_eq!(rows(&run(step, grid())), flipped(rows(&grid()), true, false));
    }
}

#[test]
fn the_view_stretches_the_picture_as_shown() {
    let mut r = Rig::new();
    r.ui("rotation-picked", json!([1]));
    r.ui("stretch-moved", json!([0, 150]));
    r.ui("stretch-moved", json!([1, 80]));
    assert!((r.view.stretch_x - 1.5).abs() < 1e-6 && (r.view.stretch_y - 0.8).abs() < 1e-6, "{:?}", r.view);
    assert_eq!(r.shown("stretch-x-text"), json!("150%"));
    if gpu_here() {
        // apply_stretch(cv2.rotate(_grid(), cv2.ROTATE_90_CLOCKWISE), 1.5, 0.8) from Python; warpAffine works in 32nds of a pixel, so a level either way
        let want: [[i32; 4]; 6] = [[6, 4, 3, 1], [16, 12, 8, 4], [17, 13, 9, 5], [18, 14, 10, 6], [19, 15, 11, 7], [8, 6, 4, 3]];
        for got in [r.process(grid()), run(r.frame_step(), grid())] {
            assert_eq!((got.width, got.height), (4, 6));
            for (y, row) in rows(&got).iter().enumerate() {
                for (x, level) in row.iter().enumerate() {
                    assert!((*level as i32 - want[y][x]).abs() <= 1, "({x}, {y}): {level} against {}", want[y][x]);
                }
            }
        }
    }
}

#[test]
fn transform_flip_combinations() {
    if !gpu_here() {
        return;
    }
    for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
        let got = run(steps(&View { flip_h, flip_v, ..View::default() }), grid());
        assert_eq!(rows(&got), flipped(rows(&grid()), flip_h, flip_v), "{flip_h} {flip_v}");
    }
}

#[test]
fn transform_rotations_match_opencv() {
    if !gpu_here() {
        return;
    }
    let turned = |turns: usize| (0..turns).fold(rows(&grid()), |r, _| clockwise(r));
    for (rotation, want) in [(None, turned(0)), (Some(90), turned(1)), (Some(180), turned(2)), (Some(270), turned(3))] {
        assert_eq!(rows(&run(steps(&View { rotation, ..View::default() }), grid())), want, "{rotation:?}");
    }
}

fn lens_state(active: &str) -> Value {
    json!({"cameras": [{"id": "1", "current": false}, {"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true, "lensZooms": [3.7]},
                       {"id": "2", "current": false, "label": "Back ~24mm OIS [phys]"}, {"id": "4", "current": false, "label": "Back Telephoto ~85mm OIS [phys]"}],
           "active_lens": active})
}

#[test]
fn the_zoom_slider_marks_each_lens() {
    let mut r = Rig::new();
    let stream = StreamInfo { source_id: "a".into(), url: "http://phone/v1/video".into(), session: 0 };
    let said = r.vc.stream_start(&r.view, "a", 0, 0.0);
    r.said(said);
    r.plugin.on_stream_start(&mut r.host, &stream);
    let said = r.vc.phone_state(&r.view, &lens_state("2"), 0.0);
    r.said(said);
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    assert_eq!(r.shown("zoom-marks"), json!([3.7]));
    let said = r.vc.stream_stop(&r.view, 0.0);
    r.said(said);
    r.plugin.on_stream_stop(&mut r.host);
    assert_eq!(r.shown("zoom-marks"), json!([]));
}
