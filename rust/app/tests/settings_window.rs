// The settings window (tests/test_settings_window.py), with sections of the test's own in a real one.
mod rig;

use rig::Rig;
use serde_json::json;
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::Value as SValue;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, ShortcutEntry, SlintSource, UiEvent, UiSpec};
use telescope_shell::settings::{self, Settings};

const SECTION: &str = r#"
import { Button, Card, SettingRow } from "@ui-kit/ui-kit.slint";
export component Section inherits Card {
    callback pressed;
    title: "Section";
    SettingRow { title: "Remove download"; Button { text: "Remove download"; clicked => { root.pressed(); } } }
}
"#;

type Heard = Arc<Mutex<Vec<String>>>;

/// A plugin with a section for each (id, tab). "broken" can't refresh itself.
struct Sections(Vec<(&'static str, &'static str)>, Heard);

impl Plugin for Sections {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "mine".into(), ..Default::default() }
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        self.0.iter().enumerate().map(|(i, (id, tab))| SettingsSection {
            id: (*id).into(), tab: (*tab).into(), order: i as i32, tab_title: String::new(), keywords: String::new(), footer: None,
            ui: UiSpec { source: SlintSource::Text(SECTION.into()), component: "Section".into(), props: json!({}) },
        }).collect()
    }
    fn on_ui(&mut self, _host: &mut dyn Host, event: &UiEvent) {
        assert!(!(event.ui == "broken" && event.callback == "shown"), "nope");
        self.1.lock().unwrap().push(format!("{} {}", event.ui, event.callback));
    }
}

fn opened(name: &str, sections: Vec<(&'static str, &'static str)>) -> (Rig, Rc<Settings>, Heard) {
    let heard = Heard::default();
    let mine = Sections(sections, heard.clone());
    let r = Rig::with(name, |_, _| vec![Box::new(mine) as Box<dyn Plugin>]);
    settings::forget();
    settings::open(&r.w, "general");
    let win = settings::current().expect("the settings window");
    win.ui.window().set_size(slint::LogicalSize::new(1000.0, 700.0));
    (r, win, heard)
}

fn shown(heard: &Heard) -> Vec<String> {
    heard.lock().unwrap().iter().filter_map(|h| h.strip_suffix(" shown").map(str::to_string)).collect()
}

