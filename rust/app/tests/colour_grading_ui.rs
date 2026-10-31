//! test_colour_grading.py's tests of what's on screen: the card in a real offscreen window, the plugin among the real built-ins.

use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance};
use std::path::{Path, PathBuf};
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::grade::monotone_curve;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 900.0;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The card in a window; a wrapper hands down the properties these tests set.
fn window(plugin: &mut ColourGradingPlugin) -> ComponentInstance {
    offscreen::install();
    let card = plugin.card().expect("a card");
    let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let doc = format!(
        "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
         export component W inherits Window {{\n    width: {WIDTH}px;\n    height: {HEIGHT}px;\n    \
         in-out property <int> page;\n    in-out property <int> curve;\n    in-out property <string> curve-note;\n    in-out property <string> curve-line;\n    \
         {c} {{ hue: Theme.hue-camera; page: root.page; curve: root.curve; curve-note: root.curve-note; curve-line: root.curve-line; }}\n}}",
        c = card.ui.component
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with("plugin/card.slint").then(|| Ok(source.clone()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc, Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    w
}

fn card() -> (ColourGradingPlugin, FakeHost) {
    let (mut plugin, mut host) = (ColourGradingPlugin::new(), FakeHost::new());
    plugin.setup(&mut host);
    plugin.set_config(&json!({})).unwrap();
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    (plugin, host)
}

fn ui(plugin: &mut ColourGradingPlugin, host: &mut FakeHost, callback: &str, arg: usize) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![json!(arg)] });
}

/// What the plugin last told the card for a property.
fn told(host: &FakeHost, key: &str) -> Json {
    host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Json>(j).ok())
        .find_map(|v| v.get(key).cloned()).unwrap_or(Json::Null)
}

/// Hands the card what the plugin told it, and lets it lay out.
fn show(w: &ComponentInstance, host: &FakeHost) -> Vec<u8> {
    for key in ["page", "curve", "curve-note", "curve-line"] {
        let value = told(host, key);
        if !value.is_null() {
            w.set_property(key, from_json(&value)).unwrap();
        }
    }
    offscreen::draw(w.window(), WIDTH, HEIGHT, 1.0).expect("a picture").rgba
}

fn shows(w: &ComponentInstance, kind: &str) -> bool {
    ElementHandle::find_by_element_type_name(w, kind).next().is_some()
}

fn labelled(w: &ComponentInstance, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(w, label).next().is_some()
}

#[test]
fn pages_show_one_at_a_time_and_the_channel_row_only_for_rgb() {
    let (mut plugin, mut host) = card();
    let w = window(&mut plugin);
    show(&w, &host);
    assert!(shows(&w, "WheelCell") && !shows(&w, "CurveBox"), "Primaries is the page to begin with");

    ui(&mut plugin, &mut host, "page-picked", 1);
    show(&w, &host);
    assert!(!shows(&w, "WheelCell") && shows(&w, "CurveBox"));
    assert!(labelled(&w, "Channel"));
    assert!(!shows(&w, "Note"));

    ui(&mut plugin, &mut host, "curve-picked-by-hand", 1);
    show(&w, &host);
    assert!(!labelled(&w, "Channel"));
    assert_eq!(told(&host, "curve-points")[0], json!({"x": 0.0, "y": 0.5}), "the hue curve's neutral line");
    assert!(shows(&w, "Note"));
}

#[test]
fn the_curve_drawn_is_the_one_applied() {
    let points = [(0.0, 0.1), (0.3, 0.2), (1.0, 0.9)];
    let (mut plugin, mut host) = card();
    let w = window(&mut plugin);
    ui(&mut plugin, &mut host, "page-picked", 1);
    let straight = show(&w, &host);

    plugin.set_config(&json!({"curves": {"all": [[0.0, 0.1], [0.3, 0.2], [1.0, 0.9]]}})).unwrap();
    plugin.on_event(&mut host, &Event::StreamStopped);
    let bent = show(&w, &host);
    assert_ne!(straight, bent, "the card paints the new curve");

    // the line it paints is the curve, in thousandths with y going down
    let line = told(&host, "curve-line");
    let words: Vec<&str> = line.as_str().unwrap().split(' ').collect();
    assert_eq!(words.len(), 200 * 3);
    for pair in words.chunks(3) {
        let (x, y) = (pair[1].parse::<f64>().unwrap() / 1000.0, pair[2].parse::<f64>().unwrap() / 1000.0);
        assert!((1.0 - y - f64::from(monotone_curve(&points, x))).abs() < 0.001, "{pair:?}");
    }
}

#[test]
fn it_is_an_optional_built_in_that_rides_in_presets_per_phone() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop/tests/fixtures/configs/linux-two-phones");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(fixture).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let snapshot: Json = serde_json::from_slice(&out.stdout).unwrap();
    assert!(snapshot["plugins"].as_array().unwrap().contains(&json!("colour_grading")), "{}", snapshot["plugins"]);
    let info = ColourGradingPlugin::new().info();
    assert!(info.optional && info.in_presets && info.device_local && !info.needs_vision);
}
