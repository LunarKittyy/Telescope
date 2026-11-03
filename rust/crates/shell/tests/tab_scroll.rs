// Tab and Shift+Tab walk one stable cycle through a scrolled area, whatever is in view, and a control that takes the focus
// is scrolled into view. A stand-in for the main window's columns and a settings page, built the way they are (a Flickable that
// never moves itself, content shifted by a scroll offset, Reveal asking for the scroll), driven through Slint's testing
// backend: nothing reaches the desktop. The real windows are checked in app/src/scenes.rs.
use slint::platform::WindowEvent;
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::rc::Rc;

thread_local!(static FOLDED: RefCell<Option<Rc<RefCell<Vec<String>>>>> = const { RefCell::new(None) });

const DOC: &str = r#"
import { Button, Card, Section, Folds, Reveal, Icons } from "@ui-kit/ui-kit.slint";
export { Folds, Reveal }
export component Scrolled inherits Window {
    width: 400px;
    height: 240px;
    in-out property <length> scroll;
    in-out property <string> pressed;
    in property <bool> hide-seven;
    Flickable {
        x: 0;
        y: 20px;
        width: parent.width;
        height: 200px;
        interactive: false;
        viewport-width: self.width;
        viewport-height: self.height;
        VerticalLayout {
            y: -root.scroll;
            width: parent.width;
            height: self.preferred-height;
            spacing: 10px;
            Button { height: 30px; text: "One"; clicked => { pressed = "One"; } }
            Button { height: 30px; text: "Two"; clicked => { pressed = "Two"; } }
            Button { height: 30px; text: "Three"; clicked => { pressed = "Three"; } }
            Button { height: 30px; text: "Four"; clicked => { pressed = "Four"; } }
            Button { height: 30px; text: "Five"; clicked => { pressed = "Five"; } }
            Button { height: 30px; text: "Six"; clicked => { pressed = "Six"; } }
            Button { height: 30px; text: "Seven"; visible: !hide-seven; clicked => { pressed = "Seven"; } }
            Button { height: 0px; text: "Zero"; visible: false; clicked => { pressed = "Zero"; } }
            Section {
                scope: "s";
                key: "Folding";
                title: "Folding";
                icon: Icons.sun;
                Button { height: 30px; text: "Inside"; clicked => { pressed = "Inside"; } }
            }
            Card {
                title: "Cards";
                icon: Icons.sun;
                fold-key: "c/Cards";
                Button { height: 30px; text: "In card"; clicked => { pressed = "In card"; } }
            }
            Button { height: 30px; text: "Eight"; clicked => { pressed = "Eight"; } }
            Button { height: 30px; text: "Nine"; clicked => { pressed = "Nine"; } }
            Button { height: 30px; text: "Ten"; clicked => { pressed = "Ten"; } }
        }
    }
}
"#;

/// Every stop of the cycle by name, in order ("" is the title of a section or card, which folds and has no name to press).
const ALL: [&str; 14] = ["One", "Two", "Three", "Four", "Five", "Six", "Seven", "", "Inside", "", "In card", "Eight", "Nine", "Ten"];

fn open() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let ui = telescope_shell::compile::compile("scrolled.slint", DOC, vec![], "Scrolled").expect("the document compiles").create().unwrap();
    // What the window does with a control that took the focus: scroll the area until the control is inside it
    let weak = ui.as_weak();
    ui.set_global_callback("Reveal", "show", move |args| {
        if let (Some(ui), [_, Value::Number(y), _, Value::Number(h)]) = (weak.upgrade(), args) {
            let (top, bottom) = (20.0, 220.0);
            let scroll = scroll(&ui);
            let to = if *y < top { scroll - (top - y) } else if y + h > bottom { scroll + (y + h - bottom) } else { return Value::Void };
            ui.set_property("scroll", Value::Number(to.max(0.0))).unwrap();
        }
        Value::Void
    })
    .unwrap();
    // What the window keeps: which keys are folded, and a version that changes with them
    let folded: Rc<RefCell<Vec<String>>> = Rc::default();
    let kept = folded.clone();
    ui.set_global_callback("Folds", "is-folded", move |args| match args.first() {
        Some(Value::String(key)) => Value::Bool(kept.borrow().iter().any(|k| k == key.as_str())),
        _ => Value::Bool(false),
    })
    .unwrap();
    FOLDED.with(|f| *f.borrow_mut() = Some(folded));
    ui.window().set_size(slint::LogicalSize::new(400.0, 240.0));
    ui.show().unwrap();
    telescope_shell::offscreen::draw(ui.window(), 400.0, 240.0, 1.0);
    ui
}

