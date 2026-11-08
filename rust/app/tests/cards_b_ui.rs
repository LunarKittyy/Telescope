// Desk mode, Auto framing, Notes and Name tag drawn on the testing backend and driven with the pointer and the keys: what the new
// controls do goes to the plugin as it did from the old ones, and a card keeps its height whatever state it is in.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use serde_json::{json, Value as Json};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::framing::FramingPlugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

type Calls = Rc<RefCell<Vec<(String, Vec<Value>)>>>;

struct Rig {
    ui: ComponentInstance,
    calls: Calls,
}

/// The plugin's card shown with its own props and then `props` on top, every callback in `callbacks` written down.
fn rig(plugin: &mut dyn Plugin, props: Json, callbacks: &[&str]) -> Rig {
    offscreen::install();
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source.clone() else { panic!("source text") };
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    let calls: Calls = Rc::default();
    for name in callbacks {
        let (log, name) = (calls.clone(), name.to_string());
        ui.set_callback(&name.clone(), move |args| {
            log.borrow_mut().push((name.clone(), args.to_vec()));
            Value::Void
        })
        .unwrap();
    }
    for all in [card.props, props] {
        for (name, value) in all.as_object().unwrap() {
            ui.set_property(name, to_property(&ui, name, value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
    ui.window().set_size(slint::LogicalSize::new(406.0, 1200.0));
    ui.show().unwrap();
    offscreen::pump();
    Rig { ui, calls }
}

impl Rig {
    fn find(&self, label: &str) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.ui, label).next().unwrap_or_else(|| panic!("no {label}"))
    }

    /// The control of this kind with this label: a row's icon and a card's own name can share the label.
    fn find_as(&self, label: &str, role: AccessibleRole) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.ui, label).find(|e| e.accessible_role() == Some(role)).unwrap_or_else(|| panic!("no {role:?} {label}"))
    }

    fn click(&self, label: &str) {
        self.click_on(&self.find(label));
    }

    fn click_as(&self, label: &str, role: AccessibleRole) {
        self.click_on(&self.find_as(label, role));
    }

    fn click_on(&self, e: &ElementHandle) {
        let (p, s) = (e.absolute_position(), e.size());
        let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
        self.ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        self.ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        self.ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        // A click on a slider's track waits out the double-click interval before it lands
        offscreen::skip(std::time::Duration::from_millis(400));
    }

    fn key(&self, key: impl Into<slint::SharedString>) {
        let text = key.into();
        self.ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        self.ui.window().dispatch_event(WindowEvent::KeyReleased { text });
        offscreen::pump();
    }

    /// What `name` was called with, each time.
    fn said(&self, name: &str) -> Vec<Vec<Value>> {
        self.calls.borrow().iter().filter(|(n, _)| n == name).map(|(_, a)| a.clone()).collect()
    }

    fn number(&self, name: &str) -> f64 {
        match self.ui.get_property(name) {
            Ok(Value::Number(n)) => n,
            other => panic!("{name}: {other:?}"),
        }
    }
}

fn num(v: &[Value], i: usize) -> f64 {
    match v.get(i) {
        Some(Value::Number(n)) => *n,
        other => panic!("argument {i}: {other:?}"),
    }
}

const DESK: &[&str] = &["clean-picked", "shape-picked", "stretch-moved", "reset-clicked", "mark-clicked", "done-clicked", "cancel-clicked"];

#[test]
fn desks_picture_tiles_pick_the_writing_and_the_shape() {
    let r = rig(&mut DeskPlugin::new(), json!({"active": true, "on": true}), DESK);
    r.click("Writing: Contrast");
    assert_eq!(r.said("clean-picked").iter().map(|a| num(a, 0)).collect::<Vec<_>>(), [1.0]);
    // Fill is short on the tile and says the whole of it in its tooltip
    r.click("Shape: Fill");
    r.click("Shape: A4");
    assert_eq!(r.said("shape-picked").iter().map(|a| num(a, 0)).collect::<Vec<_>>(), [4.0, 1.0]);
}

impl Rig {
    /// The slider of a row (a row's icon can share its label).
    fn slider(&self, label: &str) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.ui, label).find(|e| e.accessible_value_maximum().is_some()).unwrap_or_else(|| panic!("no slider {label}"))
    }
}

const RESET_LABEL: &str = "Reset";

#[test]
fn desks_width_and_height_sliders_say_which_one_moved() {
    let r = rig(&mut DeskPlugin::new(), json!({"active": true, "on": true, "stretch": [120, 90], "stretch-text": ["120%", "90%"]}), DESK);
    r.click_on(&r.slider("Width"));
    assert!(r.said("stretch-moved").iter().all(|a| num(a, 0) == 0.0) && !r.said("stretch-moved").is_empty());
    r.click_on(&r.slider("Height"));
    let said = r.said("stretch-moved");
    assert!(said.iter().any(|a| num(a, 0) == 1.0), "{said:?}");
}

