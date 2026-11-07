// `telescope mcp` against a running Telescope: the real command (its stdin and stdout are what an AI app talks to) meets the real
// window with the real Colour grading, Transforms and MCP server plugins on the testing backend, through the single-instance socket
// and its control key. The window is this test, which turns it over while the command waits.
#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::cell::Cell;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::config::{self, ConfigFile, CONFIG_FILENAME, DEV_PROFILE_ENV};
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::plugin::Plugin;
use telescope_core::services::Services;
use telescope_core::source::PairedPhone;
use telescope_core::PixelFormat;
use telescope_shell::window::{self, Window};
use telescope_system::mcp::{McpHandle, McpPlugin};
use telescope_system::single_instance as si;

/// One at a time: the tests share a process-wide profile variable.
static ONE: Mutex<()> = Mutex::new(());

struct NoLink;

impl Link for NoLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no pairing in tests".into())
    }
}

struct OneCamera;

impl Outputs for OneCamera {
    fn slots(&self) -> Vec<Slot> {
        vec![Slot { slot: 0, name: "Telescope".into(), device: String::new() }]
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::Rgba]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no cameras in tests".into())
    }
}

struct NoTunnels;

impl telescope_link::connection::Tunnels for NoTunnels {
    fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
        None
    }
    fn release(&self, _serial: &str, _remote_port: u16) {}
}

/// A window with Connection, Colour grading, Transforms and the MCP server, all switched on.
fn window_with(folder: &Path, handle: McpHandle) -> Rc<Window> {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if !DONE.replace(true) {
        i_slint_backend_testing::init_no_event_loop();
    }
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(OneCamera), audio: None, notifier: None, autostart: None,
        updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let file = ConfigFile::at(folder.join(CONFIG_FILENAME));
    assert!(file.update(|cfg| cfg["plugins"] = json!({"on": ["colour_grading", "transforms", "mcp"]})));
    let w = window::build(file, move |_bus| {
        let link: Arc<dyn Link> = Arc::new(NoLink);
        let outputs: Arc<dyn Outputs> = Arc::new(OneCamera);
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(telescope_link::plugin::ConnectionPlugin::new(link, telescope_link::shared::Shared::new(Arc::new(NoTunnels))).with_outputs(outputs)),
            Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
            Box::new(telescope_effects::transforms::TransformsPlugin::new()),
            Box::new(McpPlugin::new(handle.clone())),
        ];
        telescope_shell::plugins::in_order(plugins)
    }, Some(services), None).unwrap();
    w.process();
    w
}

/// `telescope mcp` in this profile, with the lines it prints coming through a channel.
struct Command_ {
    child: Child,
    lines: Receiver<Value>,
}

impl Command_ {
    fn start(profile: &Path) -> Command_ {
        let mut child = Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("mcp").env(DEV_PROFILE_ENV, profile)
            .env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str::<Value>(&line).unwrap_or_else(|_| panic!("stdout is for JSON-RPC only: {line}")));
            }
        });
        Command_ { child, lines }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next message, turning the window over while it waits.
    fn next(&self, window: Option<&Rc<Window>>) -> Value {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(w) = window {
                w.process();
            }
            match self.lines.recv_timeout(Duration::from_millis(5)) {
                Ok(message) => return message,
                Err(_) if Instant::now() < until => {}
                Err(_) => panic!("no answer from telescope mcp in 20 s"),
            }
        }
    }

    fn call(&mut self, window: Option<&Rc<Window>>, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let answer = self.next(window);
        assert_eq!(answer["id"], json!(id), "{answer}");
        answer
    }

    fn tool(&mut self, window: Option<&Rc<Window>>, id: u64, name: &str, arguments: Value) -> (bool, String) {
        let answer = self.call(window, id, "tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        (result["isError"] == json!(true), result["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{answer}")).to_string())
    }
}

impl Drop for Command_ {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lock_for(profile: &Path, handle: &McpHandle) {
    std::env::set_var(DEV_PROFILE_ENV, profile);
    std::fs::create_dir_all(profile).unwrap();
    let si::Acquired::Lock(lock) = si::acquire(&si::Address::current(), Duration::ZERO) else { panic!("the lock") };
    let key = si::write_control_key(&si::control_key_path(&config::config_path()));
    assert!(key.is_some());
    let mcp = handle.clone();
    si::listen_with_mcp(lock, || {}, |_| json!({"ok": false, "text": "no actions in this test"}), move || telescope_system::mcp::accept(&mcp), key);
}

#[test]
fn an_ai_app_reads_and_changes_settings_of_the_real_window_through_the_command() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = telescope_core::testing::scratch_dir("mcp-e2e");
    let handle = McpHandle::default();
    lock_for(&dir.join("profile"), &handle);
    let w = window_with(&dir, handle.clone());
    let mut mcp = Command_::start(&dir.join("profile"));
    let grading = |w: &Rc<Window>| w.state.borrow().plugin_config("colour_grading").unwrap();

