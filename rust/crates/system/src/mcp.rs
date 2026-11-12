//! The MCP server plugin: lets an AI app such as Claude Desktop read Telescope's settings, change them and measure the picture. The AI app
//! starts `telescope mcp` (mcp_bridge), which talks to the running copy over the single-instance socket, past the same control key check
//! `--action` has. This is the other end: the JSON-RPC for MCP, the tools, and the plugin that runs them on the window's thread. Off by
//! default, and while it is off a connection is turned away with a line saying where to switch it on.
//!
//! Plugins don't know each other, so every tool goes through what any plugin can use: declared settings (`Plugin::settings`) changed through
//! the plugin's own `apply_preset`, the shortcut actions, Copy diagnostics, and the bus (`picture_measure_requested`, answered by whichever
//! plugin can measure the picture).

use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use crate::remote::checked_params;
use telescope_core::event::Event;
use telescope_core::host::{Host, PluginSettings, Poster};
use telescope_core::i18n::{n_, tr, tr_ctx, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::settings::{get_path, leaf_changes, merge_changes, put_path, SettingSpec};
use telescope_core::stats::Snapshot;
use telescope_core::ui::{SettingsSection, SlintSource, UiEvent, UiSpec};
use crate::mcp_stats::{Collected, Level, WATCH_MAX_SECONDS, WINDOW_MAX};

pub const NAME: &str = "mcp";
const POST_CALL: &str = "call";
const POST_CLIENT: &str = "client";
const POST_CANCEL: &str = "cancel";
const TAB: &str = "mcp";
const HEADER: &str = "header";
const UI: &str = include_str!("../ui/mcp.slint");
/// How many tool calls the activity log keeps, newest first.
pub const LOG_KEPT: usize = 30;
/// How long the header button stays lit after the last tool call ends.
const CALM_MS: u64 = 1200;
const T_CALM: &str = "calm";
/// Outlining a setting on screen is for a few named ones: a read of more is a sweep, which would light up the whole card.
const READS_OUTLINED: usize = 6;
/// How long a nerd stats call keeps the per-plugin times being measured, for its next call to find them.
const NERD_ASK_MS: u64 = 20_000;
const T_UNWANT: &str = "unwant";
/// A tool that never gets an answer from the window thread gives up after this.
const CALL_WAIT: Duration = Duration::from_secs(25);
/// How often a call that waits looks up whether its connection is still there.
const POLL: Duration = Duration::from_millis(250);
/// A watch has a deadline of its own: its length plus this, so it never gives up before its time is up.
const WATCH_SLACK: Duration = Duration::from_secs(10);
/// Tool calls one connection may have running at once, all connections together, and AI apps connected at once. A call past a limit is refused with a reason.
const PER_CONNECTION: usize = 4;
const MAX_CALLS: usize = 12;
const MAX_CLIENTS: usize = 8;
/// A connection that stops taking answers is let go after this.
const WRITE_WAIT: Duration = Duration::from_secs(10);
/// How many undo steps are kept, oldest dropped first.
const JOURNAL_KEPT: usize = 200;
/// The connection number of a change kept from before Telescope started again: no AI app is that one, so only the header button's undo reaches it.
const PREVIOUS_RUN: u64 = u64::MAX;
/// A measuring that nobody answered at all (no plugin can measure) gives up after this, longer than the measuring itself takes.
const JOB_WAIT_MS: u64 = 10_000;
pub(crate) const MAX_LINE: usize = 1 << 20;
const LATEST: &str = "2025-06-18";
const KNOWN: [&str; 3] = ["2024-11-05", "2025-03-26", LATEST];
/// Phone and stream controls that are hard to undo in a live call stay with the person: they have `--action` and the window.
const OUT_OF_REACH: [&str; 4] = ["streaming.start_stop", "streaming.camera", "window.show_hide", "phone.pick"];

/// What a tool says: its data, or what went wrong.
pub type Reply = Result<Value, String>;

#[derive(Default)]
struct Link {
    poster: Option<Arc<dyn Poster>>,
    next: u64,
    waiting: HashMap<u64, Sender<Reply>>,
    /// The AI apps connected, by their connection's number and the name each gave (empty until it says hello).
    clients: Vec<(u64, String)>,
    sessions: u64,
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
        self.lock().clients.len()
    }

    /// The AI apps connected now, by connection and the name each gave ("" before it did).
    pub fn client_list(&self) -> Vec<(u64, String)> {
        self.lock().clients.clone()
    }

    /// The name an AI app gave when it connected (the MCP initialize message's clientInfo), for the activity log and the settings page.
    pub fn name_client(&self, session: u64, name: &str) {
        let poster = {
            let mut l = self.lock();
            let Some(client) = l.clients.iter_mut().find(|(s, _)| *s == session) else { return };
            client.1 = name.trim().chars().take(60).collect();
            l.poster.clone()
        };
        if let Some(poster) = poster {
            poster.post(POST_CLIENT, json!({"clients": self.clients(), "named": session}));
        }
    }

    fn name_of(&self, session: u64) -> String {
        self.lock().clients.iter().find(|(s, _)| *s == session).map(|(_, n)| n.clone()).unwrap_or_default()
    }

    /// A connection came in: a guard that counts it until dropped, or why it can't be served (the plugin is off).
    pub fn open_client(&self) -> Result<Client, String> {
        let (poster, session) = {
            let mut l = self.lock();
            let Some(poster) = l.poster.clone() else {
                return Err("Telescope's MCP server is switched off. Switch on MCP server in Telescope's Plugins tab, then try again.".into());
            };
            if l.clients.len() >= MAX_CLIENTS {
                return Err("Too many AI apps are connected to Telescope at once. Close one, then try again.".into());
            }
            l.sessions += 1;
            let session = l.sessions;
            l.clients.push((session, String::new()));
            (poster, session)
        };
        poster.post(POST_CLIENT, json!({"clients": self.clients()}));
        Ok(Client(self.clone(), session))
    }

    /// From a connection's thread: runs a tool on the window's thread and waits for its answer.
    pub fn call(&self, tool: &str, args: Value) -> Reply {
        self.call_as(0, tool, args)
    }

    /// The same for one connection (`Client::session`), whose changes are the only ones its revert undoes. Gives up when the window doesn't answer
    /// in time or the connection goes, and tells the plugin so it stops the work.
    pub fn call_as(&self, session: u64, tool: &str, args: Value) -> Reply {
        let (tx, rx) = channel();
        let wait = deadline(tool, &args);
        let (poster, id) = {
            let mut l = self.lock();
            let Some(poster) = l.poster.clone() else { return Err(off()) };
            if l.waiting.len() >= MAX_CALLS {
                return Err("Telescope is busy with other requests from AI apps. Try again in a moment.".into());
            }
            l.next += 1;
            let id = l.next;
            l.waiting.insert(id, tx);
            (poster, id)
        };
        poster.post(POST_CALL, json!({"id": id, "session": session, "tool": tool, "args": args}));
        let end = Instant::now() + wait;
        let gave_up = loop {
            match rx.recv_timeout(POLL) {
                Ok(reply) => break Ok(reply),
                Err(RecvTimeoutError::Disconnected) => break Err(off()),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= end => break Err("Telescope didn't answer in time.".to_string()),
                Err(RecvTimeoutError::Timeout) if session != 0 && !self.is_connected(session) => break Err("The AI app disconnected.".to_string()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        let unanswered = self.lock().waiting.remove(&id).is_some();
        gave_up.unwrap_or_else(|why| {
            if unanswered && why != off() {
                poster.post(POST_CANCEL, json!({"id": id}));
            }
            Err(why)
        })
    }

    fn is_connected(&self, session: u64) -> bool {
        self.lock().clients.iter().any(|(s, _)| *s == session)
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

/// How long a call waits for the window: a watch outlasts its own length, everything else is quick.
fn deadline(tool: &str, args: &Value) -> Duration {
    match tool {
        "watch" => Duration::from_secs(watch_seconds(args).unwrap_or(30)) + WATCH_SLACK,
        _ => CALL_WAIT,
    }
}

/// One connected AI app, counted until dropped, with a number that is only its own.
pub struct Client(McpHandle, u64);

impl Client {
    pub fn session(&self) -> u64 {
        self.1
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let poster = {
            let mut l = self.0.lock();
            l.clients.retain(|(s, _)| *s != self.1);
            l.poster.clone()
        };
        if let Some(poster) = poster {
            poster.post(POST_CLIENT, json!({"clients": self.0.clients(), "gone": self.1}));
        }
    }
}

/// For the single-instance socket: lets a `telescope mcp` connection in, or turns it away while the plugin is off.
pub fn accept(handle: &McpHandle) -> Result<crate::single_instance::McpServe, String> {
    let client = handle.open_client()?;
    let (handle, session) = (handle.clone(), client.session());
    Ok(Box::new(move |conn: crate::single_instance::Conn, after: Vec<u8>| {
        let Ok(writer) = conn.try_clone() else { return };
        writer.set_write_wait(Some(WRITE_WAIT));
        let hello: Arc<Hello> = {
            let handle = handle.clone();
            Arc::new(move |name| handle.name_client(session, name))
        };
        let call: Arc<Caller> = Arc::new(move |tool, args| handle.call_as(session, tool, args));
        serve_connection(std::io::BufReader::new(std::io::Cursor::new(after).chain(conn)), Arc::new(Mutex::new(Closing(writer))), call, hello);
        drop(client);
    }))
}

/// A connection that is let go when a write to it fails or times out, which also ends the read that waits for the next request.
struct Closing(crate::single_instance::Conn);

impl Write for Closing {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf).inspect_err(|_| self.0.close())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush().inspect_err(|_| self.0.close())
    }
}

// ── JSON-RPC ─────────────────────────────────────────────────────────

/// How a request for a tool reaches the window's thread.
pub type Caller = dyn Fn(&str, Value) -> Reply + Send + Sync;

/// Told the name an AI app gives for itself when it connects.
pub type Hello = dyn Fn(&str) + Send + Sync;

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
        tool("stats", "The Stats numbers (frame rates in and out, drops with their reasons, bitrate, decoding and frame step times and so on) over the last minute, each as min, average and max with the worst moments and how many seconds ago they happened, for finding what makes the picture lag. Needs a stream running. nerd also gives each stage's times, the balancer's moves and the app's CPU and memory.",
             stats_args, &[], true),
        tool("watch", "Collects the Stats numbers for a while (5 to 120 seconds) while the person reproduces a problem, then summarises them like stats does. The call waits until the time is up. Needs a stream running.",
             json!({"seconds": {"type": "number", "minimum": 5, "maximum": 120, "description": "How long to collect. Default 30."},
                    "level": {"type": "string", "enum": ["standard", "nerd"]},
                    "series": {"type": "array", "items": {"type": "string"}, "description": "Only these series, by name. Default all."}}), &[], true),
        tool("diagnostics", "The text Copy diagnostics gives, with recent log lines and the same personal details left out. Good for working out why something fails.",
             json!({}), &[], true),
        tool("measure", "Looks at the picture for about a second and gives it as numbers: the face's colour and brightness, whether white balance is off (a colour cast), how dark, bright and noisy it is and how much is clipped. Needs a stream running and Colour grading switched on. Ask the person to keep still. It measures the camera's own picture, before Telescope's changes, so use it to decide what to change, not to check a change afterwards.",
             json!({}), &[], true),
        tool("describe", "Lists the settings you can read and change, each with its id, what it does, its range, default and current value. Only plugins that are switched on show up; if one you expect is missing, ask the person to switch it on in Plugins.",
             json!({"plugin": {"type": "string", "description": "Only this plugin's settings, by name, such as colour_grading."}}), &[], true),
        tool("get", "Current values of settings by id (such as colour_grading.contrast). Without ids, every setting.",
             json!({"ids": {"type": "array", "items": {"type": "string"}}}), &[], true),
        tool("set", "Changes settings, by id, all at once so the camera shows no half-way steps. Every value is checked first and nothing changes unless all of them are fine. Changes show in the camera straight away, which matters in a live call, so make small steps. revert undoes them. Refused when the person has set Telescope to read only.",
             json!({"changes": {"type": "object", "description": "Setting id to new value, such as {\"colour_grading.contrast\": 1.1, \"colour_grading.on\": true}.", "additionalProperties": true}}),
             &["changes"], false),
        tool("revert", "Puts back what set and run_action changed (including what an action switched that isn't a listed setting), as it was before this session's first change to each. A setting the person or another AI app has moved since is left as it is, even if it has the same value again. A change for a camera that isn't shown, or a plugin that is off, is kept for a later revert. Actions that change something that isn't a saved setting (a mute) are named in not_restored, because they can't be put back. Refused when Telescope is set to read only.",
             json!({}), &[], false),
        tool("actions", "Lists the actions Telescope can run, like the ones keyboard shortcuts run, with what each takes. Starting or stopping the stream, switching the phone's camera and picking another phone are left to the person.",
             json!({}), &[], true),
        tool("run_action", "Runs one action from actions with its parameters. Refused when Telescope is set to read only.",
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
    let Some(fields) = message.as_object() else {
        return Some(error(&Value::Null, -32600, "That isn't a request: a message is a JSON object, and batches aren't used."));
    };
    // An answer to something this never asked is let go
    if !fields.contains_key("method") && (fields.contains_key("result") || fields.contains_key("error")) {
        return None;
    }
    let valid_id = |id: &Value| id.is_string() || id.is_i64() || id.is_u64();
    let id_of = |fields: &Map<String, Value>| fields.get("id").filter(|id| valid_id(id)).cloned().unwrap_or(Value::Null);
    if fields.get("jsonrpc") != Some(&json!("2.0")) {
        return Some(error(&id_of(fields), -32600, "That isn't JSON-RPC 2.0: jsonrpc has to be \"2.0\"."));
    }
    let Some(method) = fields.get("method").and_then(Value::as_str) else {
        return Some(error(&id_of(fields), -32600, "That isn't a request: it has no method."));
    };
    let Some(id) = fields.get("id") else { return None };
    if !valid_id(id) {
        return Some(error(&Value::Null, -32600, "That isn't a request: its id has to be a string or a whole number."));
    }
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

/// Counts a connection's running calls until dropped.
struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One connection: a JSON-RPC message per line in, one per line out. A tool call can take seconds (measure), so each runs on a thread
/// of its own and the connection keeps answering meanwhile, up to PER_CONNECTION at once. Returns when the other end closes.
pub fn serve_connection(mut reader: impl BufRead, writer: Arc<Mutex<dyn Write + Send>>, call: Arc<Caller>, hello: Arc<Hello>) {
    let running = Arc::new(AtomicUsize::new(0));
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE as u64).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with('\n') && line.len() >= MAX_LINE => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            send(&*writer, &error(&Value::Null, -32700, "That isn't JSON."));
            continue;
        };
        if message["method"] == "initialize" {
            if let Some(name) = message["params"]["clientInfo"]["name"].as_str().filter(|n| !n.trim().is_empty()) {
                hello(name);
            }
        }
        if message["method"] == "tools/call" {
            let too_many = || -> Reply { Err(format!("Too many calls from this AI app at once (at most {PER_CONNECTION}). Wait for one to finish, then ask again.")) };
            if running.fetch_add(1, Ordering::SeqCst) >= PER_CONNECTION {
                running.fetch_sub(1, Ordering::SeqCst);
                if let Some(answer) = handle_message(&message, &|_, _| too_many()) {
                    send(&*writer, &answer);
                }
                continue;
            }
            let (out, call, held) = (writer.clone(), call.clone(), Running(running.clone()));
            let id = message["id"].clone();
            let started = std::thread::Builder::new().name("mcp-call".into()).spawn(move || {
                let _held = held;
                if let Some(answer) = handle_message(&message, &*call) {
                    send(&*out, &answer);
                }
            });
            if started.is_err() {
                send(&*writer, &result(&id, tool_result(Err("Telescope couldn't start that call right now.".into()))));
            }
        } else if let Some(answer) = handle_message(&message, &*call) {
            send(&*writer, &answer);
        }
    }
}

// ── The plugin ───────────────────────────────────────────────────────

/// What an AI app may do: look only, or look and change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    #[default]
    ReadAndChange,
}

