//! The MCP server plugin: lets an AI app such as Claude Desktop read Telescope's settings, change them and measure the picture. The AI app
//! starts `telescope mcp` (mcp_bridge), which talks to the running copy over the single-instance socket, past the same control key check
//! `--action` has. This is the other end: the JSON-RPC for MCP, the tools, and the plugin that runs them on the window's thread. Off by
//! default, and while it is off a connection is turned away with a line saying where to switch it on.
//!
//! Plugins don't know each other, so every tool goes through what any plugin can use: declared settings (`Plugin::settings`) changed through
//! the plugin's own `apply_preset`, the shortcut actions, Copy diagnostics, and the bus (`picture_measure_requested`, answered by whichever
//! plugin can measure the picture).

use crate::shortcuts;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::host::{Host, PluginSettings, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::settings::{merge_changes, SettingSpec};

pub const NAME: &str = "mcp";
const POST_CALL: &str = "call";
const POST_CLIENT: &str = "client";
/// A tool that never gets an answer from the window thread gives up after this.
const CALL_WAIT: Duration = Duration::from_secs(25);
/// A measuring that nobody answered at all (no plugin can measure) gives up after this, longer than the measuring itself takes.
const JOB_WAIT_MS: u64 = 10_000;
const MAX_LINE: u64 = 1 << 20;
const LATEST: &str = "2025-06-18";
const KNOWN: [&str; 3] = ["2024-11-05", "2025-03-26", LATEST];
/// Phone and stream controls that are hard to undo in a live call stay with the person: they have `--action` and the window.
const OUT_OF_REACH: [&str; 3] = ["streaming.start_stop", "streaming.camera", "window.show_hide"];

/// What a tool says: its data, or what went wrong.
pub type Reply = Result<Value, String>;

#[derive(Default)]
struct Link {
    poster: Option<Arc<dyn Poster>>,
    next: u64,
    waiting: HashMap<u64, Sender<Reply>>,
    clients: usize,
}

/// Connects the connections' threads to the plugin on the window's thread, as RemoteActions does for `--action`. Empty while the plugin is
/// off, which is how a connection finds out.
#[derive(Clone, Default)]
pub struct McpHandle(Arc<Mutex<Link>>);

impl McpHandle {
    /// The one this program's plugin and its socket share.
    pub fn global() -> McpHandle {
        static HANDLE: OnceLock<McpHandle> = OnceLock::new();
        HANDLE.get_or_init(McpHandle::default).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Link> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn attach(&self, poster: Option<Arc<dyn Poster>>) {
        self.lock().poster = poster;
    }

    /// The plugin went away: what waits for an answer is let go.
    fn detach(&self) {
        let mut l = self.lock();
        l.poster = None;
        l.waiting.clear();
    }

    pub fn is_on(&self) -> bool {
        self.lock().poster.is_some()
    }

    /// How many AI apps are connected now.
    pub fn clients(&self) -> usize {
        self.lock().clients
    }

    /// A connection came in: a guard that counts it until dropped, or why it can't be served (the plugin is off).
    pub fn open_client(&self) -> Result<Client, String> {
        let poster = {
            let mut l = self.lock();
            let Some(poster) = l.poster.clone() else {
                return Err("Telescope's MCP server is switched off. Switch on MCP server in Telescope's Plugins tab, then try again.".into());
            };
            l.clients += 1;
            poster
        };
        poster.post(POST_CLIENT, json!({"clients": self.clients()}));
        Ok(Client(self.clone()))
    }

    /// From a connection's thread: runs a tool on the window's thread and waits for its answer.
    pub fn call(&self, tool: &str, args: Value) -> Reply {
        let (tx, rx) = channel();
        let (poster, id) = {
            let mut l = self.lock();
            let Some(poster) = l.poster.clone() else { return Err(off()) };
            l.next += 1;
            let id = l.next;
            l.waiting.insert(id, tx);
            (poster, id)
        };
        poster.post(POST_CALL, json!({"id": id, "tool": tool, "args": args}));
        let answer = rx.recv_timeout(CALL_WAIT);
        self.lock().waiting.remove(&id);
        answer.unwrap_or_else(|e| Err(match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => "Telescope didn't answer in time.".into(),
            std::sync::mpsc::RecvTimeoutError::Disconnected => off(),
        }))
    }

    fn reply(&self, id: u64, reply: Reply) {
        if let Some(tx) = self.lock().waiting.remove(&id) {
            let _ = tx.send(reply);
        }
    }
}

fn off() -> String {
    "Telescope's MCP server was switched off.".into()
}

/// One connected AI app, counted until dropped.
pub struct Client(McpHandle);

impl Drop for Client {
    fn drop(&mut self) {
        let poster = {
            let mut l = self.0.lock();
            l.clients = l.clients.saturating_sub(1);
            l.poster.clone()
        };
        if let Some(poster) = poster {
            poster.post(POST_CLIENT, json!({"clients": self.0.clients()}));
        }
    }
}

/// For the single-instance socket: lets a `telescope mcp` connection in, or turns it away while the plugin is off.
pub fn accept(handle: &McpHandle) -> Result<crate::single_instance::McpServe, String> {
    let client = handle.open_client()?;
    let handle = handle.clone();
    Ok(Box::new(move |conn: crate::single_instance::Conn| {
        let Ok(writer) = conn.try_clone() else { return };
        let call: Arc<Caller> = Arc::new(move |tool, args| handle.call(tool, args));
        serve_connection(std::io::BufReader::new(conn), Arc::new(Mutex::new(writer)), call);
        drop(client);
    }))
}

// ── JSON-RPC ─────────────────────────────────────────────────────────

/// How a request for a tool reaches the window's thread.
pub type Caller = dyn Fn(&str, Value) -> Reply + Send + Sync;

fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": false, "openWorldHint": false},
    })
}

