// test_settings_window.py on the real settings window: what's drawn, not just the list it's drawn from.
use i_slint_backend_testing::ElementHandle;
use serde_json::json;
use slint::ComponentHandle;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, SlintSource, UiSpec};
use telescope_shell::{settings, window};

struct Sections;

fn section(id: &str, order: i32) -> SettingsSection {
    let source = format!("export component Part inherits Rectangle {{ height: 40px; accessible-role: text; accessible-label: \"{id}\"; }}");
    SettingsSection {
        id: id.into(), tab: "general".into(), order, tab_title: String::new(), keywords: String::new(), footer: None,
        ui: UiSpec { source: SlintSource::Text(source), component: "Part".into(), props: json!({}) },
    }
}

impl Plugin for Sections {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "sections".into(), title: "Sections".into(), ..Default::default() }
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![section("first", 10), section("second", 0), section("third", 0)]
    }
}

// test_settings_window.py::test_sections_sit_in_order_then_registration_order
#[test]
fn sections_sit_in_order_then_registration_order() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("settings-order");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| vec![Box::new(Sections) as Box<dyn Plugin>], None, None).unwrap();
    settings::open(&w, "general");
    let shown = settings::current().expect("the settings window");
    shown.ui.window().set_size(slint::LogicalSize::new(940.0, 680.0));
    let top = |label: &str| {
        let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(&shown.ui, label).collect();
        assert!(!found.is_empty(), "{label} on the tab");
        found[0].absolute_position().y
    };
    let (first, second, third) = (top("first"), top("second"), top("third"));
    assert!(second < third && third < first, "{second} {third} {first}");
    settings::forget();
}
