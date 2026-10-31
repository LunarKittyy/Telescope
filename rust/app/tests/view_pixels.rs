// test_view_controller.py with real pixels and the real Transforms card: the view's steps run on the graphics card (they skip
// without one), and the card and the controller meet in the window.
mod rig;

use rig::{Listener, Rig};
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::frame::{Frame, FrameContext, FrameStage, PixelFormat};
use telescope_core::host::{View, ViewChange};
use telescope_core::media::{FramePipeline, Media, StagedStep};
use telescope_core::plugin::Plugin;
use telescope_core::view::{phone_window, window, PhoneZoom};
use telescope_core::view_step::steps;
use telescope_effects::transforms::TransformsPlugin;
use telescope_media::gpu::Gpu;
use telescope_shell::view_control::ViewController;

struct Nothing;

impl FrameContext for Nothing {
    fn source_id(&self) -> &str {
        "p1"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A pipeline on the graphics card, None without one.
fn pipeline() -> Option<Box<dyn FramePipeline>> {
    if Gpu::shared().is_none() {
        eprintln!("no graphics card; skipped");
        return None;
    }
    Some(telescope_media::FfmpegMedia.pipeline().unwrap())
}

fn staged(view: &View) -> Vec<StagedStep> {
    steps(view).into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect()
}

fn zooms() -> Value {
    json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}]})
}

/// _grid: every pixel different.
fn grid() -> Frame {
    let (w, h) = (24u32, 16u32);
    Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [(i * 9 % 200) as u8 + 20, (i * 5 % 200) as u8 + 20, (i % 200) as u8 + 20, 255]).collect())
}

fn flipped(frame: &Frame) -> Vec<u8> {
    let w = frame.width as usize;
    frame.data.chunks(w * 4).flat_map(|row| row.chunks(4).rev().flatten().copied().collect::<Vec<u8>>()).collect()
}

// test_view_controller.py::test_a_frozen_step_keeps_the_view_it_was_taken_with
#[test]
fn a_frozen_step_keeps_the_view_it_was_taken_with() {
    let Some(mut run) = pipeline() else { return };
    let (mut vc, mut view) = (ViewController::new(), View::default());
    vc.set_view(&mut view, &ViewChange { flip_h: Some(true), ..Default::default() }, 0.0);
    let frozen = staged(&vc.frame_view(&view));
    vc.set_view(&mut view, &ViewChange { flip_h: Some(false), rotation: Some(Some(90)), ..Default::default() }, 0.0);
    run.set_steps(frozen);
    let (input, want) = (grid(), flipped(&grid()));
    let out = run.process(input, &Nothing);
    assert_eq!((out.width, out.height, out.format), (24, 16, PixelFormat::Rgba));
    let off = out.data.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(off <= 2, "{off} levels off a plain flip");
}

