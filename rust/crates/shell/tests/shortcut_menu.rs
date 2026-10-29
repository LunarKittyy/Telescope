// A control's right-click menu (ui-kit ShortcutMenu) in a real event loop: a right press opens it, its item says which
// action, and a left click still reaches the control under it.
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{Compiler, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn click(window: &slint::Window, x: f32, y: f32, button: PointerEventButton) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
    telescope_shell::offscreen::pump();
}

#[test]
fn a_right_click_offers_a_shortcut_and_a_left_click_still_presses() {
    telescope_shell::offscreen::install();
    let src = r#"import { Button, Shortcuts } from "@ui-kit/ui-kit.slint";
export { Shortcuts }
export component W inherits Window {
    width: 300px;
    height: 200px;
    callback pressed;
    Button { x: 20px; y: 20px; width: 120px; text: "Mute"; shortcut: "microphone.mute"; clicked => { root.pressed(); } }
}"#;
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(src.into(), Default::default()));
    let errors: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
    let w = result.component("W").unwrap_or_else(|| panic!("{}", errors.join("\n"))).create().unwrap();
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    let s = seen.clone();
    w.set_callback("pressed", move |_| { s.borrow_mut().push("pressed".into()); Value::Void }).unwrap();
    let s = seen.clone();
    w.set_global_callback("Shortcuts", "add", move |args| {
        if let Some(Value::String(a)) = args.first() {
            s.borrow_mut().push(format!("add {a}"));
        }
        Value::Void
    }).unwrap();
    w.set_global_callback("Shortcuts", "bindings-for", |_| {
        Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))
    }).unwrap();
    w.set_global_property("Shortcuts", "available", Value::Bool(true)).unwrap();
    w.show().unwrap();
    let window = w.window();
    telescope_shell::offscreen::draw(window, 300.0, 200.0, 1.0);
    click(window, 60.0, 37.0, PointerEventButton::Left);
    assert_eq!(*seen.borrow(), ["pressed"], "a left click goes through to the button");
    click(window, 60.0, 37.0, PointerEventButton::Right);
    let shown = telescope_shell::offscreen::draw(window, 300.0, 200.0, 1.0).unwrap();
    if let Some(out) = std::env::var_os("SHORTCUT_MENU_PNG") {
        shown.save_png(std::path::Path::new(&out)).unwrap();
    }
    // The menu opens where the press was; its first item is Add keyboard shortcut…
    click(window, 80.0, 50.0, PointerEventButton::Left);
    assert_eq!(*seen.borrow(), ["pressed", "add microphone.mute"]);
}