impl Access {
    /// How it is saved in the plugin's settings.
    pub fn name(self) -> &'static str {
        match self {
            Access::ReadOnly => "read_only",
            Access::ReadAndChange => "read_and_change",
        }
    }

    pub fn from_name(name: &str) -> Option<Access> {
        match name {
            "read_only" => Some(Access::ReadOnly),
            "read_and_change" => Some(Access::ReadAndChange),
            _ => None,
        }
    }
}

/// What a tool call is doing to Telescope: nothing but looking, or moving things.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Activity {
    #[default]
    Rest,
    Read,
    Change,
}

impl Activity {
    fn name(self) -> &'static str {
        match self {
            Activity::Rest => "",
            Activity::Read => "read",
            Activity::Change => "change",
        }
    }
}

/// The tools that change something, which read only turns away. Auto colour grading goes through run_action.
pub fn changes_things(tool: &str) -> bool {
    matches!(tool, "set" | "revert" | "run_action")
}

/// One line of the activity log.
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub time: String,
    pub who: String,
    pub text: String,
    /// Turned away because of the access level.
    pub refused: bool,
}

/// One write an AI app made to a setting, through `set` or an action: where the setting was and where the write left it. The journal keeps these
/// in the order they happened, so a revert can tell its own writes from anyone else's and unwind the newest first.
#[derive(Clone, Debug, PartialEq)]
struct Change {
    /// Counts every write of every connection, so the order between connections is known.
    seq: u64,
    session: u64,
    plugin: String,
    key: String,
    source: Option<String>,
    was: Value,
    now: Value,
    /// The setting's revision (Host::setting_revision) just before this write and just after it, when known (not after a restart). A later revision means
    /// the person or something else moved the setting since, even when it ended up with the same value.
    rev_before: Option<u64>,
    rev: Option<u64>,
    /// Read from the file only: someone besides the AI moved the setting between this step and the next one on it, or nobody can tell (an older file).
    /// The revisions that said so don't survive a restart, so this is what carries it over.
    cut: bool,
}

