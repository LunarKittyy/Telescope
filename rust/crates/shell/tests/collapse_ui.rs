// Cards and sections fold when their title is clicked, and what's folded is remembered (tests/test_collapse.py), on the
// kit's Card and Section driven through Slint's testing backend with the same Collapsing state the window keeps.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::rc::Rc;
use telescope_shell::collapse::Collapsing;

const DOC: &str = r#"
import { Card, Section, Folds, Icons } from "@ui-kit/ui-kit.slint";
export { Folds }
export component Test inherits Window {
    width: 420px;
    height: 900px;
    in property <bool> dim;
    in property <bool> hide-light-1;
    in property <bool> hide-focus-0;
    in property <bool> plain-card;
    in property <bool> second-card: true;
    in-out property <length> extra: 0px;
    out property <length> card-height: card.height;
    out property <length> light-height: light.height;
    out property <bool> control-focused: control.has-focus;
    VerticalLayout {
        alignment: start;
        spacing: 10px;
        card := Card {
            accessible-role: groupbox;
            accessible-label: "things-card";
            title: "Things";
            icon: Icons.sun;
            fold-key: "p/Things";
            enabled: !dim;
            blocked: dim;
            Text { text: "loose"; }
            control := FocusScope { height: 20px; }
            light := Section {
                scope: "p/Things";
                key: "Light";
                title: "Light";
                icon: Icons.sun;
                Text { text: "Light 0"; }
                if !hide-light-1: Text { text: "Light 1"; }
            }
            Section {
                scope: "p/Things";
                key: "Focus";
                title: "Focus";
                icon: Icons.sun;
                if !hide-focus-0: Text { text: "Focus 0"; }
                Text { text: "Focus 1"; }
            }
        }
        if second-card: Card {
            accessible-role: groupbox;
            accessible-label: "other-card";
            title: "Other";
            icon: Icons.sun;
            fold-key: plain-card ? "" : "q/Other";
            Text { text: "other row"; }
            Section {
                scope: "q/Other";
                key: "Light";
                title: "Light";
                icon: Icons.sun;
                Text { text: "other light"; }
            }
        }
        if !second-card: Rectangle { height: 1px; }
    }
}
"#;

struct Rig {
    ui: ComponentInstance,
    state: Rc<RefCell<Collapsing>>,
    version: Rc<RefCell<f64>>,
}

thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });

fn rig() -> Rig {
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let def = telescope_shell::compile::compile("collapse.slint", DOC, vec![], "Test").expect("the document compiles");
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(420.0, 900.0));
    let state = Rc::new(RefCell::new(Collapsing::default()));
    let version = Rc::new(RefCell::new(0.0));
    let s = state.clone();
    ui.set_global_callback("Folds", "is-folded", move |args| match args.first() {
        Some(Value::String(key)) => Value::Bool(s.borrow().is_folded(key)),
        _ => Value::Bool(false),
    }).unwrap();
    let (s, v, weak) = (state.clone(), version.clone(), ui.as_weak());
    ui.set_global_callback("Folds", "toggle", move |args| {
        if let Some(Value::String(key)) = args.first() {
            if s.borrow_mut().toggle(key) {
                *v.borrow_mut() += 1.0;
                weak.unwrap().set_global_property("Folds", "version", Value::Number(*v.borrow())).unwrap();
            }
        }
        Value::Void
    }).unwrap();
    ui.show().unwrap();
    Rig { ui, state, version }
}

impl Rig {
    /// Loads folded keys the way the window does after reading its settings.
    fn load(&self, keys: &[&str]) {
        self.state.borrow_mut().load(Some(&serde_json::json!(keys)));
        *self.version.borrow_mut() += 1.0;
        self.ui.set_global_property("Folds", "version", Value::Number(*self.version.borrow())).unwrap();
    }

