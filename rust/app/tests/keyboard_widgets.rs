// Keyboard use of the kit's controls on Slint's testing backend: a dropdown works like a native combo box, and every control
// that has a main action does it on both Space and Enter. Nothing is drawn on the desktop.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, SharedString};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;
use telescope_shell::{compile, input};

const DOC: &str = r#"
import { Theme, Icons, Button, IconButton, Switch, SwitchTile, Segmented, ChoiceField, Card } from "@ui-kit/ui-kit.slint";
export { Folds } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 400px;
    height: 600px;
    in-out property <int> clicks;
    in-out property <int> icon-clicks;
    in-out property <int> flips;
    in-out property <int> tile-flips;
    in-out property <int> segment-picks;
    in-out property <int> picks;
    in-out property <int> last-pick: -1;
    out property <int> dd-shown: dd.shown;
    out property <bool> dd-open: dd.open;
    in-out property <int> cam-picks;
    out property <int> cam-shown: cam.shown;
    VerticalLayout {
        padding: 10px;
        spacing: 8px;
        Button { text: "Go"; clicked => { clicks += 1; } }
        IconButton { label: "Icon go"; icon: Icons.bell; clicked => { icon-clicks += 1; } }
        Switch { label: "Flip"; toggled(on) => { flips += 1; } }
        SwitchTile { text: "Tile"; icon: Icons.bell; toggled(on) => { tile-flips += 1; } }
        Segmented { items: ["One", "Two"]; label: "Segments"; picked(i) => { segment-picks += 1; } }
        Card {
            title: "Folding";
            fold-key: "k";
            height: 80px;
        }
        dd := ChoiceField {
            name: "Fruit";
            items: ["Apple", "Banana", "Blueberry", "Cherry"];
            current: 0;
            picked(i) => { picks += 1; last-pick = i; }
        }
        Button { text: "After"; }
        // A camera's choice: what it shows is what the parent says, so a pick the camera refuses doesn't stay on screen
        cam := ChoiceField {
            name: "Lens";
            items: ["Wide", "Normal", "Long"];
            current: 0;
            follows: true;
            picked(i) => { cam-picks += 1; }
        }
    }
}
"#;

fn rig() -> ComponentInstance {
    i_slint_backend_testing::init_no_event_loop();
    let def = compile::compile("keys.slint", DOC, Vec::new(), "Test").unwrap();
    let ui = def.create().unwrap();
    ui.show().unwrap();
    ui
}

fn press(ui: &ComponentInstance, text: SharedString) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
    // The list opens and a letter is looked up a moment after the key
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
}

fn key(ui: &ComponentInstance, k: Key) {
    press(ui, k.into());
}

fn typed(ui: &ComponentInstance, c: &str) {
    press(ui, c.into());
}

fn number(ui: &ComponentInstance, name: &str) -> i32 {
    match ui.get_property(name) {
        Ok(Value::Number(n)) => n as i32,
        other => panic!("{name}: {other:?}"),
    }
}

fn flag(ui: &ComponentInstance, name: &str) -> bool {
    ui.get_property(name) == Ok(Value::Bool(true))
}

fn dropdown(ui: &ComponentInstance) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, "Fruit").find(|e| e.accessible_role() == Some(AccessibleRole::Combobox)).expect("the dropdown")
}

/// Tabs until the focus is on the control with this name and role.
fn focus(ui: &ComponentInstance, label: &str, role: AccessibleRole) {
    let e = ElementHandle::find_by_accessible_label(ui, label).find(|e| e.accessible_role() == Some(role)).unwrap_or_else(|| panic!("no {label}"));
    let (p, s) = (e.absolute_position(), e.size());
    for _ in 0..20 {
        if let Some((x, y, w, h)) = input::focused_box(ui) {
            if x <= p.x + s.width / 2.0 && p.x + s.width / 2.0 <= x + w && y <= p.y + s.height / 2.0 && p.y + s.height / 2.0 <= y + h {
                return;
            }
        }
        key(ui, Key::Tab);
    }
    panic!("Tab never reached {label}");
}

fn on_dropdown(ui: &ComponentInstance) -> bool {
    let (p, s) = (dropdown(ui).absolute_position(), dropdown(ui).size());
    input::focused_box(ui).is_some_and(|(x, y, w, h)| (x - p.x).abs() < 1.0 && (y - p.y).abs() < 1.0 && (w - s.width).abs() < 1.0 && (h - s.height).abs() < 1.0)
}

fn rows(ui: &ComponentInstance) -> Vec<ElementHandle> {
    ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::ListItem)).find_all()
}

fn lit(ui: &ComponentInstance) -> Vec<usize> {
    rows(ui).iter().filter(|r| r.accessible_item_selected() == Some(true)).filter_map(|r| r.accessible_item_index()).collect()
}

