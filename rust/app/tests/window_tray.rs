// The tray and closing the window: what stays running, and what the plugins hear about the window being on screen.
mod rig;

use rig::{Listener, Rig};
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

fn last_quit() -> String {
    telescope_core::diagnostics::events().lines(10_000).into_iter().rfind(|l| l.contains("QUIT")).unwrap_or_default()
}

fn click(r: &Rig, what: &str) {
    r.tray.lock().unwrap().clicks.push(what.into());
    r.w.poll_tray();
    r.pump();
}

#[test]
fn the_tray_icon_says_what_streams() {
    let r = Rig::built_with_mic_tray("tray-says");
    let settle = || {
        r.pump();
        r.w.poll_tray(); // the timer, as nothing else tells it
    };
    r.emit(telescope_core::event::Event::MicChanged { enabled: true, muted: false });
    settle();
    r.start();
    settle();
    r.w.set_camera_on(false, false);
    settle();
    r.emit(telescope_core::event::Event::MicChanged { enabled: true, muted: true });
    settle();
    r.stop(None);
    settle();
    assert_eq!(r.tray.lock().unwrap().tips, ["Telescope", "Telescope: streaming the camera and mic", "Telescope: streaming the mic, camera off",
                                             "Telescope: streaming the mic, camera off (muted)", "Telescope"]);
}

#[test]
fn tray_show_quit_and_activation() {
    let r = Rig::with_tray("tray-activation");
    click(&r, "show");
    assert!(r.w.ui.window().is_visible() && !r.w.ui.window().is_minimized());
    click(&r, ""); // the icon, with the window up
    assert!(!r.w.ui.window().is_visible());
    click(&r, "");
    assert!(r.w.ui.window().is_visible());
    r.start();
    let before = quits();
    click(&r, "quit");
    assert_eq!(quits(), before + 1);
    assert!(last_quit().ends_with("The user quit from the tray"), "{}", last_quit());
    assert!(r.w.state.borrow().tray_close_notified);
    r.w.shut_down();
    assert!(r.streaming().is_empty(), "the stop comes with it");
}

#[test]
fn plugin_tray_actions_go_between_show_and_quit() {
    let r = Rig::built_tray("tray-plugins", |_, _| vec![Box::new(Muting::default()) as Box<dyn Plugin>]);
    r.w.refresh_tray_menu();
    assert_eq!(r.tray.lock().unwrap().items, ["Show", "Mute microphone", "", "Quit"]);
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
    assert_eq!(r.notes.0.lock().unwrap().len(), 1);
    assert!(!r.notes.0.lock().unwrap()[0].2, "informational, shouldn't pin itself on screen");
    close(&r);
    assert_eq!(r.notes.0.lock().unwrap().len(), 1, "it's said once");
}

#[test]
fn close_event_stops_and_accepts_without_background_stream() {
    let r = Rig::new("tray-close-idle");
    r.w.show_window();
    r.start();
    let before = quits();
    close(&r);
    assert_eq!(quits(), before + 1);
    assert!(last_quit().ends_with("The user closed the window"), "{}", last_quit());
    assert!(r.notes.0.lock().unwrap().is_empty());
    r.w.shut_down();
    assert!(r.streaming().is_empty(), "the stop comes with it");
}

#[test]
fn waiting_to_start_by_itself_keeps_running_in_the_tray() {
    let r = Rig::with_tray("tray-keep");
    r.w.show_window();
    r.w.state.borrow_mut().keep_in_tray = true;
    let before = quits();
    close(&r);
    assert_eq!(quits(), before);
    assert!(r.notes.0.lock().unwrap()[0].1.contains("Streaming starts automatically"));
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
    r.w.ui.window().set_minimized(true); // by the window manager, so nothing here is told
    r.w.watch_focus();
    r.w.ui.window().set_minimized(false);
    r.w.watch_focus();
    r.w.ui.hide().unwrap();
    r.w.watch_focus();
    r.w.process();
    let seen: Vec<String> = r.heard().into_iter().filter_map(|l| l.strip_prefix("panel shown ").map(String::from)).collect();
    assert_eq!(seen, ["false", "true", "false", "true", "false"]);
}

#[test]
fn a_setting_changed_just_before_quit_is_saved() {
    let r = Rig::with("quit-saves", |_, heard| {
        let mut plugin = Listener::new("global", heard);
        plugin.global = true;
        plugin.config = json!({"a": 1});
        vec![Box::new(plugin) as Box<dyn Plugin>]
    });
    r.configure("global", json!({"a": 2}));
    r.w.schedule_save();
    r.w.shut_down();
    assert_eq!(r.w.state.borrow().file.load()["plugin_configs"]["global"], json!({"a": 2}));
}
