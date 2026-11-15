// The Card order dialog in the real window: its rows, a drag by pointer, the restart bar and the order it saves.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value};
use slint::platform::{PointerEventButton as PointerButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};
use slint_interpreter::Value as SValue;
use std::rc::Rc;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{CardSpec, SlintSource, UiSpec};
use telescope_shell::window;

/// A plugin with a card of its own title on one rail.
struct Side(&'static str, &'static str, &'static str);

impl Plugin for Side {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: self.0.into(), title: self.1.into(), category: "camera".into(), ..Default::default() }
    }
    fn card(&mut self) -> Option<CardSpec> {
        let source = format!("import {{ Card, Icons }} from \"@ui-kit/ui-kit.slint\";\nexport component SideCard inherits Card {{ title: \"{}\"; icon: Icons.sliders; Text {{ text: \"x\"; height: 40px; }} }}", self.1);
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(source), component: "SideCard".into(), props: json!({}) }, region: self.2.into() })
    }
}

fn build(name: &str) -> (Rc<window::Window>, std::path::PathBuf) {
    telescope_shell::offscreen::install();
    let dir = telescope_core::testing::scratch_dir(name);
    let file = dir.join(CONFIG_FILENAME);
    let w = window::build(
        ConfigFile::at(file.clone()),
        |_| vec![Box::new(Side("one", "One", "left")) as Box<dyn Plugin>, Box::new(Side("two", "Two", "left")), Box::new(Side("three", "Three", "right"))],
        None,
        None,
    )
    .unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    w.ui.show().unwrap();
    w.process();
    (w, file)
}

fn saved(file: &std::path::Path) -> Value {
    serde_json::from_str::<Value>(&std::fs::read_to_string(file).unwrap()).unwrap().get("card_layout").cloned().unwrap_or(Value::Null)
}

fn pointer(w: &Rc<window::Window>, event: impl Fn(LogicalPosition) -> WindowEvent, x: f32, y: f32) {
    w.ui.window().dispatch_event(event(LogicalPosition::new(x, y)));
}

fn rows(w: &window::Window) -> Vec<(String, f64, f64)> {
    let Ok(SValue::Model(m)) = w.ui.get_property("order-rows") else { panic!("no rows") };
    (0..m.row_count()).map(|i| {
        let SValue::Struct(s) = m.row_data(i).unwrap() else { panic!("not a row") };
        let n = |k: &str| match s.get_field(k) { Some(SValue::Number(n)) => *n, _ => -1.0 };
        let name = match s.get_field("name") { Some(SValue::String(t)) => t.to_string(), _ => String::new() };
        (name, n("side"), n("at"))
    }).collect()
}

fn strip(w: &window::Window, title: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(&w.ui, title).find(|e| format!("{:?}", e.accessible_role()) == "Some(Button)" && e.size().width > 300.0).unwrap_or_else(|| panic!("no row named {title}"))
}

fn shot(w: &Rc<window::Window>, name: &str) {
    if let Ok(dir) = std::env::var("CARD_SHOTS") {
        telescope_shell::offscreen::skip(std::time::Duration::from_millis(500));
        w.process();
        let pic = telescope_shell::offscreen::draw(w.ui.window(), 1300.0, 900.0, 1.0).expect("a picture");
        pic.save_png(&std::path::Path::new(&dir).join(name)).unwrap();
    }
}

