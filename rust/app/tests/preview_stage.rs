// test_preview.py through the window: the real Preview, real frames painted on the stage and the pop-out, and the real mouse.
mod rig;

use rig::{Listener, Rig};
use serde_json::json;
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value as SValue};
use std::sync::{Arc, Mutex};
use telescope_core::event::{Event, LensBox};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::{Host, Pick, PickKind, ViewChange};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::pose::Person;
use telescope_core::preview::PreviewPass;
use telescope_shell::offscreen;
use telescope_shell::plugins::preview::Preview;
use telescope_shell::preview::{CpuPainter, Painter};
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

fn on_air_capped(name: &str) -> Stage {
    let s = stage(name);
    s.r.start();
    s.r.worker(0).says("ok", "Streaming");
    s.r.pump();
    s
}

/// On air with Smooth on: these tests come from Python's, which paints every frame.
fn on_air(name: &str) -> Stage {
    let s = on_air_capped(name);
    assert!(s.r.w.call_ui("preview", "card", "smooth-clicked"));
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
        self.press_button("Pop out");
        self.r.w.popout_window().expect("the pop-out")
    }

    /// The window's own close button on the pop-out.
    fn close_pop_out(&self, p: &ComponentInstance) {
        p.window().dispatch_event(WindowEvent::CloseRequested);
        self.r.w.process();
        self.r.w.paint_preview();
    }

    fn element(&self, label: &str) -> Option<ElementHandle> {
        ElementHandle::find_by_accessible_label(&self.r.w.ui, label).next()
    }

    /// A real click on the toolbar's button with this name, then a turn of the preview's timer.
    fn press_button(&self, label: &str) {
        let e = self.element(label).unwrap_or_else(|| panic!("no {label} button"));
        let (p, size) = (e.absolute_position(), e.size());
        self.click(LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0));
        self.r.w.paint_preview();
    }

    /// Whether the stage reads this, on the Text it's drawn with.
    fn says(&self, text: &str) -> bool {
        self.element(text).is_some()
    }

    fn lines(&self) -> Vec<serde_json::Value> {
        telescope_shell::source::to_json(&self.prop("skeleton-lines")).as_array().cloned().unwrap_or_default()
    }

    fn accent(&self) -> SValue {
        self.r.w.ui.get_global_property("Theme", "accent").unwrap()
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
fn every_window_of_the_shell_has_the_apps_icon() {
    let s = on_air("stage-icon");
    let wide = |value: SValue| matches!(value, SValue::Image(i) if i.size().width == 256);
    assert!(wide(s.r.w.ui.get_property("app-icon").unwrap()));
    assert!(wide(s.pop_out().get_property("app-icon").unwrap()));
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

// Python scales on the stream thread and signals the UI; here the window pulls the newest frame and scales it as it paints,
// so the stream keeps its full frame and is never held up by the preview
#[test]
fn frames_are_scaled_to_the_view_as_they_are_painted() {
    let s = on_air("stage-scaled");
    let frame = grey(1920, 1080);
    let sent = frame.clone();
    s.show(0, frame);
    let (held, _) = s.r.worker(0).slot().latest().unwrap();
    assert!(Arc::ptr_eq(&held.data, &sent.data) && (held.width, held.height) == (1920, 1080), "the stream's own frame stays as it was");
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

// A stream that stopped sending still gets its last picture redrawn when the window it fills is resized
#[test]
fn a_stalled_stream_is_painted_again_at_the_new_size_of_the_popout() {
    let s = on_air("stage-stalled-resize");
    let p = s.pop_out();
    p.window().set_size(slint::PhysicalSize::new(800, 600));
    s.show(0, grey(1920, 1080));
    let size = |p: &ComponentInstance| match p.get_property("frame").unwrap() {
        SValue::Image(i) => (i.size().width, i.size().height),
        other => panic!("frame is {other:?}"),
    };
    assert_eq!(size(&p), (800, 450));
    p.window().set_size(slint::PhysicalSize::new(400, 300));
    s.r.w.paint_preview();
    assert_eq!(size(&p), (400, 225), "no new frame came, the same one is drawn for the new size");
}

// The frame the stream stalled on may be one the card skipped: a resize still paints it
#[test]
fn a_stalled_stream_whose_last_frame_was_skipped_is_painted_at_the_new_size() {
    let s = on_air_capped("stage-stalled-skipped");
    s.show(0, grey(1920, 1080));
    let first = s.image_size();
    s.show(0, grey(1920, 1080));
    assert_eq!(s.image_size(), first, "the second frame is one the card skips");
    s.r.w.ui.window().set_size(slint::PhysicalSize::new(700, 500));
    s.r.w.process();
    s.r.w.paint_preview();
    let scale = s.r.w.ui.window().scale_factor();
    let stage = ((s.number("stage-width") * scale).round() as u32, (s.number("stage-height") * scale).round() as u32);
    let want = telescope_shell::preview::fit_within((1920, 1080), stage);
    assert_ne!(want, first, "the window really got smaller");
    assert_eq!(s.image_size(), want, "no new frame came, the skipped one is drawn for the new size");
}

#[test]
#[ignore = "bench: cargo test --release -p telescope-app --test preview_stage compare_split_cost -- --ignored --nocapture"]
fn compare_split_cost() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let (before, after) = (grey(w, h), blue(w, h));
        let before = Frame::new(w, h, PixelFormat::I420, before.data.iter().step_by(4).copied().chain(std::iter::repeat(128)).take((w * h * 3 / 2) as usize).collect());
        let t = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(telescope_core::colour::with_split(&before, &after, 0.5));
        }
        println!("with_split {w}x{h} I420: {:.2} ms", t.elapsed().as_secs_f64() * 100.0);
        let (before, after) = (grey(w, h), grey(w, h));
        let t = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(telescope_core::colour::with_split(&before, &after, 0.5));
        }
        println!("with_split {w}x{h} RGBA: {:.2} ms", t.elapsed().as_secs_f64() * 100.0);
    }
}

