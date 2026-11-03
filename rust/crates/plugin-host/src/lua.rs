//! The Lua runtime: one Lua 5.4 state per plugin, with no file, OS or network access beyond what the host API grants.
//!
//! A plugin's main.lua returns a table of hooks named like core's Plugin methods, plus `events` for the bus. Its globals are Lua's table,
//! string, math, utf8 and coroutine libraries, a few clock functions in os, and Telescope's own: host, tr, trn, N_, files, log, json and
//! a require that only loads the plugin's own .lua files. Every call into Rust goes through one bridge that exists only while a hook runs.

use crate::guest::{host_method, named_args, Guest, HostCalls, HOST_API};
use crate::package::capped;
use mlua::serde::{DeserializeOptions, SerializeOptions};
use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, MultiValue, RegistryKey, StdLib, Table, VmState};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How much memory a plugin's Lua state may hold.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// How long one hook may run before it's stopped, so a loop that never ends can't freeze the window.
pub const HOOK_LIMIT: Duration = Duration::from_secs(3);
/// The same for running main.lua the first time.
pub const LOAD_LIMIT: Duration = Duration::from_secs(5);

/// Longest pattern string.find, match, gmatch and gsub take from a plugin.
pub const MAX_PATTERN: usize = 128;
/// How many steps of Lua's matcher those four let a pattern take at worst on a text.
pub const MAX_PATTERN_WORK: f64 = 1e8;

/// What matching `pattern` on a text of `len` bytes could cost at worst, in steps of Lua's matcher, or None if the pattern is cut short
/// or broken (Lua then reports that itself). Lua's matcher backtracks without running Lua code, so the deadline hook never gets to stop it.
/// Each `*`, `+` or `-` with more pattern after it can be tried at every length, each `?` both ways, and a pattern that isn't anchored
/// with `^` is tried from every position.
fn pattern_work(pattern: &[u8], len: usize) -> Option<f64> {
    let anchored = pattern.first() == Some(&b'^');
    let mut i = usize::from(anchored);
    // Every piece that has to match, with the repetition it carries
    let mut pieces: Vec<Option<u8>> = Vec::new();
    while i < pattern.len() {
        match pattern[i] {
            b'(' => i += if pattern.get(i + 1) == Some(&b')') { 2 } else { 1 },
            b')' => i += 1,
            b'$' if i + 1 == pattern.len() => {
                i += 1;
                pieces.push(None);
            }
            b'%' if matches!(pattern.get(i + 1), Some(b'b')) => {
                i += 4;
                pieces.push(None);
            }
            b'%' if matches!(pattern.get(i + 1), Some(b'f')) => {
                i = set_end(pattern, i + 2)?;
                pieces.push(None);
            }
            b'%' if matches!(pattern.get(i + 1), Some(b'0'..=b'9')) => {
                i += 2;
                pieces.push(None);
            }
            first => {
                i = match first {
                    b'%' => {
                        pattern.get(i + 1)?;
                        i + 2
                    }
                    b'[' => set_end(pattern, i)?,
                    _ => i + 1,
                };
                let repeat = pattern.get(i).copied().filter(|c| b"*+-?".contains(c));
                i += usize::from(repeat.is_some());
                pieces.push(repeat);
            }
        }
    }
    let (mut stars, mut options) = (0i32, 0i32);
    // A repetition at the very end has nothing after it that could fail, so it never gives anything back
    for repeat in pieces.iter().rev().skip(1) {
        match repeat {
            Some(b'?') => options += 1,
            Some(_) => stars += 1,
            None => {}
        }
    }
    let tries = stars + i32::from(!anchored);
    let orderings: f64 = (1..=tries).map(f64::from).product();
    Some((len as f64 + 1.0).powi(tries) / orderings * 2f64.powi(options))
}

/// Where a [set] starting at `start` ends, as Lua's matcher reads it (the first character after the bracket is part of it whatever it is).
fn set_end(pattern: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if pattern.get(i) == Some(&b'^') {
        i += 1;
    }
    loop {
        let c = *pattern.get(i)?;
        i += 1;
        if c == b'%' {
            i += 1;
        }
        if *pattern.get(i)? == b']' {
            return Some(i + 1);
        }
    }
}

