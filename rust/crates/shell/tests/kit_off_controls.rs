// Controls that go off or fold away while they hold the focus or an open list take no more keys or presses. Driven through the offscreen backend on a clock
// the test moves itself: nothing reaches the desktop.
mod motion_film;

use motion_film::*;
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::offscreen;

fn key(ui: &ComponentInstance, text: impl Into<slint::SharedString>) {
    let text: slint::SharedString = text.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
    step(FRAME);
}

fn doc(widget: &str) -> String {
    format!(
        r#"
import {{ Theme, Button, IconButton, Icons, Switch, SwitchTile, ToggleTile, Pad, Jog, Dial, ColourWheel, ChoiceField, Segmented, PictureSegmented, TextField, NumberField, PictureField, Slider, ColourField }} from "@ui-kit/ui-kit.slint";
export component T inherits Window {{
    width: 300px;
    height: 200px;
    background: Theme.card;
    in-out property <bool> on: true;
    in-out property <int> hits;
    {widget}
}}
"#
    )
}

fn hits(ui: &ComponentInstance) -> f32 {
    num(ui, "hits")
}

// Focus the widget with Tab, check the keys do something while it is on, switch it off, and check they do nothing
fn goes_quiet(widget: &str, keys: &[Key]) {
    let _still = offscreen::freeze_clock();
    let ui = make(&doc(widget), 300.0, 200.0);
    key(&ui, Key::Tab);
    for k in keys {
        key(&ui, *k);
    }
    key(&ui, " ");
    key(&ui, "\n");
    assert!(hits(&ui) > 0.0, "the keys do something while it is on: {widget}");
    set(&ui, "hits", Value::Number(0.0));
    set(&ui, "on", Value::Bool(false));
    step(500);
    for k in keys {
        key(&ui, *k);
    }
    key(&ui, " ");
    key(&ui, "\n");
    assert_eq!(hits(&ui), 0.0, "and nothing once it is off: {widget}");
}

#[test]
fn a_colour_wheel_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("ColourWheel { enabled: on; key(d, s) => { hits += 1; } reset => { hits += 1; } }", &[Key::RightArrow, Key::UpArrow, Key::Home]);
}

#[test]
fn a_button_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Button { text: \"Go\"; enabled: on; clicked => { hits += 1; } }", &[]);
}

#[test]
fn an_icon_button_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("IconButton { icon: Icons.reset; label: \"Go\"; enabled: on; clicked => { hits += 1; } }", &[]);
}

#[test]
fn a_switch_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Switch { label: \"Flip\"; enabled: on; toggled(v) => { hits += 1; } }", &[]);
}

#[test]
fn a_toggle_tile_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("ToggleTile { text: \"Flip\"; enabled: on; toggled(v) => { hits += 1; } }", &[]);
}

#[test]
fn a_pad_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Pad { height: 80px; enabled: on; moved(x, y) => { hits += 1; } released(x, y) => { hits += 1; } }", &[Key::RightArrow, Key::UpArrow, Key::Home]);
}

#[test]
fn a_jog_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Jog { enabled: on; tapped(d) => { hits += 1; } }", &[Key::RightArrow, Key::LeftArrow]);
}

#[test]
fn a_dial_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Dial { stops: [\"1\", \"2\", \"3\", \"4\"]; enabled: on; moved(i) => { hits += 1; } released(i) => { hits += 1; } }", &[Key::RightArrow, Key::End]);
}

#[test]
fn a_picker_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("PictureField { name: \"None\"; enabled: on; clicked => { hits += 1; } }", &[]);
}

#[test]
fn a_choice_field_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("ChoiceField { items: [\"A\", \"B\", \"C\"]; compact: true; enabled: on; picked(i) => { hits += 1; } }", &[Key::DownArrow, Key::UpArrow, Key::End]);
}

#[test]
fn a_segment_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("Segmented { items: [\"A\", \"B\", \"C\"]; disabled: [on ? false : true, on ? false : true, on ? false : true]; picked(i) => { hits += 1; } }", &[Key::RightArrow]);
}

#[test]
fn a_picture_segment_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("PictureSegmented { items: [\"A\", \"B\", \"C\"]; disabled: [on ? false : true, on ? false : true, on ? false : true]; picked(i) => { hits += 1; } }", &[Key::RightArrow]);
}

#[test]
fn a_text_box_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("TextField { enabled: on; text: \"x\"; step(d) => { hits += 1; } edited(t) => { hits += 1; } }", &[Key::DownArrow, Key::UpArrow, Key::Escape]);
}