#[test]
fn card_preview_downscales_large_frame_before_signal() {
    let s = on_air("stage-large");
    s.show(0, grey(1200, 600));
    let width = s.number("stage-width").min(s.number("stage-height") * 2.0).round() as u32;
    assert!(width < 1200);
    assert_eq!(s.image_size(), (width, (width as f32 / 2.0).round() as u32), "scaled by the same rounding as every other frame");
    assert_eq!(s.r.worker(0).slot().latest().unwrap().0.width, 1200, "what the stream holds isn't touched");
}

#[test]
fn card_preview_copies_small_frame_and_popout_keeps_full_resolution() {
    let s = on_air("stage-small");
    let frame = grey(200, 100);
    let held = frame.data.clone();
    s.r.worker(0).slot().publish(frame);
    let holders = Arc::strong_count(&held);
    s.r.w.paint_preview();
    assert_eq!(s.image_size(), (200, 100));
    assert_eq!(Arc::strong_count(&held), holders, "the card's picture is a copy: it holds on to none of the stream's memory");
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
    assert!(Arc::ptr_eq(&s.r.worker(0).slot().latest().unwrap().0.data, &held), "the stream's frame goes on as the same memory");
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

// The frame the last stream's paint was waiting on never came: nothing turned the preview's timer between the two streams
#[test]
fn a_new_stream_frees_a_preview_stuck_waiting_for_a_lost_frame() {
    let s = on_air("stage-new-stream");
    s.show(0, grey(32, 18));
    assert!(s.has_frame());
    s.r.stop(None);
    s.r.pump();
    s.r.start();
    s.r.until("the second worker", |r| r.workers_made() == 2);
    s.r.worker(1).says("ok", "Streaming");
    s.r.pump();
    assert!(!s.has_frame());
    s.show(1, grey(32, 18));
    assert!(s.has_frame(), "its first frame has the number the last stream's had");
}

/// Won't draw the first frame it's given (a texture Slint refuses), and draws from then on.
struct RefusesOnce(bool, CpuPainter);

impl Painter for RefusesOnce {
    fn paint(&mut self, frame: &Frame, width: u32, height: u32) -> Option<slint::Image> {
        if std::mem::replace(&mut self.0, false) {
            return None;
        }
        self.1.paint(frame, width, height)
    }
}

#[test]
fn a_frame_that_fails_to_show_still_frees_the_preview() {
    use telescope_core::media::Media;
    let s = on_air("stage-broken");
    s.r.w.use_painter(Box::new(RefusesOnce(true, CpuPainter(telescope_media::FfmpegMedia.scaler()))));
    s.show(0, grey(32, 18));
    assert!(!s.has_frame(), "this one failed to show");
    s.r.w.paint_preview();
    assert_eq!(s.image_size(), (32, 18), "and the preview takes it again, not waiting on it for good");
    s.show(0, grey(64, 36));
    assert_eq!(s.image_size(), (64, 36));
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
    // The tint is made on a worker, and its first picture asks for a repaint
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !s.has_frame() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.r.w.paint_preview();
    }
    assert_eq!(s.pixel(), (255, 0, 0), "the picture on the stage");
    assert!(sent.data.chunks(4).all(|p| p == [0, 0, 0, 255]), "the frame itself");
    assert!(s.r.worker(0).slot().latest().unwrap().0.data.chunks(4).all(|p| p == [0, 0, 0, 255]), "what the stream still holds");
}

