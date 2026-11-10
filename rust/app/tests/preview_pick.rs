// test_overlays.py::test_a_pick_carries_the_picture_shown_before_any_tint, through the window's real paint: a stream's
// frame, a pass that tints it and a pick that ends on it.
mod rig;

use rig::Rig;
use slint::ComponentHandle;
use slint_interpreter::Value as SValue;
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::{Host, HostPreview, PickKind};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::preview::PreviewPass;
use telescope_shell::plugins::preview::Preview;
use telescope_shell::worker::VideoWorker;

struct Red;

impl PreviewPass for Red {
    fn tints(&self) -> bool {
        true
    }
    fn tint(&self, frame: &mut Frame) {
        frame.data_mut().iter_mut().for_each(|b| *b = 255);
    }
}

type Picture = Arc<Mutex<Option<Option<Frame>>>>;

struct Picker(Picture);

impl Plugin for Picker {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "picker".into(), title: "Picker".into(), ..Default::default() }
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        host.add_preview_pass("picker", Arc::new(Red));
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::PreviewPicked { owner, pick } = event {
            if owner == "picker" {
                *self.0.lock().unwrap() = Some(pick.as_ref().and_then(|p| p.picture.clone()));
            }
        }
    }
}

#[test]
fn a_pick_carries_the_picture_shown_before_any_tint() {
    let picture = Picture::default();
    let heard = picture.clone();
    let r = Rig::with("preview-pick", move |_, _| vec![Box::new(Picker(heard)) as Box<dyn Plugin>, Box::new(Preview::default())]);
    r.w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    r.w.ui.show().unwrap();
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let picker = r.w.state.borrow().index("picker").expect("the plugin");
    let picking = r.w.state.borrow_mut().call(picker, |_, host| host.pick_on_preview("picker", PickKind::Quad, None, ""));
    assert_eq!(picking, Some(true));
    let frame = Frame::new(16, 16, PixelFormat::Rgba, [7, 7, 7, 255].repeat(16 * 16));
    r.worker(0).slot().publish(frame);
    r.w.paint_preview();
    let id = r.w.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-has-frame")).expect("the preview card");
    // A tint is made on a worker, and its first picture asks for a repaint
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while r.w.ui.get_property(&id).unwrap() != SValue::Bool(true) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
        r.w.paint_preview();
    }
    assert_eq!(r.w.ui.get_property(&id).unwrap(), SValue::Bool(true), "the preview painted the frame");
    let done = r.w.state.borrow_mut().call(picker, |_, host| host.finish_pick("picker"));
    assert_eq!(done, Some(true));
    r.w.process();
    let picked = picture.lock().unwrap().take().expect("the pick ended").expect("with the picture");
    assert!(picked.width == 16 && picked.data.chunks(4).all(|p| p == [7, 7, 7, 255]), "the picture before the pass tinted it");
}