#[test]
fn closed_dropdown_opens_with_space_enter_and_alt_down() {
    let ui = rig();
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    assert!(!flag(&ui, "dd-open"));
    assert_eq!(dropdown(&ui).accessible_expanded(), Some(false));
    for (name, open) in [("space", Box::new(|ui: &ComponentInstance| typed(ui, " ")) as Box<dyn Fn(&ComponentInstance)>), ("enter", Box::new(|ui| key(ui, Key::Return)))] {
        open(&ui);
        assert!(flag(&ui, "dd-open"), "{name} opens it");
        assert_eq!(dropdown(&ui).accessible_expanded(), Some(true), "{name}");
        key(&ui, Key::Escape);
        assert!(!flag(&ui, "dd-open"), "Esc closes it after {name}");
    }
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Alt.into() });
    key(&ui, Key::DownArrow);
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Alt.into() });
    assert!(flag(&ui, "dd-open"), "Alt+Down opens it");
    assert_eq!(number(&ui, "picks"), 0, "without changing anything");
}

#[test]
fn closed_dropdown_changes_with_arrows_home_end_and_letters() {
    let ui = rig();
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    key(&ui, Key::DownArrow);
    assert_eq!((number(&ui, "dd-shown"), number(&ui, "picks"), flag(&ui, "dd-open")), (1, 1, false), "Down changes it without opening");
    key(&ui, Key::End);
    assert_eq!(number(&ui, "dd-shown"), 3, "End goes to the last");
    key(&ui, Key::UpArrow);
    assert_eq!(number(&ui, "dd-shown"), 2);
    key(&ui, Key::Home);
    assert_eq!(number(&ui, "dd-shown"), 0, "Home goes to the first");
    typed(&ui, "b");
    assert_eq!(number(&ui, "dd-shown"), 1, "a letter jumps to the next item with it");
    typed(&ui, "b");
    assert_eq!(number(&ui, "dd-shown"), 2, "and again to the one after");
    typed(&ui, "B");
    assert_eq!(number(&ui, "dd-shown"), 1, "wrapping round, whatever the case");
    typed(&ui, "c");
    assert_eq!(number(&ui, "dd-shown"), 3);
    assert_eq!(dropdown(&ui).accessible_value().as_deref(), Some("Cherry"), "the screen reader hears the new choice");
}

#[test]
fn open_dropdown_moves_a_highlight_and_picks_it() {
    let ui = rig();
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    typed(&ui, " ");
    assert_eq!(rows(&ui).len(), 4, "the list items are exposed");
    assert_eq!(lit(&ui), vec![0], "the current choice starts highlighted");
    key(&ui, Key::DownArrow);
    key(&ui, Key::DownArrow);
    assert_eq!(lit(&ui), vec![2]);
    assert_eq!(number(&ui, "dd-shown"), 0, "moving the highlight picks nothing yet");
    assert_eq!(number(&ui, "picks"), 0);
    key(&ui, Key::End);
    assert_eq!(lit(&ui), vec![3]);
    key(&ui, Key::Home);
    assert_eq!(lit(&ui), vec![0]);
    typed(&ui, "b");
    assert_eq!(lit(&ui), vec![1], "a letter moves the highlight");
    typed(&ui, "b");
    assert_eq!(lit(&ui), vec![2]);
    key(&ui, Key::Return);
    assert!(!flag(&ui, "dd-open"), "Enter closes it");
    assert_eq!((number(&ui, "dd-shown"), number(&ui, "last-pick"), number(&ui, "picks")), (2, 2, 1));
    assert!(on_dropdown(&ui), "the focus is back on the dropdown");
    // Space picks too
    typed(&ui, " ");
    key(&ui, Key::UpArrow);
    typed(&ui, " ");
    assert!(!flag(&ui, "dd-open"));
    assert_eq!((number(&ui, "dd-shown"), number(&ui, "picks")), (1, 2), "Space picks the highlighted item");
    assert!(on_dropdown(&ui));
}