/// A revision no setting ever has, for a loaded step that can't be told apart from an edit: it never counts as untouched.
const NEVER_UNTOUCHED: u64 = u64::MAX;

impl Change {
    fn to_json(&self, cut: bool) -> Value {
        json!({"seq": self.seq, "plugin": self.plugin, "key": self.key, "source": self.source, "was": self.was, "now": self.now, "cut": cut})
    }

    /// A change as it was saved, which no connection owns any more.
    fn from_json(v: &Value) -> Option<Change> {
        Some(Change {
            seq: v["seq"].as_u64()?, session: PREVIOUS_RUN, plugin: v["plugin"].as_str()?.to_string(), key: v["key"].as_str()?.to_string(),
            source: v["source"].as_str().map(String::from), was: v.get("was")?.clone(), now: v.get("now")?.clone(), rev_before: None, rev: None,
            cut: v["cut"].as_bool().unwrap_or(true),
        })
    }
}

/// An action that moved no saved setting, so there is nothing to put back for it (a mute, Blur now).
#[derive(Clone, Debug, PartialEq)]
struct Untracked {
    seq: u64,
    session: u64,
    action: String,
}

/// A setting by plugin and key.
type Slot = (String, String);

/// What an undo did: what went back, what was left on purpose, and the writes that stay in the journal for a later undo.
struct Unwound {
    reverted: Vec<Value>,
    left: Vec<Value>,
    kept: Vec<Change>,
    /// For each setting undone, its revision as it was before the oldest write undone (None where the host doesn't count).
    rev_before: HashMap<Slot, Option<u64>>,
}

/// A watch that is collecting: the tool call waits for it to end.
struct Watch {
    collected: Collected,
    level: Level,
    only: Vec<String>,
}

pub struct McpPlugin {
    handle: McpHandle,
    /// Measuring tool calls waiting for the plugin that measures: token to call id.
    jobs: HashMap<String, u64>,
    next_job: u64,
    /// What the AI apps changed, oldest first: a revert undoes only its own connection's (`Client::session`), so one AI app can't take back another's.
    /// They stay after the connection ends, so the header button can still undo them, up to JOURNAL_KEPT of them.
    journal: Vec<Change>,
    /// Actions that moved nothing saved, so undo can only say it can't put them back. In memory only.
    unrestored: Vec<Untracked>,
    next_seq: u64,
    /// A nerd stats call asked for the per-plugin times a moment ago, and whether the host was last told they are wanted.
    stats_lease: bool,
    stats_wanted: bool,
    access: Access,
    /// The last LOG_KEPT calls, newest first.
    log: VecDeque<LogEntry>,
    /// Calls being answered: call id to the connection and what the call is in words, for the log.
    open: HashMap<u64, (u64, String)>,
    pending: usize,
    activity: Activity,
    snapshot: Option<Snapshot>,
    watches: HashMap<u64, Watch>,
    /// What time it is, as the log shows it.
    clock: fn() -> String,
}

impl McpPlugin {
    pub fn new(handle: McpHandle) -> McpPlugin {
        McpPlugin {
            handle, jobs: HashMap::new(), next_job: 0, journal: Vec::new(), unrestored: Vec::new(), next_seq: 0, stats_lease: false, stats_wanted: false, access: Access::default(), log: VecDeque::new(), open: HashMap::new(),
            pending: 0, activity: Activity::Rest, snapshot: None, watches: HashMap::new(), clock: telescope_core::time::local_clock,
        }
    }

    pub fn access(&self) -> Access {
        self.access
    }

    /// The activity log as the page lists it, newest first.
    pub fn log(&self) -> Vec<LogEntry> {
        self.log.iter().cloned().collect()
    }

    pub fn can_undo(&self) -> bool {
        !self.journal.is_empty()
    }

    fn who(&self, session: u64) -> String {
        Some(self.handle.name_of(session)).filter(|n| !n.is_empty()).unwrap_or_else(|| tr("An AI app"))
    }

