// Tooltips and right-click on the kit's controls: where a tooltip draws and how big it is, when it may come up (after a rest, at once
// after another one, from the keyboard focus), what it is made of, and that every control a shortcut can target opens its menu on
// a right-click. Real events through the offscreen platform.
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
import {{ Button, IconButton, Switch, SwitchTile, Slider, Segmented, ChoiceField, NumberField, Shortcuts, WindowBox, Tips, TipSpot, TipLayer, TipBody, KeyFocus, DialogFrame }} from "@ui-kit/ui-kit.slint";
export {{ Shortcuts, WindowBox, Tips, TipSpot, KeyFocus }}
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
    Button {{ x: 150px; y: 296px; width: 100px; text: "Low"; tip: "Down here"; tip-detail: "It has to flip above"; }}
    Button {{ x: 330px; y: 10px; width: 80px; text: "Keys"; tip: "With keys"; tip-keys: ["Ctrl", "K"]; }}
    Button {{ x: 330px; y: 60px; width: 80px; text: "Short"; tip: "Has a shortcut"; shortcut: "t.keys"; }}
    in-out property <bool> gone;
    if !root.gone: Button {{ x: 330px; y: 110px; width: 80px; text: "Doomed"; tip: "Doomed"; }}
    TipLayer {{ }}
}}
export component Popped inherits Window {{
    width: 420px;
    height: 330px;
    in-out property <bool> opened: true;
    Button {{ x: 10px; y: 280px; width: 100px; text: "Under"; tip: "Under the pop-up"; }}
    TipLayer {{ }}
    pop := PopupWindow {{
        x: 0px;
        y: 0px;
        width: 420px;
        height: 330px;
        close-policy: no-auto-close;
        DialogFrame {{
            x: 40px;
            y: 40px;
            width: 300px;
            height: 160px;
            title: "Dialog";
            Button {{ text: "In the dialog"; tip: "From inside the dialog"; tip-detail: "A pop-up covers the window's own layer."; }}
        }}
    }}
    init => {{ pop.show(); }}
}}
export component Parts inherits Window {{
    width: 420px;
    height: 330px;
    out property <length> name-h: only-name.preferred-height;
    out property <length> name-w: only-name.preferred-width;
    out property <length> detail-h: with-detail.preferred-height;
    out property <length> detail-w: with-detail.preferred-width;
    out property <length> keys-w: with-keys.preferred-width;
    out property <length> keys-h: with-keys.preferred-height;
    out property <length> long-h: long-detail.preferred-height;
    out property <length> long-w: long-detail.preferred-width;
    out property <length> hue-w: tinted.preferred-width;
    out property <length> hue-h: tinted.preferred-height;
    only-name := TipBody {{ x: 10px; y: 10px; width: self.preferred-width; height: self.preferred-height; says: "Exposure"; }}
    with-detail := TipBody {{ x: 10px; y: 60px; width: self.preferred-width; height: self.preferred-height; says: "Exposure"; detail: "Holds the picture's brightness steady."; }}
    with-keys := TipBody {{ x: 10px; y: 120px; width: self.preferred-width; height: self.preferred-height; says: "Exposure"; keys: ["Ctrl", "E"]; }}
    tinted := TipBody {{ x: 200px; y: 10px; width: self.preferred-width; height: self.preferred-height; says: "Exposure"; hue: #ff0000; }}
    long-detail := TipBody {{ x: 10px; y: 180px; width: self.preferred-width; height: self.preferred-height; says: "Exposure"; detail: "{LONG}"; }}
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
    w.set_global_callback("Shortcuts", "key-chips", |args| {
        let keys: Vec<Value> = match args.first() {
            Some(Value::String(id)) if id.as_str() == "t.keys" => vec![Value::String("Ctrl".into()), Value::String("K".into())],
            _ => Vec::new(),
        };
        Value::Model(slint::ModelRc::new(slint::VecModel::from(keys)))
    })
    .unwrap();
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

fn global_len(w: &ComponentInstance, name: &str) -> usize {
    match w.get_global_property("Tips", name) {
        Ok(Value::Model(m)) => slint::Model::row_count(&m),
        other => panic!("Tips.{name}: {other:?}"),
    }
}

/// Whether the window has a tip up, 1 or 0 (the old count of popups)
fn popups(w: &ComponentInstance) -> usize {
    match w.get_global_property("Tips", "shown") {
        Ok(Value::Bool(b)) => b as usize,
        other => panic!("Tips.shown: {other:?}"),
    }
}

fn says(w: &ComponentInstance) -> String {
    match w.get_global_property("Tips", "says") {
        Ok(Value::String(s)) => s.to_string(),
        other => panic!("Tips.says: {other:?}"),
    }
}

fn move_to(w: &ComponentInstance, p: LogicalPosition) {
    // The app's input watch (input.rs) tells Tips where the pointer is on every move, which a test window has no winit event for
    w.invoke_global("Tips", "moved", &[Value::Number(p.x as f64), Value::Number(p.y as f64)]).unwrap();
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
    // The menu eases out after a pick
    wait(w, 300);
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

fn number(v: Result<Value, slint_interpreter::GetPropertyError>) -> f32 {
    match v {
        Ok(Value::Number(n)) => n as f32,
        other => panic!("not a number: {other:?}"),
    }
}

/// TipSpot's pure functions: lengths as numbers, and `top`'s above flag as 0 or 1
fn spot(w: &ComponentInstance, name: &str, args: &[f32]) -> Value {
    let flag_at = (name == "top").then_some(3);
    let args: Vec<Value> = args.iter().enumerate().map(|(i, a)| if Some(i) == flag_at { Value::Bool(*a != 0.0) } else { Value::Number(*a as f64) }).collect();
    w.invoke_global("TipSpot", name, &args).unwrap()
}

/// The box a tip drew between two pictures: where pixels moved by more than a shadow can (a soft shadow darkens a card by a few levels)
fn box_between(before: &telescope_shell::offscreen::Picture, after: &telescope_shell::offscreen::Picture) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..after.height {
        for x in 0..after.width {
            let i = ((y * after.width + x) * 4) as usize;
            let delta = (0..3).map(|c| before.rgba[i + c].abs_diff(after.rgba[i + c])).max().unwrap();
            if delta > 30 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn a_tip_goes_below_its_control_flips_above_near_the_bottom_and_stays_inside_the_window() {
    let (w, _) = open();
    // Below when it fits, above when it doesn't and there is more room there
    assert_eq!(spot(&w, "above", &[10.0, 32.0, 60.0, 330.0]), Value::Bool(false));
    assert_eq!(spot(&w, "above", &[296.0, 32.0, 60.0, 330.0]), Value::Bool(true));
    // Too tall for either side: it stays below, and is held to the window's top margin
    assert_eq!(spot(&w, "above", &[5.0, 32.0, 400.0, 330.0]), Value::Bool(false));
    assert_eq!(number(Ok(spot(&w, "top", &[5.0, 32.0, 400.0, 0.0, 330.0]))), 8.0);
    // The gap is 4px: below it starts under the control, above it ends over it
    assert_eq!(number(Ok(spot(&w, "top", &[10.0, 32.0, 60.0, 0.0, 330.0]))), 46.0);
    assert_eq!(number(Ok(spot(&w, "top", &[296.0, 32.0, 60.0, 1.0, 330.0]))), 232.0);
    // Centred on the control, pushed in from the sides
    assert_eq!(number(Ok(spot(&w, "left", &[100.0, 40.0, 100.0, 420.0]))), 70.0);
    assert_eq!(number(Ok(spot(&w, "left", &[0.0, 20.0, 262.0, 420.0]))), 8.0);
    assert_eq!(number(Ok(spot(&w, "left", &[380.0, 32.0, 262.0, 420.0]))), 150.0);

    // And drawn there: the low button's tip sits over it, in the window; the one by the right edge stops a margin short of it
    let before = draw(&w);
    move_to(&w, at(200.0, 312.0));
    wait(&w, 900);
    let (x0, y0, x1, y1) = box_between(&before, &draw(&w));
    assert!(y1 < 296 && y0 > 8 && x0 >= 8 && x1 < 420, "above the 296px button, inside the window: {x0},{y0} to {x1},{y1}");
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 900);
    let before = draw(&w);
    move_to(&w, at(370.0, 26.0));
    wait(&w, 900);
    let (x0, y0, x1, y1) = box_between(&before, &draw(&w));
    assert!(y0 >= 42 && x1 <= 412 && x0 >= 8, "below the top right button, a margin from the edge: {x0},{y0} to {x1},{y1}");
}

#[test]
fn the_first_tip_waits_for_a_rest_and_the_next_one_comes_up_at_once() {
    let (w, _) = open();
    move_to(&w, at(BUTTON.0, BUTTON.1));
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "not yet, 300 ms in");
    wait(&w, 300);
    assert_eq!(popups(&w), 1, "up after about half a second");
    assert_eq!(says(&w), LONG);
    // Straight onto a neighbour: its own tip, with no wait
    move_to(&w, at(245.0, 24.0));
    wait(&w, 50);
    assert_eq!(popups(&w), 0, "a warm tip still waits for the pointer to settle");
    wait(&w, 100);
    assert_eq!((popups(&w), says(&w).as_str()), (1, "An icon"));
    // Off to nothing: down at once, and for a second the next control's tip is at once too
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    assert_eq!(popups(&w), 0, "down when the pointer leaves");
    move_to(&w, at(28.0, 70.0));
    wait(&w, 150);
    assert_eq!((popups(&w), says(&w).as_str()), (1, "A flag"), "warm for a second after the last one went down");
    // Left alone for longer, the wait is back
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 1300);
    move_to(&w, at(28.0, 70.0));
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "cold again: the wait is back");
    wait(&w, 300);
    assert_eq!(popups(&w), 1);
}

/// Tips.cool(), the press that the window's input watch reports for a press anywhere
fn cool(w: &ComponentInstance) {
    w.invoke_global("Tips", "cool", &[]).unwrap();
}

fn warm(w: &ComponentInstance) {
    // One tip up and down again, so the next is warm
    move_to(w, at(EMPTY.0, EMPTY.1));
    wait(w, 1500);
    move_to(w, at(BUTTON.0, BUTTON.1));
    wait(w, 700);
    assert_eq!(popups(w), 1);
    move_to(w, at(EMPTY.0, EMPTY.1));
    wait(w, 100);
    assert_eq!(popups(w), 0);
}

#[test]
fn a_click_ends_the_warm_time_so_the_next_tip_waits_the_whole_delay() {
    let (w, _) = open();
    warm(&w);
    // Warm: a tip on a rest of about 100 ms
    move_to(&w, at(28.0, 70.0));
    wait(&w, 150);
    assert_eq!(popups(&w), 1, "warm before the click");
    // Click that control, then go to another one
    press(&w, at(28.0, 70.0), PointerEventButton::Left);
    release(&w, at(28.0, 70.0), PointerEventButton::Left);
    wait(&w, 50);
    move_to(&w, at(245.0, 24.0));
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "the click made it cold: no tip after 300 ms");
    wait(&w, 300);
    assert_eq!((popups(&w), says(&w).as_str()), (1, "An icon"), "up after the whole delay");
    // The wheel does the same, and so does a press where there is no control (the window tells Tips)
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    move_to(&w, at(28.0, 70.0));
    wait(&w, 150);
    assert_eq!(popups(&w), 1, "warm again once a tip has shown");
    w.window().dispatch_event(WindowEvent::PointerScrolled { position: at(28.0, 70.0), delta_x: 0.0, delta_y: -40.0 });
    telescope_shell::offscreen::pump();
    move_to(&w, at(245.0, 24.0));
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "cold after the wheel");
    wait(&w, 300);
    assert_eq!(popups(&w), 1);
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    move_to(&w, at(130.0, 70.0));
    wait(&w, 150);
    assert_eq!(popups(&w), 1, "warm once more");
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    cool(&w);
    move_to(&w, at(130.0, 70.0));
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "cold after a press anywhere");
    wait(&w, 300);
    assert_eq!(popups(&w), 1);
}