fn press(win: &Settings, key: impl Into<slint::SharedString>) {
    let text = key.into();
    win.ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    win.ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn typed(win: &Settings, text: &str) {
    for c in text.chars() {
        press(win, c.to_string());
    }
}

fn text(win: &Settings, name: &str) -> String {
    match win.ui.get_property(name) { Ok(SValue::String(s)) => s.to_string(), other => panic!("{name}: {other:?}") }
}

#[test]
fn show_tab_switches_and_refuses_one_that_isnt_there() {
    let (_r, win, _) = opened("settings-show-tab", vec![("one", "general"), ("two", "video")]);
    assert_eq!(win.current_tab(), "general");
    assert!(win.show_tab("video"));
    assert_eq!(win.current_tab(), "video");
    assert_eq!(text(&win, "page-title"), "Video");
    assert!(!win.show_tab("audio"));
    assert_eq!(win.current_tab(), "video");
    settings::forget();
}

#[test]
fn a_section_hears_when_its_tab_comes_on_screen() {
    let (r, win, heard) = opened("settings-hears", vec![("one", "general"), ("two", "video")]);
    assert_eq!(shown(&heard), ["one"], "the one on screen, not the one behind it");
    win.show_tab("video");
    assert_eq!(shown(&heard), ["one", "two"]);
    win.show_tab("video");
    assert_eq!(shown(&heard), ["one", "two"], "it was on screen already");
    settings::open(&r.w, "");
    assert_eq!(shown(&heard), ["one", "two"], "raising the window shows nothing new");
    win.ui.hide().unwrap();
    settings::open(&r.w, "");
    assert_eq!(shown(&heard), ["one", "two", "two"]);
    typed(&win, "z");
    assert_eq!(shown(&heard).len(), 3, "results cover the tab");
    press(&win, Key::Backspace);
    assert_eq!(shown(&heard), ["one", "two", "two", "two"], "and clearing them shows it again");
    settings::forget();
}

#[test]
fn a_section_that_cant_refresh_doesnt_break_the_window() {
    let (_r, win, heard) = opened("settings-cant-refresh", vec![("broken", "general"), ("fine", "general")]);
    assert_eq!(shown(&heard), ["fine"]);
    assert!(win.ui.window().is_visible());
    settings::forget();
}

#[test]
fn it_stays_inside_the_screen() {
    let (_r, win, _) = opened("settings-screen", vec![("one", "general")]);
    let size = |win: &Settings| { let s = win.ui.window().size(); (s.width, s.height) };
    let length = |win: &Settings, name: &str| match win.ui.get_property(name) { Ok(SValue::Number(n)) => n, other => panic!("{other:?}") };
    win.fit_to_screen((1920.0, 1080.0));
    assert_eq!(size(&win), (940, 680), "its usual size");
    win.fit_to_screen((800.0, 500.0));
    assert_eq!(size(&win), (752, 452), "clear of the edges");
    assert_eq!((length(&win, "least-width"), length(&win, "least-height")), (720.0, 452.0), "a small screen wins over the minimum");
    settings::forget();
}

#[test]
fn opening_a_search_result_does_not_click_the_setting() {
    let (_r, win, heard) = opened("settings-open-result", vec![("one", "general")]);
    typed(&win, "remove download");
    assert_eq!(text(&win, "query"), "remove download");
    press(&win, Key::Return);
    assert_eq!(text(&win, "query"), "");
    assert_eq!(win.ui.get_property("searching").unwrap(), SValue::Bool(false));
    assert!(!heard.lock().unwrap().iter().any(|h| h.ends_with("pressed")), "{:?}", heard.lock().unwrap());
    settings::forget();
}

#[test]
fn enter_on_a_focused_button_still_runs_it() {
    let (_r, win, heard) = opened("settings-enter", vec![("one", "general")]);
    let pressed = || heard.lock().unwrap().iter().filter(|h| h.ends_with("pressed")).count();
    for _ in 0..12 {
        press(&win, Key::Tab);
        press(&win, Key::Return);
        if pressed() > 0 {
            break;
        }
    }
    assert_eq!(pressed(), 1, "Tab reaches the button and Enter runs it");
    settings::forget();
}

struct Menu(Heard);

impl Plugin for Menu {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "keys".into(), ..Default::default() }
    }
    fn setup(&mut self, host: &mut dyn Host) {
        host.set_shortcut_menu(Some(vec![ShortcutEntry { id: "one".into(), action: "mute".into(), text: "Ctrl+M: Mute".into() }]));
    }
    fn on_ui(&mut self, _host: &mut dyn Host, event: &UiEvent) {
        self.0.lock().unwrap().push(format!("{} {} {}", event.ui, event.callback, event.args[0]));
    }
}

#[test]
fn the_main_window_answers_the_right_click_menu() {
    let heard = Heard::default();
    let menu = Menu(heard.clone());
    let r = Rig::with("shortcut-menu", |_, _| vec![Box::new(menu) as Box<dyn Plugin>]);
    r.pump();
    assert_eq!(r.w.ui.get_global_property("Shortcuts", "available"), Ok(SValue::Bool(true)));
    let SValue::Model(rows) = r.w.ui.invoke_global("Shortcuts", "bindings-for", &[SValue::String("mute".into()), SValue::Number(0.0)]).unwrap() else { panic!("the rows") };
    assert_eq!(slint::Model::row_count(&rows), 1);
    r.w.ui.invoke_global("Shortcuts", "add", &[SValue::String("mute".into())]).unwrap();
    assert_eq!(*heard.lock().unwrap(), ["shortcut-menu add \"mute\""]);
}

#[test]
fn the_main_window_cuts_a_capped_field() {
    let r = Rig::new("text-cap");
    let said = r.w.ui.invoke_global("TextCap", "fit", &[SValue::String("ab".into()), SValue::String("a123b".into()), SValue::Number(3.0)]).unwrap();
    assert_eq!(said, SValue::String("a1b".into()));
}
