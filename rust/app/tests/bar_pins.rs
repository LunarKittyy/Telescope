// The bottom bar's pins in the real window: the defaults, which stream they follow, what persists, what the camera pin does with two
// streams, and a bar whose Stats plugin is switched off.
mod rig;

use rig::{Listener, Rig};
use serde_json::json;
use slint::ComponentHandle;
use std::collections::BTreeMap;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::stats::Hub;
use telescope_core::ui::UiEvent;
use telescope_setup::stats::StatsPlugin;
use telescope_shell::offscreen;

type Row = BTreeMap<String, String>;

fn with_stats(name: &str) -> Rig {
    // The Stats plugin is one of the optional ones, which start off until someone switches them on
    let r = Rig::seeded(name, json!({"plugins": {"on": ["stats"]}}), |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(StatsPlugin::new())]);
    shown(&r);
    r
}

fn without_stats(name: &str) -> Rig {
    let r = Rig::new(name);
    shown(&r);
    r
}

fn shown(r: &Rig) {
    r.w.ui.window().set_size(slint::LogicalSize::new(900.0, 700.0));
    r.w.ui.show().unwrap();
    r.w.process();
    settle(r);
}

fn settle(r: &Rig) {
    for _ in 0..12 {
        r.skip(50);
        offscreen::redraw(r.w.ui.window());
        r.w.process();
    }
}

/// A stream's footer reports: its camera sentence, frame rate and throughput.
fn reports(r: &Rig, worker: usize, camera: &str, fps: u32, mbps: &str) {
    let w = r.worker(worker);
    w.says("ok", &format!("Streaming 1920 \u{d7} 1080 at 30 fps to {camera}"));
    w.says("fps", &format!("{fps} fps  1920x1080"));
    w.says("net", mbps);
    r.pump();
    settle(r);
}

fn pins(r: &Rig) -> Vec<Row> {
    r.rows("bar-pins")
}

fn pin(r: &Rig, id: &str) -> Row {
    pins(r).into_iter().find(|p| p["id"] == id).unwrap_or_else(|| panic!("no pin {id} in {:?}", pins(r)))
}

fn shown_ids(r: &Rig) -> Vec<String> {
    pins(r).into_iter().filter(|p| p["state"] == "0").map(|p| p["id"].clone()).collect()
}

fn tap_star(r: &Rig, id: &str, on: bool) {
    let event = UiEvent { ui: "card".into(), callback: "pin-toggled".into(), args: vec![json!(id), json!(on)] };
    r.w.state.borrow_mut().ui_event("stats", event);
    r.w.process();
    settle(r);
}

fn saved(r: &Rig) -> serde_json::Value {
    r.w.state.borrow_mut().save_now();
    r.w.state.borrow().file.load()["bar_pins"].clone()
}

fn focus(r: &Rig, id: &str) {
    r.w.with_flow(|flow, host| flow.focus_stream(host, id));
    r.w.process();
    settle(r);
}

#[test]
fn the_camera_the_frame_rate_and_the_throughput_are_pinned_to_begin_with() {
    let r = with_stats("bar-defaults");
    assert!(r.flag("bar-status-shown"), "idle: the dim word");
    assert_eq!(r.text("status"), "Not streaming");
    assert!(pins(&r).iter().all(|p| p["state"] == "2"), "nothing to read while idle");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    assert!(!r.flag("bar-status-shown"), "streaming: the pins say it, not a sentence");
    assert_eq!(shown_ids(&r), ["camera", "fps", "throughput"]);
    let (camera, fps, net) = (pin(&r, "camera"), pin(&r, "fps"), pin(&r, "throughput"));
    assert_eq!((camera["value"].as_str(), camera["sub"].as_str(), camera["side"].as_str()), ("Phone Camera", "1920 \u{d7} 1080", "0"));
    assert_eq!((fps["value"].as_str(), fps["side"].as_str(), net["value"].as_str(), net["side"].as_str()), ("30", "1", "12.4 Mbps", "1"));
    let x = |p: &Row| p["x"].parse::<f32>().unwrap();
    assert!(x(&camera) < x(&fps) && x(&fps) < x(&net), "left side from the left edge, right side from the right");
    assert!(x(&net) + net["w"].parse::<f32>().unwrap() <= 900.0 - 15.9, "the right edge is the bar's padding");
    // Stopping goes back to the dim word
    r.stop(None);
    r.pump();
    settle(&r);
    assert!(r.flag("bar-status-shown") && r.text("status") == "Not streaming");
    assert!(pins(&r).iter().all(|p| p["state"] == "2"));
}

