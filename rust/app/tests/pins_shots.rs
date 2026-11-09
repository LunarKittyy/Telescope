// Pictures of the Stats card and the bottom bar with its pins, new look only, for a look by eye:
// PIN_SHOTS=dir cargo nextest run -p telescope-app --test pins_shots, each as NAME.png at 2x.
mod rig;

use rig::{Listener, Rig};
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::path::{Path, PathBuf};
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::stats::Hub;
use telescope_core::testing::FakeHost;
use telescope_core::ui::SlintSource;
use telescope_setup::stats::StatsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

fn out() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("PIN_SHOTS").ok()?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn settle() {
    for _ in 0..30 {
        offscreen::pump();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

/// The card's props as the plugin sets them for this snapshot.
fn card_props(plugin: &mut StatsPlugin, host: &mut FakeHost) -> Json {
    let call = host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ")).expect("the card was set").clone();
    let _ = plugin;
    serde_json::from_str(call.trim_start_matches("set_ui_props card ")).unwrap()
}

fn stats_card(dir: &Path, name: &str, props: &Json, width: f32) {
    let mut plugin = StatsPlugin::new();
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let head = format!("export component {} inherits", card.component);
    let at = source.find(&head).unwrap();
    let open = at + source[at..].find('{').unwrap() + 1;
    let source = format!("{}\n    out property <length> shot-height: root.preferred-height;\n    hue: Theme.plugin-hue(\"streaming\");{}\nexport {{ Theme }}", &source[..open], &source[open..]);
    offscreen::install();
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    for part in [&card.props, props] {
        for (key, value) in part.as_object().unwrap() {
            ui.set_property(key, to_property(&ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        }
    }
    ui.window().set_size(slint::LogicalSize::new(width, 2400.0));
    ui.show().unwrap();
    offscreen::pump();
    settle();
    let height = match ui.get_property("shot-height") {
        Ok(Value::Number(h)) => h as f32,
        other => panic!("no preferred height: {other:?}"),
    };
    ui.window().set_size(slint::LogicalSize::new(width, height.ceil() + 24.0));
    settle();
    let pic = offscreen::draw(ui.window(), width, height.ceil() + 24.0, 2.0).expect("a picture");
    pic.save_png(&dir.join(format!("{name}.png"))).unwrap();
    ui.hide().unwrap();
}

#[test]
fn stats_card_shots() {
    let Some(dir) = out() else { return };
    let hub = Hub::new();
    let a = hub.register("phone", 0);
    a.set_link("https://127.0.0.1:40001/v1/video.h264", false);
    a.set_target_fps(30);
    for second in 0..30u64 {
        a.received(30, 250_000 + second * 1000);
        for _ in 0..29 {
            a.delivered();
        }
        a.repeated();
    }
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + 30_000_000_000, Some(1_000_000_000), Some(200 << 20));
    let mut host = FakeHost::new();
    host.focused = Some("phone".into());
    host.bar_pinned = vec!["camera".into(), "fps".into(), "throughput".into(), "stats.frames_in".into()];
    host.bar_own = vec![telescope_core::bar::BarStat::new("camera", "Virtual camera", "", "Phone Camera")];
    let mut plugin = StatsPlugin::new();
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    let props = card_props(&mut plugin, &mut host);
    stats_card(&dir, "stats-streaming", &props, 420.0);
    // Idle: the card fades and says one quiet word
    host.streaming = false;
    plugin.on_stream_stop(&mut host);
    let idle = card_props(&mut plugin, &mut host);
    stats_card(&dir, "stats-idle", &idle, 420.0);
}

fn rig() -> Rig {
    let seed = json!({"bar_pins": {"left": ["camera", "stats.frames_in", "stats.repeated"], "right": ["stats.dropped", "stats.memory", "fps", "throughput"]}, "plugins": {"on": ["stats"]}});
    let r = Rig::seeded("pins-shots", seed, |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(StatsPlugin::new())]);
    r.w.ui.window().set_size(slint::LogicalSize::new(1100.0, 700.0));
    r.w.ui.show().unwrap();
    r.w.process();
    r.start();
    let w = r.worker(0);
    w.says("ok", "Streaming 1920 \u{d7} 1080 at 30 fps to Phone Camera");
    w.says("fps", "30 fps  1920x1080");
    w.says("net", "12.4 Mbps");
    r.pump();
    let hub = Hub::new();
    let s = hub.register("pixel", 0);
    s.received(30, 1000);
    s.delivered();
    r.emit(Event::StatsUpdated { snapshot: hub.snapshot_at(telescope_core::time::monotonic_ns() + 10_000_000_000, Some(1_000_000_000), Some(200 << 20)) });
    r.w.process();
    r
}

/// The bottom `tall` logical px of the window at `width`, 2x.
fn bar_shot(r: &Rig, dir: &Path, name: &str, width: f32, tall: f32) {
    r.w.ui.window().set_size(slint::LogicalSize::new(width, 700.0));
    for _ in 0..20 {
        r.skip(50);
        offscreen::redraw(r.w.ui.window());
        r.w.process();
    }
    let pic = offscreen::draw(r.w.ui.window(), width, 700.0, 2.0).expect("a picture");
    let (cw, ch) = (pic.width as usize, (tall * 2.0) as usize);
    let top = pic.height as usize - ch;
    let mut rgba = Vec::new();
    for y in top..pic.height as usize {
        rgba.extend_from_slice(&pic.rgba[(y * cw) * 4..((y + 1) * cw) * 4]);
    }
    offscreen::Picture { width: cw as u32, height: ch as u32, rgba }.save_png(&dir.join(format!("{name}.png"))).unwrap();
    r.w.ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
}

#[test]
fn bar_shots() {
    let Some(dir) = out() else { return };
    let r = rig();
    bar_shot(&r, &dir, "bar-wide", 1100.0, 40.0);
    bar_shot(&r, &dir, "bar-narrow-folded", 560.0, 40.0);
    bar_shot(&r, &dir, "bar-very-narrow", 330.0, 40.0);
    // One side full alone
    r.w.state.borrow_mut().bar.load(Some((vec!["camera".into()], vec!["stats.dropped".into(), "stats.memory".into(), "stats.repeated".into(), "stats.frames_in".into(), "fps".into(), "throughput".into()])));
    r.w.process();
    bar_shot(&r, &dir, "bar-one-side-folded", 520.0, 40.0);
    // The folded pins, opened
    r.w.ui.window().set_size(slint::LogicalSize::new(520.0, 700.0));
    r.w.process();
    let chips: Vec<_> = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&r.w.ui, "More").collect();
    if let Some(chip) = chips.first() {
        let (p, s) = (chip.absolute_position(), chip.size());
        let at = slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
        use slint::platform::{PointerEventButton, WindowEvent};
        r.w.ui.window().dispatch_event(WindowEvent::PointerMoved { position: at });
        r.w.ui.window().dispatch_event(WindowEvent::PointerPressed { position: at, button: PointerEventButton::Left });
        r.w.ui.window().dispatch_event(WindowEvent::PointerReleased { position: at, button: PointerEventButton::Left });
        r.w.process();
        bar_shot(&r, &dir, "bar-folded-open", 520.0, 200.0);
    }
}

#[test]
fn bar_drag_shot() {
    let Some(dir) = out() else { return };
    use slint::platform::{PointerEventButton, WindowEvent};
    let r = rig();
    for _ in 0..10 {
        r.skip(50);
        r.w.process();
    }
    let rows = r.rows("bar-pins");
    let fps = rows.iter().find(|p| p["id"] == "fps").unwrap();
    let num = |k: &str| fps[k].parse::<f32>().unwrap();
    let y = 680.0;
    let start = slint::LogicalPosition::new(num("x") + num("w") / 2.0, y);
    let win = r.w.ui.window();
    win.dispatch_event(WindowEvent::PointerMoved { position: start });
    win.dispatch_event(WindowEvent::PointerPressed { position: start, button: PointerEventButton::Left });
    let mut x = start.x;
    while x > 300.0 {
        x -= 10.0;
        win.dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
        r.skip(16);
        r.w.process();
    }
    for _ in 0..8 {
        r.skip(50);
    }
    bar_shot(&r, &dir, "bar-drag-midway", 1100.0, 40.0);
}