const IDLE: &str = "Not streaming";
const IDLE_MIC: &str = "Not streaming";
const WAITING: &str = "Waiting for the first frame\u{2026}";
const CAMERA_OFF: &str = "Camera off\n\nThe phone's mic is still streaming.\nStop streaming ends it.";
const CAMERA_OFF_AUTO: &str = "Camera off\n\nNo app was using the camera, so it turned off.\nThe phone's mic is still streaming, and the camera\ncomes back when an app opens it.";

/// A stage with a Microphone stand-in that's on or off.
fn with_mic(name: &str, on: bool) -> Stage {
    let heard = Heard::default();
    let ears = heard.clone();
    let r = Rig::with(name, move |_, log| {
        let mut mic = Listener::new("microphone", log);
        mic.config = json!({"enabled": on});
        vec![Box::new(Ears(ears)) as Box<dyn Plugin>, Box::new(Preview::default()), Box::new(mic)]
    });
    r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    r.w.ui.show().unwrap();
    Stage { r, heard }
}

#[test]
fn preview_starts_active_and_toggles_off_and_back_on() {
    let s = stage("stage-toggle");
    assert!(s.on("showing") && s.element("Hide").is_some());
    assert!(s.says(IDLE));
    s.press_button("Hide");
    assert!(!s.on("showing") && s.element("Hide").is_none());
    assert!(s.says("Preview hidden"));
    s.press_button("Show");
    assert!(s.on("showing") && s.element("Hide").is_some());
    assert!(s.says(IDLE));
}

#[test]
fn the_idle_text_names_the_browser_while_a_browser_source_is_picked() {
    let s = stage("stage-idle");
    let browser = "Not streaming";
    for (source, text) in [("browser", browser), ("browser:b1", browser), ("webcam:usb-c920", "Not streaming"), ("", IDLE)] {
        s.r.w.emit(Event::SourceSelected { source_id: source.into() });
        assert!(s.says(text), "{source}");
    }
}

#[test]
fn preview_placeholder_says_waiting_while_a_stream_is_up() {
    let s = on_air("stage-waiting");
    s.press_button("Hide");
    s.press_button("Show");
    assert!(s.says(WAITING));
}

