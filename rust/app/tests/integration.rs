//! The real plugins inside the real shell State, with nothing faked between them: what fake hosts can hide shows up
//! here (a save asked for mid-hook, a setting that never reaches the file). Only the window and the phone are left out;
//! the test plays the phone over the pairing server's real HTTP.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::ui::UiEvent;
use telescope_shell::shell::{Effect, State};

/// The pairing server takes a fixed port first, so runs that pair go one at a time.
static PAIRING: Mutex<()> = Mutex::new(());

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-integration-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A dev profile with the parity offer file on (Add phone writes its QR content there) and no adb, so a phone on this
/// computer's cable stays out of it. Every test sets the same values, so it doesn't matter which runs first.
fn environment() -> PathBuf {
    let base = std::env::temp_dir().join(format!("telescope-integration-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var(telescope_core::config::DEV_PROFILE_ENV, &base);
    std::env::set_var("TELESCOPE_PARITY_OFFER", base.join("offer.json"));
    std::env::set_var("TELESCOPE_ADB", base.join("no-adb"));
    base
}

struct App {
    state: State,
    file: PathBuf,
    /// Timers plugins asked for, fired by tick() rather than waited out.
    timers: Vec<(String, String)>,
}

impl App {
    /// As window::build does it, minus the window: the plugins registered, the saved settings applied.
    fn start(dir: &Path, plugins: impl FnOnce(Arc<telescope_link::LinkService>) -> Vec<Box<dyn Plugin>>) -> App {
        let file = dir.join("telescope_config.json");
        let mut state = State::new(ConfigFile::at(&file));
        let link = Arc::new(telescope_link::LinkService::new());
        state.link = Some(link.clone());
        state.load_builtins(plugins(link));
        state.apply_saved_config();
        let mut app = App { state, file, timers: Vec::new() };
        app.settle();
        app
    }

    /// Events, posts and the effects the window would carry out that matter here (saves and timers), until quiet.
    fn settle(&mut self) {
        for _ in 0..50 {
            self.state.deliver_events();
            let effects = std::mem::take(&mut self.state.effects);
            if effects.is_empty() && self.state.bus.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    Effect::SaveSoon => {
                        self.state.save_now();
                    }
                    Effect::Schedule { plugin, token, .. } => {
                        self.timers.retain(|t| *t != (plugin.clone(), token.clone()));
                        self.timers.push((plugin, token));
                    }
                    Effect::CancelSchedule { plugin, token } => self.timers.retain(|t| *t != (plugin.clone(), token.clone())),
                    _ => {}
                }
            }
        }
    }

    /// Fires the timers due now as if their time had come.
    fn tick(&mut self) {
        for (plugin, token) in std::mem::take(&mut self.timers) {
            self.state.deliver_to(&plugin, &Event::TimerFired { owner: plugin.clone(), token });
        }
        self.settle();
    }

    fn ui(&mut self, plugin: &str, ui: &str, callback: &str, args: Vec<Value>) {
        self.state.ui_event(plugin, UiEvent { ui: ui.into(), callback: callback.into(), args });
        self.settle();
    }

    /// Runs a plugin's shortcut action, as a key or `--action` would.
    fn action(&mut self, plugin: &str, id: &str, params: Value) -> Option<String> {
        let i = self.state.slots.iter().position(|s| s.info.name == plugin).unwrap();
        let said = self.state.call(i, |p, host| p.run_action(host, id, &params)).flatten();
        self.settle();
        said
    }

    fn on_disk(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.file).unwrap_or_else(|_| "{}".into())).unwrap()
    }

    fn phones_on_disk(&self) -> Vec<Value> {
        self.on_disk().pointer("/plugin_configs/connection/phones").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}; on disk: {}", self.on_disk());
            std::thread::sleep(Duration::from_millis(40));
            self.tick();
        }
    }
}