#[test]
fn hovering_a_row_moves_the_highlight() {
    let ui = rig();
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    typed(&ui, " ");
    assert_eq!(lit(&ui), vec![0]);
    let row = rows(&ui).into_iter().find(|r| r.accessible_item_index() == Some(2)).expect("third row");
    let (p, s) = (row.absolute_position(), row.size());
    // A row's position is inside the popup, which opens 4px under the dropdown
    let (dd, h) = (dropdown(&ui).absolute_position(), dropdown(&ui).size().height);
    let (x, y) = (dd.x + p.x + s.width / 2.0, dd.y + h + 4.0 + p.y + s.height / 2.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
    assert_eq!(lit(&ui), vec![2], "hover moves the highlight");
    key(&ui, Key::DownArrow);
    assert_eq!(lit(&ui), vec![3], "the keys carry on from where the mouse left it");
}

#[test]
fn escape_closes_without_changing_and_tab_moves_on() {
    let ui = rig();
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    typed(&ui, " ");
    key(&ui, Key::DownArrow);
    key(&ui, Key::Escape);
    assert!(!flag(&ui, "dd-open"));
    assert_eq!((number(&ui, "dd-shown"), number(&ui, "picks")), (0, 0), "Esc changes nothing");
    assert!(on_dropdown(&ui), "and the focus is on the dropdown");
    assert!(lit(&ui).is_empty(), "the closed list has nothing highlighted");
    typed(&ui, " ");
    key(&ui, Key::DownArrow);
    key(&ui, Key::Tab);
    assert!(!flag(&ui, "dd-open"), "Tab closes it");
    assert_eq!(number(&ui, "picks"), 0, "without picking");
    assert!(!on_dropdown(&ui), "and moves on to the next control");
    let after = ElementHandle::find_by_accessible_label(&ui, "After").next().unwrap();
    let (p, s) = (after.absolute_position(), after.size());
    let (x, y, w, h) = input::focused_box(&ui).unwrap();
    assert!(x <= p.x + 1.0 && p.x + s.width / 2.0 <= x + w && y <= p.y + s.height / 2.0 && p.y + s.height / 2.0 <= y + h, "the focus is on the next button");
}

#[test]
fn a_long_list_scrolls_to_keep_the_highlight_in_sight() {
    i_slint_backend_testing::init_no_event_loop();
    let doc = r#"
import { ChoiceField } from "@ui-kit/ui-kit.slint";
export { WindowBox } from "@ui-kit/ui-kit.slint";
export component Long inherits Window {
    width: 300px;
    height: 200px;
    ChoiceField { name: "Number"; width: 200px; y: 10px; items: ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20"]; current: 0; }
}"#;
    let ui = compile::compile("long.slint", doc, Vec::new(), "Long").unwrap().create().unwrap();
    ui.show().unwrap();
    ui.window().set_size(slint::LogicalSize::new(300.0, 200.0));
    ui.set_global_property("WindowBox", "width", Value::Number(300.0)).unwrap();
    ui.set_global_property("WindowBox", "height", Value::Number(200.0)).unwrap();
    key(&ui, Key::Tab);
    typed(&ui, " ");
    key(&ui, Key::End);
    let last = rows(&ui).into_iter().find(|r| r.accessible_item_index() == Some(19)).expect("the last row");
    let list_top = rows(&ui).into_iter().map(|r| r.absolute_position().y).fold(f32::MAX, f32::min);
    assert!(list_top < 0.0, "the top rows scrolled out of view");
    assert_eq!(last.accessible_item_selected(), Some(true));
    assert!(last.absolute_position().y + last.size().height <= 200.0 + 1.0, "the highlighted last row is inside the window: {} (list from {list_top})", last.absolute_position().y);
    key(&ui, Key::Home);
    let first = rows(&ui).into_iter().find(|r| r.accessible_item_index() == Some(0)).unwrap();
    assert!(first.absolute_position().y >= 0.0, "scrolled back to the first row: {}", first.absolute_position().y);
}

#[test]
fn space_and_enter_both_do_a_controls_main_action() {
    let ui = rig();
    let folds = Rc::new(Cell::new(0));
    let seen = folds.clone();
    ui.set_global_callback("Folds", "toggle", move |_| {
        seen.set(seen.get() + 1);
        Value::Void
    })
    .unwrap();
    let cases: [(&str, AccessibleRole, &str); 5] = [
        ("Go", AccessibleRole::Button, "clicks"),
        ("Icon go", AccessibleRole::Button, "icon-clicks"),
        ("Flip", AccessibleRole::Switch, "flips"),
        ("Tile", AccessibleRole::Switch, "tile-flips"),
        ("Segments: Two", AccessibleRole::Tab, "segment-picks"),
    ];
    for (label, role, counter) in cases {
        focus(&ui, label, role);
        typed(&ui, " ");
        assert_eq!(number(&ui, counter), 1, "{label}: Space");
        key(&ui, Key::Return);
        assert_eq!(number(&ui, counter), 2, "{label}: Enter");
    }
    focus(&ui, "Folding", AccessibleRole::Button);
    typed(&ui, " ");
    assert_eq!(folds.get(), 1, "a card's fold heading: Space");
    key(&ui, Key::Return);
    assert_eq!(folds.get(), 2, "a card's fold heading: Enter");
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    typed(&ui, " ");
    assert!(flag(&ui, "dd-open"), "a dropdown: Space");
    key(&ui, Key::Escape);
    key(&ui, Key::Return);
    assert!(flag(&ui, "dd-open"), "a dropdown: Enter");
}

#[test]
fn a_choice_that_follows_its_parent_shows_the_parents_value_after_a_pick_the_parent_didnt_take() {
    let ui = rig();
    focus(&ui, "Lens", AccessibleRole::Combobox);
    key(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "cam-picks"), 1, "the pick was sent");
    assert_eq!(number(&ui, "cam-shown"), 0, "the camera didn't take it, so the box still says what the camera does");
    // An ordinary box keeps what was picked until its parent says otherwise
    focus(&ui, "Fruit", AccessibleRole::Combobox);
    key(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "dd-shown"), 1);
}
