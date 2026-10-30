//! The Transforms card drawn on a real (offscreen) Slint window: while Auto framing has the view, the zoom's handle and value look
//! switched off (test_transforms.py's test_auto_framing_visibly_disables_the_zoom_handle_and_value).

use serde_json::Value as Json;
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance};
use std::collections::BTreeMap;
use std::path::Path;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::SlintSource;
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

const HUE_CAMERA: [u8; 3] = [0x7f, 0xd3, 0xc4];
const TEXT_DISABLED: [u8; 3] = [0x52, 0x5b, 0x67];
const WIDTH: usize = 460;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The card in a window with the camera's hue, as the shell shows it. The interpreter lists only the properties a card declares
/// itself, not the ones it gets from Card (the hue), so a wrapper hands the card's down.
fn window(plugin: &mut TransformsPlugin) -> ComponentInstance {
    offscreen::install();
    let card = plugin.card().expect("a card");
    let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let kinds: [(&str, &[&str]); 6] = [
        ("bool", &["active", "pan-enabled", "zoom-free"]),
        ("float", &["pan-x", "pan-y", "rotation", "stretch-x", "stretch-y", "zoom", "zoom-max"]),
        ("string", &["dot-level", "dot-tip", "pan-text", "pan-tip", "reset-hint", "reset-tip", "stretch-x-text", "stretch-y-text", "zoom-text", "zoom-tip"]),
        ("[bool]", &["flips"]),
        ("[float]", &["zoom-marks"]),
        ("[string]", &["stretch-tips"]),
    ];
    let (mut declared, mut passed) = (String::new(), String::new());
    for (kind, names) in kinds {
        for name in names {
            declared += &format!("    in-out property <{kind}> {name};\n");
            passed += &format!("{name}: root.{name}; ");
        }
    }
    let doc = format!(
        "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
         export component W inherits Window {{\n    width: {WIDTH}px;\n    height: 760px;\n    callback zoom-moved(float);\n{declared}    \
         {c} {{ hue: Theme.hue-camera; {passed}zoom-moved(v) => {{ root.zoom-moved(v); }} }}\n}}",
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

/// What the card was last told for each property, from the calls the plugin made on its host.
fn props(host: &FakeHost, plugin: &mut TransformsPlugin) -> BTreeMap<String, Json> {
    let mut all: BTreeMap<String, Json> = plugin.card().unwrap().ui.props.as_object().cloned().unwrap_or_default().into_iter().collect();
    for call in &host.calls {
        if let Some(json) = call.strip_prefix("set_ui_props card ") {
            if let Ok(Json::Object(map)) = serde_json::from_str::<Json>(json) {
                all.extend(map);
            }
        }
    }
    all
}

fn show(w: &ComponentInstance, props: &BTreeMap<String, Json>) -> Vec<u8> {
    for (name, value) in props {
        // A property the wrapper doesn't hand down isn't drawn differently by anything here
        let _ = w.set_property(name, from_json(value));
    }
    offscreen::draw(w.window(), WIDTH as f32, 760.0, 1.0).expect("a picture").rgba
}

fn is(p: &[u8], colour: [u8; 3]) -> bool {
    p[..3] == colour
}

/// The pixels that are one colour in `before` and another in `after`, in groups that touch.
fn recoloured(before: &[u8], after: &[u8], from: [u8; 3], to: [u8; 3]) -> Vec<Vec<(usize, usize)>> {
    let hit: Vec<bool> = before.chunks(4).zip(after.chunks(4)).map(|(b, a)| is(b, from) && is(a, to)).collect();
    let height = hit.len() / WIDTH;
    let mut seen = vec![false; hit.len()];
    let mut groups = Vec::new();
    for start in 0..hit.len() {
        if !hit[start] || seen[start] {
            continue;
        }
        let (mut group, mut todo) = (Vec::new(), vec![start]);
        seen[start] = true;
        while let Some(i) = todo.pop() {
            let (x, y) = (i % WIDTH, i / WIDTH);
            group.push((x, y));
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < WIDTH && ny < height && hit[ny * WIDTH + nx] && !seen[ny * WIDTH + nx] {
                    seen[ny * WIDTH + nx] = true;
                    todo.push(ny * WIDTH + nx);
                }
            }
        }
        groups.push(group);
    }
    groups
}

fn auto_framing_visibly_disables_the_zoom_handle_and_value() {
    let mut plugin = TransformsPlugin::new();
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.set_config(&serde_json::json!({})).unwrap();
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    let w = window(&mut plugin);
    let free = show(&w, &props(&host, &mut plugin));

    plugin.on_event(&mut host, &Event::ViewTaken { owner: "Auto framing".into() });
    let taken = show(&w, &props(&host, &mut plugin));
    // The handle is a filled 14 px disc that went from the camera's colour to the disabled one, and so did the value's text
    let groups = recoloured(&free, &taken, HUE_CAMERA, TEXT_DISABLED);
    let handle = groups.iter().find(|g| {
        let (x0, x1) = (g.iter().map(|p| p.0).min().unwrap(), g.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (g.iter().map(|p| p.1).min().unwrap(), g.iter().map(|p| p.1).max().unwrap());
        (11..=15).contains(&(x1 - x0 + 1)) && (11..=15).contains(&(y1 - y0 + 1)) && g.len() >= 100
    });
    assert!(handle.is_some(), "no handle went grey: {:?}", groups.iter().map(|g| g.len()).collect::<Vec<_>>());
    assert!(groups.iter().any(|g| g.len() < 100), "the value's text went grey too");

    plugin.on_event(&mut host, &Event::ViewTaken { owner: String::new() });
    let released = show(&w, &props(&host, &mut plugin));
    assert!(released == free, "everything comes back when it lets go");
}

/// Where the zoom slider's handle is, as the middle of the disc that goes grey when the zoom is switched off.
fn zoom_handle(w: &ComponentInstance) -> (f64, f64) {
    let draw = |free: bool| {
        w.set_property("zoom-free", slint_interpreter::Value::Bool(free)).unwrap();
        offscreen::draw(w.window(), WIDTH as f32, 760.0, 1.0).expect("a picture").rgba
    };
    let (free, off) = (draw(true), draw(false));
    draw(true);
    let groups = recoloured(&free, &off, HUE_CAMERA, TEXT_DISABLED);
    let disc = groups.iter().find(|g| g.len() >= 100).expect("the handle");
    (disc.iter().map(|p| p.0 as f64).sum::<f64>() / disc.len() as f64 + 0.5, disc.iter().map(|p| p.1 as f64).sum::<f64>() / disc.len() as f64 + 0.5)
}

fn point(x: f64, y: f64) -> slint::LogicalPosition {
    slint::LogicalPosition::new(x as f32, y as f32)
}

fn dragging_the_zoom_sticks_to_a_lens_but_keys_dont() {
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint_interpreter::Value;
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut plugin = TransformsPlugin::new();
    let w = window(&mut plugin);
    for (name, value) in [("zoom-max", Value::Number(10.0)), ("active", Value::Bool(true)), ("zoom-marks", from_json(&serde_json::json!([3.7])))] {
        w.set_property(name, value).unwrap();
    }
    // The handle at 1x and at 10x say where the track runs
    w.set_property("zoom", Value::Number(1.0)).unwrap();
    let (x1, y) = zoom_handle(&w);
    w.set_property("zoom", Value::Number(10.0)).unwrap();
    let (x10, _) = zoom_handle(&w);
    let at = |zoom: f64| x1 + (zoom - 1.0) / 9.0 * (x10 - x1);
    let moved = Rc::new(RefCell::new(Vec::<f64>::new()));
    let seen = moved.clone();
    w.set_callback("zoom-moved", move |args| {
        if let Value::Number(n) = &args[0] {
            seen.borrow_mut().push((n * 1000.0).round() / 1000.0);
        }
        Value::Void
    }).unwrap();

    let window = w.window();
    let button = PointerEventButton::Left;
    // Three pixels short of the lens's mark: it sticks to it
    w.set_property("zoom", Value::Number(2.0)).unwrap();
    window.dispatch_event(WindowEvent::PointerPressed { position: point(at(2.0), y), button });
    window.dispatch_event(WindowEvent::PointerMoved { position: point(at(3.7) - 3.0, y) });
    window.dispatch_event(WindowEvent::PointerReleased { position: point(at(3.7) - 3.0, y), button });
    assert_eq!(moved.borrow().last(), Some(&3.7), "{:?}", moved.borrow());
    // Keys step off it freely: Tab to the slider and press left
    moved.borrow_mut().clear();
    for _ in 0..20 {
        window.dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Tab.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Tab.into() });
        window.dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::LeftArrow.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::LeftArrow.into() });
        if !moved.borrow().is_empty() {
            break;
        }
    }
    assert_eq!(moved.borrow().first(), Some(&3.69), "{:?}", moved.borrow());
    // Well clear of the mark, a drag doesn't stick to it
    moved.borrow_mut().clear();
    window.dispatch_event(WindowEvent::PointerPressed { position: point(at(3.69), y), button });
    window.dispatch_event(WindowEvent::PointerMoved { position: point(at(2.0), y) });
    window.dispatch_event(WindowEvent::PointerReleased { position: point(at(2.0), y), button });
    let last = *moved.borrow().last().expect("moved");
    assert!((1.9..2.1).contains(&last), "{:?}", moved.borrow());
}

/// The slint platform lives on the thread that made it, so the window tests take turns on one.
#[test]
fn the_transforms_card_in_a_window() {
    auto_framing_visibly_disables_the_zoom_handle_and_value();
    dragging_the_zoom_sticks_to_a_lens_but_keys_dont();
}
