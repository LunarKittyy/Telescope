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

#[test]
fn a_right_click_on_a_slider_offers_a_shortcut_and_a_left_click_still_moves_it() {
    telescope_shell::offscreen::install();
    let src = r#"import { Slider, Shortcuts } from "@ui-kit/ui-kit.slint";
export { Shortcuts }
export component W inherits Window {
    width: 300px;
    height: 200px;
    in-out property <float> at;
    Slider { x: 20px; y: 20px; width: 200px; has-readout: false; shortcut: "microphone.gain"; moved(v) => { root.at = v; } }
}"#;
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(src.into(), Default::default()));
    let errors: Vec<String> = result.diagnostics().map(|d| d.to_string()).collect();
    let w = result.component("W").unwrap_or_else(|| panic!("{}", errors.join("\n"))).create().unwrap();
    let added = Rc::new(RefCell::new(Vec::<String>::new()));
    let s = added.clone();
    w.set_global_callback("Shortcuts", "add", move |args| {
        if let Some(Value::String(a)) = args.first() {
            s.borrow_mut().push(a.to_string());
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
    click(window, 120.0, 38.0, PointerEventButton::Left);
    assert!(matches!(w.get_property("at"), Ok(Value::Number(v)) if v > 30.0), "{:?}", w.get_property("at"));
    click(window, 120.0, 38.0, PointerEventButton::Right);
    telescope_shell::offscreen::draw(window, 300.0, 200.0, 1.0);
    click(window, 140.0, 51.0, PointerEventButton::Left);
    assert_eq!(*added.borrow(), ["microphone.gain"]);
}

const LATER: &str = r#"import { Button, Shortcuts } from "@ui-kit/ui-kit.slint";
export { Shortcuts }
export component W inherits Window {
    width: 300px;
    height: 200px;
    in property <bool> lens-there;
    VerticalLayout {
        alignment: start;
        Button { text: "Other"; }
        if lens-there: Button { text: "Wide"; shortcut: "camera.lens"; }
    }
}"#;

/// The menu's items in order by keyboard: `nth` Down presses from nothing picked, then Return.
fn pick_item(window: &slint::Window, nth: usize) {
    for key in std::iter::repeat_n("\u{F701}", nth + 1).chain(["\n"]) {
        window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    telescope_shell::offscreen::pump();
}

const TWO: &str = r#"import { Shortcuts, ShortcutMenu, Card } from "@ui-kit/ui-kit.slint";
export { Shortcuts }
export component W inherits Window {
    width: 400px;
    height: 200px;
    Rectangle {
        x: 0;
        y: 100px;
        width: 200px;
        height: 80px;
        // Over a control, as the pad has it
        ShortcutMenu {
            action: "transforms.pan_x transforms.pan_y";
            adds: [{ id: "transforms.pan_x", text: "Left" }, { id: "transforms.pan_y", text: "Up" }];
        }
    }
    Card {
        y: 0;
        height: 90px;
        title: "Microphone";
        has-action: true;
        action-text: "Mute";
        action-shortcut: "microphone.mute";
        has-switch: true;
        switch-label: "Phone mic";
        switch-shortcut: "microphone.on";
    }
}"#;

// The shared menu with bindings-for answering what the window's filter does for a space-separated action.
fn two_actions() -> (slint_interpreter::ComponentInstance, Rc<RefCell<Vec<String>>>) {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("two.slint", TWO, vec![], "W").expect("the document compiles").create().unwrap();
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    for callback in ["add", "edit"] {
        let s = seen.clone();
        w.set_global_callback("Shortcuts", callback, move |args| {
            if let Some(Value::String(a)) = args.first() {
                s.borrow_mut().push(format!("{callback} {a}"));
            }
            Value::Void
        }).unwrap();
    }
    w.set_global_callback("Shortcuts", "bindings-for", |args| {
        let shown = matches!(args.first(), Some(Value::String(a)) if a.split(' ').any(|a| a == "transforms.pan_y" || a == "microphone.mute"));
        let row = || Value::Struct([("id".to_string(), Value::String("one".into())), ("text".to_string(), Value::String("Ctrl+K".into()))].into_iter().collect());
        Value::Model(slint::ModelRc::new(slint::VecModel::from(if shown { vec![row()] } else { Vec::new() })))
    }).unwrap();
    w.set_global_property("Shortcuts", "available", Value::Bool(true)).unwrap();
    w.show().unwrap();
    telescope_shell::offscreen::draw(w.window(), 400.0, 200.0, 1.0);
    (w, seen)
}

// translation_scenes_audio's "Right-click on a control with two shortcuts": the Pan pad's menu
#[test]
fn a_control_with_two_actions_offers_both_and_their_saved_shortcuts() {
    for (nth, picked) in [(0, "add transforms.pan_x"), (1, "add transforms.pan_y"), (2, "edit one")] {
        let (w, seen) = two_actions();
        click(w.window(), 100.0, 140.0, PointerEventButton::Right);
        telescope_shell::offscreen::draw(w.window(), 400.0, 200.0, 1.0);
        pick_item(w.window(), nth);
        assert_eq!(*seen.borrow(), [picked]);
        let _ = w.hide();
    }
}

// The Microphone card's Mute sits in the shared header: its button takes the menu too
#[test]
fn a_card_header_action_offers_a_shortcut_and_its_saved_ones() {
    for (nth, picked) in [(0, "add microphone.mute"), (1, "edit one")] {
        let (w, seen) = two_actions();
        let mute = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&w, "Mute").next().expect("the Mute button");
        let (p, s) = (mute.absolute_position(), mute.size());
        click(w.window(), p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Right);
        telescope_shell::offscreen::draw(w.window(), 400.0, 200.0, 1.0);
        pick_item(w.window(), nth);
        assert_eq!(*seen.borrow(), [picked]);
        let _ = w.hide();
    }
}

// The same for the on/off switch in a card's header (microphone.on, chroma_key.on and the others)
#[test]
fn a_card_header_switch_offers_a_shortcut() {
    let (w, seen) = two_actions();
    let switch = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&w, "Phone mic").next().expect("the switch");
    let (p, s) = (switch.absolute_position(), switch.size());
    click(w.window(), p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Right);
    telescope_shell::offscreen::draw(w.window(), 400.0, 200.0, 1.0);
    pick_item(w.window(), 0);
    assert_eq!(*seen.borrow(), ["add microphone.on"]);
    let _ = w.hide();
}

// test_shortcuts_plugin.py::test_controls_that_come_later_still_get_the_menu
#[test]
fn controls_that_come_later_still_get_the_menu() {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("later.slint", LATER, vec![], "W").expect("the document compiles").create().unwrap();
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    let s = seen.clone();
    w.set_global_callback("Shortcuts", "add", move |args| {
        if let Some(Value::String(a)) = args.first() {
            s.borrow_mut().push(a.to_string());
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
    let right_click = |name: &str| {
        let e = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&w, name).next().unwrap_or_else(|| panic!("no {name}"));
        let (p, s) = (e.absolute_position(), e.size());
        click(window, p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Right);
        telescope_shell::offscreen::draw(window, 300.0, 200.0, 1.0);
    };
    // The menu's first item, by keyboard: where it opens isn't this test's business
    let first_item = || {
        for key in ["\u{F701}", "\n"] {
            window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        }
        telescope_shell::offscreen::pump();
    };
    w.set_property("lens-there", Value::Bool(true)).unwrap();
    telescope_shell::offscreen::draw(window, 300.0, 200.0, 1.0);
    right_click("Other");
    first_item();
    assert!(seen.borrow().is_empty(), "an unrelated button has no menu: {:?}", seen.borrow());
    right_click("Wide");
    first_item();
    assert_eq!(*seen.borrow(), ["camera.lens"]);
}
