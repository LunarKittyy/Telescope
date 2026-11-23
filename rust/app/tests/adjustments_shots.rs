// The Adjustments chip, its list, the falling behind banner and the Automatic adjustments card in the real window, with the checks that the
// chip never pushes Start out or widens the window: ADJUST_SHOTS=dir cargo nextest run -p telescope-app --test adjustments_shots saves each as NAME.png.
mod rig;

use i_slint_backend_testing::ElementHandle;
use rig::{Listener, Rig};
use serde_json::json;
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::path::PathBuf;
use telescope_core::effort::{EffortSnapshot, MOST};
use telescope_core::event::Event;
use telescope_core::host::{Appearance, TextSize};
use telescope_core::plugin::Plugin;
use telescope_core::stats::{DropReason, Hub, NamedTiming, Snapshot, Timing};
use telescope_setup::adjustments::AdjustmentsPlugin;
use telescope_shell::offscreen;

const LOOKS: [(&str, f64); 2] = [("dark", 0.0), ("light", 3.0)];

fn out() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("ADJUST_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn rig() -> Rig {
    let r = Rig::seeded("adjustments-shots", json!({"plugins": {"on": ["adjustments"]}}), |_, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(AdjustmentsPlugin::new(std::sync::Arc::new(telescope_media::FfmpegMedia)))]
    });
    r.w.ui.window().set_size(slint::LogicalSize::new(1380.0, 700.0));
    r.w.ui.show().unwrap();
    r.w.process();
    r.start();
    r.worker(0).says("ok", "Streaming 1920 \u{d7} 1080 at 30 fps to Telescope");
    r.pump();
    r
}

fn timing(name: &str, ms: f32) -> NamedTiming {
    NamedTiming { name: name.into(), timing: Timing { mean_ms: ms, max_ms: ms * 1.5, count: 30 } }
}

/// A stream that has run for a while: `shed` of its 30 frames a second thinned out, and `items` automatic adjustments in effect.
fn snapshot(items: usize, shed: u64, level: u8) -> Snapshot {
    let hub = Hub::new();
    let stats = hub.register("pixel", 0);
    stats.set_target_fps(30);
    let began = telescope_core::time::monotonic_ns();
    let mut last = hub.snapshot_at(began, None, None);
    for second in 1..=8u64 {
        stats.received(30, 250_000);
        stats.decoded(30);
        stats.dropped(DropReason::Paced, shed);
        for _ in 0..(30 - shed) {
            stats.delivered();
        }
        last = hub.snapshot_at(began + second * 1_000_000_000, None, None);
    }
    if items >= 1 || level > 0 {
        last.app.effort = EffortSnapshot { level, auto_level: level, auto: true, ..Default::default() };
    }
    last.app.balance.on_card = items < 2;
    last.sources[0].plugins = vec![timing("background", 11.0), timing("framing", 3.5)];
    last.app.models = vec![timing("mask", 8.0), timing("people", 6.5)];
    last
}

fn feed(r: &Rig, snap: Snapshot) {
    r.emit(Event::StatsUpdated { snapshot: snap });
    for _ in 0..30 {
        r.skip(40);
        r.w.process();
    }
}

/// The phone reporting that it lowered its frame rate for heat, the third kind of adjustment in the shots.
fn phone_hot(r: &Rig, on: bool) {
    let id = r.streaming().remove(0);
    r.phone_says(&id, rig::state_with(json!({"heat_fps": if on { 20 } else { 0 }})));
    r.pump();
}

fn dress(r: &Rig, look: f64, large: bool) {
    let appearance = Appearance { text: if large { TextSize::Large } else { TextSize::Normal }, ..Default::default() };
    telescope_shell::appearance::apply_to(&r.w.ui, &appearance);
    r.w.ui.set_global_property("Theme", "look", Value::Number(look)).unwrap();
}

fn narrowest(r: &Rig) -> f32 {
    match r.w.ui.get_property("narrowest") {
        Ok(Value::Number(n)) => n as f32,
        other => panic!("narrowest is {other:?}"),
    }
}

fn settle(r: &Rig, width: f32) {
    r.w.ui.window().set_size(slint::LogicalSize::new(width, 700.0));
    for _ in 0..24 {
        r.skip(50);
        offscreen::redraw(r.w.ui.window());
        r.w.process();
    }
}

/// The top `tall` logical px of the window at `width`, 2x.
fn shot(r: &Rig, dir: &Option<PathBuf>, name: &str, width: f32, tall: f32) {
    settle(r, width);
    let Some(dir) = dir else { return };
    let pic = offscreen::draw(r.w.ui.window(), width, 700.0, 2.0).expect("a picture");
    let (cw, ch) = (pic.width as usize, (tall * 2.0) as usize);
    let rgba = pic.rgba[..cw * ch * 4].to_vec();
    offscreen::Picture { width: cw as u32, height: ch as u32, rgba }.save_png(&dir.join(format!("{name}.png"))).unwrap();
    r.w.ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
}

/// Where Start streaming ends, which has to stay inside the window.
fn start_right(r: &Rig) -> f32 {
    let start = ElementHandle::find_by_element_id(&r.w.ui, "TelescopeWindow::start-button").next().expect("the start button");
    start.absolute_position().x + start.size().width
}