#[test]
fn per_stream_pins_follow_the_focused_stream_and_never_the_one_that_reported_last() {
    let r = with_stats("bar-two");
    r.start();
    r.add("oneplus");
    reports(&r, 0, "Camera A", 30, "10 Mbps");
    reports(&r, 1, "Camera B", 24, "5 Mbps");
    // The panels show the second stream it was added last? Whatever they show is what the bar shows
    let shown = r.shown().expect("a focused stream");
    let (name, fps, net) = if shown == "pixel" { ("Camera A", "30", "10 Mbps") } else { ("Camera B", "24", "5 Mbps") };
    assert_eq!((pin(&r, "camera")["value"].as_str(), pin(&r, "fps")["value"].as_str(), pin(&r, "throughput")["value"].as_str()), (name, fps, net));
    // The other stream reports again: the bar does not move
    let other = if shown == "pixel" { 1 } else { 0 };
    r.worker(other).says("fps", "99 fps  1920x1080");
    r.worker(other).says("net", "99 Mbps");
    r.pump();
    settle(&r);
    assert_eq!((pin(&r, "fps")["value"].as_str(), pin(&r, "throughput")["value"].as_str()), (fps, net));
    // Focus moves: the bar follows with the other stream's numbers
    let to = if shown == "pixel" { "oneplus" } else { "pixel" };
    focus(&r, to);
    // Each stream's own last numbers: the other one's report of 99 was kept for it, as the number alone
    let (name, fps, net) = if to == "pixel" { ("Camera A", "99", "99 Mbps") } else { ("Camera B", "24", "5 Mbps") };
    assert_eq!((pin(&r, "camera")["value"].as_str(), pin(&r, "fps")["value"].as_str(), pin(&r, "throughput")["value"].as_str()), (name, fps, net));
    focus(&r, &shown);
    assert_eq!(pin(&r, "camera")["value"], if shown == "pixel" { "Camera A" } else { "Camera B" });
}

#[test]
fn the_camera_pin_cannot_be_taken_off_while_two_streams_run() {
    let r = with_stats("bar-camera-lock");
    r.start();
    reports(&r, 0, "Camera A", 30, "10 Mbps");
    r.w.unpin_from_bar("camera");
    assert!(!shown_ids(&r).contains(&"camera".to_string()), "one stream: it can go");
    assert_eq!(saved(&r)["left"], json!([]));
    r.add("oneplus");
    reports(&r, 1, "Camera B", 24, "5 Mbps");
    assert!(shown_ids(&r).contains(&"camera".to_string()), "two streams bring it back, whichever one is shown");
    assert_eq!(pin(&r, "camera")["locked"], "true");
    r.w.unpin_from_bar("camera");
    settle(&r);
    assert!(shown_ids(&r).contains(&"camera".to_string()), "and it stays");
    r.stop(Some("oneplus"));
    r.pump();
    settle(&r);
    assert!(!shown_ids(&r).contains(&"camera".to_string()), "one stream again: it is as unpinned as it was left");
}

