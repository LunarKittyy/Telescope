// Tooltips and right-click on the kit's controls: where a tooltip draws and how big it is, when it may come up, and that
// every control a shortcut can target opens its menu on a right-click. Real events through the offscreen platform.
use i_slint_core::window::WindowInner;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const LONG: &str = "MJPEG: every frame a full JPEG. Several times Light's data, sharper in motion and heavier on the phone and the network than Light is.";

fn doc() -> String {
    format!(
        r#"
import {{ Button, IconButton, Switch, SwitchTile, Slider, Segmented, ChoiceField, NumberField, Shortcuts, WindowBox }} from "@ui-kit/ui-kit.slint";
export {{ Shortcuts, WindowBox }}
export component W inherits Window {{
    width: 420px;
    height: 330px;
    in-out property <float> at: 50;
    in-out property <int> moves;
    Button {{ x: 10px; y: 10px; width: 200px; text: "Tipped"; tip: "{LONG}"; shortcut: "t.button"; }}
    IconButton {{ x: 230px; y: 9px; label: "Icon"; tip: "An icon"; shortcut: "t.icon"; }}
    Switch {{ x: 10px; y: 60px; label: "Flag"; tip: "A flag"; shortcut: "t.switch"; }}
    SwitchTile {{ x: 80px; y: 50px; width: 200px; text: "Tile"; tip: "A tile"; shortcut: "t.tile"; }}
    Slider {{ x: 10px; y: 110px; width: 300px; label: "Level"; has-readout: false; value: 50; tip: "How loud"; shortcut: "t.slider"; moved(v) => {{ root.at = v; root.moves += 1; }} }}
    Segmented {{ x: 10px; y: 160px; width: 300px; margin: 0px; label: "Mode"; items: ["A", "B"]; tips: ["First", "Second"]; shortcut: "t.segmented"; }}
    ChoiceField {{ x: 10px; y: 210px; width: 300px; caption: "Pick"; items: ["a", "b"]; tip: "Which one"; shortcut: "t.choice"; }}
    NumberField {{ x: 10px; y: 270px; width: 120px; label: "Num"; value: "1"; shortcut: "t.number"; }}
}}"#
    )
}