/// The chip's button, found by its tooltip's name.
fn chip(r: &Rig) -> Option<ElementHandle> {
    ElementHandle::find_by_element_type_name(&r.w.ui, "Button").find(|e| e.accessible_description().is_some_and(|d| d.starts_with("Automatic adjustments")))
}

#[test]
fn the_chip_at_every_width_and_text_size_never_pushes_start_out_or_widens_the_window() {
    let dir = out();
    let r = rig();
    for (look_name, look) in LOOKS {
        for (size, large) in [("", false), ("-large", true)] {
            dress(&r, look, large);
            feed(&r, snapshot(0, 0, 0));
            settle(&r, 1380.0);
            let bare_narrowest = narrowest(&r);
            shot(&r, &dir, &format!("chip-0-wide-{look_name}{size}"), 1380.0, 40.0);
            shot(&r, &dir, &format!("chip-0-narrowest-{look_name}{size}"), bare_narrowest, 40.0);
            feed(&r, snapshot(1, 0, 2));
            shot(&r, &dir, &format!("chip-1-wide-{look_name}{size}"), 1380.0, 40.0);
            feed(&r, snapshot(2, 0, 2));
            shot(&r, &dir, &format!("chip-2-wide-{look_name}{size}"), 1380.0, 40.0);
            phone_hot(&r, true);
            feed(&r, snapshot(3, 0, 2));
            let wide_chip = chip(&r).expect("the chip").size().width;
            shot(&r, &dir, &format!("chip-3-wide-{look_name}{size}"), 1380.0, 40.0);
            let least = narrowest(&r);
            assert!(least <= bare_narrowest + 0.5, "{look_name}{size}: the chip raises the least width from {bare_narrowest} to {least}");
            for width in [least, least + 80.0, 700.0, 900.0, 1100.0, 1380.0] {
                settle(&r, width);
                assert!(start_right(&r) <= width + 0.5, "{look_name}{size} at {width}: Start ends at {}", start_right(&r));
            }
            shot(&r, &dir, &format!("chip-3-narrowest-{look_name}{size}"), least, 40.0);
            let narrow_chip = chip(&r).expect("the chip").size().width;
            assert!(narrow_chip < wide_chip, "{look_name}{size}: the chip shrinks when narrow ({narrow_chip} against {wide_chip})");
            phone_hot(&r, false);
        }
    }
}

#[test]
fn the_list_and_the_banner() {
    let dir = out();
    let r = rig();
    for (look_name, look) in LOOKS {
        dress(&r, look, false);
        phone_hot(&r, true);
        // Frames shed with the effects as light as they go, with the list closed
        feed(&r, snapshot(3, 6, MOST));
        shot(&r, &dir, &format!("banner-{look_name}"), 1100.0, 200.0);
        feed(&r, snapshot(3, 0, 2));
        settle(&r, 1100.0);
        chip(&r).expect("the chip").invoke_accessible_default_action();
        for _ in 0..30 {
            r.skip(40);
            r.w.process();
        }
        shot(&r, &dir, &format!("list-open-{look_name}"), 1100.0, 420.0);
        // Esc closes it
        r.w.ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
        r.w.ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
        for _ in 0..20 {
            r.skip(40);
            r.w.process();
        }
        phone_hot(&r, false);
        feed(&r, snapshot(0, 0, 0));
    }
}

#[test]
fn the_settings_card() {
    let dir = out();
    let r = rig();
    for (look_name, look) in LOOKS {
        dress(&r, look, false);
        telescope_shell::settings::open(&r.w, "video");
        let settings = telescope_shell::settings::current().expect("the settings window");
        assert!(settings.show_tab("video"));
        settings.ui.window().set_size(slint::LogicalSize::new(940.0, 520.0));
        settings.ui.set_global_property("Theme", "look", Value::Number(look)).unwrap();
        for _ in 0..20 {
            offscreen::pump();
            r.skip(40);
            r.w.process();
        }
        let labels = ["Lighter effects", "Effects off a busy graphics card", "Dynamic bitrate"];
        for label in labels {
            assert!(ElementHandle::find_by_accessible_label(&settings.ui, label).next().is_some(), "{label} is on the card");
        }
        if let Some(dir) = &dir {
            let pic = offscreen::draw(settings.ui.window(), 940.0, 520.0, 1.5).expect("a picture");
            pic.save_png(&dir.join(format!("settings-video-{look_name}.png"))).unwrap();
        }
    }
}

/// Where an element with this accessible name is.
fn x_of(r: &Rig, label: &str) -> f32 {
    ElementHandle::find_by_accessible_label(&r.w.ui, label).next().unwrap_or_else(|| panic!("no {label}")).absolute_position().x
}

#[test]
fn the_chip_comes_and_goes_without_moving_what_is_beside_it() {
    let r = rig();
    settle(&r, 1380.0);
    let (start, gear) = (start_right(&r), x_of(&r, "Settings"));
    for (items, level) in [(1, 2), (0, 0), (3, 2)] {
        r.emit(Event::StatsUpdated { snapshot: snapshot(items, 0, level) });
        // One loop turn at a time: nothing next to the chip moves on the way in or out
        for step in 0..40 {
            r.skip(16);
            r.w.process();
            // Under half a pixel: layout sums can land a hair off the same spot
            let now = (start_right(&r), x_of(&r, "Settings"));
            assert!((now.0 - start).abs() < 0.5 && (now.1 - gear).abs() < 0.5, "{items} items, step {step}: {now:?} vs {:?}", (start, gear));
        }
    }
}