#[test]
fn the_wait_counts_from_where_the_pointer_rests_not_from_where_it_came_in() {
    let (w, _) = open();
    // Over the slider for two seconds, moving: never a tip
    for i in 0..20 {
        move_to(&w, at(40.0 + i as f32 * 12.0, SLIDER.1));
        wait(&w, 100);
        assert_eq!(popups(&w), 0, "still moving at step {i}");
    }
    // Settled: the tip comes a delay after the last move, not before
    wait(&w, 300);
    assert_eq!(popups(&w), 0, "300 ms of rest");
    wait(&w, 300);
    assert_eq!(popups(&w), 1, "600 ms of rest");
}

#[test]
fn a_pixel_or_two_of_jitter_is_still_a_rest_but_a_slow_creep_is_not() {
    let (w, _) = open();
    move_to(&w, at(SLIDER.0, SLIDER.1));
    for i in 0..4 {
        wait(&w, 100);
        move_to(&w, at(SLIDER.0 + if i % 2 == 0 { 2.0 } else { -2.0 }, SLIDER.1 + 1.0));
    }
    wait(&w, 150);
    assert_eq!(popups(&w), 1, "jitter inside 4 px kept the rest going");
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 1500);
    // 1.5 px every 100 ms leaves the 4 px circle every third step: the wait never completes
    for i in 0..30 {
        move_to(&w, at(SLIDER.0 + i as f32 * 1.5, SLIDER.1));
        wait(&w, 100);
        assert_eq!(popups(&w), 0, "creeping, step {i}");
    }
}