#[test]
fn stream_start_and_stop_swap_the_placeholder() {
    let s = stage("stage-swap");
    s.r.start();
    s.r.pump();
    assert!(s.says(WAITING));
    s.r.worker(0).says("ok", "Streaming");
    s.r.pump();
    s.show(0, grey(32, 18));
    assert!(s.has_frame() && !s.says(WAITING), "the picture takes its place");
    s.r.stop(None);
    s.r.pump();
    assert!(s.says(IDLE));
    assert!(!s.has_frame(), "the last picture goes with it");
}

#[test]
fn the_stage_says_the_camera_is_off_and_why() {
    let s = with_mic("stage-off-why", true);
    s.r.start();
    s.r.worker(0).says("ok", "Streaming");
    s.r.pump();
    s.show(0, grey(32, 18));
    s.r.w.set_camera_on(false, false);
    s.r.pump();
    assert!(s.says(CAMERA_OFF) && s.on("camera-off") && !s.has_frame());
    s.r.w.set_camera_on(true, false);
    s.r.pump();
    s.r.until("the new worker", |r| r.workers_made() == 2);
    s.r.worker(1).says("ok", "Streaming");
    s.r.pump();
    assert!(s.says(WAITING) && !s.on("camera-off"));
    s.r.w.set_camera_on(false, true);
    s.r.pump();
    assert!(s.says(CAMERA_OFF_AUTO) && s.on("camera-off"));
}

/// The tip the camera button shows under the mouse.
fn camera_tip(s: &Stage, text: &str) -> bool {
    let e = s.element("Device camera").expect("the camera button");
    let (p, size) = (e.absolute_position(), e.size());
    // A control just pressed shows no tip until the pointer has left it and come back
    s.send(WindowEvent::PointerMoved { position: LogicalPosition::new(600.0, 200.0) });
    s.r.skip(20);
    s.send(WindowEvent::PointerMoved { position: LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0) });
    // Just after a change tips wait longer (Tips.quiet-factor)
    s.r.skip(1400);
    let shown = s.says(text);
    s.send(WindowEvent::PointerMoved { position: LogicalPosition::new(600.0, 200.0) });
    s.r.skip(20);
    shown
}

#[test]
fn the_camera_button_turns_the_camera_off_and_says_so() {
    let s = with_mic("stage-camera-button", true);
    s.r.w.emit(Event::MicChanged { enabled: true, muted: false });
    assert!(s.on("camera-on") && s.on("camera-enabled"));
    s.press_button("Device camera");
    s.r.pump();
    assert!(!s.on("camera-on"), "the click reached the camera");
    assert!(camera_tip(&s, "Turn the phone's camera back on."));
    assert!(s.says(IDLE_MIC), "not streaming yet: the stage says what Start will do");
    s.press_button("Device camera");
    s.r.pump();
    assert!(s.on("camera-on") && s.says(IDLE));
}

#[test]
fn the_camera_can_only_go_off_with_the_mic_on() {
    let s = with_mic("stage-camera-needs-mic", false);
    s.r.w.emit(Event::MicChanged { enabled: false, muted: false });
    s.r.pump();
    assert!(!s.on("camera-enabled"));
    assert!(s.text("camera-tip").contains("mic"), "{}", s.text("camera-tip"));
    assert!(camera_tip(&s, &s.text("camera-tip")));
    s.press_button("Device camera");
    s.r.pump();
    assert!(s.on("camera-on"), "a disabled button does nothing");
}

#[test]
fn open_and_close_popout_disables_then_restores_toggle() {
    let s = stage("stage-popout-open");
    let p = s.pop_out();
    assert!(!s.on("showing") && s.on("popout"));
    s.press_button("Show");
    assert!(!s.on("showing"), "the button is disabled while the window is open");
    s.close_pop_out(&p);
    assert!(s.r.w.popout_window().is_none());
    assert!(s.on("showing") && !s.on("popout"), "the card it hid comes back");
    s.press_button("Hide");
    assert!(!s.on("showing"), "and the button works again");
}