/// The tools, which don't depend on anything running: the same list whether or not a stream is up.
pub fn tools() -> Vec<Value> {
    let stats_args = json!({
        "window_seconds": {"type": "number", "minimum": 5, "maximum": 60, "description": "How far back to summarise, up to the last minute. Default 60."},
        "level": {"type": "string", "enum": ["standard", "nerd"], "description": "Standard has the numbers on the Stats panel, nerd the ones for Stats for nerds as well. Default standard."},
        "series": {"type": "array", "items": {"type": "string"}, "description": "Only these series (such as fps, decode_ms, bitrate), by name. Default all."},
    });
    vec![
        tool("status", "What Telescope is doing now: version, whether a phone is streaming, the sources, which plugins are on, and how many AI apps are connected. Start here.",
             json!({}), &[], true),
        tool("stats", "The Stats numbers (frame rate, decoding time, bitrate, drops and so on) over the last minute, each as min, average and max with the worst moments and when they happened, for finding what makes the picture lag. Not available in this build yet.",
             stats_args, &[], true),
        tool("watch", "Collects the Stats numbers for a while (5 to 120 seconds) while the person reproduces a problem, then summarises them like stats does. Not available in this build yet.",
             json!({"seconds": {"type": "number", "minimum": 5, "maximum": 120, "description": "How long to collect. Default 30."},
                    "level": {"type": "string", "enum": ["standard", "nerd"]}}), &[], true),
        tool("diagnostics", "The text Copy diagnostics gives, with recent log lines and the same personal details left out. Good for working out why something fails.",
             json!({}), &[], true),
        tool("measure", "Looks at the picture for about a second and gives it as numbers: the face's colour and brightness, whether white balance is off (a colour cast), how dark, bright and noisy it is and how much is clipped. Needs a stream running and Colour grading switched on. Ask the person to keep still. It measures the camera's own picture, before Telescope's changes, so use it to decide what to change, not to check a change afterwards.",
             json!({}), &[], true),
        tool("describe", "Lists the settings you can read and change, each with its id, what it does, its range, default and current value. Only plugins that are switched on show up; if one you expect is missing, ask the person to switch it on in Plugins.",
             json!({"plugin": {"type": "string", "description": "Only this plugin's settings, by name, such as colour_grading."}}), &[], true),
        tool("get", "Current values of settings by id (such as colour_grading.contrast). Without ids, every setting.",
             json!({"ids": {"type": "array", "items": {"type": "string"}}}), &[], true),
        tool("set", "Changes settings, by id, all at once so the camera shows no half-way steps. Every value is checked first and nothing changes unless all of them are fine. Changes show in the camera straight away, which matters in a live call, so make small steps. revert undoes them.",
             json!({"changes": {"type": "object", "description": "Setting id to new value, such as {\"colour_grading.contrast\": 1.1, \"colour_grading.on\": true}.", "additionalProperties": true}}),
             &["changes"], false),
        tool("revert", "Puts back what set changed, as it was before the first change this session. A setting the person has moved by hand since is left as they have it.",
             json!({}), &[], false),
        tool("actions", "Lists the actions Telescope can run, like the ones keyboard shortcuts run, with what each takes. Starting or stopping the stream and switching the phone's camera are left to the person.",
             json!({}), &[], true),
        tool("run_action", "Runs one action from actions with its parameters.",
             json!({"action": {"type": "string"}, "params": {"type": "object", "additionalProperties": true}}), &["action"], false),
    ]
}