    fn run(&mut self, host: &mut dyn Host, id: u64, session: u64, tool: &str, args: &Value) {
        let text = self.describe_call(host, tool, args);
        if changes_things(tool) && self.access == Access::ReadOnly {
            self.record(host, session, tr("Refused (read only): {what}").field("what", &text), true);
            self.handle.reply(id, Err(format!("Telescope is set to read only, so {tool} was refused. The person can allow changes on the MCP tab in Telescope's settings. Everything that only reads still works.")));
            return;
        }
        self.open.insert(id, (session, text));
        self.begin(host, if changes_things(tool) { Activity::Change } else { Activity::Read });
        let reply = match tool {
            "status" => Ok(self.status(host)),
            "stats" => self.stats(host, args),
            "watch" => return self.watch(host, id, args),
            "diagnostics" => Ok(json!({"text": host.diagnostics_report(&self.own_lines())})),
            "measure" => return self.measure(host, id),
            "describe" => self.describe(host, args),
            "get" => self.get(host, args),
            "set" => self.set(host, session, args),
            "revert" => Ok(self.revert(host, session)),
            "actions" => Ok(json!({"actions": list_actions(host)})),
            "run_action" => self.run_action(host, session, args),
            other => Err(format!("There is no tool called {other:?}.")),
        };
        if let Ok(value) = &reply {
            self.outline(host, tool, args, value);
        }
        self.answer(host, id, reply);
    }

    /// The call is done: it goes in the log, the button settles and the AI app gets its answer.
    fn answer(&mut self, host: &mut dyn Host, id: u64, reply: Reply) {
        if let Some((session, text)) = self.open.remove(&id) {
            let text = if reply.is_err() { tr("Failed: {what}").field("what", text) } else { text };
            self.record(host, session, text, false);
            self.end(host);
        }
        self.handle.reply(id, reply);
    }