    /// The title that folds (the one that's a checkable button) with this name; the nth when a name repeats.
    fn title(&self, name: &str, nth: usize) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.ui, name).filter(|e| e.accessible_checkable() == Some(true)).nth(nth)
            .unwrap_or_else(|| panic!("no folding title {name}"))
    }

    fn click_at(&self, x: f32, y: f32, button: PointerEventButton) {
        let position = LogicalPosition::new(x, y);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
        // A card a click folded closes over a fraction of a second: let it finish
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
    }

    fn click(&self, e: &ElementHandle) {
        let (p, s) = (e.absolute_position(), e.size());
        self.click_at(p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Left);
    }

    fn key(&self, text: &str) {
        self.ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        self.ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    }

    /// The height of the second card, which is only there while second-card is on.
    fn other_height(&self) -> f64 {
        ElementHandle::find_by_accessible_label(&self.ui, "other-card").next().expect("the second card is there").size().height as f64
    }

    /// Whether a row is there and takes up room inside its card, by the words on it.
    fn row_shown(&self, name: &str) -> bool {
        let card = ElementHandle::find_by_accessible_label(&self.ui, "things-card").next().expect("the card");
        let bottom = card.absolute_position().y + card.size().height;
        ElementHandle::find_by_accessible_label(&self.ui, name).next().is_some_and(|row| {
            row.size().height > 0.0 && row.absolute_position().y + row.size().height <= bottom
        })
    }

    fn set(&self, name: &str, value: Value) {
        self.ui.set_property(name, value).unwrap();
        slint::platform::update_timers_and_animations();
    }
}

fn open(r: &Rig) -> f64 {
    ElementHandle::find_by_accessible_label(&r.ui, "things-card").next().expect("the card").size().height as f64
}

#[test]
fn clicking_a_card_title_hides_everything_under_it_and_again_shows_it() {
    let r = rig();
    let whole = open(&r);
    let title = r.title("Things", 0);
    r.click(&title);
    assert!(r.state.borrow().is_folded("p/Things"));
    assert!(open(&r) <= 70.0 && open(&r) > 40.0, "folded, only the header is left: {}", open(&r));
    assert_eq!(title.accessible_checked(), Some(false));
    r.click(&title);
    assert!(!r.state.borrow().is_folded("p/Things"));
    assert_eq!(open(&r), whole);
}

#[test]
fn a_section_folds_alone() {
    let r = rig();
    let whole = open(&r);
    r.click(&r.title("Light", 0));
    assert!(r.state.borrow().is_folded("p/Things/Light") && !r.state.borrow().is_folded("p/Things"));
    // Two 20px rows less (a 22px row and its 8px gap are the smallest it can be), and the header, loose row and Focus stay
    assert!(open(&r) < whole - 30.0 && open(&r) > whole - 80.0, "{} of {whole}", open(&r));
    assert_eq!(r.title("Focus", 0).accessible_checked(), Some(true));
    assert_eq!(r.title("Light", 0).accessible_checked(), Some(false));
}

#[test]
fn a_folded_section_stays_folded_when_its_card_is_folded_and_opened() {
    let r = rig();
    r.click(&r.title("Light", 0));
    let with_light_folded = open(&r);
    r.click(&r.title("Things", 0));
    r.click(&r.title("Things", 0));
    assert!(r.state.borrow().is_folded("p/Things/Light"));
    assert_eq!(open(&r), with_light_folded);
}

#[test]
fn the_rest_of_a_section_headings_row_folds_it_too() {
    let r = rig();
    let light = r.title("Light", 0);
    let (p, s) = (light.absolute_position(), light.size());
    // Right of the title, out to the card's edge
    r.click_at(380.0, p.y + s.height / 2.0, PointerEventButton::Left);
    assert!(r.state.borrow().is_folded("p/Things/Light"));
    r.click_at(380.0, p.y + s.height / 2.0, PointerEventButton::Left);
    assert!(!r.state.borrow().is_folded("p/Things/Light"));
    r.click_at(380.0, p.y + s.height / 2.0, PointerEventButton::Left);
    assert!(r.state.borrow().is_folded("p/Things/Light"));
}