#[test]
fn a_pin_its_side_and_its_order_are_kept_and_the_defaults_are_not_written() {
    let r = with_stats("bar-persist");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    r.emit(Event::StatsUpdated { snapshot: Hub::new().snapshot_at(telescope_core::time::monotonic_ns(), None, Some(200 << 20)) });
    assert!(saved(&r).is_null(), "the defaults stay out of the file");
    // Pin from the card, which puts it at the inner end of the right side
    {
        let hub = Hub::new();
        let s = hub.register("pixel", 0);
        s.received(30, 1000);
        r.emit(Event::StatsUpdated { snapshot: hub.snapshot_at(telescope_core::time::monotonic_ns() + 10_000_000_000, Some(1_000_000_000), Some(200 << 20)) });
    }
    tap_star(&r, "stats.frames_in", true);
        assert_eq!(saved(&r), json!({"left": ["camera"], "right": ["stats.frames_in", "fps", "throughput"]}));
    assert!(shown_ids(&r).contains(&"stats.frames_in".to_string()));
    // Drag the frame rate to the left side, in front of the camera
    {
        let mut st = r.w.state.borrow_mut();
        st.bar.drag_start("fps");
        st.bar.drag_to("fps", 5.0);
        assert!(st.bar.drag_end("fps"));
    }
    r.w.process();
    assert_eq!(saved(&r), json!({"left": ["fps", "camera"], "right": ["stats.frames_in", "throughput"]}));
    // A restart: the bar starts from the file
    r.w.state.borrow_mut().bar.load(None);
    assert_eq!(r.w.state.borrow().bar.persisted().0, ["camera"]);
    r.w.state.borrow_mut().apply_saved_config();
    r.w.process();
    assert_eq!(r.w.state.borrow().bar.persisted(), (vec!["fps".to_string(), "camera".to_string()], vec!["stats.frames_in".to_string(), "throughput".to_string()]));
    // Unpin from the bar's own menu
    r.w.unpin_from_bar("stats.frames_in");
    assert_eq!(saved(&r), json!({"left": ["fps", "camera"], "right": ["throughput"]}));
    // Back to the defaults: the file drops the entry again
    {
        let mut st = r.w.state.borrow_mut();
        st.bar.pin("stats.frames_in", false);
        st.bar.load(Some((vec!["camera".into()], vec!["fps".into(), "throughput".into()])));
    }
    assert!(saved(&r).is_null());
}

#[test]
fn a_damaged_pins_entry_starts_from_the_defaults() {
    let r = without_stats("bar-damaged");
    for bad in [json!("left"), json!({"left": "camera"}), json!({"left": [1, null], "right": {"a": 1}}), json!(7)] {
        let mut cfg = r.w.state.borrow().file.load();
        cfg["bar_pins"] = bad.clone();
        r.w.state.borrow().file.save(&mut cfg);
        r.w.state.borrow_mut().apply_saved_config();
        let pins = r.w.state.borrow().bar.persisted();
        if bad.is_object() {
            assert!(pins.0.iter().chain(&pins.1).all(|p| !p.is_empty()), "{bad}: {pins:?}");
        } else {
            assert_eq!(pins.0, ["camera"], "{bad}");
        }
    }
}

#[test]
fn with_the_stats_plugin_switched_off_the_bar_works_and_its_pins_vanish_quietly() {
    // Pinned from the Stats card in an earlier run, and the plugin is not there now
    let r = without_stats("bar-no-stats");
    {
        let mut st = r.w.state.borrow_mut();
        st.bar.load(Some((vec!["camera".into()], vec!["stats.cpu".into(), "fps".into(), "throughput".into()])));
    }
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    assert_eq!(shown_ids(&r), ["camera", "fps", "throughput"], "the window's own readouts are the window's");
    assert_eq!(pin(&r, "stats.cpu")["state"], "2", "the missing plugin's pin takes no room and says nothing");
    // And it stays in the file for when the plugin is back
    assert_eq!(saved(&r)["right"], json!(["stats.cpu", "fps", "throughput"]));
    assert!(r.flag("bar-status-shown") == false);
}

#[test]
fn the_bar_adds_no_redraws_to_a_window_that_is_sitting_there() {
    let r = with_stats("bar-idle");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let mut count = 0;
    for step in 0..100 {
        r.skip(100);
        if step % 10 == 0 {
            r.w.process();
        }
        if offscreen::redraw(r.w.ui.window()) {
            count += 1;
        }
    }
    assert_eq!(count, 0, "a bar with pins at rest asks for no frames");
}

// ---- Several pins, narrow bars and the pointer ----

const SIDE_PINS: &str = r#"{"bar_pins": {"left": ["camera", "stats.frames_in", "stats.repeated"], "right": ["stats.dropped", "stats.memory", "fps", "throughput"]}, "plugins": {"on": ["stats"]}}"#;