/// What the phone's app does after scanning the QR code: POST /pair/<nonce> with a proof of the token.
fn pair_as_phone(offer: &Value, phone_id: &str, name: &str) -> u16 {
    let (nonce, token) = (offer["nonce"].as_str().unwrap(), offer["token"].as_str().unwrap());
    let cert = "ab".repeat(32);
    let body = json!({"name": name, "phone_id": phone_id, "cert_sha256": cert, "ips": ["127.0.0.9"],
                      "proof": telescope_link::pairing::pairing_proof(token, nonce, phone_id, &cert)}).to_string();
    let mut conn = TcpStream::connect(("127.0.0.1", offer["port"].as_u64().unwrap() as u16)).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(conn, "POST /pair/{nonce} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut reply = String::new();
    let _ = conn.read_to_string(&mut reply);
    reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn connection_only(link: Arc<telescope_link::LinkService>) -> Vec<Box<dyn Plugin>> {
    vec![Box::new(link.connection_plugin())]
}

/// Connection with plugins whose settings are kept per phone.
fn with_phone_settings(link: Arc<telescope_link::LinkService>) -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(link.connection_plugin()),
        Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(false)),
        Box::new(telescope_effects::transforms::TransformsPlugin::new()),
    ]
}

/// Held by a test while it pairs, so other pairings stay off the port.
fn pairing() -> MutexGuard<'static, ()> {
    PAIRING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Pairs a phone through Add phone and the pairing server (hold pairing() meanwhile).
fn pair(app: &mut App, base: &Path, phone_id: &str, name: &str) {
    let offer_file = base.join("offer.json");
    let _ = std::fs::remove_file(&offer_file);
    app.ui("connection", "card", "add-phone", vec![]);
    let offer: Value = serde_json::from_str(&std::fs::read_to_string(&offer_file).expect("Add phone wrote no offer")).unwrap();
    assert_eq!(pair_as_phone(&offer, phone_id, name), 200);
    app.until("the phone to be saved", |a| a.phones_on_disk().iter().any(|p| p["id"] == phone_id));
    app.ui("connection", "card", "add-closed", vec![]);
}

#[test]
fn pairing_renaming_and_forgetting_a_phone_reach_the_settings_file() {
    let base = environment();
    let dir = folder("phones");
    let mut app = App::start(&dir, with_phone_settings);
    let _pairing = pairing();
    pair(&mut app, &base, "ph-1", "Pixel");
    let phones = app.phones_on_disk();
    assert_eq!(phones.len(), 1);
    assert_eq!((phones[0]["name"].as_str(), phones[0]["cert_sha256"].as_str()), (Some("Pixel"), Some("ab".repeat(32).as_str())));
    assert!(phones[0]["token"].as_str().is_some_and(|t| !t.is_empty()), "the token the phone proved is kept");
    assert_eq!(app.on_disk()["selected_device"], json!("ph-1"), "a first phone is picked");

    app.ui("connection", "header", "rename", vec![json!("ph-1"), json!("  Work phone ")]);
    assert_eq!(app.phones_on_disk()[0]["name"], json!("Work phone"));

    // A setting kept for this phone, so forgetting it has something to take along
    app.action("transforms", "transforms.flip_horizontal", json!({"mode": "on"}));
    app.state.save_now();
    assert_eq!(app.on_disk().pointer("/devices/ph-1/plugin_configs/transforms/flip_h"), Some(&json!(true)), "{}", app.on_disk());

    app.ui("connection", "header", "remove", vec![json!("ph-1")]);
    // A later save (anything changing) mustn't bring the forgotten phone's settings back
    app.state.save_now();
    let cfg = app.on_disk();
    assert!(app.phones_on_disk().is_empty(), "forgotten: {cfg}");
    assert!(cfg.pointer("/devices/ph-1").is_none(), "its settings went with it: {cfg}");
    assert_eq!(cfg["selected_device"], Value::Null);
}

#[test]
fn a_restart_reads_back_what_was_saved() {
    let base = environment();
    let dir = folder("restart");
    {
        let mut app = App::start(&dir, connection_only);
        let _pairing = pairing();
        pair(&mut app, &base, "ph-2", "Galaxy");
        app.ui("connection", "header", "rename-computer", vec![json!("Studio PC")]);
        app.state.save_now();
    }
    let app = App::start(&dir, connection_only);
    let cfg = app.state.plugin_config("connection").unwrap();
    assert_eq!(cfg["phones"][0]["name"], json!("Galaxy"));
    assert_eq!(cfg["computer_name"], json!("Studio PC"));
    assert_eq!(app.state.selected.as_deref(), Some("ph-2"));
}

