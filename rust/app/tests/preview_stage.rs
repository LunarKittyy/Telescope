// test_preview.py through the window: the real Preview, real frames painted on the stage and the pop-out, and the real mouse.
mod rig;

use rig::{Listener, Rig};
use serde_json::json;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value as SValue};
use std::sync::{Arc, Mutex};
use telescope_core::event::{Event, LensBox};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::{Host, Pick, PickKind};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::pose::Person;
use telescope_core::preview::PreviewPass;
use telescope_shell::offscreen;
use telescope_shell::plugins::preview::Preview;
use telescope_shell::worker::VideoWorker;

/// Hears what the stage says to the rest of the app.
#[derive(Clone, Default)]
struct Heard {
    picked: Arc<Mutex<Vec<(f32, f32)>>>,
    dragged: Arc<Mutex<Vec<(f32, f32)>>>,
    scrolled: Arc<Mutex<Vec<(f32, f32, f32)>>>,
    finished: Arc<Mutex<Vec<Option<Pick>>>>,
}

struct Ears(Heard);

impl Plugin for Ears {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "ears".into(), title: "Ears".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        match event {
            Event::FocusPointPicked { x, y } => self.0.picked.lock().unwrap().push((*x, *y)),
            Event::ViewDragged { du, dv } => self.0.dragged.lock().unwrap().push((*du, *dv)),
            Event::ViewScrolled { factor, u, v } => self.0.scrolled.lock().unwrap().push((*factor, *u, *v)),
            Event::PreviewPicked { pick, .. } => self.0.finished.lock().unwrap().push(pick.clone()),
            _ => {}
        }
    }
}

struct Red;

impl PreviewPass for Red {
    fn tints(&self) -> bool {
        true
    }
    fn tint(&self, frame: &mut Frame) {
        frame.data_mut().as_chunks_mut::<4>().0.iter_mut().for_each(|p| *p = [255, 0, 0, 255]);
    }
}

struct Stage {
    r: Rig,
    heard: Heard,
}

fn stage(name: &str) -> Stage {
    let heard = Heard::default();
    let ears = heard.clone();
    let r = Rig::with(name, move |_, _| vec![Box::new(Ears(ears)) as Box<dyn Plugin>, Box::new(Preview::default())]);
    r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    r.w.ui.show().unwrap();
    Stage { r, heard }
}

fn on_air(name: &str) -> Stage {
    let s = stage(name);
    s.r.start();
    s.r.worker(0).says("ok", "Streaming");
    s.r.pump();
    s
}

fn grey(w: u32, h: u32) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [128, 128, 128, 255].repeat((w * h) as usize))
}

/// Pure blue as an I420 frame, which the preview's scaler turns to RGB with or without a graphics card.
fn blue(w: u32, h: u32) -> Frame {
    let (y, chroma) = ((w * h) as usize, (w.div_ceil(2) * h.div_ceil(2)) as usize);
    let mut data = vec![41u8; y];
    data.extend(vec![240u8; chroma]);
    data.extend(vec![110u8; chroma]);
    Frame::new(w, h, PixelFormat::I420, data)
}

impl Stage {
    fn name(&self, n: &str) -> String {
        let id = self.r.w.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-has-frame")).expect("the preview card");
        format!("{}{n}", id.trim_end_matches("has-frame"))
    }

    fn prop(&self, n: &str) -> SValue {
        self.r.w.ui.get_property(&self.name(n)).unwrap()
    }

    fn number(&self, n: &str) -> f32 {
        match self.prop(n) {
            SValue::Number(v) => v as f32,
            other => panic!("{n} is {other:?}"),
        }
    }

    fn text(&self, n: &str) -> String {
        match self.prop(n) {
            SValue::String(v) => v.to_string(),
            other => panic!("{n} is {other:?}"),
        }
    }

    fn on(&self, n: &str) -> bool {
        self.prop(n) == SValue::Bool(true)
    }