/// How many bytes a string or number argument has, the way Lua's string functions read it.
fn text_len(arg: Option<&mlua::Value>) -> Option<usize> {
    match arg? {
        mlua::Value::String(s) => Some(s.as_bytes().len()),
        v @ (mlua::Value::Integer(_) | mlua::Value::Number(_)) => Some(shown(v).len()),
        _ => None,
    }
}

/// Refuses a call of string.find, match, gmatch or gsub whose pattern could keep Lua's matcher busy for longer than a hook may run.
fn check_pattern(name: &str, args: &MultiValue) -> mlua::Result<()> {
    // find(s, pattern, init, true) takes the pattern as plain text, which is a single scan
    if name == "find" && args.get(3).is_some_and(|plain| !matches!(plain, mlua::Value::Nil | mlua::Value::Boolean(false))) {
        return Ok(());
    }
    let (Some(subject), Some(pattern)) = (text_len(args.front()), args.get(1)) else { return Ok(()) };
    let pattern = match pattern {
        mlua::Value::String(s) => s.as_bytes().to_vec(),
        v @ (mlua::Value::Integer(_) | mlua::Value::Number(_)) => shown(v).into_bytes(),
        _ => return Ok(()),
    };
    if pattern.len() > MAX_PATTERN {
        return Err(lua_err(format!("string.{name}: a pattern can be at most {MAX_PATTERN} bytes long")));
    }
    if pattern_work(&pattern, subject).is_some_and(|work| work > MAX_PATTERN_WORK) {
        return Err(lua_err(format!(
            "string.{name}: this pattern could take far too long on a text this long. Use a simpler pattern (fewer *, +, - and ? with more pattern after them), cut the text down first, or look for plain text with string.find(text, what, 1, true)")));
    }
    Ok(())
}

/// The globals a plugin's Lua code gets beyond Lua's own table, string, math, utf8 and coroutine libraries. Frozen in tests/plugin_api.txt.
pub const GLOBALS: &[&str] = &["host", "tr", "trn", "N_", "files.read", "log.debug", "log.info", "log.warn", "log.error", "print", "json.encode",
                               "json.decode", "require", "load", "os.time", "os.clock", "os.date", "os.difftime"];

/// The registry slot the bridge reads the current hook's calls from.
const CALLS: &str = "telescope.calls";

/// Sets up Telescope's globals; gets the bridge, Lua's own load and the host method names.
const PRELUDE: &str = r##"
local bridge, base_load, host_methods = ...
local loaded = {}

function require(name)
  if type(name) ~= "string" then error("require takes a module name", 2) end
  if loaded[name] == nil then
    local source, chunkname = bridge("module", name)
    local chunk = assert(base_load(source, chunkname, "t"))
    local result = chunk(name)
    loaded[name] = result == nil and true or result
  end
  return loaded[name]
end

function load(chunk, chunkname, mode, env)
  return base_load(chunk, chunkname, "t", env)
end

host = {}
for _, name in ipairs(host_methods) do
  host[name] = function(...) return bridge("host", name, ...) end
end

function tr(text, fields, ctx) return bridge("tr", text, fields, ctx) end
function trn(singular, plural, n, fields, ctx) return bridge("trn", singular, plural, n, fields, ctx) end
function N_(text) return text end

files = { read = function(path) return bridge("read", path) end }

log = {}
for _, level in ipairs({ "debug", "info", "warn", "error" }) do
  log[level] = function(...)
    local parts = {}
    for i = 1, select("#", ...) do parts[i] = tostring((select(i, ...))) end
    bridge("log", level, table.concat(parts, " "))
  end
end
print = log.info
"##;

fn ser() -> SerializeOptions {
    SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false)
}

fn de() -> DeserializeOptions {
    DeserializeOptions::new().deny_unsupported_types(false).deny_recursive_tables(true)
}

fn to_lua(lua: &Lua, v: &Value) -> mlua::Result<mlua::Value> {
    lua.to_value_with(v, ser())
}

/// Most values and depth a table handed to the host may have, counting a shared table every time it is reached.
const MAX_NODES: usize = 200_000;
const MAX_DEPTH: usize = 64;

