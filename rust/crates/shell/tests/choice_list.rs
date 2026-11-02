// A dropdown's open list marks the item that's set, however it got set.
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { ChoiceField } from "@ui-kit/ui-kit.slint";
export component W inherits Window {
    width: 300px;
    height: 400px;
    in property <int> effect: 0;
    callback chosen(int);
    field := ChoiceField {
        x: 10px;
        y: 10px;
        width: 200px;
        compact: true;
        caption: "Effect";
        items: ["Off", "One", "Two", "Three", "Four"];
        current: effect;
        picked(i) => { root.chosen(i); }
    }
}"#;

const ROW: f32 = 26.0;
const LIST_TOP: f32 = 10.0 + 32.0 + 4.0 + 4.0;

fn open() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("list.slint", DOC, vec![], "W").expect("the document compiles").create().unwrap();
    w.show().unwrap();
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    w
}

fn click(w: &ComponentInstance, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    w.window().dispatch_event(WindowEvent::PointerMoved { position });
    w.window().dispatch_event(WindowEvent::PointerPressed { position, button });
    w.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    telescope_shell::offscreen::pump();
}

// Which row of the open list is filled, by looking at a pixel near each row's end
fn marked(w: &ComponentInstance) -> Vec<usize> {
    click(w, 60.0, 26.0);
    let picture = telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0).expect("a picture");
    if let Some(out) = std::env::var_os("CHOICE_LIST_PNG") {
        picture.save_png(std::path::Path::new(&out)).unwrap();
    }
    let at = |row: usize| {
        let y = (LIST_TOP + ROW * row as f32 + ROW / 2.0) as usize;
        let i = (y * picture.width as usize + 190) * 4;
        (picture.rgba[i], picture.rgba[i + 1], picture.rgba[i + 2])
    };
    let plain = at(4);
    let found = (0..4).filter(|r| at(*r) != plain).collect();
    w.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    w.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
    click(w, 280.0, 390.0);
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    found
}

#[test]
fn the_list_marks_what_a_pick_set() {
    let w = open();
    let weak = w.as_weak();
    w.set_callback("chosen", move |args| {
        weak.unwrap().set_property("effect", args[0].clone()).unwrap();
        Value::Void
    }).unwrap();
    assert_eq!(marked(&w), [0]);
    click(&w, 60.0, 26.0);
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    click(&w, 60.0, LIST_TOP + ROW * 2.5);
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    assert_eq!(marked(&w), [2]);
    let _ = w.hide();
}

#[test]
fn the_list_marks_what_a_saved_setting_or_a_shortcut_set() {
    let w = open();
    w.set_property("effect", Value::Number(3.0)).unwrap();
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    assert_eq!(marked(&w), [3]);
    // After a pick the plugin answered with another value
    w.set_property("effect", Value::Number(1.0)).unwrap();
    telescope_shell::offscreen::draw(w.window(), 300.0, 400.0, 1.0);
    assert_eq!(marked(&w), [1]);
    let _ = w.hide();
}