#[test]
fn the_dialog_lists_the_cards_and_a_drag_moves_one_and_saves_it() {
    let (w, file) = build("card-order-ui");
    w.open_card_order();
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(500));
    w.process();
    assert_eq!(rows(&w), [("one".into(), 0.0, 0.0), ("two".into(), 0.0, 1.0), ("three".into(), 1.0, 0.0)]);
    assert_eq!(w.ui.get_property("order-pending").unwrap(), SValue::Bool(false));
    shot(&w, "card-order-open.png");

    // Take One by the middle and let go over the lower half of Three, in the other group
    telescope_shell::offscreen::draw(w.ui.window(), 1300.0, 900.0, 1.0);
    let (one, three) = (strip(&w, "One"), strip(&w, "Three"));
    let (from, to) = (one.absolute_position(), three.absolute_position());
    let (x, y) = (from.x + 60.0, from.y + one.size().height / 2.0);
    pointer(&w, |position| WindowEvent::PointerMoved { position }, x, y);
    pointer(&w, |position| WindowEvent::PointerPressed { position, button: PointerButton::Left }, x, y);
    for step in 1..=12 {
        let at = y + (to.y + three.size().height - y) * step as f32 / 12.0 + 6.0;
        pointer(&w, |position| WindowEvent::PointerMoved { position }, x, at);
        telescope_shell::offscreen::skip(std::time::Duration::from_millis(30));
    }
    shot(&w, "card-order-drag.png");
    pointer(&w, |position| WindowEvent::PointerReleased { position, button: PointerButton::Left }, x, y + (to.y + three.size().height - y) + 6.0);
    w.process();
    assert_eq!(rows(&w), [("one".into(), 1.0, 1.0), ("two".into(), 0.0, 0.0), ("three".into(), 1.0, 0.0)]);
    assert_eq!(saved(&file), json!({"left": ["two"], "right": ["three", "one"]}));
    assert_eq!(w.ui.get_property("order-pending").unwrap(), SValue::Bool(true));
    assert_eq!(w.ui.get_property("order-right-count").unwrap(), SValue::Number(2.0));
    shot(&w, "card-order-pending.png");

    // Revert puts the running order back and the bar goes
    w.ui.invoke("order-revert", &[]).unwrap();
    assert_eq!(rows(&w), [("one".into(), 0.0, 0.0), ("two".into(), 0.0, 1.0), ("three".into(), 1.0, 0.0)]);
    assert_eq!(saved(&file), Value::Null);
    assert_eq!(w.ui.get_property("order-pending").unwrap(), SValue::Bool(false));
}

#[test]
fn a_rail_can_be_emptied_and_reset_goes_back_to_the_default() {
    let (w, file) = build("card-order-empty");
    w.open_card_order();
    w.ui.invoke("order-moved", &[SValue::String("three".into()), SValue::Number(0.0), SValue::Number(0.0)]).unwrap();
    assert_eq!(saved(&file), json!({"left": ["three", "one", "two"], "right": []}));
    assert_eq!(w.ui.get_property("order-right-count").unwrap(), SValue::Number(0.0));
    shot(&w, "card-order-empty.png");
    w.ui.invoke("order-reset", &[]).unwrap();
    assert_eq!(saved(&file), Value::Null);
    assert_eq!(rows(&w), [("one".into(), 0.0, 0.0), ("two".into(), 0.0, 1.0), ("three".into(), 1.0, 0.0)]);
    assert_eq!(w.ui.get_property("order-pending").unwrap(), SValue::Bool(false));
}

#[test]
fn a_card_says_its_own_title_for_the_dialog() {
    let (w, _) = build("card-order-title");
    for (id, title) in [("p0", "One"), ("p2", "Three")] {
        assert_eq!(w.ui.get_property(&format!("{id}-card-title")).unwrap(), SValue::String(title.into()));
    }
}

#[test]
fn settings_has_a_row_search_finds_that_opens_the_dialog() {
    let (w, _) = build("card-order-settings");
    telescope_shell::settings::open(&w, "general");
    let shown = telescope_shell::settings::current().expect("the settings window");
    shown.ui.window().set_size(slint::LogicalSize::new(940.0, 680.0));
    telescope_shell::offscreen::pump();
    assert_eq!(shown.tabs()[0], "general");
    for word in ["arrange", "order", "rails", "move cards"] {
        let found = shown.search_results(word);
        assert!(found.iter().any(|e| e.name == "Card order"), "{word} finds the row: {:?}", found.iter().map(|e| &e.name).collect::<Vec<_>>());
    }
    assert!(shown.call("shell", "card-order", "open"));
    assert_eq!(rows(&w).len(), 3, "the dialog has its rows");
    telescope_shell::settings::forget();
}


#[test]
fn the_restart_bar_shuts_all_the_way_to_nothing_while_it_still_moves() {
    let (w, _file) = build("card-order-bar");
    w.open_card_order();
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(500));
    w.process();
    w.ui.invoke("order-moved", &[SValue::String("one".into()), SValue::Number(1.0), SValue::Number(0.0)]).unwrap();
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(1500));
    w.process();
    w.ui.invoke("order-revert", &[]).unwrap();
    let room = || match w.ui.get_property("order-bar-room") { Ok(SValue::Number(v)) => v, other => panic!("{other:?}") };
    let (mut last, mut last_step) = (room(), 0.0);
    for frame in 1..60 {
        telescope_shell::offscreen::skip(std::time::Duration::from_millis(8));
        w.process();
        let now = room();
        assert!(now <= last, "frame {frame}: the bar grew again");
        if now == 0.0 {
            // It was still moving a fair way each frame when it reached 0, not creeping
            assert!(last_step > 0.004, "frame {frame}: the last steps before shut were only {last_step}");
            return;
        }
        last_step = last - now;
        last = now;
    }
    panic!("the bar didn't shut within 480 ms: {last}");
}
