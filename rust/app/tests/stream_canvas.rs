// v4l2loopback's canvas: Windows cameras take each stream's size, so nothing reloads there
#![cfg(target_os = "linux")]

// Changing the canvas: the stream stops, the virtual camera reloads, and the stream starts again, unless something changed.
mod rig;

use rig::Rig;
use std::sync::atomic::Ordering;
use telescope_core::host::RequestId;

fn on_air(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.forget_heard();
    r
}

fn resize(r: &Rig) {
    r.w.restart_canvas("panel", RequestId(1), true);
}

fn done(r: &Rig) -> Option<String> {
    r.heard().into_iter().find(|l| l.starts_with("panel done "))
}

fn wait_done(r: &Rig) -> String {
    r.until("the answer", |r| done(r).is_some());
    done(r).unwrap()
}

/// A resize that hasn't finished, of a running stream.
fn resizing(name: &str) -> Rig {
    let r = on_air(name);
    r.cameras.held.store(true, Ordering::SeqCst);
    resize(&r);
    r.pump();
    assert!(r.streaming().is_empty());
    r
}

fn finish(r: &Rig) {
    r.cameras.held.store(false, Ordering::SeqCst);
    wait_done(r);
    r.pump();
}

#[test]
fn a_canvas_resize_starts_the_stream_it_stopped_again() {
    let r = resizing("canvas-restarts");
    assert!(r.worker(0).stops());
    finish(&r);
    assert_eq!(done(&r).unwrap(), "panel done true  ");
    r.until("the stream's back", |r| r.workers_made() == 2);
    assert!(r.worker(1).after_the_others, "the old worker lets go of the camera first");
    assert_eq!(r.link.logged("stop pixel"), 0, "the phone stays up through it");
}

#[test]
fn stop_during_a_canvas_resize_is_not_undone_when_it_finishes() {
    let r = resizing("canvas-stop");
    r.start();
    r.w.toggle_stream();
    r.pump();
    finish(&r);
    assert_eq!(r.workers_made(), 2, "started and stopped by hand: the resize leaves it stopped");
    assert!(r.streaming().is_empty());
}

#[test]
fn a_canvas_reload_does_not_start_over_a_stream_started_meanwhile() {
    let r = resizing("canvas-meanwhile");
    r.start();
    finish(&r);
    assert_eq!(r.workers_made(), 2);
    assert_eq!(r.streaming(), ["pixel"]);
}

#[test]
fn a_canvas_change_while_idle_does_not_announce_a_stop() {
    let r = Rig::new("canvas-idle");
    resize(&r);
    wait_done(&r);
    assert!(!r.did_hear("panel stream_stopped"));
    assert_eq!(r.workers_made(), 0);
}

#[test]
fn a_canvas_change_while_the_phone_wakes_starts_it_again_without_stopping_it() {
    let r = Rig::new("canvas-waking");
    r.link.hold_wakes(true);
    r.w.toggle_stream();
    r.until("the wake", |r| r.link.logged("wake pixel") == 1);
    r.cameras.held.store(true, Ordering::SeqCst);
    resize(&r);
    r.pump();
    r.link.hold_wakes(false); // the first wake lands during the reload
    r.until("it landed and was let go", |r| r.link.released.lock().unwrap().iter().any(|f| f.load(Ordering::SeqCst)));
    finish(&r);
    r.until("the stream", |r| r.workers_made() == 1);
    r.pump();
    assert_eq!(r.link.logged("wake pixel"), 2, "started again");
    assert_eq!(r.link.logged("stop pixel"), 0);
}

#[test]
fn a_loopback_reload_that_raises_still_reports_back() {
    let r = on_air("canvas-panics");
    r.cameras.panics.store(true, Ordering::SeqCst);
    resize(&r);
    assert!(wait_done(&r).starts_with("panel done false"));
    assert!(r.text("status").starts_with("Reload failed"));
}

#[test]
fn canvas_reload_failure_reports_error_and_clears_callback() {
    let r = on_air("canvas-fails");
    *r.cameras.fails.lock().unwrap() = Some(("busy".into(), String::new()));
    resize(&r);
    assert_eq!(wait_done(&r), "panel done false busy ");
    r.pump();
    assert_eq!(r.text("status"), "Reload failed: busy");
    assert_eq!(r.text("status-kind"), "status_err");
    assert_eq!(r.link.logged("wake pixel"), 1, "not started again");
    assert_eq!(r.workers_made(), 1);
    assert_eq!(r.heard().iter().filter(|l| l.starts_with("panel done ")).count(), 1, "one answer");
}

#[test]
fn a_reload_command_goes_to_whoever_asked_and_only_otherwise_to_a_banner() {
    let r = on_air("canvas-command");
    *r.cameras.fails.lock().unwrap() = Some(("Run this".into(), "sudo modprobe v4l2loopback".into()));
    resize(&r);
    assert_eq!(wait_done(&r), "panel done false Run this sudo modprobe v4l2loopback");
    r.pump();
    assert!(r.banner("vcam").is_none());

    r.forget_heard();
    r.w.restart_canvas("panel", RequestId(2), false);
    r.until("the banner", |r| r.banner("vcam").is_some());
    let banner = r.banner("vcam").unwrap();
    assert_eq!(banner["text"], "Run this");
    assert_eq!(banner["details"], "sudo modprobe v4l2loopback");
    assert_eq!(banner["actions"], "Copy command");
}