fn key(ui: &ComponentInstance, text: &str) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    slint::platform::update_timers_and_animations();
    telescope_shell::offscreen::draw(ui.window(), 400.0, 240.0, 1.0);
}

/// What has the focus, by what Space presses.
fn focused(ui: &ComponentInstance) -> String {
    ui.set_property("pressed", Value::String("".into())).unwrap();
    key(ui, " ");
    match ui.get_property("pressed").unwrap() {
        Value::String(s) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

fn scroll(ui: &ComponentInstance) -> f64 {
    match ui.get_property("scroll").unwrap() {
        Value::Number(n) => n,
        other => panic!("{other:?}"),
    }
}

/// The names Tab stops on, `n` presses from where it is.
fn tabs(ui: &ComponentInstance, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            key(ui, "\t");
            focused(ui)
        })
        .collect()
}

fn fold(ui: &ComponentInstance, key: &str) {
    FOLDED.with(|f| f.borrow().as_ref().unwrap().borrow_mut().push(key.to_string()));
    let version = match ui.get_global_property("Folds", "version").unwrap() {
        Value::Number(n) => n,
        other => panic!("{other:?}"),
    };
    ui.set_global_property("Folds", "version", Value::Number(version + 1.0)).unwrap();
    slint::platform::update_timers_and_animations();
}

#[test]
fn tab_walks_everything_in_the_area_in_one_cycle_whatever_is_in_view() {
    let ui = open();
    let names = tabs(&ui, ALL.len() * 3);
    // The same stops in the same order every time round, the controls out of sight included
    let first: Vec<&str> = names[..ALL.len()].iter().map(String::as_str).collect();
    assert_eq!(first, ALL, "first time round");
    assert_eq!(names[ALL.len()..2 * ALL.len()], names[..ALL.len()], "second time round");
    assert_eq!(names[2 * ALL.len()..], names[..ALL.len()], "third time round");
}

#[test]
fn the_cycle_is_the_same_wherever_the_area_was_scrolled_to() {
    let ui = open();
    ui.set_property("scroll", Value::Number(120.0)).unwrap();
    // Nothing of the top is in view now, and Tab still goes round through all of it
    let names = tabs(&ui, ALL.len() * 2);
    assert_eq!(names[..ALL.len()], names[ALL.len()..], "the same stops from a scrolled start");
    let mut sorted = names[..ALL.len()].to_vec();
    sorted.sort();
    let mut all = ALL.map(String::from).to_vec();
    all.sort();
    assert_eq!(sorted, all, "all of them, once each");
}

#[test]
fn shift_tab_walks_the_same_cycle_backwards() {
    let ui = open();
    let forward = tabs(&ui, ALL.len());
    let back: slint::SharedString = slint::platform::Key::Backtab.into();
    let mut walked = Vec::new();
    for _ in 0..ALL.len() * 2 {
        key(&ui, &back);
        walked.push(focused(&ui));
    }
    // From the last stop, back through all of them in reverse, and round again the same
    let mut want: Vec<String> = forward.iter().rev().skip(1).cloned().collect();
    want.push(forward[ALL.len() - 1].clone());
    assert_eq!(walked[..ALL.len()], want[..], "backwards from the end");
    assert_eq!(walked[ALL.len()..], walked[..ALL.len()], "and round again the same");
}

#[test]
fn what_is_hidden_or_folded_away_is_not_a_stop() {
    let ui = open();
    let stops = |ui: &ComponentInstance| tabs(ui, ALL.len() * 2);
    assert!(!stops(&ui).iter().any(|n| n == "Zero"), "Zero has visible: false");
    ui.set_property("hide-seven", Value::Bool(true)).unwrap();
    let shorter = stops(&ui);
    assert!(!shorter.iter().any(|n| n == "Seven"), "a control a plugin hides takes no focus");
    assert!(shorter.iter().any(|n| n == "Six") && shorter.iter().any(|n| n == "Eight"));
    fold(&ui, "s/Folding");
    fold(&ui, "c/Cards");
    let folded = tabs(&ui, (ALL.len() - 3) * 2);
    assert!(!folded.iter().any(|n| n == "Inside" || n == "In card"), "a folded section or card takes no focus: {folded:?}");
    // Their titles are still there to unfold them by
    assert_eq!(folded.iter().filter(|n| n.is_empty()).count(), 4, "{folded:?}");
    assert_eq!(folded[..ALL.len() - 3], folded[ALL.len() - 3..], "and the cycle (without Seven, Inside and In card) is still the same each time round");
}

