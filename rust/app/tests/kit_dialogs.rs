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
    assert_eq!(dismissals(ui), 1.0, "the next press outside does close it");
}
