// The tray and closing the window: what stays running, and what the plugins hear about the window being on screen.
mod rig;

use rig::Rig;
use serde_json::json;
use slint::ComponentHandle;
use std::sync::{Arc, Mutex};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::TrayAction;

#[derive(Default)]
struct Muting(Arc<Mutex<Vec<String>>>);

impl Plugin for Muting {
    fn on_tray_action(&mut self, _host: &mut dyn Host, action: &str) {
        self.0.lock().unwrap().push(action.to_string());
    }
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "muting".into(), title: "Muting".into(), ..Default::default() }
    }
    fn tray_actions(&mut self) -> Vec<TrayAction> {
        vec![TrayAction { id: "mute".into(), title: "Mute microphone".into(), checkable: false, checked: false, visible: true }]
    }
}

fn close(r: &Rig) {
    r.w.close_requested();
    r.pump();
}

fn quits() -> usize {
    telescope_core::diagnostics::events().lines(10_000).iter().filter(|l| l.contains("QUIT")).count()
}

fn click(r: &Rig, what: &str) {
    r.tray.lock().unwrap().clicks.push(what.into());
    r.w.poll_tray();
    r.pump();
}

#[test]
fn the_tray_icon_says_what_streams() {
    let r = Rig::built_with_mic_tray("tray-says");
    r.emit(telescope_core::event::Event::MicChanged { enabled: true, muted: false });
    r.pump();
    r.w.refresh_tray_tip();
    r.start();
    r.pump();
    r.w.refresh_tray_tip();
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.refresh_tray_tip();
    r.emit(telescope_core::event::Event::MicChanged { enabled: true, muted: true });
    r.pump();
    r.w.refresh_tray_tip();
    r.stop(None);
    r.pump();
    r.w.refresh_tray_tip();
    assert_eq!(r.tray.lock().unwrap().tips, ["Telescope", "Telescope: streaming the camera and mic", "Telescope: streaming the mic, camera off",
                                             "Telescope: streaming the mic, camera off (muted)", "Telescope"]);
}

#[test]
fn tray_show_quit_and_activation() {
    let r = Rig::with_tray("tray-activation");
    r.w.show_window();
    assert!(r.w.ui.window().is_visible());
    click(&r, ""); // the icon, with the window up
    assert!(!r.w.ui.window().is_visible());
    click(&r, "");
    assert!(r.w.ui.window().is_visible());
    let before = quits();
    click(&r, "quit");
    assert_eq!(quits(), before + 1);
}

#[test]
fn plugin_tray_actions_go_between_show_and_quit() {
    let r = Rig::built_tray("tray-plugins", |_, _| vec![Box::new(Muting::default()) as Box<dyn Plugin>]);
    r.w.refresh_tray_menu();
    assert_eq!(r.tray.lock().unwrap().items, ["Show", "Mute microphone", "Quit"]);
}

#[test]
fn a_plugins_tray_action_reaches_it() {
    let clicked = Muting::default();
    let heard = clicked.0.clone();
    let r = Rig::built_tray("tray-plugin-click", move |_, _| vec![Box::new(clicked) as Box<dyn Plugin>]);
    click(&r, "muting/mute");
    assert_eq!(*heard.lock().unwrap(), ["mute"]);
    click(&r, "nobody/mute");
    assert_eq!(heard.lock().unwrap().len(), 1);
}

#[test]
fn close_event_minimizes_active_stream_to_tray() {
    let r = Rig::with_tray("tray-close-streaming");
    r.w.show_window();
    r.start();
    r.pump();
    let before = quits();
    close(&r);
    assert_eq!(r.streaming(), ["pixel"]);
    assert_eq!(quits(), before);
    close(&r);
    let notes = r.notes.0.lock().unwrap();
    assert_eq!(notes.len(), 1, "it's said once");
    assert!(!notes[0].2, "informational, shouldn't pin itself on screen");
}

#[test]
fn close_event_stops_and_accepts_without_background_stream() {
    let r = Rig::with_tray("tray-close-idle");
    r.w.show_window();
    let before = quits();
    close(&r);
    assert_eq!(quits(), before + 1);
    assert!(r.notes.0.lock().unwrap().is_empty());
}

#[test]
fn waiting_to_start_by_itself_keeps_running_in_the_tray() {
    let r = Rig::with_tray("tray-keep");
    r.w.show_window();
    r.w.state.borrow_mut().keep_in_tray = true;
    let before = quits();
    close(&r);
    assert_eq!(quits(), before);
    assert!(r.notes.0.lock().unwrap()[0].1.contains("starts streaming by itself"));
}

#[test]
fn start_hidden_minimizes_without_a_tray() {
    let r = Rig::with_tray("tray-hidden");
    r.w.start_hidden();
    assert!(!r.w.ui.window().is_minimized());
    drop(r);
    let r = Rig::new("tray-hidden-none");
    r.w.start_hidden();
    assert!(r.w.ui.window().is_minimized());
}

#[test]
fn the_window_says_when_it_is_on_screen() {
    // Plugins stop checking the phone while nobody looks: in the tray, minimized, or started hidden
    let r = Rig::with_tray("tray-shown");
    r.w.start_hidden();
    r.w.show_window();
    r.w.ui.window().set_minimized(true);
    r.w.report_shown();
    r.w.show_window();
    r.w.ui.hide().unwrap();
    r.w.report_shown();
    r.w.process();
    let seen: Vec<String> = r.heard().into_iter().filter_map(|l| l.strip_prefix("panel shown ").map(String::from)).collect();
    assert_eq!(seen, ["false", "true", "false", "true", "false"]);
}

#[test]
fn a_setting_changed_just_before_quit_is_saved() {
    let r = Rig::with("quit-saves", |_, _| vec![Box::new(rig::Output { config: json!({"fps": 1}) }) as Box<dyn telescope_core::plugin::Plugin>]);
    r.w.state.borrow_mut().effects.push(telescope_shell::shell::Effect::SaveSoon);
    r.w.process();
    r.w.shut_down();
    assert_eq!(r.w.state.borrow().file.load()["selected_device"], serde_json::Value::Null);
    assert!(r.w.state.borrow().file.path().exists(), "the save that was waiting went out");
}