#[test]
fn desks_sliders_are_greyed_out_not_gone_while_the_sheet_fills_the_picture() {
    let r = rig(&mut DeskPlugin::new(), json!({"active": true, "on": true, "shape-current": 4, "stretch-enabled": false}), DESK);
    r.click_on(&r.slider("Width"));
    r.key(Key::RightArrow);
    assert!(r.said("stretch-moved").is_empty());
    // They keep their room: as big as when they work
    let off = (r.slider("Width").size(), r.slider("Height").size());
    r.ui.set_property("stretch-enabled", Value::Bool(true)).unwrap();
    offscreen::pump();
    assert_eq!((r.slider("Width").size(), r.slider("Height").size()), off);
}

#[test]
fn desks_reset_sits_in_the_header_and_lights_up_only_off_100() {
    let r = rig(&mut DeskPlugin::new(), json!({"active": true, "on": true, "reset-shown": false}), DESK);
    r.click(RESET_LABEL);
    assert!(r.said("reset-clicked").is_empty());
    r.ui.set_property("reset-shown", Value::Bool(true)).unwrap();
    offscreen::pump();
    r.click(RESET_LABEL);
    assert_eq!(r.said("reset-clicked").len(), 1);
}

#[test]
fn desks_mark_button_gives_way_to_done_and_cancel_in_the_same_row() {
    let r = rig(&mut DeskPlugin::new(), json!({"active": true, "on": true}), DESK);
    r.click("Mark the sheet");
    assert_eq!(r.said("mark-clicked").len(), 1);
    r.ui.set_property("picking", Value::Bool(true)).unwrap();
    offscreen::pump();
    r.click("Done");
    r.click("Cancel");
    assert_eq!((r.said("done-clicked").len(), r.said("cancel-clicked").len()), (1, 1));
}

const FRAMING: &[&str] = &["framing-moved", "speed-moved", "room-moved", "hand-toggled"];

#[test]
fn framings_sliders_and_the_hand_tile_reach_the_plugin() {
    let r = rig(&mut FramingPlugin::new(), json!({"active": true, "on": true}), FRAMING);
    for (label, callback) in [("Framing", "framing-moved"), ("Speed", "speed-moved"), ("Room to move", "room-moved")] {
        let slider = ElementHandle::find_by_accessible_label(&r.ui, label).find(|e| e.accessible_value_maximum().is_some()).expect("the slider");
        let (p, s) = (slider.absolute_position(), slider.size());
        let position = LogicalPosition::new(p.x + s.width / 4.0, p.y + s.height / 2.0);
        r.ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        r.ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        r.ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        // A click on the track waits out the double-click interval first
        offscreen::skip(std::time::Duration::from_millis(400));
        assert!(!r.said(callback).is_empty(), "{label} says nothing");
    }
    r.click_as("Things in hand", AccessibleRole::Switch);
    assert_eq!(r.said("hand-toggled").iter().map(|a| matches!(a[0], Value::Bool(true))).collect::<Vec<_>>(), [true]);
}

#[test]
fn framings_tile_does_nothing_while_no_phone_is_paired() {
    let r = rig(&mut FramingPlugin::new(), json!({"active": false}), FRAMING);
    r.click_as("Things in hand", AccessibleRole::Switch);
    assert!(r.said("hand-toggled").is_empty());
}

const NOTES: &[&str] = &["set-picked", "new-clicked", "delete-clicked", "text-edited", "size-moved", "scroll-picked", "speed-moved", "back-clicked", "play-toggled", "next-clicked", "send-clicked", "clear-clicked"];

fn paired_notes(extra: Json) -> Rig {
    let mut props = json!({"active": true, "send-enabled": true, "clear-enabled": true, "nav-enabled": true, "delete-enabled": true});
    props.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    rig(&mut NotesPlugin::new(), props, NOTES)
}

#[test]
fn notes_icon_tiles_and_buttons_do_what_the_old_buttons_did() {
    let r = paired_notes(json!({}));
    r.click("Previous");
    r.click("Next");
    r.click("Play");
    r.click("Send to phone");
    r.click("Clear from phone");
    r.click("New notes");
    r.click("Delete notes");
    assert_eq!((r.said("back-clicked").len(), r.said("next-clicked").len(), r.said("send-clicked").len(), r.said("clear-clicked").len()), (1, 1, 1, 1));
    assert_eq!((r.said("new-clicked").len(), r.said("delete-clicked").len()), (1, 1));
    assert!(matches!(r.said("play-toggled")[0][0], Value::Bool(true)));
}

#[test]
fn notes_speed_and_play_stay_but_are_greyed_out_by_hand() {
    let r = paired_notes(json!({"show-auto": false, "scroll": 1}));
    let before = r.find_as("Speed", AccessibleRole::Slider).size();
    r.click_as("Speed", AccessibleRole::Slider);
    r.key(Key::RightArrow);
    r.click("Play");
    assert!(r.said("speed-moved").is_empty());
    assert!(r.said("play-toggled").is_empty());
    // Previous and Next are still there to use
    r.click("Next");
    assert_eq!(r.said("next-clicked").len(), 1);
    // Back to Auto, the same room and both work
    r.ui.set_property("show-auto", Value::Bool(true)).unwrap();
    offscreen::pump();
    assert_eq!(r.find_as("Speed", AccessibleRole::Slider).size(), before);
    r.click_as("Speed", AccessibleRole::Slider);
    r.key(Key::RightArrow);
    assert!(!r.said("speed-moved").is_empty());
    r.click("Play");
    assert_eq!(r.said("play-toggled").len(), 1);
}