#[test]
fn a_number_box_that_goes_off_with_the_focus_takes_no_more_keys() {
    goes_quiet("NumberField { enabled: on; value: \"5\"; step(d) => { hits += 1; } committed(t) => { hits += 1; } }", &[Key::DownArrow, Key::UpArrow]);
}

const LIST: &str = r#"
import { Theme, ChoiceField } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 300px;
    background: Theme.card;
    in-out property <bool> on: true;
    in-out property <int> hits;
    in-out property <int> mode;
    out property <bool> listed: f.open;
    f := ChoiceField {
        x: 10px;
        y: 10px;
        width: 200px;
        compact: true;
        caption: "Mode";
        items: ["Off", "One", "Two", "Three"];
        current: mode;
        enabled: on;
        picked(i) => { hits += 1; }
    }
}
"#;

#[test]
fn a_choice_list_that_is_open_when_the_field_goes_off_closes_and_picks_nothing() {
    let _still = offscreen::freeze_clock();
    let ui = make(LIST, 300.0, 300.0);
    press(&ui, 60.0, 26.0);
    release(&ui, 60.0, 26.0);
    step(100);
    offscreen::draw(ui.window(), 300.0, 300.0, 1.0);
    step(100);
    assert!(matches!(ui.get_property("listed"), Ok(Value::Bool(true))), "the list is up");
    set(&ui, "on", Value::Bool(false));
    step(400);
    offscreen::draw(ui.window(), 300.0, 300.0, 1.0);
    assert!(matches!(ui.get_property("listed"), Ok(Value::Bool(false))), "it goes down with the field");
    for k in [Key::DownArrow, Key::Return] {
        key(&ui, k);
    }
    key(&ui, " ");
    for y in [50.0, 76.0, 102.0] {
        press(&ui, 60.0, y);
        release(&ui, 60.0, y);
    }
    step(400);
    assert_eq!(hits(&ui), 0.0, "and nothing picks");
}

const FOLD: &str = r#"
import { Theme, Card, Section, Button, Folds } from "@ui-kit/ui-kit.slint";
export { Folds }
export component T inherits Window {
    width: 300px;
    height: 300px;
    background: Theme.card;
    in-out property <int> hits;
    out property <length> card-h: c.height;
    out property <length> part-h: part.height;
    VerticalLayout {
        alignment: start;
        x: 10px;
        width: 280px;
        c := Card {
            title: "Card";
            fold-key: "t/Card";
            Button { text: "Go"; clicked => { hits += 1; } }
            part := Section {
                scope: "t/Card";
                key: "Part";
                title: "Part";
                Button { text: "Also"; clicked => { hits += 1; } }
            }
        }
    }
}
"#;

fn fold_rig() -> ComponentInstance {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;
    let ui = make(FOLD, 300.0, 300.0);
    let folded: Rc<RefCell<HashSet<String>>> = Rc::default();
    let version = Rc::new(std::cell::Cell::new(1.0));
    let f = folded.clone();
    ui.set_global_callback("Folds", "is-folded", move |a| match &a[0] {
        Value::String(k) => Value::Bool(f.borrow().contains(k.as_str())),
        _ => Value::Bool(false),
    })
    .unwrap();
    let (f, v, weak) = (folded, version, ui.as_weak());
    ui.set_global_callback("Folds", "toggle", move |a| {
        if let Value::String(k) = &a[0] {
            let mut set = f.borrow_mut();
            if !set.remove(k.as_str()) {
                set.insert(k.to_string());
            }
        }
        v.set(v.get() + 1.0);
        weak.unwrap().set_global_property("Folds", "version", Value::Number(v.get())).unwrap();
        Value::Void
    })
    .unwrap();
    ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
    offscreen::draw(ui.window(), 300.0, 300.0, 1.0);
    step(FRAME);
    ui
}

fn centre(ui: &ComponentInstance, label: &str, checkable: bool) -> (f32, f32) {
    use i_slint_backend_testing::ElementHandle;
    let e = ElementHandle::find_by_accessible_label(ui, label).find(|e| (e.accessible_checkable() == Some(true)) == checkable).unwrap_or_else(|| panic!("no {label}"));
    let (p, s) = (e.absolute_position(), e.size());
    (p.x + s.width / 2.0, p.y + s.height / 2.0)
}

fn click_at(ui: &ComponentInstance, at: (f32, f32)) {
    press(ui, at.0, at.1);
    release(ui, at.0, at.1);
}

