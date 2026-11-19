// Host::preview_follows through the real window: while a plugin asks for it, the preview draws another stream's picture and the panels stay put.
mod rig;

use rig::Rig;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::HostPreview;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_shell::worker::VideoWorker;

struct Redirector;

impl Plugin for Redirector {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "redirector".into(), title: "Redirector".into(), ..Default::default() }
    }
}

fn grey(w: u32, h: u32) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [128, 128, 128, 255].repeat((w * h) as usize))
}

/// pixel (worker 0) sends 64 x 36 and oneplus (worker 1) 32 x 18.
fn two_streams(name: &str) -> Rig {
    let r = Rig::with(name, |_, _| vec![Box::new(Redirector) as Box<dyn Plugin>]);
    r.start();
    r.add("oneplus");
    r.worker(0).says("ok", "Streaming");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    r.worker(0).slot().publish(grey(64, 36));
    r.worker(1).slot().publish(grey(32, 18));
    r
}

fn follow(r: &Rig, source: Option<&str>, target: Option<&str>) {
    let i = r.w.state.borrow().index("redirector").expect("the plugin");
    r.w.state.borrow_mut().call(i, |_, host| host.preview_follows("redirector", source, target));
}

fn focus(r: &Rig, id: &str) {
    r.w.with_flow(|flow, host| flow.focus_stream(host, id));
    r.pump();
}

fn drawn(r: &Rig) -> (u32, u32) {
    let (picture, _) = r.w.preview_picture().expect("a picture");
    (picture.metadata().width, picture.metadata().height)
}

#[test]
fn the_preview_draws_the_followed_stream_while_the_panels_show_the_other() {
    let r = two_streams("follows-draws");
    focus(&r, "oneplus");
    assert_eq!(drawn(&r), (32, 18), "its own picture to begin with");
    follow(&r, Some("oneplus"), Some("pixel"));
    assert_eq!(drawn(&r), (64, 36), "pixel's output");
    assert_eq!(r.shown().as_deref(), Some("oneplus"), "the panels stay on the phone being adjusted");
    assert_eq!(r.w.preview_source().as_deref(), Some("pixel"), "so what is made from it isn't taken for oneplus's");
}

#[test]
fn only_the_stream_it_was_asked_for_follows() {
    let r = two_streams("follows-only-that");
    follow(&r, Some("oneplus"), Some("pixel"));
    focus(&r, "pixel");
    assert_eq!(drawn(&r), (64, 36));
    assert_eq!(r.w.preview_source().as_deref(), Some("pixel"));
    focus(&r, "oneplus");
    assert_eq!(drawn(&r), (64, 36), "moving back follows again");
}

#[test]
fn clearing_it_gives_the_streams_own_picture_back() {
    let r = two_streams("follows-clear");
    focus(&r, "oneplus");
    follow(&r, Some("oneplus"), Some("pixel"));
    follow(&r, None, None);
    assert_eq!(drawn(&r), (32, 18));
    assert_eq!(r.w.preview_source().as_deref(), Some("oneplus"));
}

#[test]
fn a_stream_that_isnt_running_is_not_followed() {
    let r = two_streams("follows-gone");
    focus(&r, "oneplus");
    follow(&r, Some("oneplus"), Some("nobody"));
    assert_eq!(drawn(&r), (32, 18));
}

#[test]
fn the_thumbnails_keep_their_own_streams() {
    let r = two_streams("follows-tiles");
    focus(&r, "oneplus");
    let before = (r.tile("pixel"), r.tile("oneplus"));
    follow(&r, Some("oneplus"), Some("pixel"));
    r.pump();
    assert_eq!((r.tile("pixel"), r.tile("oneplus")), before);
}
