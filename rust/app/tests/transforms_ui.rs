//! The Transforms card drawn on a real (offscreen) Slint window and driven with the pointer and the keys, what it does going to the
//! plugin as it does in the app.

use serde_json::Value as Json;
use slint::platform::{PointerEventButton, WindowEvent};
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

const HUE_CAMERA: [u8; 3] = [0x7f, 0xd3, 0xc4];
const TEXT_DISABLED: [u8; 3] = [0x52, 0x5b, 0x67];
// The Classic theme's colours, which this rig draws in: a slider handle's edge at rest (the camera colour three parts to one of the card), a disabled one's, and the value's ink
const HANDLE_EDGE: [u8; 3] = [101, 165, 155];
const HANDLE_OFF: [u8; 3] = [0x2f, 0x36, 0x41];
const INK: [u8; 3] = [0xe7, 0xea, 0xef];
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
        ("string", &["dot-level", "dot-tip", "pan-add-x", "pan-add-y", "pan-text", "pan-tip", "reset-hint", "reset-tip", "stretch-x-text", "stretch-y-text", "zoom-text", "zoom-tip"]),
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
        "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nexport {{ Theme }}\nimport {{ {c} }} from \"plugin/card.slint\";\n\
         export component W inherits Window {{\n    width: {WIDTH}px;\n    height: 760px;\n    callback zoom-moved(float);\n    \
         callback pad-pointer(float, float);\n    callback pad-key(string, bool);\n    callback pad-reset();\n{declared}    \
         {c} {{ hue: Theme.hue-camera; {passed}zoom-moved(v) => {{ root.zoom-moved(v); }} pad-pointer(x, y) => {{ root.pad-pointer(x, y); }} \
         pad-key(k, s) => {{ root.pad-key(k, s); }} pad-reset => {{ root.pad-reset(); }} }}\n}}",
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
    w.set_global_property("Theme", "classic", Value::Bool(true)).unwrap();
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
    p[..3].iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 2)
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

type Rig = Rc<RefCell<(TransformsPlugin, FakeHost)>>;

/// The plugin as the window starts it, a phone paired if `phone`.
fn rig(phone: bool) -> Rig {
    let (mut plugin, mut host) = (TransformsPlugin::new(), FakeHost::new());
    plugin.setup(&mut host);
    plugin.set_config(&serde_json::json!({})).unwrap();
    if phone {
        plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    }
    Rc::new(RefCell::new((plugin, host)))
}

fn tell(rig: &Rig, callback: &str, args: Vec<Json>) {
    let (plugin, host) = &mut *rig.borrow_mut();
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
}

/// The card is shown what the plugin last said.
fn sync(rig: &Rig, w: &ComponentInstance) -> Vec<u8> {
    let (plugin, host) = &mut *rig.borrow_mut();
    show(w, &props(host, plugin))
}

fn to_json(v: &Value) -> Json {
    match v {
        Value::Number(n) => serde_json::json!(n),
        Value::Bool(b) => serde_json::json!(b),
        Value::String(s) => serde_json::json!(s.as_str()),
        _ => Json::Null,
    }
}