/// Walks v the way from_value will, so a table that shares itself over and over (a DAG that expands to billions of nodes) is refused first.
fn check_size(v: &mlua::Value, nodes: &mut usize, depth: usize) -> mlua::Result<()> {
    *nodes += 1;
    if *nodes > MAX_NODES || depth > MAX_DEPTH {
        return Err(lua_err("a value passed to the host is too big or nested too deep"));
    }
    if let mlua::Value::Table(t) = v {
        for pair in t.pairs::<mlua::Value, mlua::Value>() {
            let (k, v) = pair?;
            check_size(&k, nodes, depth + 1)?;
            check_size(&v, nodes, depth + 1)?;
        }
    }
    Ok(())
}

fn from_lua(lua: &Lua, v: mlua::Value) -> mlua::Result<Value> {
    check_size(&v, &mut 0, 0)?;
    lua.from_value_with(v, de())
}

fn lua_err(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::RuntimeError(e.to_string())
}

/// A Lua error as one readable message: what the plugin's code said and where (error() puts the file and line first), without the
/// traceback, which goes to the log.
fn message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::CallbackError { cause, traceback } => {
            log::debug!("{traceback}");
            message(cause)
        }
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => {
            let (first, trace) = m.split_once("\nstack traceback:").unwrap_or((m, ""));
            if !trace.is_empty() {
                log::debug!("stack traceback:{trace}");
            }
            first.to_string()
        }
        mlua::Error::MemoryError(_) => format!("it used more than {} MB of memory", MEMORY_LIMIT >> 20),
        other => other.to_string(),
    }
}

/// A Lua value shown in a filled-in text, the way tostring() shows it.
fn shown(v: &mlua::Value) -> String {
    match v {
        mlua::Value::String(s) => s.to_string_lossy(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() { format!("{n:.1}") } else { n.to_string() }
        }
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Nil => "nil".into(),
        other => format!("{other:?}"),
    }
}

/// A fields table as (name, shown value) pairs.
fn fields(v: Option<mlua::Value>) -> mlua::Result<Vec<(String, String)>> {
    let Some(mlua::Value::Table(t)) = v else { return Ok(Vec::new()) };
    t.pairs::<String, mlua::Value>().map(|kv| kv.map(|(k, v)| (k, shown(&v)))).collect()
}

fn opt_string(v: Option<mlua::Value>) -> String {
    match v {
        Some(mlua::Value::String(s)) => s.to_string_lossy(),
        _ => String::new(),
    }
}

/// What the bridge does for each call from the prelude.
fn bridge_call(lua: &Lua, calls: &mut HostCalls<'_>, op: &str, args: MultiValue) -> mlua::Result<MultiValue> {
    let mut args = args.into_iter();
    let mut next = || args.next();
    let text = |v: Option<mlua::Value>| -> mlua::Result<String> {
        match v {
            Some(mlua::Value::String(s)) => Ok(s.to_str()?.to_string()),
            _ => Err(lua_err(format!("{op} needs a text"))),
        }
    };
    let one = |v: mlua::Value| Ok(MultiValue::from_vec(vec![v]));
    match op {
        "host" => {
            let name = text(next())?;
            let method = host_method(&name).ok_or_else(|| lua_err(format!("host has no method {name}")))?;
            let positional: Vec<Value> = std::iter::from_fn(&mut next).map(|v| from_lua(lua, v)).collect::<mlua::Result<_>>()?;
            let named = named_args(method, positional).map_err(lua_err)?;
            let answer = calls.call(&name, named).map_err(lua_err)?;
            one(to_lua(lua, &answer)?)
        }
        "tr" => {
            let msgid = text(next())?;
            let pairs = fields(next())?;
            let ctx = opt_string(next());
            let refs: Vec<(&str, &dyn std::fmt::Display)> = pairs.iter().map(|(k, v)| (k.as_str(), v as &dyn std::fmt::Display)).collect();
            one(mlua::Value::String(lua.create_string(calls.context().tr(&msgid, &ctx, &refs))?))
        }
        "trn" => {
            let (singular, plural) = (text(next())?, text(next())?);
            let n = match next() {
                Some(mlua::Value::Integer(i)) => i,
                Some(mlua::Value::Number(f)) => f as i64,
                _ => return Err(lua_err("trn needs a number as its third argument")),
            };
            let pairs = fields(next())?;
            let ctx = opt_string(next());
            let refs: Vec<(&str, &dyn std::fmt::Display)> = pairs.iter().map(|(k, v)| (k.as_str(), v as &dyn std::fmt::Display)).collect();
            one(mlua::Value::String(lua.create_string(calls.context().trn(&singular, &plural, n, &ctx, &refs))?))
        }
        "read" => {
            let path = text(next())?;
            one(mlua::Value::String(lua.create_string(calls.context().read_text(&path).map_err(lua_err)?)?))
        }
        "module" => {
            let name = text(next())?;
            if name.is_empty() || !name.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
                return Err(lua_err(format!("{name:?} isn't a module name")));
            }
            let path = format!("{}.lua", name.replace('.', "/"));
            let source = calls.context().read_text(&path).map_err(lua_err)?;
            Ok(MultiValue::from_vec(vec![mlua::Value::String(lua.create_string(source)?), mlua::Value::String(lua.create_string(format!("@{path}"))?)]))
        }
        "log" => {
            let level = text(next())?;
            let line = text(next())?;
            let name = &calls.context().info.name;
            match level.as_str() {
                "debug" => log::debug!("[{name}] {line}"),
                "warn" => log::warn!("[{name}] {line}"),
                "error" => log::error!("[{name}] {line}"),
                _ => log::info!("[{name}] {line}"),
            }
            Ok(MultiValue::new())
        }
        other => Err(lua_err(format!("no bridge call {other}"))),
    }
}