#[test]
fn passing_over_controls_while_warm_does_not_flash_a_chain_of_tips() {
    let (w, _) = open();
    warm(&w);
    for p in [(28.0, 70.0), (130.0, 70.0), (245.0, 24.0), (160.0, 178.0), (160.0, 234.0)] {
        move_to(&w, at(p.0, p.1));
        wait(&w, 60);
        assert_eq!(popups(&w), 0, "passing {p:?}");
    }
    wait(&w, 100);
    assert_eq!(popups(&w), 1, "settled on the last: its tip, after a short rest");
    assert_eq!(says(&w), "Which one");
}

#[test]
fn after_a_value_changed_the_next_tip_waits_two_and_a_half_times_as_long_for_a_few_seconds() {
    let (w, _) = open();
    w.invoke_global("Tips", "changed", &[]).unwrap();
    wait(&w, 1000);
    move_to(&w, at(BUTTON.0, BUTTON.1));
    wait(&w, 900);
    assert_eq!(popups(&w), 0, "900 ms in, past the usual wait");
    wait(&w, 450);
    assert_eq!(popups(&w), 1, "up after 2.5 times 500 ms");
    // Warm waits are stretched too
    w.invoke_global("Tips", "changed", &[]).unwrap();
    move_to(&w, at(245.0, 24.0));
    wait(&w, 150);
    assert_eq!(popups(&w), 0, "a warm tip waits 250 ms just after a change");
    wait(&w, 150);
    assert_eq!(popups(&w), 1);
    // A few seconds on it is back to normal
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 3000);
    move_to(&w, at(28.0, 70.0));
    wait(&w, 600);
    assert_eq!(popups(&w), 1, "quiet is over");
}

