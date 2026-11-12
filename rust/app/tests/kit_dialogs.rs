// A press beside a kit dialog is its Cancel; one that starts inside it never is.

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::{compile, input, offscreen};

// A dialog in a pop-up that covers the window, as the plugins place theirs
const DIALOG: &str = r#"
import { DialogFrame, Slider } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in-out property <int> dismissals;
    in-out property <int> level: 5;
    public function open() { pop.show(); }
    pop := PopupWindow {
        x: 0px;
        y: 0px;
        width: 600px;
        height: 500px;
        close-policy: no-auto-close;
        DialogFrame {
            x: 100px;
            y: 100px;
            width: 300px;
            height: 200px;
            title: "Rename";
            dismissable: true;
            dismissed => { root.dismissals += 1; }
            Slider { width: 260px; minimum: 0; maximum: 10; value: level; moved(v) => { root.level = v; } }
        }
    }
}
"#;

fn window() -> ComponentInstance {
    offscreen::install();
    let ui = compile::compile("test.slint", DIALOG, Vec::new(), "T").map(|d| d.create().unwrap()).unwrap_or_else(|e| panic!("{e}"));
    ui.window().set_size(slint::LogicalSize::new(600.0, 500.0));
    ui.show().unwrap();
    ui.invoke("open", &[]).unwrap();
    settle(&ui);
    ui
}

fn settle(ui: &ComponentInstance) {
    offscreen::draw(ui.window(), 600.0, 500.0, 1.0);
}

fn dismissals(ui: &ComponentInstance) -> f32 {
    match ui.get_property("dismissals").unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{v:?}"),
    }
}

fn press(ui: &ComponentInstance, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    input::pressed_at(ui, position);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
}

fn release(ui: &ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerReleased { position: LogicalPosition::new(x, y), button: PointerEventButton::Left });
}

fn click(ui: &ComponentInstance, x: f32, y: f32) {
    press(ui, x, y);
    release(ui, x, y);
    settle(ui);
}

#[test]
fn a_press_beside_a_dialog_is_its_cancel_and_one_inside_is_not() {
    let ui = &window();
    click(ui, 200.0, 280.0);
    assert_eq!(dismissals(ui), 0.0, "a click on the dialog itself");
    click(ui, 250.0, 150.0);
    assert_eq!(dismissals(ui), 0.0, "and on its title");
    click(ui, 20.0, 20.0);
    // It eases out first, then says so
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(300));
    assert_eq!(dismissals(ui), 1.0, "a press outside it");
}

#[test]
fn a_drag_that_starts_in_a_dialog_and_ends_outside_leaves_it_open() {
    let ui = &window();
    press(ui, 200.0, 280.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(550.0, 450.0) });
    release(ui, 550.0, 450.0);
    settle(ui);
    assert_eq!(dismissals(ui), 0.0, "pressed inside, released outside");
    click(ui, 550.0, 450.0);
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(300));
    assert_eq!(dismissals(ui), 1.0, "the next press outside does close it");
}

// A dialog that is fading away takes no more presses or keys, so the first decision (a press beside it is Cancel) stands
const SAVING: &str = r#"
import { DialogFrame, Button } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in-out property <int> saves;
    in-out property <int> lefts;
    in-out property <int> dismissals;
    public function open() { pop.show(); }
    pop := PopupWindow {
        x: 0px;
        y: 0px;
        width: 600px;
        height: 500px;
        close-policy: no-auto-close;
        frame := DialogFrame {
            x: 100px;
            y: 100px;
            width: 300px;
            height: 200px;
            title: "Rename";
            dismissable: true;
            dismissed => { root.dismissals += 1; }
            left => { root.lefts += 1; }
            Button { text: "Save"; clicked => { root.saves += 1; } }
        }
    }
}
"#;

fn count(ui: &ComponentInstance, name: &str) -> f32 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_dialog_that_is_fading_out_takes_no_press_or_key_and_keeps_its_first_decision() {
    offscreen::install();
    // The clock stands still, so the fade is under way for the whole sweep of clicks
    let _still = offscreen::freeze_clock();
    let ui = compile::compile("saving.slint", SAVING, Vec::new(), "T").map(|d| d.create().unwrap()).unwrap_or_else(|e| panic!("{e}"));
    ui.window().set_size(slint::LogicalSize::new(600.0, 500.0));
    ui.show().unwrap();
    ui.invoke("open", &[]).unwrap();
    settle(&ui);
    let keys = |ui: &ComponentInstance, list: &[slint::SharedString]| {
        for text in list {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.clone() });
        }
    };
    // Tab to Save: Enter on it saves while the dialog is up
    keys(&ui, &[slint::platform::Key::Tab.into(), "\n".into()]);
    assert_eq!(count(&ui, "saves"), 1.0, "Enter on Save saves in a dialog that is up");
    ui.set_property("saves", Value::Number(0.0)).unwrap();
    // Press beside the dialog (Cancel); while it fades Enter and every click on the dialog do nothing
    click(&ui, 20.0, 20.0);
    keys(&ui, &["\n".into(), " ".into()]);
    for y in (110..300).step_by(8) {
        for x in (110..400).step_by(8) {
            click(&ui, x as f32, y as f32);
        }
    }
    assert_eq!(count(&ui, "saves"), 0.0, "nothing saved while it was leaving");
    offscreen::skip(std::time::Duration::from_millis(300));
    settle(&ui);
    assert_eq!((count(&ui, "dismissals"), count(&ui, "lefts"), count(&ui, "saves")), (1.0, 1.0, 0.0), "it cancelled");
}

// A dialog that keeps itself open ignores a press beside it (it must not ease out and come back), and `dismiss()` still closes it
const KEPT: &str = r#"
import { DialogFrame } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in-out property <int> dismissals;
    in-out property <int> lefts;
    public function open() { pop.show(); }
    in-out property <bool> closing;
    pop := PopupWindow {
        x: 0px;
        y: 0px;
        width: 600px;
        height: 500px;
        close-policy: no-auto-close;
        Rectangle {
            width: 0px;
            height: 0px;
            property <bool> go: root.closing;
            changed go => { if (self.go) { frame.dismiss(); } }
        }
        frame := DialogFrame {
            x: 100px;
            y: 100px;
            width: 300px;
            height: 200px;
            title: "Pair";
            dismissable: true;
            keep-open: true;
            dismissed => { root.dismissals += 1; }
            left => { root.lefts += 1; }
        }
    }
}
"#;

#[test]
fn a_dialog_that_keeps_itself_open_ignores_a_press_beside_it_but_not_dismiss() {
    offscreen::install();
    let ui = compile::compile("test.slint", KEPT, Vec::new(), "T").map(|d| d.create().unwrap()).unwrap_or_else(|e| panic!("{e}"));
    ui.window().set_size(slint::LogicalSize::new(600.0, 500.0));
    ui.show().unwrap();
    ui.invoke("open", &[]).unwrap();
    settle(&ui);
    click(&ui, 20.0, 20.0);
    offscreen::skip(std::time::Duration::from_millis(300));
    assert_eq!((count(&ui, "dismissals"), count(&ui, "lefts")), (0.0, 0.0), "a press beside it leaves it up and does not start easing out");
    ui.set_property("closing", Value::Bool(true)).unwrap();
    settle(&ui);
    offscreen::skip(std::time::Duration::from_millis(300));
    assert_eq!((count(&ui, "dismissals"), count(&ui, "lefts")), (1.0, 1.0), "dismiss closes it");
}