fn instructions() -> &'static str {
    "Telescope sends a phone's camera to the computer as a virtual webcam. Use status first. describe lists every setting with its range, get reads them and set changes them (all at once, checked, live in the camera, undone by revert). measure reads the picture as numbers, which is how to choose colour grading. diagnostics has the text Copy diagnostics gives. Make small changes: the person may be in a call."
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// What a tool's reply is as MCP says it: text content, flagged when it went wrong.
fn tool_result(reply: Reply) -> Value {
    match reply {
        Ok(data) => json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&data).unwrap_or_default()}], "isError": false}),
        Err(text) => json!({"content": [{"type": "text", "text": text}], "isError": true}),
    }
}

/// The answer to one JSON-RPC message, or None for a notification, which gets none. Knows nothing of sockets or the window: the
/// tools run through `call`.
pub fn handle_message(message: &Value, call: &dyn Fn(&str, Value) -> Reply) -> Option<Value> {
    let id = message.get("id").filter(|id| !id.is_null())?;
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error(id, -32600, "That isn't a request: it has no method."));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = if KNOWN.contains(&asked) { asked } else { LATEST };
            result(id, json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "telescope", "version": crate::version::display_version()},
                "instructions": instructions(),
            }))
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({"tools": tools()})),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            if !tools().iter().any(|t| t["name"] == name) {
                return Some(error(id, -32602, &format!("There is no tool called {name:?}.")));
            }
            let args = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a) if a.is_object() => a.clone(),
                Some(_) => return Some(result(id, tool_result(Err("The arguments have to be an object.".into())))),
            };
            result(id, tool_result(call(name, args)))
        }
        other => error(id, -32601, &format!("Telescope's MCP server doesn't do {other}.")),
    })
}

fn send(writer: &Mutex<dyn Write + Send>, message: &Value) {
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    let _ = w.write_all(format!("{message}\n").as_bytes()).and_then(|()| w.flush());
}

/// One connection: a JSON-RPC message per line in, one per line out. A tool call can take seconds (measure), so each runs on a thread
/// of its own and the connection keeps answering meanwhile. Returns when the other end closes.
pub fn serve_connection(mut reader: impl BufRead, writer: Arc<Mutex<dyn Write + Send>>, call: Arc<Caller>) {
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with('\n') && line.len() as u64 >= MAX_LINE => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            send(&*writer, &error(&Value::Null, -32700, "That isn't JSON."));
            continue;
        };
        if message["method"] == "tools/call" {
            let (writer, call) = (writer.clone(), call.clone());
            std::thread::spawn(move || {
                if let Some(answer) = handle_message(&message, &*call) {
                    send(&*writer, &answer);
                }
            });
        } else if let Some(answer) = handle_message(&message, &*call) {
            send(&*writer, &answer);
        }
    }
}

// ── The plugin ───────────────────────────────────────────────────────

/// A setting this session changed through `set`: where it was, and where `set` left it, so a revert can tell it from the person's own edits.
#[derive(Clone, Debug, PartialEq)]
struct Change {
    plugin: String,
    key: String,
    original: Value,
    last: Value,
    source: Option<String>,
}

pub struct McpPlugin {
    handle: McpHandle,
    /// Measuring tool calls waiting for the plugin that measures: token to call id.
    jobs: HashMap<String, u64>,
    next_job: u64,
    changes: Vec<Change>,
}

impl McpPlugin {
    pub fn new(handle: McpHandle) -> McpPlugin {
        McpPlugin { handle, jobs: HashMap::new(), next_job: 0, changes: Vec::new() }
    }