    fn has_frame(&self) -> bool {
        self.on("has-frame")
    }

    fn image_size(&self) -> (u32, u32) {
        match self.prop("frame") {
            SValue::Image(i) => (i.size().width, i.size().height),
            other => panic!("frame is {other:?}"),
        }
    }

    fn pixel(&self) -> (u8, u8, u8) {
        let SValue::Image(image) = self.prop("frame") else { panic!("no picture") };
        let rgb = image.to_rgb8().expect("the picture's pixels");
        let p = rgb.as_slice()[0];
        (p.r, p.g, p.b)
    }

    fn show(&self, worker: usize, frame: Frame) {
        self.r.worker(worker).slot().publish(frame);
        self.r.w.paint_preview();
    }

    fn call(&self, plugin: &str, f: impl FnOnce(&mut dyn Host)) {
        let i = self.r.w.state.borrow().index(plugin).expect("the plugin");
        self.r.w.state.borrow_mut().call(i, |_, host| f(host));
        self.r.w.process();
        self.r.pump();
    }

    /// Where (u, v) of the picture is in the window, by the rect the card's own bindings put it in.
    fn at(&self, u: f32, v: f32) -> LogicalPosition {
        let (x, y, ..) = self.r.w.card_rect("preview").expect("the card");
        let (sw, sh) = (self.number("stage-width"), self.number("stage-height"));
        let (pw, ph) = (self.number("pic-width"), self.number("pic-height"));
        LogicalPosition::new(x + 1.0 + (sw - pw) / 2.0 + u * pw, y + 1.0 + (sh - ph) / 2.0 + v * ph)
    }

    fn send(&self, event: WindowEvent) {
        self.r.w.ui.window().dispatch_event(event);
        self.r.w.process();
    }

