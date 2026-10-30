// Folding in the real window, with a plugin that only declares a card the standard way (tests/test_collapse.py's "In the
// window" part). Driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{CardSpec, KeyInput, SlintSource, UiSpec};
use telescope_shell::window;

const CARD: &str = r#"
import { Card, Section, Icons } from "@ui-kit/ui-kit.slint";
export component CardyCard inherits Card {
    in property <bool> shown: true;
    visible: shown;
    title: "Cardy";
    icon: Icons.sun;
    Text { text: "loose"; }
    Section { scope: root.fold-key; key: "Light"; title: "Light"; icon: Icons.sun; Text { text: "Light 0"; } Text { text: "Light 1"; } }
    Section { scope: root.fold-key; key: "Focus"; title: "Focus"; icon: Icons.sun; Text { text: "Focus 0"; } Text { text: "Focus 1"; } }
}
"#;

const SECOND: &str = r#"
import { Card, Icons } from "@ui-kit/ui-kit.slint";
export component SecondCard inherits Card {
    title: "Second";
    icon: Icons.sun;
    Text { text: "row"; }
}
"#;

struct Second;

impl Plugin for Second {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "second".into(), title: "Second".into(), category: "look".into(), icon: "sun".into(), ..Default::default() }
    }
    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { region: "right".into(), ui: UiSpec { source: SlintSource::Text(SECOND.into()), component: "SecondCard".into(), props: serde_json::json!({}) } })
    }
}

struct Cardy;

impl Plugin for Cardy {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "cardy".into(), title: "Cardy".into(), category: "camera".into(), icon: "sun".into(), ..Default::default() }
    }
    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { region: "right".into(), ui: UiSpec { source: SlintSource::Text(CARD.into()), component: "CardyCard".into(), props: serde_json::json!({}) } })
    }
    fn on_key(&mut self, host: &mut dyn Host, key: &KeyInput) -> bool {
        if key.text == "h" && !key.released {
            host.set_ui_props("card", &serde_json::json!({"shown": false}));
        }
        false
    }
}

fn built(label: &str) -> (std::rc::Rc<window::Window>, std::path::PathBuf) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(label);
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| vec![Box::new(Cardy), Box::new(Second)], None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1400.0, 900.0));
    w.ui.show().unwrap();
    (w, dir)
}

fn title(w: &window::Window, name: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(&w.ui, name).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("no folding title {name}"))
}

#[test]
fn a_plugin_added_later_is_collapsible_without_doing_anything() {
    let (w, _dir) = built("collapse-later");
    title(&w, "Cardy").invoke_accessible_default_action();
    title(&w, "Cardy").invoke_accessible_default_action();
    title(&w, "Light").invoke_accessible_default_action();
    assert_eq!(w.state.borrow().collapse.keys(), ["cardy/Cardy/Light"]);
    title(&w, "Cardy").invoke_accessible_default_action();
    assert!(w.state.borrow().collapse.is_folded("cardy/Cardy"));
}

#[test]
fn resizing_into_another_layout_keeps_folds() {
    let (w, _dir) = built("collapse-resize");
    title(&w, "Light").invoke_accessible_default_action();
    w.ui.window().set_size(slint::LogicalSize::new(500.0, 900.0));
    slint::platform::update_timers_and_animations();
    assert!(w.state.borrow().collapse.is_folded("cardy/Cardy/Light"));
    assert_eq!(title(&w, "Light").accessible_checked(), Some(false));
    assert_eq!(title(&w, "Focus").accessible_checked(), Some(true));
}

#[test]
fn a_plugin_that_hides_its_card_keeps_it_hidden_when_folded() {
    let (w, _dir) = built("collapse-hidden");
    let below = |w: &window::Window| title(w, "Second").absolute_position().y;
    let with_card = below(&w);
    use slint::platform::WindowEvent;
    w.ui.window().dispatch_event(WindowEvent::KeyPressed { text: "h".into() });
    w.ui.window().dispatch_event(WindowEvent::KeyReleased { text: "h".into() });
    w.process();
    let without = below(&w);
    assert!(without < with_card - 50.0, "the card under it moves up: {with_card} to {without}");
    // Folded and opened again while hidden: it stays hidden, so the one under it stays where it is
    for (i, folded) in [true, false].into_iter().enumerate() {
        w.state.borrow_mut().collapse.set_folded("cardy/Cardy", folded);
        w.ui.set_global_property("Folds", "version", slint_interpreter::Value::Number(100.0 + i as f64)).unwrap();
        slint::platform::update_timers_and_animations();
        assert_eq!(below(&w), without, "folded: {folded}");
    }
}