    fn run(&mut self, host: &mut dyn Host, id: u64, tool: &str, args: &Value) {
        let reply = match tool {
            "status" => Ok(self.status(host)),
            "stats" | "watch" => stats_stub(args),
            "diagnostics" => Ok(json!({"text": host.diagnostics_report(&self.own_lines())})),
            "measure" => return self.measure(host, id),
            "describe" => self.describe(host, args),
            "get" => self.get(host, args),
            "set" => self.set(host, args),
            "revert" => Ok(self.revert(host)),
            "actions" => Ok(json!({"actions": shortcuts::actions_json(host).into_iter().filter(|a| !OUT_OF_REACH.contains(&a["id"].as_str().unwrap_or(""))).collect::<Vec<_>>()})),
            "run_action" => run_action(host, args),
            other => Err(format!("There is no tool called {other:?}.")),
        };
        self.handle.reply(id, reply);
    }

    fn own_lines(&self) -> Vec<(String, String)> {
        vec![("MCP server".into(), format!("on, {} connected", self.handle.clients()))]
    }

    fn status(&self, host: &dyn Host) -> Value {
        let (width, height) = host.canvas_size();
        json!({
            "telescope": {"version": crate::version::display_version()},
            "mcp": {"clients": self.handle.clients(), "frames": "never sent: this build has no frame tool"},
            "streaming": {
                "streaming": host.is_streaming(), "starting": host.is_starting(), "streams": host.stream_count(),
                "camera_on": host.is_camera_on(), "canvas": {"width": width, "height": height},
            },
            "focused_source": host.focused_source_id(),
            "background_streams": host.background_streams().into_iter().map(|b| json!({"source": b.source_id, "name": b.name})).collect::<Vec<_>>(),
            "vision": host.vision_state(),
            "plugins_running": host.running_plugins().into_iter().map(|(name, title)| json!({"name": name, "title": title})).collect::<Vec<_>>(),
            "stats": "not available in this build",
        })
    }

    fn describe(&self, host: &dyn Host, args: &Value) -> Reply {
        let wanted = args["plugin"].as_str();
        let groups: Vec<_> = host.plugin_settings().into_iter().filter(|g| wanted.is_none_or(|w| g.name == w)).collect();
        if groups.is_empty() {
            return Err(match wanted {
                Some(w) => format!("{w} has no settings to change here. It may be switched off (ask the person to switch it on in Plugins) or not offer any yet."),
                None => "No running plugin offers settings to change.".into(),
            });
        }
        let plugins: Vec<Value> = groups.iter().map(|g| {
            let config = host.plugin_config(&g.name).unwrap_or(Value::Null);
            let settings: Vec<Value> = g.settings.iter().map(|s| {
                let mut d = s.describe(&config);
                d["id"] = json!(format!("{}.{}", g.name, s.key));
                d
            }).collect();
            json!({"plugin": g.name, "title": g.title, "settings": settings})
        }).collect();
        Ok(json!({"plugins": plugins}))
    }

    fn get(&self, host: &dyn Host, args: &Value) -> Reply {
        let groups = host.plugin_settings();
        let mut values = Map::new();
        let mut missing = Vec::new();
        let ids: Vec<String> = match args.get("ids") {
            Some(Value::Array(list)) => list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Some(Value::Null) | None => groups.iter().flat_map(|g| g.settings.iter().map(move |s| format!("{}.{}", g.name, s.key))).collect(),
            Some(_) => return Err("ids has to be a list of setting ids.".into()),
        };
        for id in ids {
            match split_id(&groups, &id) {
                Some((group, spec)) => {
                    let config = host.plugin_config(&group.name).unwrap_or(Value::Null);
                    values.insert(id, spec.read(&config));
                }
                None => missing.push(id),
            }
        }
        if missing.is_empty() { Ok(json!({"values": values})) } else { Err(unknown_ids(&groups, &missing)) }
    }