#[test]
fn a_popout_opened_with_the_card_hidden_leaves_it_hidden() {
    let s = stage("stage-popout-hidden");
    s.press_button("Hide");
    let p = s.pop_out();
    s.close_pop_out(&p);
    assert!(s.r.w.popout_window().is_none());
    assert!(!s.on("showing"));
}

// What's checked here stops at the window: focus_window itself needs a real desktop
#[test]
fn second_popout_request_reuses_visible_window() {
    let s = stage("stage-popout-again");
    let p = s.pop_out();
    p.window().set_size(slint::PhysicalSize::new(700, 400));
    p.set_property("layer-font", SValue::String("marked".into())).unwrap();
    p.window().set_minimized(true);
    s.press_button("Pop out");
    let again = s.r.w.popout_window().expect("the pop-out");
    assert_eq!(again.get_property("layer-font").unwrap(), SValue::String("marked".into()), "the same window, not a new one");
    assert_eq!(again.window().size(), slint::PhysicalSize::new(700, 400));
    assert!(!p.window().is_minimized(), "it comes forward");
    p.window().set_minimized(true);
    s.r.w.paint_preview();
    assert!(p.window().is_minimized(), "only when asked");
}

#[test]
fn the_card_paints_every_second_frame_but_all_of_them_reach_the_slot() {
    let s = on_air_capped("stage-half");
    s.show(0, blue(32, 18));
    assert_eq!(s.image_size(), (32, 18), "the first one shows at once");
    s.show(0, grey(64, 36));
    assert_eq!(s.image_size(), (32, 18), "the second is skipped");
    assert_eq!(s.r.worker(0).slot().latest().map(|(f, _)| f.width), Some(64), "only by the card");
    s.r.w.paint_preview();
    assert_eq!(s.image_size(), (32, 18), "and stays skipped however often the timer looks");
    s.show(0, blue(48, 27));
    assert_eq!(s.image_size(), (48, 27), "the third shows");

    assert!(s.r.w.call_ui("preview", "card", "smooth-clicked"));
    s.r.pump();
    assert!(s.on("smooth-on"));
    s.show(0, grey(64, 36));
    assert_eq!(s.image_size(), (64, 36), "Smooth shows every frame");
    s.show(0, blue(32, 18));
    assert_eq!(s.image_size(), (32, 18));

    assert!(s.r.w.call_ui("preview", "card", "smooth-clicked"));
    s.r.pump();
    s.show(0, grey(64, 36));
    assert_eq!(s.image_size(), (32, 18), "and off again halves them again");
}

#[test]
fn the_pop_out_shows_every_frame_with_smooth_off() {
    let s = on_air_capped("stage-popout-every");
    let p = s.pop_out();
    p.window().set_size(slint::PhysicalSize::new(160, 90));
    let blue_shown = || {
        let SValue::Image(image) = p.get_property("frame").unwrap() else { panic!("no picture") };
        let rgb = image.to_rgb8().expect("the picture's pixels");
        rgb.as_slice()[0].b > 200
    };
    for i in 0..6 {
        let blue_one = i % 2 == 0;
        s.show(0, if blue_one { blue(160, 90) } else { grey(160, 90) });
        assert_eq!(blue_shown(), blue_one, "frame {i}");
    }
}

#[test]
fn popout_set_frame_and_resize_guards() {
    let s = on_air("stage-popout-frame");
    let p = s.pop_out();
    p.window().set_size(slint::PhysicalSize::new(640, 400));
    s.show(0, grey(160, 90));
    let SValue::Image(image) = p.get_property("frame").unwrap() else { panic!("no picture") };
    assert_eq!(p.get_property("has-frame").unwrap(), SValue::Bool(true));
    assert_eq!((image.size().width, image.size().height), (160, 90));
    assert_eq!((p.window().size().width, p.window().size().height), (640, 360), "it takes the picture's shape");
    p.window().set_size(slint::PhysicalSize::new(640, 362));
    s.show(0, grey(160, 90));
    assert_eq!(p.window().size().height, 362, "within 4 px it stays");
    s.r.stop(None);
    s.r.pump();
    p.window().set_size(slint::PhysicalSize::new(500, 300));
    s.r.w.paint_preview();
    assert_eq!((p.window().size().width, p.window().size().height), (500, 300), "no picture, no shape to take");
}