    let init = mcp.call(Some(&w), 1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("telescope"));
    mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let listed = mcp.call(Some(&w), 2, "tools/list", json!({}));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 11);
    assert_eq!(handle.clients(), 1, "the command is counted as a connected AI app");

    let (failed, status) = mcp.tool(Some(&w), 3, "status", json!({}));
    assert!(!failed, "{status}");
    let status: Value = serde_json::from_str(&status).unwrap();
    assert!(status["plugins_running"].as_array().unwrap().iter().any(|p| p["name"] == "colour_grading"), "{status}");

    let (failed, described) = mcp.tool(Some(&w), 4, "describe", json!({}));
    assert!(!failed, "{described}");
    let described: Value = serde_json::from_str(&described).unwrap();
    let plugins: Vec<&str> = described["plugins"].as_array().unwrap().iter().map(|p| p["plugin"].as_str().unwrap()).collect();
    assert_eq!(plugins, ["colour_grading", "transforms"]);
    let contrast = described["plugins"][0]["settings"].as_array().unwrap().iter().find(|s| s["id"] == "colour_grading.contrast").unwrap().clone();
    assert_eq!((contrast["min"].clone(), contrast["max"].clone(), contrast["value"].clone()), (json!(0.0), json!(2.0), json!(1.0)));

    // Bad input names every problem and changes nothing
    let (failed, why) = mcp.tool(Some(&w), 5, "set", json!({"changes": {"colour_grading.contrast": 9, "transforms.zoom": "far", "colour_grading.sparkle": 1}}));
    assert!(failed && why.starts_with("Nothing was changed.") && why.contains("out of range") && why.contains("sparkle"), "{why}");
    assert_eq!(grading(&w)["contrast"], json!(1.0));

    // A plugin that keeps something else than asked (panning at zoom 1) undoes the whole batch, the flip too, and revert has nothing to undo
    let (failed, why) = mcp.tool(Some(&w), 50, "set", json!({"changes": {"transforms.flip_h": true, "transforms.pan_x": 0.5}}));
    assert!(failed && why.starts_with("Nothing was changed.") && why.contains("pan_x"), "{why}");
    assert_eq!(w.state.borrow().plugin_config("transforms").unwrap()["flip_h"], json!(false), "the flip didn't stay");
    let (_, back) = mcp.tool(Some(&w), 51, "revert", json!({}));
    assert_eq!(serde_json::from_str::<Value>(&back).unwrap()["reverted"], json!([]));

    // Picking another phone can drop a call, so it isn't offered and isn't run, though Connection really lists it
    assert!(w.state.borrow().slots.iter().any(|s| s.actions.iter().any(|a| a.id == "phone.pick")), "Connection lists it");
    let (_, listed_actions) = mcp.tool(Some(&w), 52, "actions", json!({}));
    assert!(!listed_actions.contains("phone.pick") && listed_actions.contains("transforms.zoom"), "{listed_actions}");
    let (failed, why) = mcp.tool(Some(&w), 53, "run_action", json!({"action": "phone.pick", "params": {"op": "next"}}));
    assert!(failed && why.contains("left to the person"), "{why}");

    // Good input reaches the real plugin, several settings at once, and the rest stays
    let (failed, done) = mcp.tool(Some(&w), 6, "set", json!({"changes": {"colour_grading.contrast": 1.3, "colour_grading.masters.gain": 0.9, "transforms.flip_h": true}}));
    assert!(!failed, "{done}");
    let after = grading(&w);
    assert_eq!((after["contrast"].clone(), after["masters"]["gain"].clone()), (json!(1.3), json!(0.9)));
    assert_eq!(w.state.borrow().plugin_config("transforms").unwrap()["flip_h"], json!(true));
    let (_, got) = mcp.tool(Some(&w), 7, "get", json!({"ids": ["colour_grading.contrast", "transforms.flip_h"]}));
    assert_eq!(serde_json::from_str::<Value>(&got).unwrap()["values"], json!({"colour_grading.contrast": 1.3, "transforms.flip_h": true}));

    // Measuring goes over the bus to the real Colour grading, which has no stream to look at
    let (failed, why) = mcp.tool(Some(&w), 8, "measure", json!({}));
    assert!(failed && why.contains("No stream is running"), "{why}");

    // Stats are a stub in this build
    let (failed, why) = mcp.tool(Some(&w), 9, "stats", json!({}));
    assert!(failed && why.contains("not available in this build"), "{why}");

    let (failed, back) = mcp.tool(Some(&w), 10, "revert", json!({}));
    assert!(!failed, "{back}");
    assert_eq!(serde_json::from_str::<Value>(&back).unwrap()["reverted"].as_array().unwrap().len(), 3);
    assert_eq!(grading(&w)["contrast"], json!(1.0));
    assert_eq!(w.state.borrow().plugin_config("transforms").unwrap()["flip_h"], json!(false));

    drop(mcp);
    w.shut_down();
    assert!(!handle.is_on(), "switching the plugin off lets the socket turn connections away");
}

#[test]
fn with_the_server_off_or_telescope_not_running_the_command_still_answers_and_says_why() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = telescope_core::testing::scratch_dir("mcp-off");
    // Nothing running in this profile
    std::env::set_var(DEV_PROFILE_ENV, dir.join("nobody"));
    let mut mcp = Command_::start(&dir.join("nobody"));
    let init = mcp.call(None, 1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    assert!(init["result"]["instructions"].as_str().unwrap().contains("isn't running"), "{init}");
    let (failed, why) = mcp.tool(None, 2, "status", json!({}));
    assert!(failed && why.contains("isn't running"), "{why}");
    drop(mcp);

    // Running, with the MCP server switched off (nothing attached to its handle)
    let handle = McpHandle::default();
    lock_for(&dir.join("profile"), &handle);
    let mut mcp = Command_::start(&dir.join("profile"));
    let (failed, why) = mcp.tool(None, 1, "set", json!({"changes": {"colour_grading.contrast": 2}}));
    assert!(failed && why.contains("switched off") && why.contains("Plugins"), "{why}");
    assert_eq!(handle.clients(), 0);
    let listed = mcp.call(None, 2, "tools/list", json!({}));
    assert_eq!(listed["result"]["tools"], json!(telescope_system::mcp::tools()), "the tools are still offered while it's off, so a client that listed once keeps them");
}