    fn set(&mut self, host: &mut dyn Host, args: &Value) -> Reply {
        let Some(changes) = args.get("changes").and_then(Value::as_object).filter(|c| !c.is_empty()) else {
            return Err("changes has to be an object of setting id to new value, such as {\"colour_grading.contrast\": 1.1}.".into());
        };
        let groups = host.plugin_settings();
        // Every change is checked before any is made
        let mut by_plugin: Vec<(String, Map<String, Value>)> = Vec::new();
        let (mut problems, mut unknown) = (Vec::new(), Vec::new());
        for (id, value) in changes {
            let Some((group, spec)) = split_id(&groups, id) else {
                unknown.push(id.clone());
                continue;
            };
            match by_plugin.iter_mut().find(|(name, _)| *name == group.name) {
                Some((_, map)) => {
                    map.insert(spec.key.clone(), value.clone());
                }
                None => by_plugin.push((group.name.clone(), Map::from_iter([(spec.key.clone(), value.clone())]))),
            }
        }
        if !unknown.is_empty() {
            problems.push(unknown_ids(&groups, &unknown));
        }
        for (name, map) in &by_plugin {
            let group = groups.iter().find(|g| g.name == *name).expect("grouped by a known plugin");
            let config = host.plugin_config(name).unwrap_or(Value::Null);
            if let Err(list) = merge_changes(&group.settings, &config, map) {
                problems.extend(list);
            }
        }
        if !problems.is_empty() {
            return Err(format!("Nothing was changed.\n{}", problems.join("\n")));
        }
        let source = host.focused_source_id();
        let mut done = Vec::new();
        for (name, map) in &by_plugin {
            let group = groups.iter().find(|g| g.name == *name).expect("grouped by a known plugin");
            let before = host.plugin_config(name).unwrap_or(Value::Null);
            let after = host.change_plugin_settings(name, map).map_err(|list| format!("{}\nThe change may be partly in.", list.join("\n")))?;
            for key in map.keys() {
                let spec = group.settings.iter().find(|s| s.key == *key).expect("checked above");
                let (was, now) = (spec.read(&before), spec.read(&after));
                done.push(json!({"id": format!("{name}.{key}"), "was": was, "now": now}));
                self.remember(name, key, was, now, source.clone());
            }
        }
        Ok(json!({"changed": done, "note": "The camera shows these now. revert puts them back."}))
    }

    /// Keeps the first value a setting had this session and where it was left.
    fn remember(&mut self, plugin: &str, key: &str, was: Value, now: Value, source: Option<String>) {
        match self.changes.iter_mut().find(|c| c.plugin == plugin && c.key == key && c.source == source) {
            Some(c) => c.last = now,
            None => self.changes.push(Change { plugin: plugin.into(), key: key.into(), original: was, last: now, source }),
        }
    }

    fn revert(&mut self, host: &mut dyn Host) -> Value {
        if self.changes.is_empty() {
            return json!({"reverted": [], "left": [], "note": "Nothing was changed through MCP this session."});
        }
        let groups = host.plugin_settings();
        let shown = host.focused_source_id();
        let (mut reverted, mut left) = (Vec::new(), Vec::new());
        for c in std::mem::take(&mut self.changes) {
            let id = format!("{}.{}", c.plugin, c.key);
            let Some(spec) = groups.iter().find(|g| g.name == c.plugin).and_then(|g| g.settings.iter().find(|s| s.key == c.key)) else {
                left.push(json!({"id": id, "why": "the plugin is switched off now"}));
                continue;
            };
            let now = spec.read(&host.plugin_config(&c.plugin).unwrap_or(Value::Null));
            if c.source != shown {
                left.push(json!({"id": id, "why": "a different camera is shown now, so it was left alone"}));
            } else if now != c.last {
                left.push(json!({"id": id, "why": "the person changed it by hand since, so it was left as they have it", "now": now}));
            } else {
                let map = Map::from_iter([(c.key.clone(), c.original.clone())]);
                match host.change_plugin_settings(&c.plugin, &map) {
                    Ok(_) => reverted.push(json!({"id": id, "back_to": c.original})),
                    Err(list) => left.push(json!({"id": id, "why": list.join(" ")})),
                }
            }
        }
        json!({"reverted": reverted, "left": left})
    }

    fn measure(&mut self, host: &mut dyn Host, id: u64) {
        self.next_job += 1;
        let token = format!("measure{}", self.next_job);
        self.jobs.insert(token.clone(), id);
        host.schedule(JOB_WAIT_MS, &format!("job:{token}"));
        host.emit(Event::PictureMeasureRequested { token });
    }