#[test]
fn only_the_left_button_folds() {
    let r = rig();
    let title = r.title("Things", 0);
    let (p, s) = (title.absolute_position(), title.size());
    r.click_at(p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Right);
    assert!(!r.state.borrow().is_folded("p/Things"));
    r.click_at(p.x + s.width / 2.0, p.y + s.height / 2.0, PointerEventButton::Left);
    assert!(r.state.borrow().is_folded("p/Things"));
}

#[test]
fn a_dimmed_card_still_folds() {
    let r = rig();
    r.set("dim", Value::Bool(true));
    r.click(&r.title("Things", 0));
    assert!(r.state.borrow().is_folded("p/Things"));
}

#[test]
fn card_and_section_titles_are_named_keyboard_toggles() {
    for key in [" ", "\n"] {
        let r = rig();
        let (title, section) = (r.title("Things", 0), r.title("Light", 0));
        assert_eq!(title.accessible_checked(), Some(true));
        assert_eq!(title.accessible_description().as_deref(), Some("Expanded. Press Space or Enter to collapse."));
        // Tab reaches the card's title first, then the section's
        r.key("\t");
        r.key(key);
        assert!(r.state.borrow().is_folded("p/Things") && !r.state.borrow().is_folded("p/Things/Light"), "key {key:?}");
        assert_eq!(title.accessible_checked(), Some(false));
        assert_eq!(title.accessible_description().as_deref(), Some("Collapsed. Press Space or Enter to expand."));
        r.key(key);
        assert!(!r.state.borrow().is_folded("p/Things"));
        // The section comes next in Tab order, after the control the card holds
        r.key("\t");
        r.key("\t");
        r.key(key);
        assert!(r.state.borrow().is_folded("p/Things/Light") && !r.state.borrow().is_folded("p/Things/Focus"), "key {key:?}");
        assert_eq!(section.accessible_checked(), Some(false));
    }
}

#[test]
fn folding_moves_focus_out_of_the_rows_it_hides() {
    let r = rig();
    // Tab: the card's title, then the control in its rows
    r.key("\t");
    r.key("\t");
    assert_eq!(r.ui.get_property("control-focused").unwrap(), Value::Bool(true));
    r.state.borrow_mut().toggle("p/Things");
    *r.version.borrow_mut() += 1.0;
    r.ui.set_global_property("Folds", "version", Value::Number(*r.version.borrow())).unwrap();
    slint::platform::update_timers_and_animations();
    assert_eq!(r.ui.get_property("control-focused").unwrap(), Value::Bool(false));
    // Space now reaches the title, which unfolds the card again
    r.key(" ");
    assert!(!r.state.borrow().is_folded("p/Things"));
}

#[test]
fn unfolding_a_card_keeps_rows_a_plugin_hid_itself() {
    let r = rig();
    r.set("hide-light-1", Value::Bool(true)); // what a mode toggle does
    r.click(&r.title("Things", 0));
    // A plugin changing rows while the card is folded
    r.set("hide-focus-0", Value::Bool(true));
    r.set("hide-light-1", Value::Bool(false));
    r.click(&r.title("Things", 0));
    assert!(r.row_shown("Light 1") && !r.row_shown("Focus 0"));
    assert!(r.row_shown("Light 0"));
}

#[test]
fn everything_starts_open() {
    let r = rig();
    assert!(r.state.borrow().keys().is_empty());
    for row in ["loose", "Light 0", "Light 1", "Focus 0", "Focus 1"] {
        assert!(r.row_shown(row), "{row} starts folded away");
    }
    for title in ["Things", "Light", "Focus"] {
        assert_eq!(r.title(title, 0).accessible_checked(), Some(true), "{title}");
    }
}