#[test]
fn a_tip_stays_up_while_the_pointer_rests_and_goes_when_its_control_does() {
    let (w, _) = open();
    move_to(&w, at(370.0, 126.0));
    wait(&w, 900);
    assert_eq!((popups(&w), says(&w).as_str()), (1, "Doomed"));
    wait(&w, 5000);
    assert_eq!(popups(&w), 1, "still there while its control is");
    w.set_property("gone", Value::Bool(true)).unwrap();
    wait(&w, 100);
    wait(&w, 900);
    assert_eq!(popups(&w), 0, "a control that went away takes its tip with it");
}

#[test]
fn the_keyboard_focus_shows_the_tip_at_once() {
    let (w, _) = open();
    w.set_global_property("KeyFocus", "keyboard", Value::Bool(true)).unwrap();
    for name in [LONG, "An icon"] {
        let key = slint::platform::Key::Tab;
        w.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        w.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        telescope_shell::offscreen::pump();
        wait(&w, 50);
        assert_eq!((popups(&w), says(&w).as_str()), (1, name), "up with the focus, no wait");
    }
}

#[test]
fn a_tip_carries_its_detail_and_the_keys_it_was_given_or_its_shortcut_has() {
    let (w, _) = open();
    move_to(&w, at(200.0, 312.0));
    wait(&w, 900);
    assert_eq!(w.get_global_property("Tips", "detail").unwrap(), Value::String("It has to flip above".into()));
    assert_eq!(global_len(&w, "keys"), 0, "no keys, no caps");
    move_to(&w, at(370.0, 26.0));
    wait(&w, 100);
    assert_eq!(global_len(&w, "keys"), 2, "keys given");
    move_to(&w, at(EMPTY.0, EMPTY.1));
    wait(&w, 100);
    move_to(&w, at(370.0, 76.0));
    wait(&w, 100);
    assert_eq!((says(&w).as_str(), global_len(&w, "keys")), ("Has a shortcut", 2), "keys of its saved shortcut");
}