/// A plugin's Lua state and the table its main.lua returned.
pub struct LuaGuest {
    lua: Lua,
    plugin: RegistryKey,
    deadline: Arc<Mutex<Option<Instant>>>,
    /// check-plugin's run: get_config() fails on what can't be saved, where Telescope leaves it out.
    strict_config: AtomicBool,
}

impl LuaGuest {
    /// A fresh sandboxed state with Telescope's globals, then main (a path inside the plugin) run in it.
    pub fn load(calls: &mut HostCalls<'_>, main: &str) -> Result<LuaGuest, String> {
        let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE | StdLib::OS, LuaOptions::default())
            .map_err(|e| message(&e))?;
        let deadline = Arc::new(Mutex::new(None));
        Self::sandbox(&lua, &deadline).map_err(|e| message(&e))?;
        let source = calls.context().read_text(main)?;
        let mut guest = LuaGuest { plugin: lua.create_registry_value(mlua::Value::Nil).map_err(|e| message(&e))?, lua, deadline, strict_config: AtomicBool::new(false) };
        let returned = guest.run(calls, LOAD_LIMIT, |lua| {
            lua.load(source.as_str()).set_name(format!("@{main}")).set_mode(ChunkMode::Text).call::<mlua::Value>(())
        })?;
        let mlua::Value::Table(table) = returned else {
            return Err(crate::text::tr("Its {file} has to return the plugin's table at the end.", &[("file", &main)]));
        };
        guest.plugin = guest.lua.create_registry_value(table).map_err(|e| message(&e))?;
        Ok(guest)
    }

    fn sandbox(lua: &Lua, deadline: &Arc<Mutex<Option<Instant>>>) -> mlua::Result<()> {
        let g = lua.globals();
        let os: Table = g.get("os")?;
        let safe_os = lua.create_table()?;
        for name in ["time", "clock", "date", "difftime"] {
            safe_os.set(name, os.get::<Function>(name)?)?;
        }
        g.set("os", safe_os)?;
        for name in ["dofile", "loadfile"] {
            g.set(name, mlua::Value::Nil)?;
        }
        let string: Table = g.get("string")?;
        string.set("dump", mlua::Value::Nil)?;
        // Lua's pattern matcher can backtrack for ages without running a Lua instruction, which the deadline can't interrupt
        for name in ["find", "match", "gmatch", "gsub"] {
            let original: Function = string.get(name)?;
            string.set(name, lua.create_function(move |_, args: MultiValue| {
                check_pattern(name, &args)?;
                original.call::<MultiValue>(args)
            })?)?;
        }
        let json = lua.create_table()?;
        json.set("encode", lua.create_function(|lua, v: mlua::Value| {
            serde_json::to_string(&from_lua(lua, v)?).map_err(lua_err)
        })?)?;
        json.set("decode", lua.create_function(|lua, text: String| {
            let v: Value = serde_json::from_str(&text).map_err(lua_err)?;
            to_lua(lua, &v)
        })?)?;
        g.set("json", json)?;
        let bridge = lua.create_function(|lua, args: MultiValue| {
            let calls: Function = lua.named_registry_value(CALLS).map_err(|_| lua_err("Telescope's functions only work while a hook runs"))?;
            calls.call::<MultiValue>(args)
        })?;
        let names: Vec<&str> = HOST_API.iter().map(|m| m.name).collect();
        let base_load: Function = g.get("load")?;
        lua.load(PRELUDE).set_name("=telescope").call::<()>((bridge, base_load, names))?;
        lua.set_memory_limit(MEMORY_LIMIT)?;
        let deadline = Arc::clone(deadline);
        lua.set_hook(HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            match *deadline.lock().unwrap_or_else(|e| e.into_inner()) {
                Some(at) if Instant::now() > at => Err(lua_err("it took too long and was stopped")),
                _ => Ok(VmState::Continue),
            }
        })?;
        Ok(())
    }

    /// Runs f with the bridge pointing at calls and a time limit; the bridge stops working when it returns.
    fn run<R>(&self, calls: &mut HostCalls<'_>, limit: Duration, f: impl FnOnce(&Lua) -> mlua::Result<R>) -> Result<R, String> {
        let result = self.lua.scope(|scope| {
            let bridge = scope.create_function_mut(|lua, (op, args): (String, MultiValue)| bridge_call(lua, calls, &op, args))?;
            self.lua.set_named_registry_value(CALLS, bridge)?;
            *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + limit);
            let result = f(&self.lua);
            *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.lua.set_named_registry_value(CALLS, mlua::Value::Nil)?;
            result
        });
        result.map_err(|e| capped(&message(&e)).to_string())
    }

    fn table(&self) -> Option<Table> {
        self.lua.registry_value(&self.plugin).ok()
    }

    /// Runs a lookup that may go through a metatable (a plugin that inherits from a base) under the deadline, so a looping __index is stopped.
    fn looked_up<R>(&self, f: impl FnOnce() -> mlua::Result<Option<R>>) -> Option<R> {
        let started = {
            let mut at = self.deadline.lock().unwrap_or_else(|e| e.into_inner());
            at.is_none().then(|| *at = Some(Instant::now() + HOOK_LIMIT)).is_some()
        };
        let found = f().ok().flatten();
        if started {
            *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        found
    }

    fn function(&self, hook: &str) -> Option<Function> {
        let table = self.table()?;
        self.looked_up(|| table.get::<Option<Function>>(hook))
    }

    fn handler(&self, event: &str) -> Option<Function> {
        let table = self.table()?;
        self.looked_up(|| {
            let Some(events) = table.get::<Option<Table>>("events")? else { return Ok(None) };
            events.get::<Option<Function>>(event)
        })
    }

    /// Whether get_config() fails on a value that can't be saved, for check-plugin.
    pub fn set_strict_config(&self, strict: bool) {
        self.strict_config.store(strict, Ordering::SeqCst);
    }

    /// The Lua state, for the test harness.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }
}