    fn press(&self, position: LogicalPosition) {
        self.send(WindowEvent::PointerMoved { position });
        self.send(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    }

    fn drag(&self, position: LogicalPosition) {
        self.send(WindowEvent::PointerMoved { position });
    }

    fn release(&self, position: LogicalPosition) {
        self.send(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    }

    fn click(&self, position: LogicalPosition) {
        self.press(position);
        self.release(position);
    }

    fn wheel(&self, position: LogicalPosition, notches: f32) {
        self.send(WindowEvent::PointerMoved { position });
        self.send(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: notches * 60.0 });
    }

    fn pop_out(&self) -> ComponentInstance {
        self.r.w.call_ui("preview", "card", "pop-out");
        self.r.w.paint_preview();
        self.r.w.popout_window().expect("the pop-out")
    }

    fn picked(&self) -> Vec<(f32, f32)> {
        self.heard.picked.lock().unwrap().clone()
    }

    fn dragged(&self) -> Vec<(f32, f32)> {
        self.heard.dragged.lock().unwrap().clone()
    }
}

/// A frame much wider than the stage, so there are bars above and below it.
fn letterboxed(name: &str) -> Stage {
    let s = on_air(name);
    s.show(0, grey(400, 100));
    assert!(s.has_frame());
    assert!(s.number("pic-height") < s.number("stage-height") - 40.0, "there are bars");
    s
}

fn near(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.004 && (a.1 - b.1).abs() < 0.004
}

#[test]
fn a_click_on_the_frame_is_a_point_in_it_and_the_bars_are_ignored() {
    let s = letterboxed("stage-click");
    s.click(s.at(0.25, 0.5));
    assert_eq!(s.picked(), [], "not while the lens can't focus on a point");
    assert!(!s.on("marker-shown"));
    s.r.w.emit(Event::FocusPointAvailable { available: true });
    assert_eq!(s.text("stage-cursor"), "cross");
    s.click(s.at(0.25, 0.5));
    s.click(s.at(0.25, -0.2));
    assert_eq!(s.picked().len(), 1, "{:?}", s.picked());
    assert!(near(s.picked()[0], (0.25, 0.5)), "{:?}", s.picked());
    assert!(s.on("marker-shown"));
    assert!((s.number("marker-u") - 0.25).abs() < 0.004 && (s.number("marker-v") - 0.5).abs() < 0.004);
}

#[test]
fn a_click_doesnt_focus_where_the_picture_only_pans() {
    let s = letterboxed("stage-pan-only");
    s.r.w.emit(Event::ViewPannable { pannable: true });
    s.click(s.at(0.25, 0.5));
    assert_eq!(s.picked(), []);
    assert!(!s.on("marker-shown"));
}

#[test]
fn dragging_the_preview_pans_instead_of_focusing() {
    let s = letterboxed("stage-drag");
    s.r.w.emit(Event::FocusPointAvailable { available: true });
    s.r.w.emit(Event::ViewPannable { pannable: true });
    assert_eq!(s.text("stage-cursor"), "grab");

    let start = s.at(0.25, 0.5);
    s.press(start);
    s.drag(LogicalPosition::new(start.x + 2.0, start.y)); // a wobble, still a click
    assert_eq!(s.dragged(), []);
    s.drag(s.at(0.35, 0.6));
    assert_eq!(s.text("stage-cursor"), "grabbing");
    s.drag(s.at(0.45, 0.6));
    s.release(s.at(0.45, 0.6));
    let moved = s.dragged();
    assert_eq!(moved.len(), 2, "{moved:?}");
    assert!(near(moved[0], (0.1, 0.1)) && near(moved[1], (0.1, 0.0)), "from the press, as fractions of the frame: {moved:?}");
    assert_eq!(s.picked(), []);
    assert_eq!(s.text("stage-cursor"), "grab");

    s.press(start);
    s.release(LogicalPosition::new(start.x + 1.0, start.y));
    assert_eq!(s.picked().len(), 1);
    assert!(near(s.picked()[0], (0.25, 0.5)), "a click still focuses, where it was pressed: {:?}", s.picked());
}

#[test]
fn the_wheel_over_the_frame_zooms_around_the_mouse() {
    let s = letterboxed("stage-wheel");
    s.wheel(s.at(0.25, 0.5), 1.0);
    s.wheel(s.at(0.25, 0.5), -2.0);
    s.wheel(s.at(0.25, -0.2), 1.0); // in the bars: nothing
    let seen = s.heard.scrolled.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!((seen[0].0 - 1.1).abs() < 1e-4 && (seen[1].0 - 1.0 / 1.21).abs() < 1e-4, "{seen:?}");
    assert!(near((seen[0].1, seen[0].2), (0.25, 0.5)) && near((seen[1].1, seen[1].2), (0.25, 0.5)), "{seen:?}");
}

#[test]
fn a_pick_takes_the_mouse_from_focusing_and_panning() {
    let s = letterboxed("stage-pick");
    s.r.w.emit(Event::FocusPointAvailable { available: true });
    s.r.w.emit(Event::ViewPannable { pannable: true });
    s.call("ears", |host| assert!(host.pick_on_preview("ears", PickKind::Rect, None, "")));
    assert!(s.on("picking"));
    assert_eq!(s.text("pick-cursor"), "cross");
    s.press(s.at(0.25, 0.25));
    s.drag(s.at(0.5, 0.5));
    s.release(s.at(0.5, 0.5));
    assert_eq!((s.picked(), s.dragged()), (vec![], vec![]));
    let finished = s.heard.finished.lock().unwrap().clone();
    assert_eq!(finished.len(), 1);
    let pick = finished[0].clone().expect("the rectangle");
    let corners: Vec<f32> = pick.shown.iter().flatten().copied().collect();
    assert!(corners.iter().zip([0.25, 0.25, 0.5, 0.5]).all(|(a, b)| (a - b).abs() < 0.004), "{corners:?}");
    assert!(!s.on("picking"));
    assert_eq!(s.text("stage-cursor"), "grab", "back to panning once it's done");
}

#[test]
fn a_click_on_a_warped_picture_focuses_where_it_came_from() {
    let s = letterboxed("stage-warp");
    s.r.w.emit(Event::FocusPointAvailable { available: true });
    s.call("ears", |host| host.set_preview_warp("Desk", Some([2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0]))); // zoomed in twice by a plugin
    s.click(s.at(0.25, 0.5));
    assert_eq!(s.picked().len(), 1);
    assert!(near(s.picked()[0], (0.125, 0.25)), "{:?}", s.picked());
    assert!(s.on("warped"), "and the lens outlines and skeleton stay hidden meanwhile");
}

fn lens_box() -> LensBox {
    LensBox { label: "3\u{d7}".into(), x0: 0.25, y0: 0.25, x1: 0.75, y1: 0.75 }
}

/// Whether anything near this spot of the window as drawn is brighter than the black picture under it.
fn lit_near(s: &Stage, at: LogicalPosition) -> bool {
    let picture = offscreen::draw(s.r.w.ui.window(), 1200.0, 800.0, 1.0).expect("a picture of the window");
    let row = picture.width as i32;
    (-3..=3).flat_map(|dy| (-3..=3).map(move |dx| (dx, dy))).any(|(dx, dy)| {
        let i = ((at.y as i32 + dy) * row + at.x as i32 + dx) as usize * 4;
        picture.rgba[i..i + 3].iter().any(|c| *c > 100)
    })
}

#[test]
fn the_lens_boxes_are_drawn_over_the_picture_and_not_past_a_warp() {
    let s = on_air("stage-boxes");
    s.show(0, Frame::new(640, 360, PixelFormat::Rgba, [0, 0, 0, 255].repeat(640 * 360)));
    let (left, mid) = (s.at(0.25, 0.5), s.at(0.5, 0.5));
    assert!(!lit_near(&s, left), "nothing moved yet");
    s.r.w.emit(Event::LensBoxes { boxes: vec![lens_box()], moved: true });
    assert!(lit_near(&s, left), "the box's left edge");
    assert!(!lit_near(&s, mid), "it's an outline");
    s.call("ears", |host| host.set_preview_warp("Desk", Some([2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0])));
    assert!(!lit_near(&s, left), "the box would land where the picture no longer is");
}

fn shoulders() -> serde_json::Value {
    Person::new([("left_shoulder", 0.7, 0.5, 0.99), ("right_shoulder", 0.3, 0.5, 0.99)]).to_value()
}

#[test]
fn the_skeleton_is_drawn_over_the_picture() {
    let s = on_air("stage-skeleton");
    s.show(0, Frame::new(640, 360, PixelFormat::Rgba, [0, 0, 0, 255].repeat(640 * 360)));
    let (on_line, off_line) = (s.at(0.5, 0.5), s.at(0.5, 0.2));
    s.r.w.emit(Event::VisionStateChanged { state: "ready".into() });
    assert!(!lit_near(&s, on_line));
    assert!(s.on("skeleton-shown"));
    assert!(s.r.w.call_ui("preview", "card", "skeleton-clicked"));
    s.r.w.emit(Event::PeopleSeen { source_id: "pixel".into(), people: vec![shoulders()] });
    assert!(lit_near(&s, on_line), "the line between the shoulders");
    assert!(!lit_near(&s, off_line));
}

#[test]
fn the_popout_pans_zooms_and_shows_boxes_too() {
    let s = on_air("stage-popout");
    s.r.w.emit(Event::ViewPannable { pannable: true });
    s.r.w.emit(Event::LensBoxes { boxes: vec![lens_box()], moved: true });
    let p = s.pop_out();
    s.show(0, grey(640, 360));
    let get = |n: &str| p.get_property(n).unwrap();
    assert_eq!(get("has-frame"), SValue::Bool(true));
    assert_eq!(get("stage-cursor"), SValue::String("grab".into()));
    assert_eq!(get("boxes-opacity"), SValue::Number(1.0));
    let boxes = telescope_shell::source::to_json(&get("lens-boxes"));
    assert_eq!(boxes[0]["label"], json!("3\u{d7}"));

    let position = |x: f32, y: f32| LogicalPosition::new(x, y);
    let send = |e: WindowEvent| {
        p.window().dispatch_event(e);
        s.r.w.process();
    };
    send(WindowEvent::PointerMoved { position: position(100.0, 100.0) });
    send(WindowEvent::PointerPressed { position: position(100.0, 100.0), button: PointerEventButton::Left });
    send(WindowEvent::PointerMoved { position: position(228.0, 100.0) });
    send(WindowEvent::PointerReleased { position: position(228.0, 100.0), button: PointerEventButton::Left });
    let moved = s.dragged();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(near(moved[0], (0.2, 0.0)), "{moved:?}");
    send(WindowEvent::PointerScrolled { position: position(320.0, 180.0), delta_x: 0.0, delta_y: 60.0 });
    let seen = s.heard.scrolled.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!((seen[0].0 - 1.1).abs() < 1e-4 && near((seen[0].1, seen[0].2), (0.5, 0.5)), "{seen:?}");
}

#[test]
fn a_pick_runs_on_the_popout_while_it_is_open() {
    let s = on_air("stage-popout-pick");
    let p = s.pop_out();
    s.show(0, grey(640, 360));
    s.call("ears", |host| assert!(host.pick_on_preview("ears", PickKind::Point, None, "Click")));
    assert_eq!(p.get_property("picking").unwrap(), SValue::Bool(true));
    assert!(!s.on("picking"), "the card is hidden");
    assert!(!s.on("showing"));
    let position = LogicalPosition::new(320.0, 90.0);
    p.window().dispatch_event(WindowEvent::PointerMoved { position });
    p.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    p.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    s.r.w.process();
    let finished = s.heard.finished.lock().unwrap().clone();
    assert_eq!(finished.len(), 1);
    let shown = finished[0].clone().expect("the point").shown;
    assert!(near((shown[0][0], shown[0][1]), (0.5, 0.25)), "{shown:?}");
    assert_eq!(p.get_property("picking").unwrap(), SValue::Bool(false));
}

#[test]
fn the_popout_goes_blank_when_the_stream_stops() {
    let s = on_air("stage-popout-stop");
    let p = s.pop_out();
    s.show(0, grey(64, 36));
    assert_eq!(p.get_property("has-frame").unwrap(), SValue::Bool(true));
    s.r.stop(None);
    s.r.pump();
    s.r.w.paint_preview();
    assert_eq!(p.get_property("has-frame").unwrap(), SValue::Bool(false));
}

#[test]
fn frames_are_scaled_to_the_view_on_the_stream_thread() {
    let s = on_air("stage-scaled");
    s.show(0, grey(1920, 1080));
    let (w, h) = (s.number("stage-width"), s.number("stage-height"));
    let scale = (w / 1920.0).min(h / 1080.0);
    assert!(scale < 1.0);
    assert_eq!(s.image_size(), ((1920.0 * scale).round() as u32, (1080.0 * scale).round() as u32));

    let p = s.pop_out();
    p.window().set_size(slint::PhysicalSize::new(800, 600));
    s.show(0, grey(1920, 1080));
    let SValue::Image(image) = p.get_property("frame").unwrap() else { panic!("no picture") };
    assert_eq!((image.size().width, image.size().height), (800, 450));
}

#[test]
fn card_preview_downscales_large_frame_before_signal() {
    let s = on_air("stage-large");
    s.show(0, grey(1200, 600));
    let width = s.number("stage-width").min(s.number("stage-height") * 2.0).round() as u32;
    assert!(width < 1200);
    assert_eq!(s.image_size(), (width, width / 2));
}

#[test]
fn card_preview_copies_small_frame_and_popout_keeps_full_resolution() {
    let s = on_air("stage-small");
    s.show(0, grey(200, 100));
    assert_eq!(s.image_size(), (200, 100));
    let p = s.pop_out();
    s.show(0, grey(200, 100));
    let SValue::Image(image) = p.get_property("frame").unwrap() else { panic!("no picture") };
    assert_eq!((image.size().width, image.size().height), (200, 100));
}

#[test]
fn a_frame_sized_for_the_view_goes_up_unscaled_and_keeps_its_colours() {
    let s = on_air("stage-colours");
    s.show(0, blue(320, 180));
    assert_eq!(s.image_size(), (320, 180));
    let (r, g, b) = s.pixel();
    assert!(r < 12 && g < 12 && b > 240, "blue stays blue: {:?}", (r, g, b));
}

#[test]
fn on_frame_updates_card_pixmap_and_clears_busy() {
    let s = on_air("stage-frames");
    assert!(!s.has_frame());
    s.show(0, blue(32, 18));
    assert!(s.has_frame());
    s.show(0, grey(64, 36));
    assert_eq!(s.image_size(), (64, 36), "the next frame goes up too");
}

#[test]
fn process_frame_is_zero_copy_when_inactive_or_busy() {
    let s = on_air("stage-hidden");
    s.r.w.call_ui("preview", "card", "toggle");
    let frame = grey(32, 18);
    let held = frame.data.clone();
    s.r.worker(0).slot().publish(frame);
    let holders = Arc::strong_count(&held);
    s.r.w.paint_preview();
    assert!(!s.has_frame());
    assert!(matches!(s.prop("frame"), SValue::Image(i) if i.size().width == 0), "nothing was drawn for it");
    assert_eq!(Arc::strong_count(&held), holders, "and nothing kept it");
}

#[test]
fn host_hide_pauses_the_card_without_turning_it_off() {
    let s = on_air("stage-tray");
    s.r.w.ui.hide().unwrap();
    s.show(0, grey(32, 18));
    assert!(!s.has_frame());
    assert!(s.on("showing"));
    assert_eq!(s.text("toggle-text"), "Hide");
    s.r.w.ui.show().unwrap();
    s.show(0, grey(32, 18));
    assert!(s.has_frame());
}

#[test]
fn a_frame_that_arrives_after_stop_does_not_cover_the_idle_text() {
    let s = on_air("stage-late");
    s.show(0, grey(32, 18));
    assert!(s.has_frame());
    s.r.stop(None);
    s.r.pump();
    s.show(0, grey(32, 18));
    assert!(!s.has_frame());
    assert!(s.text("stage-text").starts_with("Not streaming"));
}

#[test]
fn a_frame_from_just_before_the_camera_went_off_is_dropped() {
    let heard = Heard::default();
    let ears = heard.clone();
    let r = Rig::with("stage-camera-off", move |_, log| {
        let mut mic = Listener::new("microphone", log);
        mic.config = json!({"enabled": true});
        vec![Box::new(Ears(ears)) as Box<dyn Plugin>, Box::new(Preview::default()), Box::new(mic)]
    });
    r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    r.w.ui.show().unwrap();
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let s = Stage { r, heard };
    s.show(0, grey(32, 18));
    assert!(s.has_frame());
    s.r.w.set_camera_on(false, false);
    s.r.pump();
    assert!(s.on("camera-off") && !s.has_frame(), "the stage says so");
    s.show(0, grey(32, 18));
    assert!(!s.has_frame());
    let p = s.pop_out();
    s.r.w.set_camera_on(true, false);
    s.r.pump();
    s.r.until("the new worker", |r| r.workers_made() == 2);
    s.r.worker(1).says("ok", "Streaming");
    s.r.pump();
    s.show(1, grey(32, 18));
    assert_eq!(p.get_property("has-frame").unwrap(), SValue::Bool(true));
    s.r.w.set_camera_on(false, false);
    s.r.pump();
    s.r.w.paint_preview();
    assert_eq!(p.get_property("has-frame").unwrap(), SValue::Bool(false), "and the pop-out goes blank with it");
}

#[test]
fn a_new_stream_frees_a_preview_stuck_waiting_for_a_lost_frame() {
    let s = on_air("stage-new-stream");
    s.show(0, grey(32, 18));
    assert!(s.has_frame());
    s.r.stop(None);
    s.r.pump();
    s.r.w.paint_preview();
    s.r.start();
    s.r.until("the second worker", |r| r.workers_made() == 2);
    s.r.worker(1).says("ok", "Streaming");
    s.r.pump();
    assert!(!s.has_frame());
    s.show(1, grey(32, 18));
    assert!(s.has_frame(), "its first frame has the number the last stream's had");
}

#[test]
fn a_frame_that_fails_to_show_still_frees_the_preview() {
    let s = on_air("stage-broken");
    s.show(0, Frame::new(0, 0, PixelFormat::Rgba, Vec::new()));
    s.show(0, grey(32, 18));
    assert_eq!(s.image_size(), (32, 18));
}

#[test]
fn a_large_frame_does_not_pin_the_column_open() {
    let s = stage("stage-pinned");
    s.r.w.ui.window().set_size(slint::LogicalSize::new(800.0, 600.0));
    s.r.start();
    s.r.worker(0).says("ok", "Streaming");
    s.r.pump();
    s.r.w.process();
    let before = (s.number("stage-width"), s.r.w.card_rect("preview").unwrap().2);
    s.show(0, grey(1920, 1080));
    assert!(s.has_frame());
    assert_eq!((s.number("stage-width"), s.r.w.card_rect("preview").unwrap().2), before);
    assert!(s.number("pic-width") <= before.0);
}

#[test]
fn the_preview_shows_the_picture_before_the_colour_step_left_of_the_split() {
    let s = on_air("stage-split");
    let frame = Frame::new(80, 40, PixelFormat::Rgba, [245, 245, 245, 255].repeat(80 * 40));
    assert!(Arc::ptr_eq(&s.r.w.with_colour_split(frame.clone()).data, &frame.data), "no split asked for");
    s.r.w.state.borrow().colour_before.lock().unwrap().insert("pixel".into(), Frame::new(80, 40, PixelFormat::Rgba, [10, 10, 10, 255].repeat(80 * 40)));
    s.r.w.state.borrow_mut().colour.set_split("grade", Some(0.5));
    let shown = s.r.w.with_colour_split(frame.clone());
    assert!(frame.data.chunks(4).all(|p| p[0] == 245), "drawn on a copy");
    assert_eq!((shown.data[0], shown.data[60 * 4]), (10, 245));
    s.r.w.state.borrow().colour_before.lock().unwrap().clear();
    assert!(Arc::ptr_eq(&s.r.w.with_colour_split(frame.clone()).data, &frame.data), "nothing kept from this stream");
    s.r.w.state.borrow_mut().colour.set_split("grade", None);
    assert!(Arc::ptr_eq(&s.r.w.with_colour_split(frame.clone()).data, &frame.data));
}

#[test]
fn layers_tint_the_preview_copy_and_not_the_frame_apps_get() {
    let s = on_air("stage-tint");
    s.call("ears", |host| host.add_preview_pass("red", Arc::new(Red)));
    let frame = Frame::new(64, 36, PixelFormat::Rgba, [0, 0, 0, 255].repeat(64 * 36));
    let sent = frame.clone();
    s.show(0, frame);
    assert_eq!(s.pixel(), (255, 0, 0), "the picture on the stage");
    assert!(sent.data.chunks(4).all(|p| p == [0, 0, 0, 255]), "the frame itself");
    assert!(s.r.worker(0).slot().latest().unwrap().0.data.chunks(4).all(|p| p == [0, 0, 0, 255]), "what the stream still holds");
}