fn transforms(app: &App) -> Value {
    app.state.plugin_config("transforms").unwrap()
}

#[test]
fn each_phone_keeps_its_own_settings_when_picking_between_them() {
    let base = environment();
    let dir = folder("two-phones");
    let mut app = App::start(&dir, with_phone_settings);
    let _pairing = pairing();
    pair(&mut app, &base, "ph-a", "Pixel");
    pair(&mut app, &base, "ph-b", "Galaxy");
    assert_eq!(app.state.selected.as_deref(), Some("ph-b"), "a newly paired phone gets the panels");
    app.action("transforms", "transforms.flip_vertical", json!({"mode": "on"}));
    assert_eq!(transforms(&app)["flip_v"], json!(true));

    // Connection's own action, as a key would: the panels move to the other phone and its settings come back
    app.action("connection", "phone.pick", json!({"op": "next"}));
    assert_eq!(app.state.selected.as_deref(), Some("ph-a"));
    assert_eq!(transforms(&app)["flip_v"], json!(false), "ph-a never had it on");
    let cfg = app.on_disk();
    assert_eq!(cfg["selected_device"], json!("ph-a"));
    assert_eq!(cfg.pointer("/devices/ph-b/plugin_configs/transforms/flip_v"), Some(&json!(true)), "ph-b's kept: {cfg}");

    app.action("connection", "phone.pick", json!({"op": "next"}));
    assert_eq!(transforms(&app)["flip_v"], json!(true), "ph-b's back with it");

    // Forgetting the picked one moves to the other without saving the forgotten one's settings back anywhere
    app.ui("connection", "header", "remove", vec![json!("ph-b")]);
    app.state.save_now();
    let cfg = app.on_disk();
    assert_eq!(app.state.selected.as_deref(), Some("ph-a"));
    assert_eq!(transforms(&app)["flip_v"], json!(false));
    assert!(cfg.pointer("/devices/ph-b").is_none(), "{cfg}");
    assert_eq!(cfg.pointer("/plugin_configs/connection/phones/0/id"), Some(&json!("ph-a")));
}

#[test]
fn a_shortcut_runs_another_plugins_action_and_its_setting_is_saved() {
    let base = environment();
    let dir = folder("shortcuts");
    std::fs::write(dir.join("telescope_config.json"), json!({"version": 3, "plugin_configs": {"shortcuts": {"bindings": [
        {"id": "s1", "keys": "Ctrl+Alt+H", "action": "transforms.flip_horizontal", "params": {"mode": "toggle"}, "global": false}
    ]}}}).to_string()).unwrap();
    let mut app = App::start(&dir, |link| {
        let mut plugins = with_phone_settings(link);
        plugins.push(Box::new(telescope_system::shortcuts::ShortcutsPlugin::with_backend(Default::default(), Box::new(|_| None))));
        plugins
    });
    let _pairing = pairing();
    pair(&mut app, &base, "ph-k", "Pixel");
    // The window hands every key to every plugin; Shortcuts takes this one and runs Transforms' action through the host
    let key = telescope_core::ui::KeyInput { text: "h".into(), ctrl: true, alt: true, ..Default::default() };
    let i = app.state.slots.iter().position(|s| s.info.name == "shortcuts").unwrap();
    assert_eq!(app.state.call(i, |p, host| p.on_key(host, &key)), Some(true));
    let up = telescope_core::ui::KeyInput { released: true, ..key };
    app.state.call(i, |p, host| p.on_key(host, &up));
    app.settle();
    app.state.save_now();
    assert_eq!(transforms(&app)["flip_h"], json!(true));
    assert_eq!(app.on_disk().pointer("/devices/ph-k/plugin_configs/transforms/flip_h"), Some(&json!(true)));
    // The controls' right-click menu lists it, through the real host
    let (owner, entries) = app.state.shortcut_menu.clone().expect("Shortcuts set the menu");
    assert_eq!(owner, "shortcuts");
    assert_eq!((entries[0].id.as_str(), entries[0].action.as_str()), ("s1", "transforms.flip_horizontal"));
}