/// Something that shows a shift: blobs, the same every run (_scene).
fn scene(u: f64, v: f64) -> f64 {
    let cell = |x: i64, y: i64| ((x.wrapping_mul(7_919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761) >> 13 & 255) as f64;
    let (x, y) = (u * 32.0, v * 18.0);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let top = cell(x0, y0) * (1.0 - fx) + cell(x0 + 1, y0) * fx;
    let bottom = cell(x0, y0 + 1) * (1.0 - fx) + cell(x0 + 1, y0 + 1) * fx;
    top * (1.0 - fy) + bottom * fy
}

const SIZE: (u32, u32) = (320, 180);

/// That part of the full view, at the stream's size.
fn part_of_the_scene(zoom: f64, pan_x: f64, pan_y: f64) -> Frame {
    let ((x0, w), (y0, h)) = (window(zoom, pan_x), window(zoom, pan_y));
    let data = (0..SIZE.0 * SIZE.1).flat_map(|i| {
        let (u, v) = (((i % SIZE.0) as f64 + 0.5) / SIZE.0 as f64, ((i / SIZE.0) as f64 + 0.5) / SIZE.1 as f64);
        let grey = scene(x0 + u * w, y0 + v * h) as u8;
        [grey, grey, grey, 255]
    }).collect();
    Frame::new(SIZE.0, SIZE.1, PixelFormat::Rgba, data)
}

/// _from_phone: what the phone sends for a share.
fn from_phone(share: &PhoneZoom) -> Frame {
    let (zoom, pan_x, pan_y) = phone_window(Some(share));
    part_of_the_scene(zoom, pan_x, pan_y)
}

/// How far two pictures are apart on average, green only (as the scene's grey).
fn error(a: &Frame, b: &Frame) -> f64 {
    let total: u64 = a.data.chunks(4).zip(b.data.chunks(4)).map(|(a, b)| u64::from(a[1].abs_diff(b[1]))).sum();
    total as f64 / u64::from(SIZE.0 * SIZE.1) as f64
}

// test_view_controller.py::test_a_new_phone_share_is_cropped_for_only_once_its_frames_arrive
#[test]
fn a_new_phone_share_is_cropped_for_only_once_its_frames_arrive() {
    let Some(mut run) = pipeline() else { return };
    let (mut vc, mut view) = (ViewController::new(), View::default());
    vc.stream_start(&view, "p1", 1, 0.0);
    vc.phone_state(&view, &zooms(), 0.0);
    vc.take("Auto framing");
    vc.steer(&mut view, "Auto framing", 3.0, 0.0, 0.0, 0.0);
    let (gate, old) = (vc.gate(), vc.sent().unwrap());
    run.set_steps(staged(&vc.frame_view(&view)));
    // What the worker does with each frame
    let through = |run: &mut Box<dyn FramePipeline>, frame: Frame, at: f64| {
        if let Some(view) = gate.for_frame("p1", &frame, at, false) {
            run.replace_steps("view", FrameStage::View, steps(&view));
        }
        run.process(frame, &Nothing)
    };
    through(&mut run, from_phone(&old), 0.0);
    vc.steer(&mut view, "Auto framing", 3.0, 0.22, 0.0, 0.01); // just past the side of what the phone sends: a new share at once
    let new = vc.sent().unwrap();
    assert!(new != old && gate.waiting());
    // Said::Restep: the window gives the stream new steps
    run.replace_steps("view", FrameStage::View, steps(&vc.frame_view(&view)));
    let truth = part_of_the_scene(3.0, 0.22, 0.0);
    let (mut late, mut out) = (from_phone(&old), Frame::new(1, 1, PixelFormat::Rgba, vec![0; 4]));
    for i in 0..3 {
        late = from_phone(&old); // the phone hasn't caught up yet
        out = through(&mut run, late.clone(), 0.02 + f64::from(i) * 0.01);
        assert!(gate.waiting());
    }
    // The new share's crop on a frame from the old one
    let mut naive = telescope_media::FfmpegMedia.pipeline().unwrap();
    naive.set_steps(staged(&vc.shown(&view)));
    let naive = naive.process(late, &Nothing);
    let (kept, wrong) = (error(&out, &truth), error(&naive, &truth));
    assert!(kept < 0.5 * wrong, "{kept} against {wrong}");
    let out = through(&mut run, from_phone(&new), 0.06);
    assert!(!gate.waiting());
    assert!(error(&out, &truth) < 12.0, "{}", error(&out, &truth));
}

#[test]
fn a_turned_view_stands_the_picture_up_for_the_pose_model() {
    let turn = telescope_core::vision::Turn { rotation: Some(90), ..Default::default() };
    let up = telescope_vision::ops::turned(&telescope_vision::ops::Rgb::filled(60, 40, 0), turn);
    assert_eq!((up.w, up.h), (40, 60));
}

// test_view_controller.py::test_the_transforms_card_greys_out_zoom_and_pan_while_something_else_moves_them
#[test]
fn the_transforms_card_greys_out_zoom_and_pan_while_something_else_moves_them() {
    let r = Rig::with("view-greys", |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(TransformsPlugin::new())]);
    let props = |r: &Rig| {
        let mut state = r.w.state.borrow_mut();
        let at = state.index("transforms").unwrap();
        state.slots[at].plugin.as_mut().unwrap().card().unwrap().ui.props
    };
    let with_host = |r: &Rig, f: &dyn Fn(&mut dyn telescope_core::Host)| {
        let mut state = r.w.state.borrow_mut();
        let at = state.index("transforms").unwrap();
        state.call(at, |_, host| f(host));
        drop(state);
        r.pump();
    };
    r.configure("transforms", json!({"zoom": 2.0}));
    with_host(&r, &|host| host.set_view(ViewChange { zoom: Some(2.0), ..Default::default() }));
    r.emit(telescope_core::Event::PhonesChanged { count: 1 });
    r.pump();
    let card = props(&r);
    assert_eq!((card["active"].clone(), card["zoom-free"].clone(), card["pan-enabled"].clone()), (json!(true), json!(true), json!(true)));
    with_host(&r, &|host| assert!(host.take_view("Auto framing")));
    let card = props(&r);
    assert_eq!((card["zoom-free"].clone(), card["pan-enabled"].clone()), (json!(false), json!(false)));
    assert_eq!(card["active"], json!(true), "the flips stay on");
    assert!(card["zoom-tip"].as_str().unwrap().starts_with("Auto framing is moving the zoom and pan"), "{}", card["zoom-tip"]);
    r.emit(telescope_core::Event::ViewDragged { du: 0.1, dv: 0.0 }); // dragging the preview doesn't fight it
    r.pump();
    assert_eq!(props(&r)["pan-x"], json!(0.0));
    with_host(&r, &|host| {
        host.steer_view("Auto framing", 3.0, 0.1, 0.0);
        host.release_view("Auto framing");
    });
    let card = props(&r);
    assert_eq!((card["zoom-free"].clone(), card["pan-enabled"].clone()), (json!(true), json!(true)));
    assert_eq!(r.w.state.borrow().view.zoom, 2.0, "the saved zoom is back");
}