#[test]
fn two_plugins_with_the_same_titles_fold_separately() {
    let r = rig();
    let other = r.other_height();
    r.click(&r.title("Light", 0));
    assert!(r.state.borrow().is_folded("p/Things/Light") && !r.state.borrow().is_folded("q/Other/Light"));
    assert_eq!(r.other_height(), other);
    r.click(&r.title("Light", 1));
    assert!(r.state.borrow().is_folded("q/Other/Light"));
    assert!(r.other_height() < other);
}

#[test]
fn loaded_state_is_applied_to_cards_already_there_and_to_ones_added_later() {
    let r = rig();
    r.set("second-card", Value::Bool(false));
    r.load(&["p/Things", "q/Other/Light"]);
    assert!(open(&r) <= 70.0);
    r.set("second-card", Value::Bool(true)); // a plugin whose card appears after the settings were read
    let other = r.other_height();
    r.set("second-card", Value::Bool(false));
    r.set("second-card", Value::Bool(true));
    r.load(&["p/Things", "q/Other/Light"]);
    assert_eq!(r.other_height(), other);
    assert_eq!(r.title("Light", 0).accessible_checked(), Some(false)); // Other's section, under the folded Things card
}

#[test]
fn a_card_that_goes_and_comes_back_is_folded_as_it_was() {
    let r = rig();
    r.click(&r.title("Other", 0));
    assert!(r.state.borrow().is_folded("q/Other"));
    let folded = r.other_height();
    r.set("second-card", Value::Bool(false));
    // The gone card doesn't take its fold with it
    assert_eq!(r.state.borrow().keys(), ["q/Other"]);
    r.set("second-card", Value::Bool(true));
    assert_eq!(r.other_height(), folded);
}

#[test]
fn widgets_without_a_card_header_are_left_alone() {
    let r = rig();
    r.set("plain-card", Value::Bool(true));
    assert!(ElementHandle::find_by_accessible_label(&r.ui, "Other").all(|e| e.accessible_checkable() != Some(true)));
    let before = r.other_height();
    let other = ElementHandle::find_by_accessible_label(&r.ui, "Other").next().unwrap();
    r.click(&other);
    assert!(r.state.borrow().keys().is_empty() && r.other_height() == before);
}

const STRETCH: &str = r#"
import { Card, Section, Folds, Icons } from "@ui-kit/ui-kit.slint";
export { Folds }
export component Test inherits Window {
    width: 420px;
    height: 700px;
    in property <bool> in-section;
    VerticalLayout {
        Card {
            title: "Things";
            icon: Icons.sun;
            fold-key: "p/Things";
            Rectangle { accessible-role: text; accessible-label: "fixed"; height: 20px; }
            if !in-section: Rectangle { accessible-role: text; accessible-label: "grow"; vertical-stretch: 1; min-height: 10px; }
            if in-section: Section {
                scope: "p/Things";
                key: "Light";
                title: "Light";
                icon: Icons.sun;
                vertical-stretch: 1;
                Rectangle { accessible-role: text; accessible-label: "grow"; vertical-stretch: 1; min-height: 10px; }
            }
        }
    }
}
"#;

#[test]
fn a_row_keeps_its_stretch_inside_the_fold() {
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    for in_section in [false, true] {
        let ui = telescope_shell::compile::compile("stretch.slint", STRETCH, vec![], "Test").expect("the document compiles").create().unwrap();
        ui.window().set_size(slint::LogicalSize::new(420.0, 700.0));
        ui.set_property("in-section", Value::Bool(in_section)).unwrap();
        ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
        ui.show().unwrap();
        let height = |name: &str| ElementHandle::find_by_accessible_label(&ui, name).next().unwrap_or_else(|| panic!("no {name}")).size().height;
        assert_eq!(height("fixed"), 20.0);
        assert!(height("grow") > 450.0, "the card's spare height goes to the row that stretches: {}, in a section: {in_section}", height("grow"));
    }
}