    fn job_done(&mut self, host: &mut dyn Host, token: &str, reply: Reply) {
        if let Some(id) = self.jobs.remove(token) {
            host.cancel_schedule(&format!("job:{token}"));
            self.handle.reply(id, reply);
        }
    }
}

/// STATS STUB: stats and watch will read the Stats snapshot that lives in core once it is merged (one snapshot about a second, the last minute
/// of every series). They need that snapshot as an `Event` on the bus or a reader on `Host`, with each series' name, unit and a time for
/// every sample. The answer will be, per series, min, average and max over the window plus the few worst moments with their times, and
/// `watch` will be a job like measure that keeps collecting for the requested seconds. Until then both say so.
fn stats_stub(_args: &Value) -> Reply {
    Err("Stats are not available in this build of Telescope. status and diagnostics still work.".into())
}

/// The plugin and setting an id names ("colour_grading.masters.gain": the plugin up to the first dot, then the key).
fn split_id<'a>(groups: &'a [PluginSettings], id: &str) -> Option<(&'a PluginSettings, &'a SettingSpec)> {
    let (plugin, key) = id.split_once('.')?;
    let group = groups.iter().find(|g| g.name == plugin)?;
    group.settings.iter().find(|s| s.key == key).map(|s| (group, s))
}

fn unknown_ids(groups: &[PluginSettings], ids: &[String]) -> String {
    let known: Vec<String> = groups.iter().flat_map(|g| g.settings.iter().map(move |s| format!("{}.{}", g.name, s.key))).collect();
    let mut text = format!("There is no setting called {}.", ids.iter().map(|i| format!("{i:?}")).collect::<Vec<_>>().join(", "));
    let near: Vec<&str> = known.iter().map(String::as_str).filter(|k| ids.iter().any(|i| i.len() >= 3 && (k.contains(i.as_str()) || i.contains(k)))).collect();
    if !near.is_empty() {
        text.push_str(&format!(" Did you mean {}?", near.join(" or ")));
    } else if groups.is_empty() {
        text.push_str(" No running plugin offers settings (plugins that are off don't).");
    } else {
        text.push_str(" describe lists them.");
    }
    text
}

fn run_action(host: &mut dyn Host, args: &Value) -> Reply {
    let Some(action) = args["action"].as_str() else { return Err("action has to be an action id from actions.".into()) };
    if OUT_OF_REACH.contains(&action) {
        return Err(format!("{action} is left to the person: starting, stopping and switching the camera can drop a call. They can use the window or `telescope --action {action}`."));
    }
    let params = match args.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => return Err("params has to be an object of name to value.".into()),
    };
    let answer = shortcuts::run_remote_request(host, &json!({"action": action, "params": params}));
    let text = answer["text"].as_str().unwrap_or("").to_string();
    if answer["ok"] == true { Ok(json!({"said": text})) } else { Err(text) }
}

impl Plugin for McpPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("MCP server").into(),
            about: n_("Lets an AI app such as Claude Desktop look at Telescope and change its settings, through a command that opens no network port.").into(),
            when_off: n_("Without it AI apps can't connect to Telescope.").into(),
            category: "extras".into(),
            icon: "plug".into(),
            optional: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.handle.attach(host.poster());
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        self.handle.detach();
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::Posted { owner, token, data } if owner == NAME && token == POST_CALL => {
                let id = data["id"].as_u64().unwrap_or(0);
                self.run(host, id, data["tool"].as_str().unwrap_or(""), &data["args"]);
            }
            Event::Posted { owner, token, data } if owner == NAME && token == POST_CLIENT => {
                // The first AI app to connect is announced, so nobody finds out by surprise
                if data["clients"].as_u64() == Some(1) {
                    host.send_notification("Telescope", &tr("An AI app is connected to Telescope through the MCP server."), false);
                }
            }
            Event::PictureMeasured { token, ok, message, data } => {
                let reply = if *ok { Ok(data.clone()) } else { Err(message.clone()) };
                self.job_done(host, token, reply);
            }
            Event::TimerFired { owner, token } if owner == NAME => {
                if let Some(job) = token.strip_prefix("job:") {
                    let why = "Nothing measured the picture. Measuring needs Colour grading switched on in Plugins, and a stream running.";
                    self.job_done(host, job, Err(why.into()));
                }
            }
            _ => {}
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        self.own_lines()
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