fn open() -> (ComponentInstance, Rc<RefCell<Vec<String>>>) {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("tips.slint", &doc(), vec![], "W").expect("the document compiles").create().unwrap();
    let added = Rc::new(RefCell::new(Vec::<String>::new()));
    let a = added.clone();
    w.set_global_callback("Shortcuts", "add", move |args| {
        if let Some(Value::String(id)) = args.first() {
            a.borrow_mut().push(id.to_string());
        }
        Value::Void
    })
    .unwrap();
    w.set_global_callback("Shortcuts", "bindings-for", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    w.set_global_property("Shortcuts", "available", Value::Bool(true)).unwrap();
    w.set_global_property("WindowBox", "width", Value::Number(420.0)).unwrap();
    w.set_global_property("WindowBox", "height", Value::Number(330.0)).unwrap();
    w.show().unwrap();
    draw(&w);
    (w, added)
}

fn draw(w: &ComponentInstance) -> telescope_shell::offscreen::Picture {
    telescope_shell::offscreen::draw(w.window(), 420.0, 330.0, 1.0).unwrap()
}

fn at(x: f32, y: f32) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

fn wait(w: &ComponentInstance, ms: u64) {
    telescope_shell::offscreen::skip(Duration::from_millis(ms));
    draw(w);
}

fn popups(w: &ComponentInstance) -> usize {
    WindowInner::from_pub(w.window()).active_popups().len()
}

fn move_to(w: &ComponentInstance, p: LogicalPosition) {
    w.window().dispatch_event(WindowEvent::PointerMoved { position: p });
    telescope_shell::offscreen::pump();
}

fn press(w: &ComponentInstance, p: LogicalPosition, button: PointerEventButton) {
    w.window().dispatch_event(WindowEvent::PointerPressed { position: p, button });
    telescope_shell::offscreen::pump();
}

fn release(w: &ComponentInstance, p: LogicalPosition, button: PointerEventButton) {
    w.window().dispatch_event(WindowEvent::PointerReleased { position: p, button });
    telescope_shell::offscreen::pump();
}

const BUTTON: (f32, f32) = (110.0, 26.0);
const SLIDER: (f32, f32) = (160.0, 128.0);
const EMPTY: (f32, f32) = (380.0, 300.0);

#[test]
fn a_long_tooltip_sizes_to_its_text_stays_in_the_window_and_draws_over_what_is_below() {
    let (w, _) = open();
    move_to(&w, at(BUTTON.0, BUTTON.1));
    let before = draw(&w);
    wait(&w, 900);
    let after = draw(&w);
    assert_eq!(popups(&w), 1);
    // The box is what changed: its edges, where its three lines of text end, and the switch row it covers
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..after.height {
        for x in 0..after.width {
            let i = ((y * after.width + x) * 4) as usize;
            if before.rgba[i..i + 3] != after.rgba[i..i + 3] {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    assert!(x1 < after.width && y1 < after.height, "inside the window: {x0},{y0} to {x1},{y1}");
    assert!(y1 - y0 >= 55, "tall enough for its text, not cut to a line: {} px", y1 - y0);
    assert!(x1 - x0 <= 386 + 18, "wrapped inside the window, not run past it: {} px", x1 - x0);
    let at_switch = ((70 * after.width + 30) * 4) as usize;
    assert!(before.rgba[at_switch..at_switch + 3] != after.rgba[at_switch..at_switch + 3], "over the switch row below the button: {y0} to {y1}");
}

#[test]
fn a_tooltip_comes_up_from_a_resting_hover_on_each_kind_of_control() {
    let (w, _) = open();
    for (name, p) in [("button", (110.0, 26.0)), ("icon button", (245.0, 24.0)), ("switch", (28.0, 70.0)), ("tile", (130.0, 70.0)), ("slider", SLIDER),
                      ("segment", (235.0, 178.0)), ("dropdown", (160.0, 234.0))] {
        move_to(&w, at(EMPTY.0, EMPTY.1));
        wait(&w, 900);
        assert_eq!(popups(&w), 0, "{name}: nothing before");
        move_to(&w, at(p.0, p.1));
        wait(&w, 900);
        assert_eq!(popups(&w), 1, "{name}: up after a rest");
    }
}

#[test]
fn pressing_a_control_hides_its_tooltip_and_it_stays_down_until_the_pointer_leaves_and_comes_back() {
    let (w, _) = open();
    let p = at(BUTTON.0, BUTTON.1);
    move_to(&w, p);
    wait(&w, 900);
    assert_eq!(popups(&w), 1);
    press(&w, p, PointerEventButton::Left);
    assert_eq!(popups(&w), 0, "down with the press");
    release(&w, p, PointerEventButton::Left);
    move_to(&w, at(BUTTON.0 + 3.0, BUTTON.1));
    wait(&w, 900);
    assert_eq!(popups(&w), 0, "not straight after being used");
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    move_to(&w, p);
    wait(&w, 900);
    assert_eq!(popups(&w), 1, "back after leaving and coming back");
}

#[test]
fn a_press_before_the_tooltip_was_due_keeps_it_from_coming_up() {
    let (w, _) = open();
    let p = at(BUTTON.0, BUTTON.1);
    move_to(&w, p);
    wait(&w, 200);
    press(&w, p, PointerEventButton::Left);
    release(&w, p, PointerEventButton::Left);
    wait(&w, 900);
    assert_eq!(popups(&w), 0);
}

#[test]
fn dragging_a_slider_shows_no_tooltip_where_the_pointer_rests() {
    let (w, _) = open();
    move_to(&w, at(SLIDER.0, SLIDER.1));
    press(&w, at(SLIDER.0, SLIDER.1), PointerEventButton::Left);
    for x in [180.0, 220.0, 260.0, 280.0] {
        move_to(&w, at(x, SLIDER.1));
        wait(&w, 300);
        assert_eq!(popups(&w), 0, "while dragging at {x}");
    }
    wait(&w, 900);
    assert_eq!(popups(&w), 0, "resting with the button still down");
    release(&w, at(280.0, SLIDER.1), PointerEventButton::Left);
    wait(&w, 900);
    assert_eq!(popups(&w), 0, "and not once it's let go");
}

#[test]
fn the_wheel_on_a_control_counts_as_using_it() {
    let (w, _) = open();
    for (name, p) in [("button", BUTTON), ("slider", SLIDER)] {
        move_to(&w, at(EMPTY.0, EMPTY.1));
        wait(&w, 900);
        move_to(&w, at(p.0, p.1));
        wait(&w, 900);
        assert_eq!(popups(&w), 1, "{name}: up");
        w.window().dispatch_event(WindowEvent::PointerScrolled { position: at(p.0, p.1), delta_x: 0.0, delta_y: -40.0 });
        telescope_shell::offscreen::pump();
        wait(&w, 50);
        assert_eq!(popups(&w), 0, "{name}: down with the wheel");
        wait(&w, 900);
        assert_eq!(popups(&w), 0, "{name}: and it stays down");
    }
}

fn pick_first(w: &ComponentInstance) {
    for key in ["\u{F701}", "\n"] {
        w.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    telescope_shell::offscreen::pump();
    draw(w);
}

#[test]
fn a_right_click_on_a_slider_opens_its_menu_and_leaves_the_value_alone() {
    let (w, added) = open();
    let p = at(280.0, SLIDER.1);
    move_to(&w, p);
    press(&w, p, PointerEventButton::Right);
    release(&w, p, PointerEventButton::Right);
    draw(&w);
    assert_eq!(w.get_property("moves").unwrap(), Value::Number(0.0), "the value didn't move");
    assert_eq!(w.get_property("at").unwrap(), Value::Number(50.0));
    pick_first(&w);
    assert_eq!(*added.borrow(), ["t.slider"]);
    // A left click still sets it
    press(&w, p, PointerEventButton::Left);
    release(&w, p, PointerEventButton::Left);
    assert!(matches!(w.get_property("at"), Ok(Value::Number(v)) if v > 80.0), "{:?}", w.get_property("at"));
}

#[test]
fn a_right_click_opens_the_shortcut_menu_on_every_kind_of_control() {
    let (w, added) = open();
    let all = [
        ("t.button", (110.0, 26.0)), ("t.icon", (245.0, 24.0)), ("t.switch", (28.0, 70.0)), ("t.tile", (130.0, 70.0)), ("t.slider", SLIDER),
        ("t.segmented", (235.0, 178.0)), ("t.choice", (160.0, 234.0)), ("t.number", (70.0, 286.0)),
    ];
    for (id, p) in all {
        let p = at(p.0, p.1);
        move_to(&w, p);
        press(&w, p, PointerEventButton::Right);
        release(&w, p, PointerEventButton::Right);
        draw(&w);
        pick_first(&w);
        assert_eq!(added.borrow().last().map(String::as_str), Some(id), "{id}: the menu opened and offered its own action");
    }
    assert_eq!(added.borrow().len(), all.len());
}