/// A window with many pins pinned, a stream running and the Stats card's readouts offered.
fn full_bar(name: &str) -> Rig {
    let r = Rig::seeded(name, serde_json::from_str(SIDE_PINS).unwrap(), |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(StatsPlugin::new())]);
    shown(&r);
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let hub = Hub::new();
    let s = hub.register("pixel", 0);
    s.received(30, 1000);
    s.delivered();
    r.emit(Event::StatsUpdated { snapshot: hub.snapshot_at(telescope_core::time::monotonic_ns() + 10_000_000_000, Some(1_000_000_000), Some(200 << 20)) });
    r.w.process();
    settle(&r);
    r
}

fn resize(r: &Rig, width: f32) {
    r.w.ui.window().set_size(slint::LogicalSize::new(width, 700.0));
    r.w.process();
    settle(r);
}

fn num(p: &Row, key: &str) -> f32 {
    p[key].parse().unwrap()
}

/// Where the bar's drawn pins are and which are folded, checked for what must hold at any width.
fn check_bar(r: &Rig, width: f32) -> (Vec<String>, Vec<String>) {
    let all = pins(r);
    let pad = 16.0;
    let drawn: Vec<&Row> = all.iter().filter(|p| p["state"] == "0").collect();
    let folded: Vec<&Row> = all.iter().filter(|p| p["state"] == "1").collect();
    for p in &drawn {
        assert!(num(p, "x") >= pad - 0.01 && num(p, "x") + num(p, "w") <= width - pad + 0.01, "{} sits inside the bar at {width}: {p:?}", p["id"]);
    }
    for a in &drawn {
        for b in &drawn {
            if a["id"] < b["id"] {
                assert!(num(a, "x") + num(a, "w") <= num(b, "x") + 0.01 || num(b, "x") + num(b, "w") <= num(a, "x") + 0.01, "{} and {} overlap at {width}", a["id"], b["id"]);
            }
        }
    }
    // No pin changed side
    let side = |id: &str| if ["camera", "stats.frames_in", "stats.repeated"].contains(&id) { "0" } else { "1" };
    for p in all.iter().filter(|p| p["state"] != "2" && p["id"] != "") {
        assert_eq!(p["side"], side(&p["id"]), "{} stays on its side at {width}", p["id"]);
    }
    // The chips hold what the fold left
    let held = |s: &str| folded.iter().filter(|p| p["side"] == s).count() as f32;
    let number = |name: &str| match r.w.ui.get_property(name) { Ok(slint_interpreter::Value::Number(n)) => n as f32, other => panic!("{name} {other:?}") };
    assert_eq!(number("bar-left-folded"), held("0"), "the left chip counts its pins at {width}");
    assert_eq!(number("bar-right-folded"), held("1"), "the right chip counts its pins at {width}");
    (drawn.iter().map(|p| p["id"].clone()).collect(), folded.iter().map(|p| p["id"].clone()).collect())
}

#[test]
fn a_wide_bar_shows_every_pin_and_a_narrow_one_folds_in_the_middle() {
    let r = full_bar("bar-fold");
    resize(&r, 1400.0);
    let (drawn, folded) = check_bar(&r, 1400.0);
    assert_eq!((drawn.len(), folded.len()), (7, 0), "{drawn:?}");
    let mut seen = vec![];
    for width in [900.0, 640.0, 480.0, 360.0, 260.0, 160.0] {
        resize(&r, width);
        let (drawn, folded) = check_bar(&r, width);
        // The outermost pin of a side is the last to go
        if width >= 480.0 {
            assert!(drawn.contains(&"camera".to_string()) && drawn.contains(&"throughput".to_string()), "{width}: {drawn:?}");
        }
        seen.push(folded.len());
    }
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "narrower folds more, never fewer: {seen:?}");
    assert!(seen.last().copied().unwrap_or(0) > 0 && seen[0] <= seen[3], "{seen:?}");
    // Wide again: they all come back
    resize(&r, 1400.0);
    assert_eq!(check_bar(&r, 1400.0).1.len(), 0);
}

