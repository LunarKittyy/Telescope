//! Connection's dialogs on a real (offscreen) Slint window: how long lists scroll and how popups behave.

use serde_json::json;
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance, Value};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use telescope_core::source::PairedPhone;
use telescope_link::plugin::phone_rows;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn window(doc: &str) -> ComponentInstance {
    offscreen::install();
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(|path: &Path| {
        let found = path.ends_with("plugin/connection.slint").then(|| Ok(include_str!("../../crates/link/ui/connection.slint").to_string()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc.to_string(), Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    w
}

fn length(w: &ComponentInstance, name: &str) -> f64 {
    match w.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name}: {other:?}"),
    }
}

fn phones(n: usize) -> Value {
    let phones: Vec<PairedPhone> = (0..n).map(|i| PairedPhone {
        id: format!("id-{i}"), name: format!("Phone {i}"), token: "tok".into(), ips: vec![format!("10.0.1.{i}")], active_ip: None,
        cert_sha256: "ab".repeat(32),
    }).collect();
    from_json(&json!(phone_rows(&phones, &[])))
}

fn a_long_phone_list_scrolls_instead_of_growing_off_the_screen() {
    let w = window(r#"import { PhoneList, PhoneRow } from "plugin/connection.slint";
export component W inherits Window {
    width: 420px;
    height: 600px;
    in property <[PhoneRow]> rows;
    out property <length> shown: list.height;
    out property <bool> scrolls: list.scrolls;
    list := PhoneList { x: 0; y: 0; width: 400px; rows-shown: rows; }
}"#);
    offscreen::draw(w.window(), 420.0, 600.0, 1.0);
    w.set_property("rows", phones(1)).unwrap();
    offscreen::draw(w.window(), 420.0, 600.0, 1.0);
    let short = length(&w, "shown");
    assert!(short > 0.0 && !bool_of(&w, "scrolls"));
    w.set_property("rows", phones(100)).unwrap();
    offscreen::draw(w.window(), 420.0, 600.0, 1.0);
    assert_eq!(length(&w, "shown"), 320.0);
    assert!(bool_of(&w, "scrolls"));
}

fn bool_of(w: &ComponentInstance, name: &str) -> bool {
    w.get_property(name).unwrap() == Value::Bool(true)
}

fn lots_of_phones_scroll_instead_of_growing_the_dialog() {
    let w = window(r#"import { UsbPicks, UsbPick } from "plugin/connection.slint";
export component W inherits Window {
    width: 420px;
    height: 400px;
    in property <[UsbPick]> picks;
    out property <length> shown: list.height;
    out property <bool> scrolls: list.scrolls;
    out property <length> wanted: list.wanted;
    list := UsbPicks { x: 0; y: 0; width: 400px; picks: root.picks; }
}"#);
    let picks = |n: usize| from_json(&json!((0..n).map(|i| json!({"serial": format!("s{i}"), "label": format!("Pair Phone {i}"), "enabled": true})).collect::<Vec<_>>()));
    w.set_property("picks", picks(20)).unwrap();
    offscreen::draw(w.window(), 420.0, 400.0, 1.0);
    let twenty = length(&w, "shown");
    assert!(bool_of(&w, "scrolls"));
    w.set_property("picks", picks(4)).unwrap();
    offscreen::draw(w.window(), 420.0, 400.0, 1.0);
    assert_eq!(length(&w, "shown"), twenty); // four rows show either way
    assert!(!bool_of(&w, "scrolls"), "{} of {twenty}", length(&w, "wanted"));
    w.set_property("picks", picks(1)).unwrap();
    offscreen::draw(w.window(), 420.0, 400.0, 1.0);
    assert!(length(&w, "shown") < twenty);
}

/// How far down the page something other than the window's red shows at x, in pixels.
fn bottom(picture: &offscreen::Picture, x: u32) -> u32 {
    (0..picture.height).rev().find(|y| {
        let at = ((y * picture.width + x) * 4) as usize;
        picture.rgba[at..at + 3] != [255, 0, 0]
    }).unwrap_or(0)
}

fn plugging_in_after_it_opened_grows_the_dialog_to_fit() {
    let w = window(r#"import { ConnectionCard, UsbPick } from "plugin/connection.slint";
export component W inherits Window {
    width: 600px;
    height: 700px;
    background: red;
    in property <[UsbPick]> picks;
    in property <bool> open;
    ConnectionCard { x: 0; y: 0; width: 500px; height: 10px; visible: true; add-open: open; picks: root.picks; usb-text: "Plug it in"; add-subtitle: "Open it"; }
}"#);
    offscreen::draw(w.window(), 600.0, 700.0, 1.0);
    w.set_property("open", Value::Bool(true)).unwrap();
    offscreen::pump();
    let before = bottom(&offscreen::draw(w.window(), 600.0, 700.0, 1.0).unwrap(), 300);
    assert!(before > 20);
    let picks = from_json(&json!([{"serial": "s1", "label": "Pair Pixel 7", "enabled": true}]));
    w.set_property("picks", picks).unwrap();
    offscreen::pump();
    offscreen::draw(w.window(), 600.0, 700.0, 1.0);
    let after = bottom(&offscreen::draw(w.window(), 600.0, 700.0, 1.0).unwrap(), 300);
    assert!(after >= before + 16, "{before} to {after}"); // the USB row was taller than its one line of text
}

fn the_choice_field_says_when_its_list_opens_and_closes() {
    let w = window(r#"import { ChoiceField } from "@ui-kit/ui-kit.slint";
export component W inherits Window {
    width: 400px;
    height: 300px;
    out property <[string]> log;
    callback changed-open(bool);
    ChoiceField { x: 20px; y: 20px; width: 200px; height: 34px; items: ["A", "B"]; changed open => { root.changed-open(self.open); } }
}"#);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    w.set_callback("changed-open", move |args| { s.borrow_mut().push(args[0] == Value::Bool(true)); Value::Void }).unwrap();
    let click = |x: f32, y: f32| click(&w, x, y);
    offscreen::draw(w.window(), 400.0, 300.0, 1.0);
    click(100.0, 37.0);
    assert_eq!(*seen.borrow(), [true]);
    click(350.0, 280.0); // outside
    assert_eq!(*seen.borrow(), [true, false]);
    click(100.0, 37.0);
    click(100.0, 72.0); // on the first item
    assert_eq!(*seen.borrow(), [true, false, true, false]);
}

fn settle(w: &ComponentInstance) {
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(3));
        offscreen::pump();
        offscreen::draw(w.window(), 600.0, 620.0, 1.0);
    }
}

fn click(w: &ComponentInstance, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    let button = slint::platform::PointerEventButton::Left;
    let window = w.window();
    window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
    window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
    window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
    settle(w);
}

fn your_phones_asks_before_it_renames_or_removes() {
    let w = window(r#"import { PhonePicker, PhoneRow } from "plugin/connection.slint";
export component W inherits Window {
    width: 600px;
    height: 620px;
    in property <[PhoneRow]> rows;
    in property <bool> open;
    callback said(string, string);
    PhonePicker {
        x: 0; y: 0; width: 300px; height: 40px;
        rows: root.rows;
        computer-name: "Desk";
        phones-open: open;
        rename(id, name) => { root.said(id, name); }
        rename-computer(name) => { root.said("computer", name); }
        remove(id) => { root.said("remove", id); }
    }
}"#);
    let said = Rc::new(RefCell::new(Vec::new()));
    let s = said.clone();
    w.set_callback("said", move |args| { s.borrow_mut().push(format!("{:?}", args)); Value::Void }).unwrap();
    w.set_property("rows", phones(2)).unwrap();
    w.set_property("open", Value::Bool(true)).unwrap();
    settle(&w);
    click(&w, 357.0, 185.0); // Phone 0's pencil
    click(&w, 395.0, 243.0); // the question is in the way
    click(&w, 362.0, 287.0); // OK
    assert_eq!(*said.borrow(), [format!("{:?}", [Value::String("id-0".into()), Value::String("Phone 0".into())])]);
    click(&w, 395.0, 243.0); // Phone 1's bin
    click(&w, 362.0, 273.0); // No
    assert_eq!(said.borrow().len(), 1);
    click(&w, 395.0, 243.0);
    click(&w, 258.0, 273.0); // Yes
    assert_eq!(said.borrow()[1], format!("{:?}", [Value::String("remove".into()), Value::String("id-1".into())]));
    click(&w, 353.0, 379.0); // Change name
    click(&w, 362.0, 287.0);
    assert_eq!(said.borrow()[2], format!("{:?}", [Value::String("computer".into()), Value::String("Desk".into())]));
}

fn the_virtual_camera_question_comes_with_the_startup_choice() {
    let w = window(r#"import { ConnectionCard } from "plugin/connection.slint";
export component W inherits Window {
    width: 600px;
    height: 620px;
    in property <bool> open;
    callback said(bool, bool);
    ConnectionCard { x: 0; y: 0; width: 500px; height: 10px; vcam-open: open; vcam-answered(ok, persist) => { root.said(ok, persist); } }
}"#);
    let said = Rc::new(RefCell::new(Vec::new()));
    let s = said.clone();
    w.set_callback("said", move |args| { s.borrow_mut().push((args[0].clone(), args[1].clone())); Value::Void }).unwrap();
    w.set_property("open", Value::Bool(true)).unwrap();
    settle(&w);
    let shown = offscreen::draw(w.window(), 600.0, 620.0, 1.0).unwrap();
    assert!(shown.height > 0);
    click(&w, 38.0, 111.0); // the startup switch, on to begin with
    click(&w, 431.0, 151.0); // OK
    assert_eq!(*said.borrow(), [(Value::Bool(true), Value::Bool(false))]);
}

// Slint's platform belongs to the thread that set it, so they take turns on one test
#[cfg(not(windows))] // clicks where Linux's fonts put the buttons
#[test]
fn connection_dialogs() {
    a_long_phone_list_scrolls_instead_of_growing_off_the_screen();
    lots_of_phones_scroll_instead_of_growing_the_dialog();
    plugging_in_after_it_opened_grows_the_dialog_to_fit();
    the_choice_field_says_when_its_list_opens_and_closes();
    your_phones_asks_before_it_renames_or_removes();
    the_virtual_camera_question_comes_with_the_startup_choice();
}