#[test]
fn notes_text_size_and_scroll_reach_the_plugin_and_typing_edits_the_text() {
    let r = paired_notes(json!({}));
    r.click("Scroll: By hand");
    assert_eq!(r.said("scroll-picked").iter().map(|a| num(a, 0)).collect::<Vec<_>>(), [1.0]);
    let size = ElementHandle::find_by_accessible_label(&r.ui, "Text size").find(|e| e.accessible_value_maximum().is_some()).expect("the slider");
    let (p, s) = (size.absolute_position(), size.size());
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    r.ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    r.ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    offscreen::skip(std::time::Duration::from_millis(400));
    assert!(!r.said("size-moved").is_empty());
    r.click_as("Notes", AccessibleRole::TextInput);
    r.key("a");
    assert_eq!(r.said("text-edited").last().map(|a| a[0].clone()), Some(Value::String("a".into())));
}

const TAG: &[&str] = &["text-edited", "choice-picked"];

#[test]
fn name_tags_frame_style_and_size_tiles_pick_by_index() {
    let r = rig(&mut NameTagPlugin::new(), json!({"on": true}), TAG);
    r.click("Position: Top right");
    r.click("Style: Light");
    r.click("Style: Pill");
    r.click("Size: Large");
    let picks: Vec<(f64, f64)> = r.said("choice-picked").iter().map(|a| (num(a, 0), num(a, 1))).collect();
    assert_eq!(picks, [(1.0, 4.0), (0.0, 2.0), (0.0, 1.0), (2.0, 2.0)]);
    // The two rows of looks are one choice: only the last pick is lit
    assert_eq!(r.number("style-current"), 1.0);
    assert_eq!(r.number("position-current"), 4.0);
    assert_eq!(r.number("size-current"), 2.0);
}

#[test]
fn name_tags_fields_edit_the_three_texts() {
    let r = rig(&mut NameTagPlugin::new(), json!({"on": true}), TAG);
    for (label, field, text) in [("Name", 0.0, "A"), ("Pronouns", 1.0, "B"), ("Second line", 2.0, "C")] {
        let e = ElementHandle::find_by_accessible_label(&r.ui, label).find(|e| e.accessible_placeholder_text().is_some()).expect("the field");
        let (p, s) = (e.absolute_position(), e.size());
        let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
        r.ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        r.ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        r.key(text);
        assert_eq!(r.said("text-edited").last().map(|a| (num(a, 0), a[1].clone())), Some((field, Value::String(text.into()))), "{label}");
    }
}

/// The y of the control labelled `probe` after a change to the card, and after each of the loop turns that follow.
fn steps(plugin: &mut dyn Plugin, before: Json, change: Json, probe: &str) -> Vec<f32> {
    let r = rig(plugin, before, &[]);
    let y = || r.find(probe).absolute_position().y;
    let mut seen = vec![y()];
    for (name, value) in change.as_object().unwrap() {
        r.ui.set_property(name, to_property(&r.ui, name, value)).unwrap();
    }
    for _ in 0..8 {
        seen.push(y());
        offscreen::pump();
    }
    seen.push(y());
    seen
}

fn stays(seen: &[f32], what: &str) {
    assert!(seen.iter().all(|y| (*y - seen[0]).abs() < 0.5), "{what} moved: {seen:?}");
}

#[test]
fn the_four_cards_keep_every_row_where_it_is_when_their_state_changes() {
    stays(&steps(&mut DeskPlugin::new(), json!({"active": true, "picking": false}), json!({"picking": true}), "Height"), "Desk, marking the sheet");
    stays(&steps(&mut DeskPlugin::new(), json!({"active": true, "stretch-enabled": true}), json!({"stretch-enabled": false, "shape-current": 4}), "Height"), "Desk, filling the picture");
    stays(&steps(&mut NotesPlugin::new(), json!({"active": true, "show-auto": true}), json!({"show-auto": false, "scroll": 1}), "Send to phone"), "Notes, by hand");
    stays(&steps(&mut NotesPlugin::new(), json!({"active": true, "text": ""}), json!({"text": "Good morning"}), "Send to phone"), "Notes, with something written");
    stays(&steps(&mut FramingPlugin::new(), json!({"active": true, "hand": false}), json!({"hand": true, "framing": 20, "framing-text": "Waist"}), "Room to move"), "Auto framing");
    stays(&steps(&mut NameTagPlugin::new(), json!({"on": false}), json!({"on": true, "name": "Alex", "style-current": 2, "position-current": 3, "size-current": 0}), "Size"), "Name tag");
}