#[test]
fn a_full_side_folds_alone_and_the_other_keeps_every_pin() {
    let r = Rig::seeded("bar-one-side", json!({"bar_pins": {"left": [], "right": ["stats.dropped", "stats.memory", "stats.repeated", "stats.frames_in", "fps", "throughput"]}, "plugins": {"on": ["stats"]}}), |_, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(StatsPlugin::new())]
    });
    shown(&r);
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let hub = Hub::new();
    let s = hub.register("pixel", 0);
    s.received(30, 1000);
    r.emit(Event::StatsUpdated { snapshot: hub.snapshot_at(telescope_core::time::monotonic_ns() + 10_000_000_000, Some(1_000_000_000), Some(200 << 20)) });
    r.w.process();
    resize(&r, 380.0);
    let all = pins(&r);
    assert!(all.iter().any(|p| p["state"] == "1"), "the right side folds");
    let number = |name: &str| match r.w.ui.get_property(name) { Ok(slint_interpreter::Value::Number(n)) => n, other => panic!("{name} {other:?}") };
    assert_eq!(number("bar-left-folded"), 0.0);
    assert!(number("bar-right-folded") > 0.0);
    // The pins of the right side that stay are the outer ones
    assert!(all.iter().filter(|p| p["state"] == "0").any(|p| p["id"] == "throughput"));
    assert_eq!(pin(&r, "stats.dropped")["state"], "1", "the innermost goes first");
}

fn pointer(r: &Rig, event: slint::platform::WindowEvent) {
    r.w.ui.window().dispatch_event(event);
    r.w.process();
}

#[test]
fn a_pin_dragged_across_the_bar_lands_on_the_other_side_and_the_others_make_room() {
    use slint::platform::{PointerEventButton, WindowEvent};
    let r = Rig::seeded("bar-drag", json!({"plugins": {"on": ["stats"]}}), |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(StatsPlugin::new())]);
    shown(&r);
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let y = 700.0 - 20.0;
    let (fps, camera) = (pin(&r, "fps"), pin(&r, "camera"));
    let centre = |p: &Row| slint::LogicalPosition::new(num(p, "x") + num(p, "w") / 2.0, y);
    let start = centre(&fps);
    pointer(&r, WindowEvent::PointerMoved { position: start });
    pointer(&r, WindowEvent::PointerPressed { position: start, button: PointerEventButton::Left });
    // Held and moved over the left side, in front of the camera
    let mut x = start.x;
    while x > 20.0 {
        x -= 10.0;
        pointer(&r, WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
        r.skip(16);

    }
    assert_eq!(r.w.state.borrow().bar.dragging(), Some("fps"), "past the first few pixels it is a drag");
    let held = pin(&r, "camera");
    assert!(num(&held, "x") > num(&camera, "x") + 10.0, "the camera made room: it slid right");
    r.skip(400);
    offscreen::redraw(r.w.ui.window());
    pointer(&r, WindowEvent::PointerReleased { position: slint::LogicalPosition::new(x, y), button: PointerEventButton::Left });
    settle(&r);
    assert_eq!(r.w.state.borrow().bar.persisted(), (vec!["fps".to_string(), "camera".to_string()], vec!["throughput".to_string()]));
    assert_eq!(saved(&r), json!({"left": ["fps", "camera"], "right": ["throughput"]}));
    assert!(r.w.state.borrow().bar.dragging().is_none());
    assert_eq!(pin(&r, "fps")["side"], "0");
}

#[test]
fn a_press_that_does_not_move_is_not_a_drag() {
    use slint::platform::{PointerEventButton, WindowEvent};
    let r = with_stats("bar-click");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let p = pin(&r, "fps");
    let at = slint::LogicalPosition::new(num(&p, "x") + 4.0, 680.0);
    pointer(&r, WindowEvent::PointerMoved { position: at });
    pointer(&r, WindowEvent::PointerPressed { position: at, button: PointerEventButton::Left });
    pointer(&r, WindowEvent::PointerMoved { position: slint::LogicalPosition::new(at.x + 1.0, at.y) });
    pointer(&r, WindowEvent::PointerReleased { position: at, button: PointerEventButton::Left });
    assert!(r.w.state.borrow().bar.dragging().is_none());
    assert!(saved(&r).is_null());
}

// ---- Motion ----