#[test]
fn a_control_out_of_sight_is_scrolled_into_view_when_tab_reaches_it() {
    let ui = open();
    assert_eq!(scroll(&ui), 0.0);
    let (top, bottom) = (20.0, 220.0);
    let mut moved = false;
    for _ in 0..ALL.len() {
        key(&ui, "\t");
        let (_, y, _, h) = telescope_shell::input::focused_box(&ui).expect("something has the focus");
        assert!(f64::from(y) >= top - 0.5 && f64::from(y + h) <= bottom + 0.5, "{y}+{h} is outside the area at scroll {}", scroll(&ui));
        moved |= scroll(&ui) > 0.0;
    }
    assert!(moved, "the area never scrolled");
    // Round to the first control: the area comes back up to it
    key(&ui, "\t");
    assert_eq!(scroll(&ui), 0.0, "Tab round to the first control scrolls back up to it");
}

const NAV: &str = r#"
import { SettingsNav, NavTab } from "shell/settings.slint";
export component Nav inherits Window {
    width: 300px;
    height: 200px;
    in-out property <int> current;
    out property <length> offset: nav.offset;
    out property <length> content: nav.content;
    nav := SettingsNav {
        x: 10px;
        y: 0px;
        width: 215px;
        height: root.height;
        tabs: [
            { id: "a", name: "A" }, { id: "b", name: "B" }, { id: "c", name: "C" }, { id: "d", name: "D" }, { id: "e", name: "E" },
            { id: "f", name: "F" }, { id: "g", name: "G" }, { id: "h", name: "H" }, { id: "i", name: "I" }, { id: "j", name: "J" },
        ];
        current <=> root.current;
    }
}
"#;

fn number(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn the_settings_tab_column_scrolls_in_a_short_window_and_keeps_the_picked_tab_in_sight() {
    telescope_shell::offscreen::install();
    let ui = telescope_shell::compile::compile("nav.slint", NAV, vec![], "Nav").expect("the document compiles").create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(300.0, 200.0));
    ui.show().unwrap();
    telescope_shell::offscreen::draw(ui.window(), 300.0, 200.0, 1.0);
    // Ten 38px rows don't fit in 200px
    assert!(number(&ui, "content") > 200.0);
    assert_eq!(number(&ui, "offset"), 0.0);
    key(&ui, "\t");
    let content = number(&ui, "content");
    let named = |k: slint::platform::Key| slint::SharedString::from(k);
    key(&ui, &named(slint::platform::Key::End));
    assert_eq!(number(&ui, "current"), 9.0);
    assert!((number(&ui, "offset") - (content - 200.0)).abs() < 0.5, "the last tab is brought into view: {}", number(&ui, "offset"));
    key(&ui, &named(slint::platform::Key::Home));
    assert_eq!(number(&ui, "current"), 0.0);
    assert_eq!(number(&ui, "offset"), 0.0, "and the first one back");
    // Arrowing down past the bottom edge scrolls only as far as it takes
    for _ in 0..6 {
        key(&ui, &named(slint::platform::Key::DownArrow));
    }
    let offset = number(&ui, "offset");
    assert!(offset > 0.0 && offset < content - 200.0, "{offset}");
    assert!(4.0 + 6.0 * 38.0 + 38.0 <= offset + 200.0, "tab 6 is in view at offset {offset}");
    // The wheel scrolls it too, and not past its ends
    let at = slint::LogicalPosition::new(100.0, 100.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at });
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: at, delta_x: 0.0, delta_y: 500.0 });
    assert_eq!(number(&ui, "offset"), 0.0);
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: at, delta_x: 0.0, delta_y: -500.0 });
    assert!((number(&ui, "offset") - (content - 200.0)).abs() < 0.5, "wheel down stops at the end");
}
