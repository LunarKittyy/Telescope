// The Presets menu in the real window, clicked with pointer events: save, load, rename and delete take effect.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_shell::window;

/// A plugin with one setting that presets save and load.
struct Dial(Arc<Mutex<i64>>);

impl Plugin for Dial {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "dial".into(), title: "Dial".into(), in_presets: true, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        json!({"level": *self.0.lock().unwrap()})
    }
    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        *self.0.lock().unwrap() = cfg.get("level").and_then(Value::as_i64).unwrap_or(0);
        Ok(())
    }
}

fn click_at(w: &Rc<window::Window>, x: f32, y: f32) {
    let window = w.ui.window();
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    w.process();
    telescope_shell::offscreen::pump();
}

fn labelled(w: &window::Window, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(&w.ui, label).collect()
}

/// Where the pop-up's top left is, since its elements report their place inside it: the menu opens 6px under the Presets button, and
/// the Rename and Delete list is in the same pop-up, beside it.
fn popup_origin(w: &Rc<window::Window>, _beside: Option<usize>) -> (f32, f32) {
    let button = labelled(w, "Presets").into_iter().next().unwrap();
    let (p, s) = (button.absolute_position(), button.size());
    (p.x, p.y + s.height + 6.0)
}

/// Clicks the nth element with this label at its centre, like a mouse would; origin is where its pop-up starts.
fn press_in(w: &Rc<window::Window>, label: &str, nth: usize, origin: (f32, f32)) {
    telescope_shell::offscreen::draw(w.ui.window(), 1300.0, 900.0, 1.0);
    let all = labelled(w, label);
    let e = all.get(nth).unwrap_or_else(|| panic!("nothing named {label} #{nth}"));
    let (p, s) = (e.absolute_position(), e.size());
    click_at(w, origin.0 + p.x + s.width / 2.0, origin.1 + p.y + s.height / 2.0);
}

fn open_menu(w: &Rc<window::Window>) {
    press_in(w, "Presets", 0, (0.0, 0.0));
}

fn menu_row(w: &Rc<window::Window>, label: &str) {
    press_in(w, label, 0, popup_origin(w, None));
    // The menu eases out after a pick, and a prompt eases in
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
}

#[test]
fn the_menu_loads_renames_and_deletes_a_preset_by_clicking() {
    telescope_shell::offscreen::install();
    let dir = telescope_core::testing::scratch_dir("presets-menu");
    let level = Arc::new(Mutex::new(3));
    let plugin_level = level.clone();
    let w = window::build(
        ConfigFile::at(dir.join(CONFIG_FILENAME)),
        move |_| vec![Box::new(telescope_shell::plugins::presets::PresetsPlugin::new()), Box::new(Dial(plugin_level))],
        None,
        None,
    )
    .unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    w.ui.show().unwrap();
    w.process();

    // Save the current settings as a preset, then change the setting
    open_menu(&w);
    menu_row(&w, "Save current as…");
    assert!(!labelled(&w, "Save preset").is_empty(), "Save opens its prompt");
    press_in(&w, "Save", 0, (0.0, 0.0));
    // The prompt eases out before its pop-up goes
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
    assert!(labelled(&w, "Save preset").is_empty(), "the prompt closes");
    *level.lock().unwrap() = 9;

    // Clicking the preset loads it
    open_menu(&w);
    menu_row(&w, "Preset 1");
    assert_eq!(*level.lock().unwrap(), 3, "clicking the preset loads it");

    // A picture of the menu and of its Rename list, when asked for (CARD_SHOTS=dir)
    if let Ok(dir) = std::env::var("CARD_SHOTS") {
        let shot = |name: &str| {
            telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
            w.process();
            let pic = telescope_shell::offscreen::draw(w.ui.window(), 1300.0, 900.0, 2.0).expect("a picture");
            let (cw, ch) = (pic.width as usize / 2 - 100, 560usize);
            let mut rgba = Vec::new();
            for y in 0..ch {
                rgba.extend_from_slice(&pic.rgba[(y * pic.width as usize) * 4..(y * pic.width as usize + cw) * 4]);
            }
            telescope_shell::offscreen::Picture { width: cw as u32, height: ch as u32, rgba }.save_png(&std::path::Path::new(&dir).join(name)).unwrap();
        };
        open_menu(&w);
        shot("presets-menu.png");
        press_in(&w, "Rename", 0, popup_origin(&w, None));
        shot("presets-menu-rename.png");
        click_at(&w, 900.0, 700.0);
        telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
        w.process();
    }

    // Rename opens its prompt, and typing a name and confirming renames the preset
    open_menu(&w);
    press_in(&w, "Rename", 0, popup_origin(&w, None));
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
    press_in(&w, "Preset 1", 1, popup_origin(&w, None));
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
    assert!(!labelled(&w, "Rename preset").is_empty(), "Rename opens its prompt");
    for c in "Desk".chars() {
        w.ui.window().dispatch_event(WindowEvent::KeyPressed { text: c.to_string().into() });
        w.ui.window().dispatch_event(WindowEvent::KeyReleased { text: c.to_string().into() });
    }
    press_in(&w, "Rename", 0, (0.0, 0.0));
    // The prompt eases out before its pop-up goes
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
    assert!(labelled(&w, "Rename preset").is_empty(), "the prompt closes");
    open_menu(&w);
    assert!(!labelled(&w, "Desk").is_empty() && labelled(&w, "Preset 1").is_empty(), "the preset has its new name");

    // Delete removes it
    menu_row(&w, "Delete");
    press_in(&w, "Desk", 1, popup_origin(&w, None));
    telescope_shell::offscreen::skip(std::time::Duration::from_millis(400));
    w.process();
    open_menu(&w);
    assert!(labelled(&w, "Desk").is_empty(), "the preset is gone");
}