#[test]
fn name_detail_keys_and_a_tint_each_show_only_when_set() {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("tips.slint", &doc(), vec![], "Parts").expect("the document compiles").create().unwrap();
    w.show().unwrap();
    let picture = telescope_shell::offscreen::draw(w.window(), 420.0, 330.0, 1.0).unwrap();
    let get = |name: &str| number(w.get_property(name));
    assert!(get("detail-h") > get("name-h") + 10.0, "a detail line adds a line under the name: {} against {}", get("detail-h"), get("name-h"));
    assert!(get("detail-w") > get("name-w"), "and may widen it");
    assert!(get("keys-w") > get("name-w") + 20.0, "keys add caps on the right of the name");
    assert_eq!(get("keys-h"), get("name-h"), "on the name's line");
    assert!(get("long-h") > get("detail-h") + 15.0, "a long detail wraps onto more lines");
    assert!(get("long-w") <= 262.0, "and wraps at about 36 characters, not the window's width: {} px", get("long-w"));
    assert!(get("hue-w") > get("name-w"), "a tint adds its dot");
    assert_eq!(get("hue-h"), get("name-h"));
    // Pixels: the dot is red where one was asked for, nowhere else; a cap is darker than the tip's surface
    let count = |x0: u32, y0: u32, w: u32, h: u32, f: &dyn Fn([u8; 3]) -> bool| {
        let mut n = 0;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let i = ((y * picture.width + x) * 4) as usize;
                if f([picture.rgba[i], picture.rgba[i + 1], picture.rgba[i + 2]]) {
                    n += 1;
                }
            }
        }
        n
    };
    let red = |p: [u8; 3]| p[0] > 180 && p[1] < 80 && p[2] < 80;
    let dark = |p: [u8; 3]| p[0] < 0x2c && p[0] == p[1] && p[1] == p[2];
    assert!(count(200, 10, get("hue-w") as u32, get("hue-h") as u32, &red) >= 20, "the tint's dot");
    assert_eq!(count(10, 10, get("name-w") as u32, get("name-h") as u32, &red), 0, "no dot without a tint");
    assert!(count(12, 122, get("keys-w") as u32 - 4, get("keys-h") as u32 - 4, &dark) >= 60, "key caps");
    assert_eq!(count(12, 12, get("name-w") as u32 - 4, get("name-h") as u32 - 4, &dark), 0, "no caps without keys");
}

#[test]
fn a_tip_of_a_control_in_a_dialog_draws_over_the_dialog() {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("tips.slint", &doc(), vec![], "Popped").expect("the document compiles").create().unwrap();
    w.set_global_property("WindowBox", "width", Value::Number(420.0)).unwrap();
    w.set_global_property("WindowBox", "height", Value::Number(330.0)).unwrap();
    w.show().unwrap();
    let before = draw(&w);
    draw(&w);
    // The dialog's button sits under its title, inside its padding
    move_to(&w, at(120.0, 112.0));
    wait(&w, 900);
    assert_eq!(popups(&w), 1, "the tip of a control in the pop-up comes up");
    assert_eq!(says(&w), "From inside the dialog");
    let (x0, y0, x1, y1) = box_between(&before, &draw(&w));
    // Over the dialog's face, below the button, where the pop-up's own layer draws it
    assert!(x0 < 140 && x1 > 140 && y0 > 120 && y1 < 330, "over the dialog, under its button: {x0},{y0} to {x1},{y1}");
}