#[test]
fn a_pick_shows_a_hidden_preview() {
    let s = letterboxed("stage-pick-reveals");
    s.press_button("Hide");
    assert!(!s.on("showing"));
    s.call("ears", |host| assert!(host.pick_on_preview("ears", PickKind::Point, None, "")));
    assert!(s.on("showing"), "started through the overlays, heard by the card");
}

#[test]
fn turning_plugin_colours_off_repaints_every_card_in_the_app_purple() {
    let s = stage("stage-hues");
    let own = s.prop("hue");
    assert_ne!(own, s.accent());
    s.call("ears", |host| host.set_plugin_hues(false));
    assert_eq!(s.prop("hue"), s.accent());
    s.call("ears", |host| host.set_plugin_hues(true));
    assert_eq!(s.prop("hue"), own);
}

#[test]
fn lens_boxes_show_for_a_moment_after_a_move_then_fade() {
    let s = on_air("stage-lens-button");
    s.show(0, Frame::new(640, 360, PixelFormat::Rgba, [0, 0, 0, 255].repeat(640 * 360)));
    let left = s.at(0.25, 0.5);
    let button = |s: &Stage| s.element("Lens outlines").expect("the button").size().width;
    assert_eq!(button(&s), 32.0, "icon only at a fixed width: it can't widen the stage");
    s.press_button("Lens outlines");
    assert!(!s.on("lenses-on"), "nothing to outline yet");
    s.r.w.emit(Event::LensBoxes { boxes: vec![lens_box()], moved: false });
    assert!(s.on("lenses-enabled") && !lit_near(&s, left), "known, but nothing moved");
    let before = s.r.w.card_rect("preview").unwrap();
    s.press_button("Lens outlines");
    assert!(lit_near(&s, left), "always on");
    assert_eq!((button(&s), s.r.w.card_rect("preview").unwrap()), (32.0, before));
    s.press_button("Lens outlines");
    assert!(!lit_near(&s, left));
}

/// Shoulders at 0.45 and 0.55 of the width, 0.4 down, and the left arm straight out.
fn figure() -> serde_json::Value {
    Person::new([("left_shoulder", 0.55, 0.4, 0.99), ("right_shoulder", 0.45, 0.4, 0.99), ("left_hip", 0.53, 0.75, 0.9), ("right_hip", 0.47, 0.75, 0.9),
                 ("nose", 0.5, 0.28, 0.99), ("left_eye", 0.51, 0.26, 0.99), ("left_ear", 0.53, 0.27, 0.9), ("left_elbow", 0.62, 0.42, 0.9), ("left_wrist", 0.69, 0.42, 0.9)]).to_value()
}

/// A vision service in a state of the test's choosing, noting who asks for people.
#[derive(Default)]
struct Vision {
    state: Mutex<String>,
    wanted: Mutex<Vec<(String, bool)>>,
}

impl telescope_core::vision::VisionService for Vision {
    fn attach(&self, _bus: telescope_core::BusSender) {}
    fn state(&self) -> String {
        self.state.lock().unwrap().clone()
    }
    fn load(&self) {}
    fn person_mask(&self, _source_id: &str, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn want_people(&self, owner: &str, on: bool) {
        self.wanted.lock().unwrap().push((owner.into(), on));
    }
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
        unimplemented!()
    }
    fn download(&self, _switch_on: &str) {}
    fn cancel_download(&self) {}
    fn remove_pack(&self) -> Result<(), String> {
        Ok(())
    }
}