    /// The controls that show what a call read or changed are outlined for a moment: every change, and a read of a few named settings.
    fn outline(&self, host: &mut dyn Host, tool: &str, args: &Value, value: &Value) {
        let ids_in = |list: &Value, key: &str| -> Vec<String> {
            list.as_array().map(|a| a.iter().filter_map(|c| c[key].as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let (ids, changed) = match tool {
            "set" | "run_action" => (ids_in(&value["changed"], "id"), true),
            "revert" => (ids_in(&value["reverted"], "id"), true),
            "get" => {
                let named: Vec<String> = args["ids"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                (if named.len() <= READS_OUTLINED { named } else { Vec::new() }, false)
            }
            _ => return,
        };
        if !ids.is_empty() {
            host.emit(Event::SettingsTouched { ids, changed });
        }
    }

    /// The call in plain words, for the log.
    fn describe_call(&self, host: &dyn Host, tool: &str, args: &Value) -> String {
        let groups = host.plugin_settings();
        let named = |id: &str| setting_name(&groups, id);
        match tool {
            "status" => tr("Read status"),
            "stats" => tr("Read stats"),
            "watch" => tr("Watched stats for {n} s").field("n", watch_seconds(args).unwrap_or(30)),
            "diagnostics" => tr("Read diagnostics"),
            "measure" => tr("Measured the picture"),
            "describe" => match args["plugin"].as_str() {
                Some(p) => tr("Listed the {plugin} settings").field("plugin", groups.iter().find(|g| g.name == p).map(|g| g.title.clone()).unwrap_or_else(|| p.to_string())),
                None => tr("Listed every setting"),
            },
            "get" => match args["ids"].as_array().filter(|a| !a.is_empty()) {
                Some(ids) => tr("Read {names}").field("names", listed(ids.iter().filter_map(Value::as_str).map(&named).collect())),
                None => tr("Read every setting"),
            },
            "set" => match args["changes"].as_object().filter(|c| !c.is_empty()) {
                Some(changes) => tr("Set {changes}").field("changes", listed(changes.iter().map(|(id, v)| tr("{name} to {value}").field("name", named(id)).field("value", value_text(v))).collect())),
                None => tr("Set settings"),
            },
            "revert" => tr("Undid its changes"),
            "actions" => tr("Listed the actions"),
            "run_action" => {
                let id = args["action"].as_str().unwrap_or("");
                let label = host.shortcut_actions().into_iter().find(|(_, a)| a.id == id).map(|(_, a)| a.label);
                tr("Ran {action}").field("action", label.unwrap_or_else(|| id.to_string()))
            }
            other => other.to_string(),
        }
    }

    fn record(&mut self, host: &mut dyn Host, session: u64, text: String, refused: bool) {
        let who = self.who(session);
        self.push_log(who, text, refused);
        self.push_page(host);
    }

    fn push_log(&mut self, who: String, text: String, refused: bool) {
        self.log.push_front(LogEntry { time: (self.clock)(), who, text, refused });
        self.log.truncate(LOG_KEPT);
    }

    /// A call starts: the button lights, as a look or as a change.
    fn begin(&mut self, host: &mut dyn Host, kind: Activity) {
        self.pending += 1;
        self.activity = self.activity.max(kind);
        host.cancel_schedule(T_CALM);
        self.push_header(host);
    }

    /// A call ends: the button stays lit a moment longer, then settles.
    fn end(&mut self, host: &mut dyn Host) {
        self.pending = self.pending.saturating_sub(1);
        if self.pending == 0 {
            host.schedule(CALM_MS, T_CALM);
        }
        self.push_header(host);
    }

    fn header_props(&self) -> Value {
        let connected = self.handle.clients() > 0;
        json!({
            "shown": true,
            "connected": connected,
            "activity": self.activity.name(),
            "can-undo": self.can_undo(),
            "open-label": tr("MCP server settings"),
            "open-tip": if connected { tr("An AI app is connected.") } else { tr("No AI app is connected. Open the MCP settings.") },
            "undo-label": tr("Undo what the AI changed"),
            "undo-tip": if self.can_undo() { tr("Undo what the AI changed.") } else { tr("The AI hasn't changed anything.") },
        })
    }

    /// The command that starts the MCP server, as the entry an AI app's MCP settings take.
    pub fn connect_entry() -> String {
        let program = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "telescope".into());
        format!("\"telescope\": {{ \"command\": {}, \"args\": [\"mcp\"] }}", json!(program))
    }

    fn page_props(&self) -> Value {
        let clients: Vec<String> = self.handle.client_list().into_iter().map(|(_, n)| if n.is_empty() { tr("An AI app") } else { n }).collect();
        let log: Vec<Value> = self.log.iter().map(|e| json!({"time": e.time, "who": e.who, "text": e.text, "kind": if e.refused { "refused" } else { "" }})).collect();
        json!({
            "clients": clients,
            "none-text": tr("No AI app is connected"),
            "access-items": [tr("Read only"), tr("Read and change")],
            "access-current": if self.access == Access::ReadOnly { 0 } else { 1 },
            "command": Self::connect_entry(),
            "copy-tip": tr("Copy the entry for an AI app's MCP settings."),
            "log": log,
            "log-empty": tr("Nothing yet"),
        })
    }

    fn push_header(&self, host: &mut dyn Host) {
        host.set_ui_props(HEADER, &self.header_props());
    }

    fn push_page(&self, host: &mut dyn Host) {
        host.set_ui_props(TAB, &self.page_props());
        self.push_header(host);
    }

    fn own_lines(&self) -> Vec<(String, String)> {
        let access = if self.access == Access::ReadOnly { "read only" } else { "read and change" };
        vec![("MCP server".into(), format!("on, {} connected, {access}", self.handle.clients()))]
    }

    fn status(&self, host: &dyn Host) -> Value {
        let (width, height) = host.canvas_size();
        let numbers = self.snapshot.as_ref().is_some_and(|s| !s.sources.is_empty());
        json!({
            "telescope": {"version": crate::version::display_version()},
            "mcp": {"clients": self.handle.clients(), "access": self.access.name(), "frames": "never sent: this build has no frame tool"},
            "streaming": {
                "streaming": host.is_streaming(), "starting": host.is_starting(), "streams": host.stream_count(),
                "camera_on": host.is_camera_on(), "canvas": {"width": width, "height": height},
            },
            "focused_source": host.focused_source_id(),
            "background_streams": host.background_streams().into_iter().map(|b| json!({"source": b.source_id, "name": b.name})).collect::<Vec<_>>(),
            "vision": host.vision_state(),
            "plugins_running": host.running_plugins().into_iter().map(|(name, title)| json!({"name": name, "title": title})).collect::<Vec<_>>(),
            "stats": if numbers { "numbers are coming in: use stats or watch" } else { "no stream is running, so there are no numbers yet" },
        })
    }

    /// The Stats numbers over the last seconds, from the snapshot core puts on the bus: the same numbers the Stats panel draws, panel or no panel.
    fn stats(&mut self, host: &mut dyn Host, args: &Value) -> Reply {
        let window = match args.get("window_seconds") {
            None | Some(Value::Null) => WINDOW_MAX,
            Some(v) => v.as_f64().filter(|n| (5.0..=WINDOW_MAX as f64).contains(n)).map(|n| n as usize).ok_or_else(|| format!("window_seconds is a number from 5 to {WINDOW_MAX}."))?,
        };
        let level = Level::parse(args["level"].as_str())?;
        let only = series_names(args)?;
        if level == Level::Nerd {
            // The per-plugin times are only measured while somebody asks, so this asks, for a little while
            self.stats_lease = true;
            self.sync_stats(host);
            host.schedule(NERD_ASK_MS, T_UNWANT);
        }
        let Some(snapshot) = self.snapshot.as_ref().filter(|s| !s.sources.is_empty()) else {
            return Err("No stream is running, so there are no numbers. Start the phone's stream and ask again.".into());
        };
        let mut report = Collected::from_snapshot(snapshot, window).report(level, &only);
        if level == Level::Nerd && !snapshot.detailed {
            report["note"] = json!("Each plugin's frame step and event times start being measured now. Ask again in a few seconds to have them.");
        }
        Ok(report)
    }

    /// Collects the numbers for a while, then answers with them. The call waits, and the window carries on.
    fn watch(&mut self, host: &mut dyn Host, id: u64, args: &Value) {
        let started = (|| -> Result<(u64, Level, Vec<String>), String> {
            let seconds = watch_seconds(args).ok_or_else(|| format!("seconds is a number from 5 to {WATCH_MAX_SECONDS}."))?;
            Ok((seconds, Level::parse(args["level"].as_str())?, series_names(args)?))
        })();
        match started {
            Err(why) => self.answer(host, id, Err(why)),
            Ok((seconds, level, only)) => {
                host.schedule(seconds * 1000, &format!("watch:{id}"));
                self.watches.insert(id, Watch { collected: Collected::default(), level, only });
                self.sync_stats(host);
            }
        }
    }

    /// The per-plugin times are measured while a nerd stats call's ask is fresh or a nerd watch is collecting: each is tracked on its own, and the host is told
    /// when either changes, so one ending never lets go of what the other still needs.
    fn sync_stats(&mut self, host: &mut dyn Host) {
        let wanted = self.stats_lease || self.watches.values().any(|w| w.level == Level::Nerd);
        if wanted != self.stats_wanted {
            self.stats_wanted = wanted;
            host.want_stats(NAME, wanted);
        }
    }

    /// A call whose AI app stopped waiting (it timed out or disconnected): what runs for it stops.
    fn cancel(&mut self, host: &mut dyn Host, id: u64) {
        if self.watches.remove(&id).is_some() {
            host.cancel_schedule(&format!("watch:{id}"));
            self.sync_stats(host);
        }
        let tokens: Vec<String> = self.jobs.iter().filter(|(_, job)| **job == id).map(|(token, _)| token.clone()).collect();
        for token in tokens {
            self.jobs.remove(&token);
            host.cancel_schedule(&format!("job:{token}"));
        }
        self.answer(host, id, Err("The AI app stopped waiting for this.".into()));
    }

    fn watch_done(&mut self, host: &mut dyn Host, id: u64) {
        let Some(watch) = self.watches.remove(&id) else { return };
        self.sync_stats(host);
        let reply = if watch.collected.is_empty() {
            Err("No stream was running while this watched, so there are no numbers. Start the phone's stream, then watch while the problem happens.".into())
        } else {
            Ok(watch.collected.report(watch.level, &watch.only))
        };
        self.answer(host, id, reply);
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

    fn set(&mut self, host: &mut dyn Host, session: u64, args: &Value) -> Reply {
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
        let revs = self.journal_revisions(host);
        let before_all = all_configs(host);
        let mut applied: Vec<(String, Value)> = Vec::new();
        let mut done = Vec::new();
        for (name, map) in &by_plugin {
            let group = groups.iter().find(|g| g.name == *name).expect("grouped by a known plugin");
            let before = host.plugin_config(name).unwrap_or(Value::Null);
            applied.push((name.clone(), before.clone()));
            match host.change_plugin_settings(name, map) {
                Ok(after) => {
                    // A setting can move others with it (zooming out puts the pan back), so every one that differs is kept, not only those asked for
                    for spec in &group.settings {
                        let (was, now) = (spec.read(&before), spec.read(&after));
                        let asked = map.contains_key(&spec.key);
                        if asked || was != now {
                            let mut change = json!({"id": format!("{name}.{}", spec.key), "was": was, "now": now});
                            if !asked {
                                change["also"] = json!("moved because of another change");
                            }
                            done.push(change);
                        }
                    }
                }
                Err(list) => {
                    // The plugin kept something else than was asked: the whole batch goes back, so nothing is left that revert doesn't know of
                    for (name, before) in applied.iter().rev().filter(|(_, before)| before.is_object()) {
                        host.apply_preset(name, before);
                    }
                    host.schedule_save();
                    return Err(format!("Nothing was changed.\n{}", list.join("\n")));
                }
            }
        }
        // What moved is what the plugins now hold against what they held, so anything that went with a change is journalled too, declared or not
        let writes = moves(host, &before_all, &all_configs(host));
        self.remember_at(host, session, writes, source, &revs);
        Ok(json!({"changed": done, "note": "The camera shows these now. revert puts them back."}))
    }

    /// The revision of every setting the journal has a step for, as the host has them now: what a write is measured against, to see whether
    /// somebody else moved the setting before it.
    fn journal_revisions(&self, host: &dyn Host) -> HashMap<Slot, u64> {
        self.journal.iter().map(|c| ((c.plugin.clone(), c.key.clone()), host.setting_revision(&c.plugin, &c.key))).collect()
    }

    /// Journals writes with no revisions known: only the values tell whose they are.
    #[cfg(test)]
    fn remember(&mut self, session: u64, writes: Vec<(String, String, Value, Value)>, source: Option<String>) {
        self.remember_with(session, writes, source, None);
    }

    fn remember_at(&mut self, host: &mut dyn Host, session: u64, writes: Vec<(String, String, Value, Value)>, source: Option<String>, before: &HashMap<Slot, u64>) {
        self.remember_with(session, writes, source, Some((&*host, before)));
        host.schedule_save();
    }

    /// Journals writes as they happened. A write that moved nothing is no step. A connection moving the same setting again with nobody in between
    /// is still one step, from where it began to where it is now (so dragging a slider doesn't fill the journal), and a step that ends where it
    /// began goes away. Nobody in between means the value and, where the host counts them, the revision are still what the step left. Past
    /// JOURNAL_KEPT the oldest are forgotten.
    fn remember_with(&mut self, session: u64, writes: Vec<(String, String, Value, Value)>, source: Option<String>, revs: Option<(&dyn Host, &HashMap<Slot, u64>)>) {
        for (plugin, key, was, now) in writes {
            if was == now {
                continue;
            }
            self.next_seq += 1;
            let seq = self.next_seq;
            let rev_before = revs.and_then(|(_, before)| before.get(&(plugin.clone(), key.clone())).copied());
            let rev = revs.map(|(host, _)| host.setting_revision(&plugin, &key));
            let latest = self.journal.iter().rposition(|c| c.plugin == plugin && c.key == key && c.source == source);
            let untouched = |c: &Change| c.session == session && c.now == was && (c.rev.is_none() || rev_before.is_none() || c.rev == rev_before);
            match latest.filter(|&i| untouched(&self.journal[i])) {
                Some(i) => {
                    let mut step = self.journal.remove(i);
                    step.now = now;
                    step.seq = seq;
                    step.rev = rev;
                    if step.was != step.now {
                        self.journal.push(step);
                    }
                }
                None => self.journal.push(Change { seq, session, plugin, key, source: source.clone(), was, now, rev_before, rev, cut: false }),
            }
        }
        let extra = self.journal.len().saturating_sub(JOURNAL_KEPT);
        self.journal.drain(..extra);
    }

    fn revert(&mut self, host: &mut dyn Host, session: u64) -> Value {
        let (mine, rest): (Vec<Change>, Vec<Change>) = std::mem::take(&mut self.journal).into_iter().partition(|c| c.session == session);
        self.journal = rest;
        let (untracked, others): (Vec<Untracked>, Vec<Untracked>) = std::mem::take(&mut self.unrestored).into_iter().partition(|u| u.session == session);
        self.unrestored = others;
        let not_restored = not_restored(&untracked);
        if mine.is_empty() {
            let mut answer = json!({"reverted": [], "left": [], "note": "Nothing was changed through MCP this session."});
            if !not_restored.is_empty() {
                answer["not_restored"] = json!(not_restored);
                answer["note"] = json!("No setting was changed through MCP this session, so there is nothing to put back. Some actions it ran change things that aren't settings, which can't be put back (see not_restored).");
            }
            return answer;
        }
        let Unwound { reverted, left, kept, rev_before } = unwind(host, mine);
        self.keep(kept);
        self.resync(host, &reverted, &rev_before);
        host.schedule_save();
        let mut answer = json!({"reverted": reverted, "left": left});
        if !not_restored.is_empty() {
            answer["not_restored"] = json!(not_restored);
        }
        answer
    }

    /// Steps loaded from the file have no revisions, which would let any later edit that lands on the same value pass for the AI's own work. The newest
    /// step on a setting is tied to where it stands now, so only an edit after this counts against it. An older one follows it only when nothing but the AI
    /// moved the setting between the two (it was saved that way), or else it gets a revision that never matches and is left as it is.
    fn stamp_loaded(&mut self, host: &dyn Host) {
        let mut slots: Vec<Slot> = self.journal.iter().map(|c| (c.plugin.clone(), c.key.clone())).collect();
        slots.sort();
        slots.dedup();
        for slot in slots {
            let mut steps: Vec<usize> = (0..self.journal.len()).filter(|&i| (&self.journal[i].plugin, &self.journal[i].key) == (&slot.0, &slot.1)).collect();
            steps.sort_by_key(|&i| std::cmp::Reverse(self.journal[i].seq));
            // What the setting's revision was before the newer step on it
            let mut above: Option<u64> = None;
            for (n, i) in steps.into_iter().enumerate() {
                let c = &mut self.journal[i];
                if c.rev.is_none() && c.rev_before.is_none() {
                    let tied = match above {
                        _ if n == 0 => host.setting_revision(&c.plugin, &c.key),
                        Some(above) if !c.cut => above,
                        _ => NEVER_UNTOUCHED,
                    };
                    (c.rev_before, c.rev) = (Some(tied), Some(tied));
                }
                above = c.rev_before;
            }
        }
    }

    /// The journal as it is saved: each step says whether anyone but the AI moved its setting before the next write to it, which the revisions can tell now
    /// and nothing can after a restart.
    fn journal_json(&self) -> Vec<Value> {
        self.journal.iter().map(|c| {
            let next = self.journal.iter().filter(|n| n.plugin == c.plugin && n.key == c.key && n.seq > c.seq).min_by_key(|n| n.seq);
            let cut = match (next, c.rev) {
                (None, _) => false,
                (Some(next), Some(rev)) if next.rev_before.is_some() => next.rev_before != Some(rev),
                _ => c.cut,
            };
            c.to_json(cut)
        }).collect()
    }

    /// Steps an undo couldn't take yet (another camera is shown, a plugin is off) go back in the journal, in the order they happened.
    fn keep(&mut self, kept: Vec<Change>) {
        self.journal.extend(kept);
        self.journal.sort_by_key(|c| c.seq);
    }

    /// Putting a setting back is a move of it too. The step below it in the journal (the one that left it with the value it was put back to) is
    /// still the latest word on it, so it takes the new revision, and a later undo doesn't take its own work for somebody's edit. Only when it is
    /// the step the undone one began from: one that ends in the same value but was edited past since stays out of date.
    fn resync(&mut self, host: &dyn Host, reverted: &[Value], rev_before: &HashMap<Slot, Option<u64>>) {
        let shown = host.focused_source_id();
        for done in reverted {
            let Some((plugin, key)) = done["id"].as_str().and_then(|id| id.split_once('.')) else { continue };
            let back = &done["back_to"];
            let began_at = rev_before.get(&(plugin.to_string(), key.to_string())).copied().flatten();
            let still_latest = |c: &Change| began_at.is_none() || c.rev.is_none() || c.rev == began_at;
            if let Some(c) = self.journal.iter_mut().rev().find(|c| c.plugin == plugin && c.key == key && c.source == shown && c.now == *back && still_latest(c)) {
                c.rev = Some(host.setting_revision(plugin, key));
            }
        }
    }

    /// The header button's undo: puts back what any AI app changed, the newest write first whichever connection made it, so a setting two of them moved
    /// goes back to where it began. It is the person's own action, so it works whatever the access level.
    pub fn undo_all(&mut self, host: &mut dyn Host) {
        let all = std::mem::take(&mut self.journal);
        let not_restored = not_restored(&std::mem::take(&mut self.unrestored));
        let Unwound { reverted, left, kept, rev_before } = unwind(host, all);
        self.keep(kept);
        self.resync(host, &reverted, &rev_before);
        let ids: Vec<String> = reverted.iter().filter_map(|c| c["id"].as_str().map(String::from)).collect();
        let skipped = left.len() + not_restored.len();
        let text = if skipped == 0 { tr("Undid what the AI changed") } else { tr("Undid what the AI changed, except {n} it couldn't put back").field("n", skipped) };
        self.push_log(tr("You"), text, false);
        if !ids.is_empty() {
            host.emit(Event::SettingsTouched { ids, changed: true });
        }
        host.schedule_save();
        self.push_page(host);
    }

    /// Runs an action. What it moved is journalled and outlined like a set's, and the answer says when there is nothing for revert to undo. Everything
    /// the plugins hold is looked at before and after, so an action's own state (Privacy blur's switch, the microphone, the background) goes back too.
    fn run_action(&mut self, host: &mut dyn Host, session: u64, args: &Value) -> Reply {
        let Some(id) = args["action"].as_str() else { return Err("action has to be an action id from actions.".into()) };
        if OUT_OF_REACH.contains(&id) {
            return Err(format!("{id} is left to the person: starting, stopping, switching the camera and picking another phone can drop a call. They can use the window or `telescope --action {id}`."));
        }
        let given = match args.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(p)) => p.clone(),
            Some(_) => return Err("params has to be an object of name to value.".into()),
        };
        let Some((plugin, action)) = host.shortcut_actions().into_iter().find(|(_, a)| a.id == id) else {
            return Err(format!("There is no action called {id:?}. actions lists them."));
        };
        let params = Value::Object(checked_params(&action, &given, true)?);
        let revs = self.journal_revisions(host);
        let before = all_configs(host);
        let source = host.focused_source_id();
        let Some(said) = host.run_action(&plugin, id, &params) else {
            return Err(format!("{} can't be used right now (no device, or its control is off).", action.label));
        };
        let moved = moves(host, &before, &all_configs(host));
        let changed: Vec<Value> = moved.iter().map(|(plugin, key, was, now)| json!({"id": format!("{plugin}.{key}"), "was": was, "now": now})).collect();
        if changed.is_empty() {
            self.next_seq += 1;
            self.unrestored.push(Untracked { seq: self.next_seq, session, action: action.label.clone() });
            let extra = self.unrestored.len().saturating_sub(JOURNAL_KEPT);
            self.unrestored.drain(..extra);
        }
        self.remember_at(host, session, moved, source, &revs);
        if changed.is_empty() {
            Ok(json!({"said": said, "note": "No setting moved, so revert has nothing to put back for this action. It may have changed something that isn't a setting (a mute, Blur now), which revert can't put back, and its answer will say so. Anything it does later is not tracked."}))
        } else {
            Ok(json!({"said": said, "changed": changed, "note": "revert puts the settings it moved back. Anything it does later, or outside the settings (a mute, Blur now), is not tracked."}))
        }
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
            self.answer(host, id, reply);
        }
    }

    fn set_access(&mut self, host: &mut dyn Host, access: Access) {
        if self.access != access {
            self.access = access;
            host.schedule_save();
        }
        self.push_page(host);
    }
}

/// Setting ids as the log words them: the setting's name in plain words, and for a plugin's on switch the plugin.
fn setting_name(groups: &[PluginSettings], id: &str) -> String {
    let words = |key: &str| {
        let text = key.replace(['_', '.'], " ");
        let mut chars = text.chars();
        chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
    };
    match split_id(groups, id) {
        Some((group, spec)) if spec.key == "on" => group.title.clone(),
        Some((_, spec)) => words(&spec.key),
        None => id.to_string(),
    }
}

/// A value as the log words it.
fn value_text(value: &Value) -> String {
    match value {
        Value::Bool(true) => tr("on"),
        Value::Bool(false) => tr("off"),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Up to three things in a row, then how many more.
fn listed(mut items: Vec<String>) -> String {
    let more = items.len().saturating_sub(3);
    items.truncate(3);
    let text = items.join(", ");
    if more > 0 { tr("{list} and {n} more").field("list", text).field("n", more) } else { text }
}

fn watch_seconds(args: &Value) -> Option<u64> {
    match args.get("seconds") {
        None | Some(Value::Null) => Some(30),
        Some(v) => v.as_f64().filter(|n| (5.0..=WATCH_MAX_SECONDS as f64).contains(n)).map(|n| n as u64),
    }
}

/// The series names a call asks for, none for all.
fn series_names(args: &Value) -> Result<Vec<String>, String> {
    match args.get("series") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) => Ok(list.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
        Some(_) => Err("series has to be a list of names.".into()),
    }
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

/// The actions as the tool lists them, without the ones left to the person.
fn list_actions(host: &dyn Host) -> Vec<Value> {
    host.shortcut_actions().into_iter().filter(|(_, a)| !OUT_OF_REACH.contains(&a.id.as_str()))
        .map(|(_, a)| json!({"id": a.id, "group": a.group, "label": a.label, "params": a.params, "current": a.current})).collect()
}

/// What every running plugin holds now, by plugin: its whole get_config(), not only the settings it declares, since an action can move what isn't
/// declared (Privacy blur's switch). This plugin's own is left out.
fn all_configs(host: &dyn Host) -> Vec<(String, Value)> {
    let mut names: Vec<String> = host.running_plugins().into_iter().map(|(name, _)| name).collect();
    names.extend(host.plugin_settings().into_iter().map(|g| g.name));
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| name != NAME && seen.insert(name.clone()));
    names.into_iter().filter_map(|name| host.plugin_config(&name).map(|config| (name, config))).collect()
}

/// What moved between two looks at every plugin's config, as (plugin, key, was, now). A declared setting is read the way the plugin's settings read
/// it (a value that was never written is its default), so one that only started to be written isn't a move; anything else is a spot in the config,
/// with null for a value that wasn't there.
fn moves(host: &dyn Host, before: &[(String, Value)], after: &[(String, Value)]) -> Vec<(String, String, Value, Value)> {
    let groups = host.plugin_settings();
    let mut out = Vec::new();
    for (plugin, now) in after {
        let Some((_, was)) = before.iter().find(|(name, _)| name == plugin) else { continue };
        let specs: &[SettingSpec] = groups.iter().find(|g| g.name == *plugin).map(|g| g.settings.as_slice()).unwrap_or_default();
        for (key, was_at, now_at) in leaf_changes(was, now) {
            let (was_at, now_at) = match specs.iter().find(|s| s.key == key) {
                Some(spec) => (spec.read(was), spec.read(now)),
                None => (was_at, now_at),
            };
            if was_at != now_at {
                out.push((plugin.clone(), key, was_at, now_at));
            }
        }
    }
    out
}

/// What undo says about actions that moved nothing it could put back.
fn not_restored(untracked: &[Untracked]) -> Vec<Value> {
    untracked.iter().map(|u| json!({
        "action": u.action,
        "why": "it didn't move any saved setting, so there is nothing to put back. If it changed something that isn't a setting (a mute, Blur now), that stays as it is.",
    })).collect()
}

/// Puts back what these writes did, the newest first, and says what went back and what was left. A setting goes back only while nobody has
/// touched it since: it has to still hold the value its write gave it, and where the host counts revisions it must not have moved since (the
/// person landing on the same value again is still an edit). Anything else stays as it is. What goes back is put back plugin by plugin in one
/// go so settings that depend on each other are valid together, and when that would move a setting that isn't going back (zooming out puts
/// the pan back to the middle) it isn't done at all. A write for a camera that isn't shown, or a plugin that is off, isn't lost: it comes
/// back in `kept` for an undo while it can be done.
fn unwind(host: &mut dyn Host, mut changes: Vec<Change>) -> Unwound {
    changes.sort_by_key(|c| std::cmp::Reverse(c.seq));
    let groups = host.plugin_settings();
    let shown = host.focused_source_id();
    let (mut reverted, mut left, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    // Where each setting stands once the writes undone so far are back, and what its revision was before the one undone last
    let mut standing: HashMap<Slot, Value> = HashMap::new();
    let mut standing_rev: HashMap<Slot, Option<u64>> = HashMap::new();
    let mut back: Vec<(String, Map<String, Value>)> = Vec::new();
    for c in changes {
        let id = format!("{}.{}", c.plugin, c.key);
        let Some(config) = host.plugin_config(&c.plugin) else {
            left.push(json!({"id": id, "why": "the plugin is switched off now, so it was kept: undo again once it is on"}));
            kept.push(c);
            continue;
        };
        let slot = (c.plugin.clone(), c.key.clone());
        let spec = groups.iter().find(|g| g.name == c.plugin).and_then(|g| g.settings.iter().find(|s| s.key == c.key));
        let read = |config: &Value| match spec {
            Some(spec) => spec.read(config),
            None => get_path(config, &c.key).cloned().unwrap_or(Value::Null),
        };
        let now = standing.get(&slot).cloned().unwrap_or_else(|| read(&config));
        let expected = standing_rev.get(&slot).copied().unwrap_or_else(|| Some(host.setting_revision(&c.plugin, &c.key)));
        if c.source != shown {
            left.push(json!({"id": id, "why": "a different camera is shown now, so it was kept: undo again while that camera is shown"}));
            kept.push(c);
        } else if now != c.now {
            left.push(json!({"id": id, "why": "it was changed since by hand or by another AI app, so it was left as it is", "now": now}));
        } else if matches!((expected, c.rev), (Some(a), Some(b)) if a != b) {
            left.push(json!({"id": id, "why": "it was edited since (and has the same value again), so it was left as it is", "now": now}));
        } else {
            standing.insert(slot.clone(), c.was.clone());
            standing_rev.insert(slot, c.rev_before);
            match back.iter_mut().find(|(name, _)| *name == c.plugin) {
                Some((_, map)) => {
                    map.insert(c.key, c.was);
                }
                None => back.push((c.plugin, Map::from_iter([(c.key, c.was)]))),
            }
        }
    }
    for (plugin, map) in back {
        let specs: &[SettingSpec] = groups.iter().find(|g| g.name == plugin).map(|g| g.settings.as_slice()).unwrap_or_default();
        let (declared, raw): (Map<String, Value>, Map<String, Value>) = map.iter().map(|(k, v)| (k.clone(), v.clone())).partition(|(k, _)| specs.iter().any(|s| s.key == *k));
        let before = host.plugin_config(&plugin).unwrap_or(Value::Null);
        let undo = |host: &mut dyn Host| {
            if before.is_object() {
                host.apply_preset(&plugin, &before);
                host.schedule_save();
            }
        };
        // The declared ones go in through the plugin's checks, then the rest are put into what that left
        let put = if declared.is_empty() { Ok(before.clone()) } else { host.change_plugin_settings(&plugin, &declared) };
        let put = put.map(|after| {
            if raw.is_empty() {
                return after;
            }
            let mut patched = after.clone();
            for (key, was) in &raw {
                put_path(&mut patched, key, was.clone());
            }
            host.apply_preset(&plugin, &patched);
            host.schedule_save();
            host.plugin_config(&plugin).unwrap_or(patched)
        });
        match put {
            Ok(after) => {
                let also: Vec<String> = specs.iter().filter(|s| !map.contains_key(&s.key) && s.read(&before) != s.read(&after)).map(|s| format!("{plugin}.{}", s.key)).collect();
                if also.is_empty() {
                    reverted.extend(map.iter().map(|(key, was)| json!({"id": format!("{plugin}.{key}"), "back_to": was})));
                } else {
                    undo(host);
                    let why = format!("putting it back would also have moved {}, which has been changed since, so it was left", also.join(", "));
                    left.extend(map.keys().map(|key| json!({"id": format!("{plugin}.{key}"), "why": why})));
                }
            }
            Err(list) => {
                undo(host);
                left.extend(map.keys().map(|key| json!({"id": format!("{plugin}.{key}"), "why": list.join(" ")})));
            }
        }
    }
    Unwound { reverted, left, kept, rev_before: standing_rev }
}

impl Plugin for McpPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("MCP server").into(),
            about: n_("Lets an AI app such as Claude Desktop look at Telescope and change its settings, through a command that opens no network port.").into(),
            when_off: n_("Without it AI apps can't connect to Telescope.").into(),
            category: "extras".into(),
            icon: "sliders".into(),
            optional: true,
            header_side: "right".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.handle.attach(host.poster());
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.stamp_loaded(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.handle.detach();
        self.stats_lease = false;
        self.watches.clear();
        self.stats_wanted = false;
        host.want_stats(NAME, false);
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec { source: SlintSource::Text(UI.into()), component: "McpButton".into(), props: self.header_props() })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: TAB.into(),
            tab: TAB.into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "McpSettings".into(), props: self.page_props() },
            order: 0,
            tab_title: tr("MCP"),
            keywords: tr_ctx("search", "mcp ai claude model assistant agent connect access read only"),
            footer: None,
        }]
    }

    fn get_config(&self) -> Value {
        // The undo journal is kept with the settings, so what the AI changed can still be undone after Telescope restarts
        json!({"access": self.access.name(), "journal": self.journal_json()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        // A missing or unknown value is the default: turning the plugin on is the deliberate choice
        self.access = cfg.get("access").and_then(Value::as_str).and_then(Access::from_name).unwrap_or_default();
        // A config without a journal (only the access level) leaves the one it has
        if let Some(list) = cfg.get("journal").and_then(Value::as_array) {
            let mut journal: Vec<Change> = list.iter().filter_map(Change::from_json).collect();
            journal.sort_by_key(|c| c.seq);
            let extra = journal.len().saturating_sub(JOURNAL_KEPT);
            journal.drain(..extra);
            self.next_seq = self.next_seq.max(journal.last().map_or(0, |c| c.seq));
            self.journal = journal;
        }
        Ok(())
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        match (event.ui.as_str(), event.callback.as_str()) {
            (HEADER, "open") => host.open_settings(TAB),
            (HEADER, "undo") => self.undo_all(host),
            (TAB, "access-picked") => {
                let access = if event.args.first().and_then(Value::as_u64) == Some(0) { Access::ReadOnly } else { Access::ReadAndChange };
                self.set_access(host, access);
            }
            (TAB, "copy-clicked") => host.copy_text(&Self::connect_entry()),
            _ => {}
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::Posted { owner, token, data } if owner == NAME && token == POST_CALL => {
                let id = data["id"].as_u64().unwrap_or(0);
                self.run(host, id, data["session"].as_u64().unwrap_or(0), data["tool"].as_str().unwrap_or(""), &data["args"]);
            }
            Event::Posted { owner, token, data } if owner == NAME && token == POST_CANCEL => {
                self.cancel(host, data["id"].as_u64().unwrap_or(0));
            }
            Event::Posted { owner, token, data } if owner == NAME && token == POST_CLIENT => {
                // The first AI app to connect is announced, so nobody finds out by surprise
                if data["gone"].is_null() && data["named"].is_null() && data["clients"].as_u64() == Some(1) {
                    host.send_notification("Telescope", &tr("An AI app is connected to Telescope through the MCP server."), false);
                }
                self.push_page(host);
            }
            Event::StatsUpdated { snapshot } => {
                for watch in self.watches.values_mut() {
                    watch.collected.push(snapshot);
                }
                self.snapshot = Some(snapshot.clone());
            }
            Event::PictureMeasured { token, ok, message, data } => {
                let reply = if *ok { Ok(data.clone()) } else { Err(message.clone()) };
                self.job_done(host, token, reply);
            }
            Event::TimerFired { owner, token } if owner == NAME => {
                if let Some(job) = token.strip_prefix("job:") {
                    let why = "Nothing measured the picture. Measuring needs Colour grading switched on in Plugins, and a stream running.";
                    self.job_done(host, job, Err(why.into()));
                } else if let Some(id) = token.strip_prefix("watch:").and_then(|id| id.parse().ok()) {
                    self.watch_done(host, id);
                } else if token == T_CALM {
                    if self.pending == 0 {
                        self.activity = Activity::Rest;
                        self.push_header(host);
                    }
                } else if token == T_UNWANT {
                    self.stats_lease = false;
                    self.sync_stats(host);
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
