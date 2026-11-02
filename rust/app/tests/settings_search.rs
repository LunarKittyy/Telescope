// Using the search box (tests/test_settings_search.py), with a section of the test's own in a real settings window.
mod rig;

use rig::Rig;
use serde_json::json;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};
use slint_interpreter::Value as SValue;
use std::rc::Rc;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, SlintSource, UiSpec};
use telescope_shell::settings::{self, Settings};

const CARD: &str = r#"
import { Card, SettingRow } from "@ui-kit/ui-kit.slint";
export component Sparkles inherits Card {
    in property <bool> more;
    title: "Sparkles";
    keywords: "glitter shimmer";
    SettingRow { title: "Max zoom"; keywords: "magnify"; }
    SettingRow { title: "Shown thing"; }
    SettingRow { title: "Hidden thing"; visible: false; }
    if more: SettingRow { title: "Extra zoom"; }
}
"#;

struct Mine;

impl Plugin for Mine {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "mine".into(), ..Default::default() }
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: "card".into(), tab: "general".into(), order: 0, tab_title: String::new(), keywords: "twinkly".into(), footer: None,
            ui: UiSpec { source: SlintSource::Text(CARD.into()), component: "Sparkles".into(), props: json!({}) },
        }]
    }
}

fn opened(name: &str) -> (Rig, Rc<Settings>) {
    let r = Rig::with(name, |_, _| vec![Box::new(Mine) as Box<dyn Plugin>]);
    settings::forget();
    settings::open(&r.w, "general");
    let win = settings::current().expect("the settings window");
    win.ui.window().set_size(slint::LogicalSize::new(1000.0, 700.0));
    (r, win)
}

fn names(win: &Settings, query: &str) -> Vec<String> {
    win.search_results(query).into_iter().map(|e| e.name).collect()
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

fn cleared(win: &Settings) {
    let n = match win.ui.get_property("query") { Ok(SValue::String(q)) => q.chars().count(), _ => 0 };
    for _ in 0..n {
        press(win, Key::Backspace);
    }
}

fn prop(win: &Settings, name: &str) -> SValue {
    win.ui.get_property(name).unwrap()
}

fn text(win: &Settings, name: &str) -> String {
    match prop(win, name) { SValue::String(s) => s.to_string(), other => panic!("{name}: {other:?}") }
}

fn number(win: &Settings, name: &str) -> f64 {
    match prop(win, name) { SValue::Number(n) => n, other => panic!("{name}: {other:?}") }
}

fn results(win: &Settings) -> Vec<String> {
    let SValue::Model(rows) = prop(win, "results") else { panic!("the results") };
    rows.iter().map(|row| {
        let SValue::Struct(row) = row else { panic!("a result") };
        match row.get_field("name") { Some(SValue::String(s)) => s.to_string(), _ => panic!("a name") }
    }).collect()
}

#[test]
fn a_plugins_own_section_is_searchable_without_doing_anything() {
    let (_r, win) = opened("search-own-section");
    assert_eq!(names(&win, "glitter"), ["Sparkles", "Max zoom", "Shown thing"], "a card's words find it, then what's under it");
    assert_eq!(names(&win, "magnify"), ["Max zoom"], "a row's own words find only that row");
    assert!(names(&win, "twinkly").contains(&"Sparkles".to_string()), "the section's words");
    assert!(!names(&win, "sparkels").is_empty());
    settings::forget();
}

#[test]
fn hidden_rows_arent_offered() {
    let (_r, win) = opened("search-hidden");
    assert_eq!(names(&win, "thing"), ["Shown thing"]);
    settings::forget();
}

#[test]
fn a_search_reads_the_settings_once_when_it_starts() {
    let (_r, win) = opened("search-once");
    typed(&win, "extra");
    assert!(!results(&win).contains(&"Extra zoom".to_string()));
    win.set_props("mine", "card", &json!({"more": true}));
    typed(&win, " z");
    assert!(!results(&win).contains(&"Extra zoom".to_string()), "typing on looks through what the search began with");
    cleared(&win);
    typed(&win, "extra");
    assert_eq!(results(&win)[0], "Extra zoom", "a new search reads it again: rows come and go");
    settings::forget();
}

#[test]
fn typing_shows_results_and_clearing_goes_back_to_the_tab() {
    let (_r, win) = opened("search-typing");
    win.show_tab("plugins");
    typed(&win, "zoom");
    assert_eq!(prop(&win, "searching"), SValue::Bool(true));
    assert!(!results(&win).is_empty());
    assert_eq!(text(&win, "page-title"), "Search results");
    assert_eq!(win.current_tab(), "plugins", "where it was is remembered");
    cleared(&win);
    assert_eq!(prop(&win, "searching"), SValue::Bool(false));
    assert_eq!(text(&win, "page-title"), "Plugins");
    settings::forget();
}

#[test]
fn opening_a_result_goes_to_its_tab_and_lights_the_setting() {
    let (_r, win) = opened("search-open");
    win.show_tab("plugins");
    typed(&win, "max zoom");
    press(&win, Key::Return);
    assert_eq!(win.current_tab(), "general");
    assert_eq!(text(&win, "query"), "");
    assert_eq!(prop(&win, "flashing"), SValue::Bool(true));
    assert!(number(&win, "flash-y") >= 0.0 && number(&win, "flash-h") > 0.0 && number(&win, "flash-w") > 0.0);
    settings::forget();
}

#[test]
fn nothing_found_says_so_and_offers_a_way_on() {
    let (_r, win) = opened("search-nothing");
    typed(&win, "zzzzzz");
    assert!(text(&win, "results-empty").contains("Nothing matches"));
    assert!(results(&win).is_empty());
    cleared(&win);
    typed(&win, "zoom");
    assert_eq!(text(&win, "results-empty"), "");
    assert!(!results(&win).is_empty());
    settings::forget();
}

#[test]
fn when_no_entry_has_every_word_the_ones_with_some_are_offered_and_say_so() {
    let (_r, win) = opened("search-partial");
    assert_eq!(names(&win, "zoom meeting")[..1], ["Max zoom"]);
    typed(&win, "zoom meeting");
    assert_eq!(text(&win, "results-note"), "Nothing matches every word. These match some of them.");
    assert!(!results(&win).is_empty());
    cleared(&win);
    typed(&win, "max zoom");
    assert_eq!(text(&win, "results-note"), "");
    settings::forget();
}

#[test]
fn arrow_keys_and_enter_work_from_the_search_box() {
    let (_r, win) = opened("search-keys");
    typed(&win, "s");
    assert!(results(&win).len() >= 2, "{:?}", results(&win));
    assert_eq!(number(&win, "result"), 0.0);
    press(&win, Key::DownArrow);
    assert_eq!(number(&win, "result"), 1.0);
    press(&win, Key::UpArrow);
    press(&win, Key::UpArrow);
    assert_eq!(number(&win, "result"), 0.0);
    press(&win, Key::Escape);
    assert_eq!(text(&win, "query"), "", "clears first");
    assert!(win.ui.window().is_visible(), "closes on the next one");
    settings::forget();
}

#[test]
fn clicking_a_tab_while_searching_leaves_the_search() {
    let (_r, win) = opened("search-tab");
    typed(&win, "zoom");
    win.ui.invoke("tab-picked", &[SValue::Number(win.tabs().iter().position(|t| t == "plugins").unwrap() as f64)]).unwrap();
    assert_eq!(text(&win, "query"), "");
    assert_eq!(prop(&win, "searching"), SValue::Bool(false));
    assert_eq!(text(&win, "page-title"), "Plugins");
    settings::forget();
}