fn say_vision(s: &Stage, vision: &Vision, state: &str) {
    *vision.state.lock().unwrap() = state.into();
    s.r.w.emit(Event::VisionStateChanged { state: state.into() });
}

fn with_vision(s: &Stage) -> Arc<Vision> {
    let vision = Arc::new(Vision::default());
    s.r.w.state.borrow_mut().vision = Some(vision.clone());
    vision
}

#[test]
fn the_skeleton_button_only_shows_with_the_vision_download() {
    let s = on_air("stage-skeleton-button");
    let vision = with_vision(&s);
    for (state, shown) in [("missing", false), ("off", true), ("failed", false), ("ready", true)] {
        say_vision(&s, &vision, state);
        assert_eq!(s.element("Skeleton").is_some(), shown, "{state}");
    }
    s.press_button("Skeleton");
    assert_eq!(vision.wanted.lock().unwrap().last(), Some(&("Preview skeleton".to_string(), true)));
    say_vision(&s, &vision, "missing");
    assert_eq!(vision.wanted.lock().unwrap().last(), Some(&("Preview skeleton".to_string(), false)), "the download went away with the button on");
}

#[test]
fn the_skeleton_draws_the_shown_streams_people_over_the_preview() {
    let s = on_air("stage-skeleton-people");
    s.show(0, Frame::new(640, 360, PixelFormat::Rgba, [0, 0, 0, 255].repeat(640 * 360)));
    let vision = with_vision(&s);
    say_vision(&s, &vision, "ready");
    let seen = |source: &str| s.r.w.emit(Event::PeopleSeen { source_id: source.into(), people: vec![figure()] });
    seen("pixel");
    assert!(s.lines().is_empty(), "not asked for");
    s.press_button("Skeleton");
    assert_eq!(vision.wanted.lock().unwrap().last(), Some(&("Preview skeleton".to_string(), true)));
    seen("another phone");
    assert!(s.lines().is_empty());
    seen("pixel");
    assert!(lit_near(&s, s.at(0.5, 0.4)), "the shoulders' line");
    s.press_button("Skeleton");
    assert_eq!(vision.wanted.lock().unwrap().last(), Some(&("Preview skeleton".to_string(), false)));
    assert!(s.lines().is_empty());
    assert!(!lit_near(&s, s.at(0.5, 0.4)));
}

#[test]
fn the_skeleton_follows_the_zoom_and_marks_an_arm_held_out() {
    let s = on_air("stage-skeleton-zoom");
    s.show(0, Frame::new(640, 360, PixelFormat::Rgba, [0, 0, 0, 255].repeat(640 * 360)));
    s.r.w.emit(Event::VisionStateChanged { state: "ready".into() });
    s.press_button("Skeleton");
    s.call("ears", |host| host.set_view(ViewChange { zoom: Some(2.0), ..Default::default() }));
    s.r.w.emit(Event::PeopleSeen { source_id: "pixel".into(), people: vec![figure()] });
    let lines = s.lines();
    let close = |a: &serde_json::Value, b: f64| (a.as_f64().unwrap() - b).abs() < 1e-3;
    let shoulders = lines.iter().find(|l| close(&l["y0"], 0.3) && close(&l["y1"], 0.3)).expect("the shoulders, moved by the zoom");
    let mut xs = [shoulders["x0"].as_f64().unwrap(), shoulders["x1"].as_f64().unwrap()];
    xs.sort_by(f64::total_cmp);
    assert!((xs[0] - 0.4).abs() < 1e-3 && (xs[1] - 0.6).abs() < 1e-3 && shoulders["out"] == false, "{xs:?}");
    assert_eq!(lines.iter().filter(|l| l["out"] == true).count(), 2, "shoulder to elbow and elbow to wrist, not the side of the body or the face");
    assert!(lit_near(&s, s.at(0.5, 0.3)), "drawn where the zoom put it");
}