/// Where (left edge) and how solid a pin is on screen right now, by its name.
fn on_screen(r: &Rig, name: &str) -> (f32, f32) {
    // A pin that has faded right out is not drawn at all
    match i_slint_backend_testing::ElementHandle::find_by_accessible_label(&r.w.ui, name).next() {
        Some(e) => (e.absolute_position().x, e.computed_opacity()),
        None => (f32::NAN, 0.0),
    }
}

fn frames(r: &Rig, name: &str, steps: u32, each_ms: u64) -> Vec<(f32, f32)> {
    let mut out = vec![on_screen(r, name)];
    for _ in 0..steps {
        offscreen::skip(std::time::Duration::from_millis(each_ms));
        offscreen::redraw(r.w.ui.window());
        out.push(on_screen(r, name));
    }
    out
}

#[test]
fn an_unpinned_pin_fades_where_it_was_and_the_one_beside_it_glides_over() {
    let r = with_stats("bar-motion-unpin");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    let (fps_before, _) = on_screen(&r, "Frame rate");
    let (net_before, net_alpha) = on_screen(&r, "Throughput");
    assert_eq!(net_alpha, 1.0);
    r.w.unpin_from_bar("throughput");
    let fade = frames(&r, "Throughput", 20, 16);
    assert!(fade[1].1 < 1.0 && fade[1].1 > 0.0, "the first frame is part way, not gone: {:?}", &fade[..4]);
    assert!(fade.windows(2).all(|w| w[1].1 <= w[0].1 + 0.001), "it only fades out");
    assert!(fade.iter().filter(|f| f.1 > 0.0).all(|f| (f.0 - net_before).abs() < 0.5), "and it fades where it was");
    assert!(fade.last().unwrap().1 < 0.05);
    // The frame rate moves over to the right edge, from where it is, without a jump
    let slide = frames(&r, "Frame rate", 30, 16);
    let travel = slide.last().unwrap().0 - fps_before;
    assert!(travel > 20.0, "it made its way right by {travel}");
    let steps: Vec<f32> = slide.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(steps.iter().all(|s| *s >= -0.01), "never backwards: {steps:?}");
    assert!(steps[0] < travel * 0.6, "and does not jump in its first frame: {steps:?}");
}

#[test]
fn pinning_again_while_a_pin_is_going_turns_it_round_from_where_it_is() {
    let r = with_stats("bar-motion-turn");
    r.start();
    reports(&r, 0, "Phone Camera", 30, "12.4 Mbps");
    r.w.unpin_from_bar("fps");
    offscreen::skip(std::time::Duration::from_millis(40));
    offscreen::redraw(r.w.ui.window());
    let (_, half) = on_screen(&r, "Frame rate");
    assert!(half > 0.0 && half < 1.0, "part way out: {half}");
    r.w.state.borrow_mut().bar.pin("fps", true);
    r.w.process();
    let back = frames(&r, "Frame rate", 20, 16);
    assert!(back[1].1 >= half - 0.001, "it turns from where it was, not from nothing or full: {half} then {:?}", &back[..3]);
    assert!(back.last().unwrap().1 > 0.95, "and is back");
}

#[test]
fn folding_and_unfolding_are_motions_both_ways() {
    let r = full_bar("bar-motion-fold");
    resize(&r, 1400.0);
    let (x_wide, a_wide) = on_screen(&r, "Memory");
    assert_eq!(a_wide, 1.0);
    // Narrow: the pin slides toward its chip while it fades
    r.w.ui.window().set_size(slint::LogicalSize::new(380.0, 700.0));
    r.w.process();
    let fold = frames(&r, "Memory", 24, 16);
    assert!(fold.last().unwrap().1 < 0.05, "gone into the chip");
    assert!(fold.windows(2).all(|w| w[1].1 <= w[0].1 + 0.001));
    assert!(fold[1].1 > 0.0, "not at once: {:?}", &fold[..3]);
    // Wide again: it comes back out
    r.w.ui.window().set_size(slint::LogicalSize::new(1400.0, 700.0));
    r.w.process();
    let unfold = frames(&r, "Memory", 24, 16);
    assert!(unfold[1].1 > 0.0 && unfold[1].1 < 1.0, "{:?}", &unfold[..3]);
    assert!(unfold.last().unwrap().1 > 0.95);
    let _ = x_wide;
}