const RAIL: &str = r#"
import { Folds, Icons } from "@ui-kit/ui-kit.slint";
import { CameraCard, Lens } from "cards/camera.slint";
export { Folds }
export component Test inherits Window {
    width: 420px;
    height: 1800px;
    in property <[Lens]> lenses;
    in property <int> columns: 1;
    VerticalLayout {
        alignment: start;
        CameraCard {
            accessible-role: groupbox;
            accessible-label: "camera-card";
            active: true;
            fold-key: "camera_control/Camera";
            lenses: root.lenses;
            lens-columns: root.columns;
        }
    }
}
"#;

// The real Camera card's lens row, which is filled in after the card is up, like LensPanel in the Python test
#[test]
fn a_row_that_grows_once_folding_is_installed_gets_its_new_height() {
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let card_text = include_str!("../ui/cards/camera.slint").to_string();
    let ui = telescope_shell::compile::compile("rail.slint", RAIL, vec![("cards/camera.slint".into(), card_text)], "Test").expect("the document compiles").create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(420.0, 1800.0));
    ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
    ui.show().unwrap();
    let card = || ElementHandle::find_by_accessible_label(&ui, "camera-card").next().expect("the card");
    let empty = card().size().height;
    let lenses: Vec<Value> = (0..5).map(|i| Value::Struct(slint_interpreter::Struct::from_iter([
        ("id".to_string(), Value::String(i.to_string().into())), ("number".to_string(), Value::String(i.to_string().into())),
        ("unit".to_string(), Value::String("mm".into())), ("rest".to_string(), Value::String("".into())),
        ("name".to_string(), Value::String(format!("Back {i}mm").into())),
    ]))).collect();
    ui.set_property("columns", Value::Number(3.0)).unwrap();
    ui.set_property("lenses", Value::Model(slint::ModelRc::new(slint::VecModel::from(lenses)))).unwrap();
    slint::platform::update_timers_and_animations();
    let bottom = card().absolute_position().y + card().size().height;
    let tiles: Vec<ElementHandle> = (0..5).map(|i| ElementHandle::find_by_accessible_label(&ui, &format!("Back {i}mm")).next().unwrap_or_else(|| panic!("no tile {i}"))).collect();
    let first_row = tiles[0].absolute_position().y;
    let last_row = tiles[4].absolute_position().y;
    assert!(last_row > first_row, "two rows of tiles");
    for tile in &tiles {
        assert!(tile.size().height > 0.0 && tile.absolute_position().y + tile.size().height < bottom, "the card holds every tile");
    }
    assert!(card().size().height >= empty + (last_row - first_row), "{} from {empty}", card().size().height);
}

const HEADER: &str = r#"
import { Card, Icons } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 420px;
    height: 200px;
    in property <bool> dim;
    in-out property <int> actions;
    VerticalLayout {
        alignment: start;
        Card {
            title: "Things";
            icon: Icons.sun;
            enabled: !dim;
            blocked: dim;
            has-action: true;
            action-text: "Reset";
            action-icon: Icons.sun;
            action => { root.actions += 1; }
            has-extra: true;
            extra-icon: Icons.sun;
            extra-label: "Guide";
            extra => { root.actions += 1; }
            has-switch: true;
            switch-label: "Use it";
            switch-toggled => { root.actions += 1; }
        }
    }
}
"#;

#[test]
fn a_dimmed_cards_header_buttons_take_no_click() {
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    for (dim, heard) in [(false, 3.0), (true, 0.0)] {
        let ui = telescope_shell::compile::compile("header.slint", HEADER, vec![], "Test").expect("the document compiles").create().unwrap();
        ui.window().set_size(slint::LogicalSize::new(420.0, 200.0));
        ui.show().unwrap();
        ui.set_property("dim", Value::Bool(dim)).unwrap();
        for label in ["Reset", "Guide", "Use it"] {
            let e = ElementHandle::find_by_accessible_label(&ui, label).next().unwrap_or_else(|| panic!("no {label}"));
            let (p, s) = (e.absolute_position(), e.size());
            let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position });
            ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
            ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        }
        assert_eq!(ui.get_property("actions").unwrap(), Value::Number(heard), "dim: {dim}");
    }
}