/// These of the card's callbacks go to the plugin, as the shell sends them.
fn wire(rig: &Rig, w: &ComponentInstance, callbacks: &[&'static str]) {
    for callback in callbacks {
        let (rig, callback) = (rig.clone(), *callback);
        w.set_callback(callback, move |args| {
            tell(&rig, callback, args.iter().map(to_json).collect());
            Value::Void
        }).unwrap();
    }
}

/// The handle's or dot's middle: the disc that goes grey when `property` goes off.
fn disc(w: &ComponentInstance, property: &str) -> (f64, f64) {
    let draw = |on: bool| {
        w.set_property(property, Value::Bool(on)).unwrap();
        offscreen::draw(w.window(), WIDTH as f32, 760.0, 1.0).expect("a picture").rgba
    };
    let (on, off) = (draw(true), draw(false));
    draw(true);
    // The zoom slider's handle is a box with an edge; the pad's dot is still a disc
    let (from, to) = if property == "zoom-free" { (HANDLE_EDGE, HANDLE_OFF) } else { (HUE_CAMERA, TEXT_DISABLED) };
    let groups = recoloured(&on, &off, from, to);
    // The box's edge breaks into a few pieces at its rounded corners, which together are centred on it
    let found = if property == "zoom-free" { groups.concat() } else { groups.iter().find(|g| g.len() >= 100).expect("the disc").clone() };
    assert!(found.len() >= 50, "the handle");
    (found.iter().map(|p| p.0 as f64).sum::<f64>() / found.len() as f64 + 0.5, found.iter().map(|p| p.1 as f64).sum::<f64>() / found.len() as f64 + 0.5)
}

fn drag(w: &ComponentInstance, from: (f64, f64), to: (f64, f64)) {
    let (window, button) = (w.window(), PointerEventButton::Left);
    window.dispatch_event(WindowEvent::PointerMoved { position: point(from.0, from.1) });
    window.dispatch_event(WindowEvent::PointerPressed { position: point(from.0, from.1), button });
    window.dispatch_event(WindowEvent::PointerMoved { position: point(to.0, to.1) });
    window.dispatch_event(WindowEvent::PointerReleased { position: point(to.0, to.1), button });
}

fn double_click(w: &ComponentInstance, at: (f64, f64)) {
    let (window, position, button) = (w.window(), point(at.0, at.1), PointerEventButton::Left);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    for _ in 0..2 {
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }
}

fn key(w: &ComponentInstance, key: slint::platform::Key) {
    w.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    w.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
}

fn the_card_is_greyed_out_until_a_phone_is_paired() {
    let rig = rig(false);
    let w = window(&mut rig.borrow_mut().0);
    wire(&rig, &w, &["zoom-moved"]);
    sync(&rig, &w);
    w.set_property("active", Value::Bool(true)).unwrap();
    let handle = disc(&w, "zoom-free");
    let greyed = sync(&rig, &w);
    assert_eq!(w.get_property("active").unwrap(), Value::Bool(false));
    drag(&w, handle, (handle.0 + 60.0, handle.1));
    assert_eq!(rig.borrow().0.zoom(), 1.0, "nothing on it answers without a phone");
    {
        let (plugin, host) = &mut *rig.borrow_mut();
        plugin.on_event(host, &Event::PhonesChanged { count: 1 });
    }
    assert!(sync(&rig, &w) != greyed, "it comes on");
    assert_eq!(w.get_property("active").unwrap(), Value::Bool(true));
    drag(&w, handle, (handle.0 + 60.0, handle.1));
    assert!(rig.borrow().0.zoom() > 1.0, "and answers");
}

fn the_pan_pad_works_from_the_keyboard() {
    use slint::platform::Key;
    let rig = rig(true);
    tell(&rig, "zoom-moved", vec![serde_json::json!(2.0)]);
    let w = window(&mut rig.borrow_mut().0);
    wire(&rig, &w, &["pad-key"]);
    sync(&rig, &w);
    // Tab reaches it, like any control that takes keys
    for _ in 0..30 {
        key(&w, Key::Tab);
        key(&w, Key::RightArrow);
        if rig.borrow().0.pan().0 > 0.0 {
            break;
        }
    }
    assert!(rig.borrow().0.pan().0 > 0.0, "the pad never took the key");
    w.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    key(&w, Key::DownArrow);
    w.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
    assert_eq!(rig.borrow().0.pan().1, 0.1);
    key(&w, Key::Home);
    assert_eq!(rig.borrow().0.pan(), (0.0, 0.0));
}

fn a_double_click_resets_zoom_and_pan() {
    let rig = rig(true);
    tell(&rig, "zoom-moved", vec![serde_json::json!(4.2)]);
    let w = window(&mut rig.borrow_mut().0);
    wire(&rig, &w, &["zoom-moved", "pad-pointer", "pad-reset"]);
    sync(&rig, &w);
    let zoom = disc(&w, "zoom-free");
    tell(&rig, "pad-pointer", vec![serde_json::json!(0.6), serde_json::json!(0.0)]);
    sync(&rig, &w);
    // On the dot, off centre: only the double-click itself can bring it back
    let dot = disc(&w, "pan-enabled");
    double_click(&w, dot);
    assert_eq!((rig.borrow().0.pan().0, rig.borrow().0.zoom()), (0.0, 4.2));
    tell(&rig, "pad-pointer", vec![serde_json::json!(0.6), serde_json::json!(0.0)]);
    sync(&rig, &w);
    double_click(&w, zoom);
    assert_eq!((rig.borrow().0.zoom(), rig.borrow().0.pan().0), (1.0, 0.0));
}

fn auto_framing_visibly_disables_the_zoom_handle_and_value() {
    let mut plugin = TransformsPlugin::new();
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.set_config(&serde_json::json!({})).unwrap();
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    let w = window(&mut plugin);
    let moved = Rc::new(RefCell::new(0));
    let count = moved.clone();
    w.set_callback("zoom-moved", move |_| {
        *count.borrow_mut() += 1;
        Value::Void
    }).unwrap();
    let free = show(&w, &props(&host, &mut plugin));
    let handle = disc(&w, "zoom-free");

    plugin.on_event(&mut host, &Event::ViewTaken { owner: "Auto framing".into() });
    let taken = show(&w, &props(&host, &mut plugin));
    drag(&w, handle, (handle.0 + 60.0, handle.1));
    assert_eq!(*moved.borrow(), 0, "the slider is off while it's taken");
    // The handle is a 14 x 24 px box whose edge went from the camera's colour to the disabled one, and the value's text went grey
    let groups = recoloured(&free, &taken, HANDLE_EDGE, HANDLE_OFF);
    let all: Vec<(usize, usize)> = groups.concat();
    let grey_handle = (!all.is_empty()).then(|| {
        let (x0, x1) = (all.iter().map(|p| p.0).min().unwrap(), all.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (all.iter().map(|p| p.1).min().unwrap(), all.iter().map(|p| p.1).max().unwrap());
        (12..=16).contains(&(x1 - x0 + 1)) && (22..=26).contains(&(y1 - y0 + 1)) && all.len() >= 50
    });
    assert_eq!(grey_handle, Some(true), "no handle went grey: {:?}", groups.iter().map(|g| g.len()).collect::<Vec<_>>());
    assert!(recoloured(&free, &taken, INK, TEXT_DISABLED).iter().any(|g| g.len() >= 1), "the value's text went grey too");

    plugin.on_event(&mut host, &Event::ViewTaken { owner: String::new() });
    let released = show(&w, &props(&host, &mut plugin));
    assert!(released == free, "everything comes back when it lets go");
    drag(&w, handle, (handle.0 + 60.0, handle.1));
    assert!(*moved.borrow() > 0, "and the slider answers again");
}

fn pan_slider_clamps_resets_and_emits() {
    let rig = rig(true);
    tell(&rig, "zoom-moved", vec![serde_json::json!(2.0)]);
    let w = window(&mut rig.borrow_mut().0);
    wire(&rig, &w, &["pad-pointer", "pad-reset"]);
    sync(&rig, &w);
    let centre = disc(&w, "pan-enabled");
    // A little way along: in steps of 1/200 of the way, as the slider it replaces
    drag(&w, centre, (centre.0 + 11.0, centre.1));
    let x = rig.borrow().0.pan().0;
    assert!(x > 0.0 && (x * 200.0 - (x * 200.0).round()).abs() < 1e-3, "{x}");
    assert_eq!(rig.borrow().0.pan().1, 0.0);
    assert_eq!(rig.borrow().1.view.pan_x, x as f32, "what the view was told");
    // Past the end stops at the end
    sync(&rig, &w);
    drag(&w, disc(&w, "pan-enabled"), (centre.0 + 600.0, centre.1 - 600.0));
    assert_eq!(rig.borrow().0.pan(), (1.0, -1.0));
    sync(&rig, &w);
    double_click(&w, disc(&w, "pan-enabled"));
    assert_eq!(rig.borrow().0.pan(), (0.0, 0.0));
    // With nothing to pan around in, the pad's off
    tell(&rig, "zoom-moved", vec![serde_json::json!(1.0)]);
    sync(&rig, &w);
    assert_eq!(w.get_property("pan-enabled").unwrap(), Value::Bool(false));
    drag(&w, centre, (centre.0 + 30.0, centre.1));
    assert_eq!(rig.borrow().0.pan(), (0.0, 0.0));
}

fn point(x: f64, y: f64) -> slint::LogicalPosition {
    slint::LogicalPosition::new(x as f32, y as f32)
}

fn dragging_the_zoom_sticks_to_a_lens_but_keys_dont() {
    let rig = rig(true);
    let w = window(&mut rig.borrow_mut().0);
    for (name, value) in [("zoom-max", Value::Number(10.0)), ("active", Value::Bool(true)), ("zoom-marks", from_json(&serde_json::json!([3.7])))] {
        w.set_property(name, value).unwrap();
    }
    // The handle at 1x and at 10x say where the track runs
    w.set_property("zoom", Value::Number(1.0)).unwrap();
    let (x1, y) = disc(&w, "zoom-free");
    w.set_property("zoom", Value::Number(10.0)).unwrap();
    let (x10, _) = disc(&w, "zoom-free");
    let at = |zoom: f64| x1 + (zoom - 1.0) / 9.0 * (x10 - x1);
    let moved = Rc::new(RefCell::new(Vec::<f64>::new()));
    let (seen, to_plugin) = (moved.clone(), rig.clone());
    w.set_callback("zoom-moved", move |args| {
        if let Value::Number(n) = &args[0] {
            seen.borrow_mut().push((n * 1000.0).round() / 1000.0);
            tell(&to_plugin, "zoom-moved", vec![serde_json::json!(n)]);
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
    assert_eq!(rig.borrow().0.zoom(), 3.7, "the plugin's zoom goes with it");
    // Keys step off it freely: back through the card to the slider and press left (Left walks a segment row's options
    // the same way, where Tab forwards would meet it coming back)
    moved.borrow_mut().clear();
    for _ in 0..40 {
        window.dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Backtab.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Backtab.into() });
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
    assert!((1.9..2.1).contains(&rig.borrow().0.zoom()));
}

/// The slint platform lives on the thread that made it, so the window tests take turns on one.
#[test]
fn the_transforms_card_in_a_window() {
    auto_framing_visibly_disables_the_zoom_handle_and_value();
    dragging_the_zoom_sticks_to_a_lens_but_keys_dont();
    the_card_is_greyed_out_until_a_phone_is_paired();
    the_pan_pad_works_from_the_keyboard();
    a_double_click_resets_zoom_and_pan();
    pan_slider_clamps_resets_and_emits();
}