impl Guest for LuaGuest {
    fn call(&mut self, calls: &mut HostCalls<'_>, hook: &str, args: Vec<Value>) -> Result<Value, String> {
        let func = if hook == "on_event" {
            let event = args.first().cloned().unwrap_or_default();
            let name = match event.get("event").and_then(Value::as_str) {
                Some("custom") => event.get("name").and_then(Value::as_str).unwrap_or(""),
                Some(name) => name,
                None => "",
            };
            self.handler(name).ok_or_else(|| format!("no handler for {name}"))?
        } else {
            self.function(hook).ok_or_else(|| format!("no hook {hook}"))?
        };
        self.run(calls, HOOK_LIMIT, |lua| {
            let lua_args = args.iter().map(|a| to_lua(lua, a)).collect::<mlua::Result<Vec<_>>>()?;
            let returned: mlua::Value = func.call(MultiValue::from_vec(lua_args))?;
            if hook == "get_config" && self.strict_config.load(Ordering::SeqCst) {
                check_size(&returned, &mut 0, 0)?;
                return lua.from_value_with(returned, de().deny_unsupported_types(true))
                    .map_err(|e| lua_err(format!("get_config() has to return what JSON can hold (tables, text, numbers, true/false): {e}")));
            }
            from_lua(lua, returned)
        })
    }

    fn defines(&self, hook: &str) -> bool {
        self.function(hook).is_some()
    }

    fn listens(&self, event: &str) -> bool {
        self.handler(event).is_some()
    }
}
