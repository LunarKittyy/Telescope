// The Plugins tab's search looks through the names and descriptions in the language shown (its own process: the
// translator is global), in the real settings window.
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model};
use slint_interpreter::{ComponentInstance, Value};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_shell::{settings, window};

struct Named(PluginInfo);

impl Plugin for Named {
    fn info(&self) -> PluginInfo {
        self.0.clone()
    }
}

fn info(name: &str, title: &str, about: &str, category: &str, required: bool) -> PluginInfo {
    PluginInfo { name: name.into(), title: title.into(), about: about.into(), category: category.into(), required, ..Default::default() }
}

/// The Python tests' _grouped: seven plugins in three groups.
fn grouped() -> Vec<Box<dyn Plugin>> {
    vec![
        info("connection", "Connection", "What Connection does.", "extras", true),
        info("camera_control", "Camera", "What Camera does.", "extras", false),
        info("presets", "Presets", "What Presets does.", "extras", false),
        info("zoom_tool", "Zoom tool", "Makes the picture bigger.", "camera", false),
        info("alpha", "Alpha", "What Alpha does.", "camera", false),
        info("echo", "Echo", "What Echo does.", "sound", false),
        info("lost", "Lost", "What Lost does.", "no_such_group", false),
    ].into_iter().map(|i| Box::new(Named(i)) as Box<dyn Plugin>).collect()
}

fn field(item: &Value, name: &str) -> Value {
    let Value::Struct(s) = item else { panic!("{item:?}") };
    s.get_field(name).cloned().unwrap_or_else(|| panic!("no {name}"))
}

fn text(value: Value) -> String {
    let Value::String(s) = value else { panic!("{value:?}") };
    s.to_string()
}

/// The groups the tab shows now, each with its tiles' titles.
fn shown(ui: &ComponentInstance) -> Vec<(String, Vec<String>)> {
    let Ok(Value::Model(groups)) = ui.get_property("groups") else { panic!("no groups") };
    groups.iter().map(|g| {
        let Value::Model(tiles) = field(&g, "tiles") else { panic!("no tiles") };
        (text(field(&g, "title")), tiles.iter().map(|t| text(field(&t, "title"))).collect())
    }).collect()
}

fn tiles(ui: &ComponentInstance) -> Vec<String> {
    shown(ui).into_iter().flat_map(|(_, t)| t).collect()
}

fn search(ui: &ComponentInstance, query: &str) {
    ui.set_property("plugin-query", Value::String(query.into())).unwrap();
    ui.invoke("plugins-filter-changed", &[]).unwrap();
}

fn there(ui: &ComponentInstance, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).next().is_some()
}

#[test]
fn the_search_finds_by_name_and_description_in_the_language_shown() {
    telescope_core::i18n::set_translator(|m| match m {
        "Presets" => Some("Voreinstellungen".into()),
        "What Presets does." => Some("Speichert Einstellungen.".into()),
        _ => None,
    });
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("plugins-language");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| grouped(), None, None).unwrap();
    settings::open(&w, "plugins");
    let s = settings::current().unwrap();
    // Only what's inside the page's clip can be found
    s.ui.window().set_size(slint::LogicalSize::new(800.0, 3000.0));
    assert_eq!(tiles(&s.ui).len(), 7);
    assert!(there(&s.ui, "Install"), "the added box is there to start with");
    search(&s.ui, "vorein");
    assert_eq!(shown(&s.ui), [("Extras".to_string(), vec!["Voreinstellungen".to_string()])]);
    search(&s.ui, "speichert");
    assert_eq!(tiles(&s.ui), ["Voreinstellungen"]);
    search(&s.ui, "presets");
    assert!(tiles(&s.ui).is_empty(), "the English isn't what's shown");
    search(&s.ui, "picture bigger");
    assert_eq!(tiles(&s.ui), ["Zoom tool"]);
    assert!(!there(&s.ui, "Install") && !there(&s.ui, "Show all plugins"), "neither the added box nor the empty note");
    search(&s.ui, "nothing like it");
    assert!(tiles(&s.ui).is_empty());
    assert!(there(&s.ui, "No plugins match “nothing like it”."));
    ElementHandle::find_by_accessible_label(&s.ui, "Show all plugins").next().expect("the button").invoke_accessible_default_action();
    assert_eq!(s.ui.get_property("plugin-query").unwrap(), Value::String("".into()));
    assert_eq!(tiles(&s.ui).len(), 7);
    assert!(there(&s.ui, "Install"));
    settings::forget();
    telescope_core::i18n::set_translator(|_| None);
}