fn rows_stay_quiet_while_folding(title: &str, button: &str, height: &str) {
    let _still = offscreen::freeze_clock();
    let ui = fold_rig();
    let go = centre(&ui, button, false);
    let open = num(&ui, height);
    click_at(&ui, go);
    assert_eq!(hits(&ui), 1.0, "{button} answers while it is open");
    set(&ui, "hits", Value::Number(0.0));
    click_at(&ui, centre(&ui, title, true));
    let mut mid = false;
    for _ in 0..6 {
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 300.0, 1.0);
        click_at(&ui, go);
        let h = num(&ui, height);
        mid = mid || (h > 0.0 && h < open - 1.0);
    }
    assert!(mid, "the fold was under way");
    assert_eq!(hits(&ui), 0.0, "{button} takes no click while its rows fold away");
}

#[test]
fn a_card_that_folds_away_takes_no_click_in_its_rows() {
    rows_stay_quiet_while_folding("Card", "Go", "card-h");
}

#[test]
fn a_section_that_folds_away_takes_no_click_in_its_rows() {
    rows_stay_quiet_while_folding("Part", "Also", "part-h");
}

fn keys_stay_quiet_while_folding(title: &str, button: &str, height: &str) {
    let _still = offscreen::freeze_clock();
    let ui = fold_rig();
    let open = num(&ui, height);
    // Fold it by its title (which keeps the focus), Tab on into the rows that are still going, and press the keys that act
    click_at(&ui, centre(&ui, title, true));
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 300.0, 1.0);
    let h = num(&ui, height);
    assert!(h > 0.0 && h < open - 1.0, "still folding: {h} of {open}");
    key(&ui, Key::Tab);
    key(&ui, " ");
    key(&ui, "\n");
    assert_eq!(hits(&ui), 0.0, "{button} takes no key while its rows fold away");
}

#[test]
fn a_card_that_folds_away_takes_no_key_in_its_rows() {
    keys_stay_quiet_while_folding("Card", "Go", "card-h");
}

#[test]
fn a_section_that_folds_away_takes_no_key_in_its_rows() {
    keys_stay_quiet_while_folding("Part", "Also", "part-h");
}

const MENU: &str = r#"
import { Theme, MenuSurface, MenuItem } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 200px;
    height: 100px;
    background: Theme.card;
    in-out property <bool> going;
    in-out property <int> hits;
    MenuSurface {
        x: 10px;
        y: 10px;
        width: 150px;
        height: 40px;
        leaving: going;
        MenuItem { title: "Pick"; activate => { hits += 1; } }
    }
}
"#;

#[test]
fn a_menu_that_is_easing_out_takes_no_more_picks() {
    let _still = offscreen::freeze_clock();
    let ui = make(MENU, 200.0, 100.0);
    step(500);
    press(&ui, 50.0, 20.0);
    release(&ui, 50.0, 20.0);
    assert_eq!(hits(&ui), 1.0, "a click on a row picks it while the menu is up");
    set(&ui, "hits", Value::Number(0.0));
    set(&ui, "going", Value::Bool(true));
    step(FRAME);
    for _ in 0..3 {
        press(&ui, 50.0, 20.0);
        release(&ui, 50.0, 20.0);
    }
    assert_eq!(hits(&ui), 0.0, "and none while it fades away");
}

const BUBBLE: &str = r#"
import { Theme, Bubble, Button } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 400px;
    height: 300px;
    background: Theme.card;
    in-out property <int> hits;
    out property <bool> up: b.open;
    public function go() { b.show(); }
    public function away() { b.close(); }
    Rectangle {
        x: 150px;
        y: 200px;
        width: 100px;
        height: 30px;
        b := Bubble {
            width: parent.width;
            height: parent.height;
            label: "Box";
            Button { text: "Inside"; clicked => { hits += 1; } }
        }
    }
}
"#;

#[test]
fn a_bubble_that_is_fading_away_takes_no_click_and_no_key_in_its_controls() {
    use i_slint_backend_testing::ElementHandle;
    let _still = offscreen::freeze_clock();
    let ui = make(BUBBLE, 400.0, 300.0);
    ui.invoke("go", &[]).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 400.0, 300.0, 1.0);
    step(500);
    offscreen::draw(ui.window(), 400.0, 300.0, 1.0);
    let at = |ui: &ComponentInstance| {
        let e = ElementHandle::find_by_accessible_label(ui, "Inside").next().expect("the button in the bubble");
        let (p, s) = (e.absolute_position(), e.size());
        (p.x + s.width / 2.0, p.y + s.height / 2.0)
    };
    let spot = at(&ui);
    click_at(&ui, spot);
    assert_eq!(hits(&ui), 1.0, "a click on the button works while the bubble is up");
    set(&ui, "hits", Value::Number(0.0));
    ui.invoke("away", &[]).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 400.0, 300.0, 1.0);
    click_at(&ui, spot);
    key(&ui, " ");
    key(&ui, "\n");
    assert_eq!(hits(&ui), 0.0, "and neither does anything while it fades away");
}
