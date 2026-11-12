//! Lua plugins end to end: the sandbox, the two example plugins, check-plugin's report and the frozen plugin API.

use serde_json::{json, Value};
use serde_json::to_value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use telescope_core::event::{LensBox, RememberedSource, SourceName};
use telescope_core::host::{HostShell, Pick, PickKind, View};
use telescope_core::plugin::{load_config_tolerantly, Plugin, PluginInfo, StreamInfo};
use telescope_core::testing::{scratch_dir, FakeHost};
use telescope_core::ui::{added_issue_key, SlintSource, UiEvent};
use telescope_core::Event;
use telescope_plugin_host::guest::{Guest, GuestPlugin, HostCalls, PluginContext, HOOKS, HOST_API};
use telescope_plugin_host::lua::GLOBALS;
use telescope_plugin_host::testing::{check, validate_shader, PluginTest};
use telescope_plugin_host::{OLDEST_PLUGIN_API, PLUGIN_API};

const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/example-plugins");

fn example(name: &str) -> PathBuf {
    Path::new(EXAMPLES).join(name)
}

/// A plugin folder with this main.lua and plugin.json keys.
fn plugin(name: &str, main: &str, extra: Value) -> PathBuf {
    let folder = scratch_dir("lua").join(name);
    std::fs::create_dir(&folder).unwrap();
    let mut manifest = json!({"name": name, "api": 2});
    for (k, v) in extra.as_object().into_iter().flatten() {
        manifest[k] = v.clone();
    }
    std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(folder.join("main.lua"), main).unwrap();
    folder
}

// ── The sandbox ─────────────────────────────────────────────────────────────

#[test]
fn a_plugin_can_reach_no_files_programs_or_network() {
    let folder = plugin("sealed", r#"
        local gone = { io = io, package = package, debug = debug, dofile = dofile, loadfile = loadfile, ["string.dump"] = string.dump,
                       ["os.execute"] = os.execute, ["os.remove"] = os.remove, ["os.rename"] = os.rename, ["os.exit"] = os.exit,
                       ["os.getenv"] = os.getenv, ["os.tmpname"] = os.tmpname, require_outside = pcall(require, "..x") or nil }
        local left = {}
        for name in pairs(gone) do left[#left + 1] = name end
        table.sort(left)
        return { left = function() return left end, now = function() return type(os.time()) end }
    "#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.call("left", vec![]).unwrap(), json!({}), "an empty table: nothing dangerous is there");
    assert_eq!(t.call("now", vec![]).unwrap(), json!("number"));
}

#[test]
fn a_hook_that_never_ends_is_stopped() {
    let folder = plugin("loops", "return { setup = function() while true do end end }\n", json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    let started = std::time::Instant::now();
    let err = t.call("setup", vec![]).unwrap_err();
    assert!(err.contains("took too long") && started.elapsed() < std::time::Duration::from_secs(10), "{err}");
    assert_eq!(t.call_without_host("get_config", vec![]).unwrap_or_default(), Value::Null, "the plugin still answers afterwards");
}

/// Runs `run` of a plugin with this body on another thread and gives its answer, or fails if it hasn't one after a few seconds.
fn answer_soon(name: &str, body: &str) -> Result<Value, String> {
    let folder = plugin(name, &format!("return {{ run = function() {body} end }}\n"), json!({}));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(PluginTest::open(&folder).and_then(|mut t| t.call("run", vec![])));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the pattern froze the plugin's thread")
}

/// How long a call takes, with its answer, for the patterns that must be stopped quickly.
fn timed(name: &str, body: &str) -> (Result<Value, String>, std::time::Duration) {
    let started = std::time::Instant::now();
    let answer = answer_soon(name, body);
    (answer, started.elapsed())
}

#[test]
fn patterns_that_would_run_for_ages_are_stopped_with_an_error() {
    // Lua's own matcher runs each of these for seconds to hours without a Lua instruction in between, so the deadline hook never gets a turn
    let attacks = [
        ("nested_stars", r#"return string.rep("a", 10):match(string.rep("a*", 60) .. "b")"#),
        ("balanced", r#"return string.rep("(", 1000000):match("%b()")"#),
        ("optionals", r#"return string.rep("a", 40):match("^" .. string.rep("a?", 40) .. string.rep("a", 40) .. "b")"#),
        ("plain_needle", r#"return string.rep("a", 1000000):find(string.rep("a", 500000) .. "b", 1, true)"#),
        ("long_literal", r#"return string.rep("a", 1000000):match(string.rep("a", 500000) .. "b")"#),
        ("long_bracket", r#"return string.rep("a", 1000000):match("[" .. string.rep("x", 5000) .. "]b")"#),
        ("long_bracket_loop", r#"return string.rep("a", 1000000):match("[" .. string.rep("x", 5000) .. "]*b")"#),
        ("back_reference", r#"return string.rep("a", 100000):match("(a*)%1b")"#),
        ("empty_back_references", r#"return string.match(string.rep("a", 1000000), "(x*)" .. string.rep("%1", 100000) .. "b")"#),
        ("frontier", r#"return string.rep("a", 1000000):match("%f[" .. string.rep("x", 5000) .. "]b")"#),
        ("long_replacement", r#"return (string.rep("x", 1000000)):gsub("", string.rep("%0", 1000000))"#),
        ("find", r#"return string.find(string.rep("a", 3000), "a*a*a*b")"#),
        ("gsub", r#"return (string.rep("a", 3000)):gsub("a*a*a*b", "x")"#),
        ("gmatch", r#"for _ in string.gmatch(string.rep("a", 3000), "a*a*a*b") do end"#),
    ];
    for (name, body) in attacks {
        let (answer, took) = timed(name, body);
        let err = answer.unwrap_err();
        assert!(err.contains("too many steps") && err.contains("simpler pattern"), "{name}: {err}");
        eprintln!("{name}: {took:?}");
        assert!(took < std::time::Duration::from_secs(5), "{name} took {took:?}"); // about 0.3 s here, 1.4 s on a CI runner
    }
}

#[test]
fn a_pattern_that_was_stopped_leaves_the_plugin_working() {
    let folder = plugin("after", r#"return {
        slow = function() return string.rep("a", 10):match(string.rep("a*", 60) .. "b") end,
        fine = function() return ("key=value"):match("(%w+)=(%w+)") end }
    "#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(t.call("slow", vec![]).unwrap_err().contains("too many steps"));
    assert_eq!(t.call("fine", vec![]).unwrap(), json!("key"));
}

/// The first few kinds of subjects and shapes of pattern that make a matcher slow, for the stress test to mix.
fn stress_case(pick: &mut impl FnMut(usize) -> usize) -> (Vec<u8>, Vec<u8>, usize, String) {
    const SIZES: &[usize] = &[1_000, 20_000, 300_000, 1_000_000, 2_000_000];
    let n = SIZES[pick(SIZES.len())];
    let subject: Vec<u8> = match pick(6) {
        0 => vec![b'a'; n],
        1 => b"ab".iter().copied().cycle().take(n).collect(),
        2 => vec![b'('; n],
        3 => std::iter::repeat_n(b'a', n - 1).chain([b'b']).collect(),
        4 => (0..n).map(|_| b"ab (x\n"[pick(6)]).collect(),
        _ => b"word ".iter().copied().cycle().take(n).collect(),
    };
    const RUNS: &[usize] = &[1, 2, 10, 1_000, 100_000, 500_000];
    const SUFFIXES: &[&str] = &["", "", "*", "+", "-", "?"];
    let mut pattern = String::new();
    if pick(5) == 0 {
        pattern.push('^');
    }
    for _ in 0..1 + pick(6) {
        let suffix = SUFFIXES[pick(SUFFIXES.len())];
        match pick(14) {
            0 => pattern.push_str(&"a".repeat(RUNS[pick(RUNS.len())])),
            1 => pattern.push_str(&format!("a{suffix}")),
            2 => pattern.push_str(&format!(".{suffix}")),
            3 => pattern.push_str(&format!("[ab]{suffix}")),
            4 => pattern.push_str(&format!("[{}a]{suffix}", "xyz".repeat(1 + pick(4000)))),
            5 => pattern.push_str(&format!("[^{}]{suffix}", "xyz".repeat(1 + pick(4000)))),
            6 => pattern.push_str(&format!("(a{suffix})")),
            7 => pattern.push_str(&format!("(.{suffix})")),
            8 => pattern.push_str(["%1", "%2"][pick(2)]),
            9 => pattern.push_str(["%b()", "%bab", "%b()%b()"][pick(3)]),
            10 => pattern.push_str(["%f[%w]", "%f[%s]", "%f[^a]"][pick(3)]),
            11 => pattern.push_str(&format!("%w{suffix}")),
            12 => pattern.push_str(["b", "()", "$", "x", "%s"][pick(5)]),
            _ => pattern.push_str(&"(a)".repeat(1 + pick(10))),
        }
    }
    let repl = ["x", "%0%0", "%1", &"%0".repeat(1 + pick(1000)), "<%0>%1"][pick(5)].to_string();
    (subject, pattern.into_bytes(), pick(4), repl)
}

#[test]
fn no_pattern_shape_runs_for_long_whatever_the_text() {
    use mlua::{Lua, LuaOptions, MultiValue, StdLib, Value};
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut pick = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as usize % n
    };
    const RUNNER: &str = r#"
        local subject, pattern, mode, repl = ...
        if mode == 0 then return pcall(string.find, subject, pattern)
        elseif mode == 1 then return pcall(string.match, subject, pattern)
        elseif mode == 2 then return pcall(function() local n = 0 for _ in string.gmatch(subject, pattern) do n = n + 1 if n >= 3 then break end end return n end)
        else return pcall(string.gsub, subject, pattern, repl, 50) end
    "#;
    let (mut slowest, mut stopped) = (std::time::Duration::ZERO, 0);
    for round in 0..120 {
        let (subject, pattern, mode, repl) = stress_case(&mut pick);
        let shown = format!("round {round}: mode {mode}, {} byte subject starting {:?}, pattern {:?}", subject.len(), String::from_utf8_lossy(&subject[..subject.len().min(6)]),
                            String::from_utf8_lossy(&pattern[..pattern.len().min(60)]));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let lua = Lua::new_with(StdLib::STRING | StdLib::TABLE | StdLib::MATH, LuaOptions::default()).unwrap();
            lua.set_memory_limit(telescope_plugin_host::lua::MEMORY_LIMIT).unwrap();
            let string: mlua::Table = lua.globals().get("string").unwrap();
            telescope_plugin_host::lua_pattern::install(&lua, &string).unwrap();
            let args = MultiValue::from_vec(vec![Value::String(lua.create_string(&subject).unwrap()), Value::String(lua.create_string(&pattern).unwrap()),
                                                 Value::Integer(mode as i64), Value::String(lua.create_string(&repl).unwrap())]);
            let started = std::time::Instant::now();
            let answer: MultiValue = lua.load(RUNNER).call(args).unwrap();
            let ok = matches!(answer.front(), Some(Value::Boolean(true)));
            let _ = tx.send((started.elapsed(), ok));
        });
        let (took, ok) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|_| panic!("ran for over 5 s: {shown}"));
        if !ok {
            stopped += 1;
        }
        slowest = slowest.max(took);
    }
    eprintln!("120 stress calls, {stopped} stopped by an error, the slowest took {slowest:?}");
}

#[test]
fn a_gsub_that_would_build_a_huge_string_fails_before_it_does() {
    let (answer, took) = timed("huge", r#"return (string.rep("a", 1000000):gsub("^.*$", string.rep("%0", 1000000)))"#);
    let err = answer.unwrap_err();
    assert!(err.contains("memory"), "{err}"); // the host words an uncaught one its own way
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    // Also with captures, with a function and with a table that hand back big pieces
    let got = answer_soon("huge_more", r#"
        local big = string.rep("a", 1000000)
        local function failure(f) local ok, e = pcall(f) return ok == false and type(e) == "string" and e:find("not enough memory", 1, true) ~= nil end
        return {
            failure(function() return big:gsub("(.*)", string.rep("%1", 1000000)) end),
            failure(function() return big:gsub(".", function(c) return string.rep(c, 1000000) end) end),
            failure(function() return big:gsub("^.*$", { [big] = string.rep(big, 100) }) end),
            (("abc"):gsub("b", "%0%0")),
        }
    "#).unwrap();
    assert_eq!(got, json!([true, true, true, "abbc"]));
}

#[test]
fn nested_gsubs_share_one_output_limit() {
    let (answer, took) = timed("nested_gsub", r#"
        local big = string.rep("x", 16 * 1024 * 1024)
        local function f(c) if c == "a" then return big end return ("ab"):gsub(".", f) end
        local ok, e = pcall(function() return ("ab"):gsub(".", f) end)
        return { ok, type(e), type(e) == "string" and e:find("not enough memory", 1, true) ~= nil }
    "#);
    assert_eq!(answer.unwrap(), json!([false, "string", true]));
    assert!(took < std::time::Duration::from_secs(20), "took {took:?}");
    let got = answer_soon("nested_gsub_after", r#"
        local function f(c) if c == "a" then return "xx" end return ("ab"):gsub("a", "y") end
        return ("ab"):gsub(".", f)
    "#).unwrap();
    assert_eq!(got, json!("xxyb"));
}

#[test]
fn errors_from_the_string_functions_are_plain_strings_as_in_lua() {
    let got = answer_soon("plain_errors", r#"
        local function caught(f, ...) local ok, e = pcall(f, ...) return ok, e end
        local _, bad = caught(function() for _ in string.gmatch("x", "(") do end end)
        local _, malformed = caught(function() return ("x"):find("[a") end)
        local _, budget = caught(function() return string.rep("a", 3000):match("a*a*a*b") end)
        local _, argument = caught(function() return ("x"):rep({}) .. string.find() end)
        local _, thrown = caught(function() return ("abc"):gsub("b", function() error({ code = 7 }) end) end)
        local _, indexed = caught(function() return ("abc"):gsub("b", setmetatable({}, { __index = function() error("no index", 0) end })) end)
        local _, tail = caught(function() return string.find("x", "[a") end)
        return {
            type(bad), bad:match("unfinished capture") ~= nil, bad:match("^%S+:%d+: ") ~= nil,
            type(malformed), malformed:match("malformed pattern %(missing ']'%)$") ~= nil,
            type(budget), budget:match("too many steps") ~= nil,
            type(argument),
            type(thrown), thrown.code, indexed,
            tail:match("^%S+:%d+: malformed") ~= nil,
        }
    "#).unwrap();
    assert_eq!(got, json!(["string", true, true, "string", true, "string", true, "string", "table", 7, "no index", true]));
}

// ── The pattern matcher against Lua's own ───────────────────────────────────

const SUBJECTS: &[&str] = &[
    "", "a", "hello world", "  padded  ", "THE (quick) [brown] {fox}", "key = value", "2024-05-06", "aaa", "abc123def456", "f(a(b)c)d",
    "x\0y\0z", "one,two,,three", "%d+ literal", "tab\tand\nnewline", "\u{ff}\u{fe} high \u{80} bytes", "THE END the end", "abababababab", "((()))",
    "a.b*c-d", "[x]", "snake_case_name = 10", "<b>bold</b> and <i>it</i>",
];

const PATTERNS: &[&str] = &[
    "", "a", "^", "$", "^a", "a$", "^$", ".", ".-", ".*", ".+", "a*", "a+", "a-", "a?", "a*b", "a-b", "a?b?c", "ab*", "(ab)*", "(ab)+", "[ab]*c", "a.-b",
    "a.*b", "^(.-)b", "a-$", ".-$", "^.-$", "x*", "%a+", "%A+", "%d+", "%D+", "%s+", "%S+", "%w+", "%W+", "%l+", "%u+", "%p+", "%c", "%x+", "%g+", "%z",
    "%a%d", "%w+%s", "%s*", "%s-", "[abc]", "[^abc]", "[a-c]+", "[%a_][%w_]*", "[%d%s]+", "[a-]", "[]]", "[^]]", "[]a]", "[%]]", "[%a-z]", "[z-a]",
    "[+-]", "[%w%-]+", "[^%s]+", "[a-cx-z]", "[%d-9]", "[\u{80}-\u{ff}]+", "[\0]", "%f[%w]%w+", "%f[%W]", "%f[%a]", "%f[^%s]", "%f[%z]", "%f[%l]%l*",
    "%f[%u]%u+", "%b()", "%b[]", "%b{}", "%bxy", "%b<>", "(%b())", "%b()%b[]", "(a)(b)", "(a*)(b)", "()", "()a()", "(a)()", "((a))", "(((a)))",
    "(.)%1", "(.)(.)%2%1", "(a+)b%1", "(ab)%1", "(%w)%1", "(a)%1%1", "%1", "%2", "%0", "(a", "a)", "(a)(", "[a", "%", "a%", "%f", "%fa", "%b", "%b(", "%ba",
    "(.-)%s*=%s*(.*)", "^(%w+)", "(%d+)-(%d+)-(%d+)", "^%s*(.-)%s*$", "([^,]*)", "[^,]+", ",", "%.", "%%", "%(", "%[", "\0", "(%w+)=(%w+)", "(a?)(b?)",
    "(%a+)(%d*)", "<(%w+)>(.-)</%1>", "(%w+)_(%w+)", "%d+%.?%d*", "^%d", "%$", "a^", "^^", "$$", "(^a)", "(a$)", "()%d+()", "(()a)", "(.)(.)(.)(.)",
    "^(a-)(b?)$", "(h)(e)(l)(l)(o)", "[%a%d]+", "%s*(%w+)%s*", "(.*)(.*)", "(.-)(.-)", ".?.?.?", "-", "+", "*", "?", "a**", "a+?", "[a-z]-%d", "%w-%d",
    "(%d)(%d)(%d)(%d)(%d)(%d)(%d)(%d)(%d)(%d)", "[%%]", "[%a%A]+", "[^%a%A]", "[a-a]", "[%z]", "\u{ff}", "%\0",
];

/// Calls every string function on every subject and pattern in a state, as one text per call.
const RUNNER: &str = r##"
local subjects, patterns, extra = ...
local out = {}
local function ser(...)
  local t = table.pack(...)
  local o = {}
  for i = 1, t.n do o[i] = type(t[i]) .. ":" .. tostring(t[i]) end
  return table.concat(o, "|")
end
local function rec(...) out[#out + 1] = ser(pcall(...)) end
local table_repl = setmetatable({ a = "A", b = false, [""] = "E", hello = "H", ["1"] = "ONE", [1] = "int" }, { __index = function(_, k) return #k > 3 and "<" .. k .. ">" or nil end })
local function fn_repl(a, b, c)
  if a == "a" then return false elseif a == "b" then return nil elseif a == "c" then return 42 elseif a == "d" then return {} end
  return "<" .. tostring(a) .. "," .. tostring(b) .. "," .. tostring(c) .. ">"
end
local function loop(s, p, init)
  local r = {}
  for a, b in string.gmatch(s, p, init) do
    r[#r + 1] = tostring(a) .. "," .. tostring(b)
    if #r > 60 then break end
  end
  return table.concat(r, ";")
end
local function method(s, p) return s:find(p), s:match(p), s:gsub(p, "%0%0") end
for _, s in ipairs(subjects) do
  for _, p in ipairs(patterns) do
    rec(string.find, s, p)
    rec(string.find, s, p, 2)
    rec(string.find, s, p, -3)
    rec(string.find, s, p, 0)
    rec(string.find, s, p, 100)
    rec(string.find, s, p, -100)
    rec(string.find, s, p, 1, true)
    rec(string.find, s, p, 3, 1)
    rec(string.match, s, p)
    rec(string.match, s, p, -4)
    rec(string.match, s, p, 2)
    rec(loop, s, p)
    rec(loop, s, p, 3)
    rec(loop, s, p, -2)
    rec(loop, s, p, 50)
    rec(string.gsub, s, p, "<%0>")
    rec(string.gsub, s, p, "%1")
    rec(string.gsub, s, p, "%1|%2")
    rec(string.gsub, s, p, "%%x%")
    rec(string.gsub, s, p, "")
    rec(string.gsub, s, p, 5)
    rec(string.gsub, s, p, "x", 2)
    rec(string.gsub, s, p, "x", 0)
    rec(string.gsub, s, p, "x", -1)
    rec(string.gsub, s, p, table_repl)
    rec(string.gsub, s, p, fn_repl)
    rec(string.gsub, s, p, function(...) return select("#", ...) end)
    rec(method, s, p)
  end
end
for i, code in ipairs(extra) do
  local f, err = load(code, "=extra" .. i)
  rec(f or function() error(err, 0) end)
end
return out
"##;

const EXTRA: &[&str] = &[
    "return string.find()",
    "return string.find('a')",
    "return string.find(nil, 'a')",
    "return string.find('a', {})",
    "return string.find('a', 'a', 'x')",
    "return string.find('a', 'a', 1.5)",
    "return string.find('abc', 'b', '2')",
    "return string.find('abc', 'b', 2.0)",
    "return string.find(12345, 34)",
    "return string.find(1.5, '%.')",
    "return string.match(true, 'a')",
    "return ('abc'):find({})",
    "return ('abc'):gsub('b')",
    "return ('abc'):gsub('b', nil)",
    "return ('abc'):gsub('b', true)",
    "return ('abc'):gsub('b', 'x', 'y')",
    "return ('abc'):gsub('b', 'x', 1.5)",
    "return ('abc'):gsub('b', {}, 1)",
    "return ('abc'):gsub('(b)', '%2')",
    "return ('abc'):gsub('b', '%')",
    "return ('abc'):gsub('b', '%a')",
    "return ('abc'):gsub('()b', '%1')",
    "return ('abc'):gsub('b', function() return {} end)",
    "return ('abc'):gsub('b', function() return true end)",
    "return ('abc'):gsub('b', function() return 1.5 end)",
    "return ('abc'):gsub('b', function() return 7 end)",
    "return ('abc'):gsub('b', function() error('boom', 0) end)",
    "return string.gsub(123, 2, 9)",
    "return string.gsub('abc', 'b', 'x', math.maxinteger)",
    "return string.gsub('abc', 'b', 'x', math.mininteger)",
    "return string.gsub('abc', '', '-')",
    "return string.gsub('', '', '-')",
    "return string.gsub('abc', '^', '>')",
    "return string.gsub('abc', '$', '<')",
    "return string.gsub('hello world', '%w+', '%0 %0', 1)",
    "return string.gsub('abc def', '(%w+) (%w+)', '%2 %1')",
    "return string.gsub('abc', '.', {a = 1, b = true})",
    "return string.gsub('abc', '.', setmetatable({}, {__index = function(_, k) return k:upper() end}))",
    "return string.gsub('abc', '()', '%1')",
    "return string.gmatch('abc')",
    "return string.gmatch()",
    "local it = string.gmatch('abc', '.') it() it() it() return it(), it()",
    "local it = string.gmatch('a1b2', '%a(%d)') return it(), it(), it()",
    "local r = {} for a in string.gmatch('abc', '') do r[#r+1] = a end return #r",
    "local r = {} for w in ('one two  three'):gmatch('%S+') do r[#r+1] = w end return table.concat(r, ',')",
    "local r = {} for k, v in ('a=1, b=2'):gmatch('(%w+)=(%w+)') do r[#r+1] = k .. v end return table.concat(r, ',')",
    "local r = {} for p in ('abc'):gmatch('()') do r[#r+1] = p end return table.concat(r, ',')",
    "return string.find(string.rep('(', 33) .. string.rep(')', 33), string.rep('(', 33) .. string.rep(')', 33))",
    "return string.find('a', string.rep('()', 32) .. 'a')",
    "return string.find('a', string.rep('()', 33) .. 'a')",
    "return string.find(string.rep('a', 300), string.rep('a?', 250))",
    "return string.find(string.rep('a', 300), string.rep('a?', 150))",
    "return string.find(string.rep('a', 300), string.rep('a', 250))",
    "return string.find(string.rep('ab', 50), string.rep('(a)(b)', 20))",
    "return string.find('abc', 'b', math.maxinteger)",
    "return string.find('abc', 'b', math.mininteger)",
    "return string.find('abc', '', 4)",
    "return string.find('abc', '', 5)",
    "return string.find('abc', '', -1)",
    "return string.find('abc', '', 3, true)",
    "return string.find('a\\0b', '\\0')",
    "return string.find('a\\0b', 'b', 1, true)",
    "return string.find('a.b', '.', 1, true)",
    "return string.find('a.b', '.', 2, true)",
    "return string.find('a.b', 'b')",
    "return string.find('aXb', 'X', -2)",
    "return string.find('abc', 'abcd', 1, true)",
    "return string.find('abc', 'c', 1, true)",
    "return string.find('abcabc', 'ca', 1, true)",
    "return string.find('abcabc', 'cab', 1, true)",
    "return string.find(string.rep('a', 100) .. 'b', string.rep('a', 50) .. 'b', 1, true)",
    "return string.match('  x  ', '^%s*(.-)%s*$')",
    "return string.match('key = a b c', '^(%w+)%s*=%s*(.-)%s*$')",
    "return string.match('2024-05-06T07:08', '(%d+)-(%d+)-(%d+)T(%d+):(%d+)')",
    "return string.match('THE (quick) fox', '%((%a+)%)')",
    "return string.match('f(a(b)c)d', '%b()')",
    "return string.match('abc', '()b()')",
    "return string.match('hello', '.', -1)",
    "return string.match('hello', '.', -10)",
    "return string.match('x', '(x)(y?)')",
    "return string.format('%q', string.match('\\255\\254', '[\\128-\\255]+'))",
    "return string.match(('x'):rep(5000), '^(x*)$') == ('x'):rep(5000)",
    "return #string.gsub(('word '):rep(20000), '%s+', ', ')",
    "return select('#', string.find('abc', '(a)(b)(c)'))",
    "return string.gsub('abc', '%w', '%%')",
    "return string.gsub('abc', 'b', '%0%0%0')",
    "return string.gsub('a b', '(%w)', '%1%1')",
    "return string.gsub('abc', '(a)(b)(c)', '%3%2%1')",
    "return string.gsub('abc', 'x*', '-')",
    "return string.gsub('abc', 'b*', '-')",
    "return string.gsub('a,b,,c', ',', ';')",
    "return string.gsub('\\195\\164\\195\\182', '[\\128-\\255][\\128-\\255]', '?')",
    "return string.gsub('abc', '(', 'x')",
];

/// Runs the runner on these in Lua's own string library and in the port, and fails with the first results that differ. Gives how many results it compared.
fn compare_with_lua(subjects: &[String], patterns: &[String], extra: &[&str]) -> usize {
    use mlua::{Lua, LuaOptions, MultiValue, StdLib};
    let run = |ported: bool| -> Vec<String> {
        let lua = Lua::new_with(StdLib::STRING | StdLib::TABLE | StdLib::MATH, LuaOptions::default()).unwrap();
        if ported {
            telescope_plugin_host::lua_pattern::install(&lua, &lua.globals().get("string").unwrap()).unwrap();
        }
        let strings = |list: &[String]| lua.create_sequence_from(list.iter().map(|s| lua.create_string(s).unwrap())).unwrap();
        let runner: mlua::Function = lua.load(RUNNER).set_name("=runner").into_function().unwrap();
        let out: mlua::Table = runner.call(MultiValue::from_vec(vec![
            mlua::Value::Table(strings(subjects)),
            mlua::Value::Table(strings(patterns)),
            mlua::Value::Table(lua.create_sequence_from(extra.iter().map(|s| lua.create_string(s).unwrap())).unwrap()),
        ])).unwrap();
        out.sequence_values::<mlua::LuaString>().map(|v| v.unwrap().to_string_lossy().to_string()).collect()
    };
    let (native, ported) = (run(false), run(true));
    assert_eq!(native.len(), ported.len());
    let pairs = subjects.len() * patterns.len();
    let per_pair = (native.len() - extra.len()) / pairs;
    let mut different = Vec::new();
    for (i, (a, b)) in native.iter().zip(&ported).enumerate() {
        if a != b {
            let at = if i < pairs * per_pair {
                let pair = i / per_pair;
                format!("subject {:?} pattern {:?} variant {}", subjects[pair / patterns.len()], patterns[pair % patterns.len()], i % per_pair)
            } else {
                format!("extra {:?}", extra[i - pairs * per_pair])
            };
            different.push(format!("{at}\n   lua:  {a}\n   port: {b}"));
        }
    }
    assert!(different.is_empty(), "{} of {} results differ from Lua's:\n{}", different.len(), native.len(), different.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
    native.len()
}

#[test]
fn the_matcher_gives_the_results_of_luas_own() {
    let subjects: Vec<String> = SUBJECTS.iter().map(|s| s.to_string()).collect();
    let patterns: Vec<String> = PATTERNS.iter().map(|s| s.to_string()).collect();
    let compared = compare_with_lua(&subjects, &patterns, EXTRA);
    eprintln!("{} patterns, {} subjects, {} extra scripts, {compared} results compared", PATTERNS.len(), SUBJECTS.len(), EXTRA.len());
}

#[test]
fn the_matcher_agrees_with_luas_on_made_up_patterns() {
    // A fixed pseudo-random walk, so a failure can be reproduced
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut pick = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as usize % n
    };
    const PIECES: &[&str] = &["a", "b", "c", ".", "%a", "%d", "%s", "%w", "%A", "%.", "%%", "[ab]", "[^a]", "[a-c]", "[%d%s]", "(", ")", "()", "%1", "%2", "%b()", "%bab",
                              "%f[%w]", "%f[%s]", "%f[^%w]", "^", "$", "-", "x", "1", " ", "[", "]", "%", "(.)", "(a)", "(%w+)"];
    const SUFFIXES: &[&str] = &["", "", "", "*", "+", "-", "?"];
    const LETTERS: &[u8] = b"aabbc1 2()  .%";
    let patterns: Vec<String> = (0..700)
        .map(|_| (0..1 + pick(7)).map(|_| format!("{}{}", PIECES[pick(PIECES.len())], SUFFIXES[pick(SUFFIXES.len())])).collect::<String>())
        .collect();
    let subjects: Vec<String> = (0..12).map(|_| (0..pick(12)).map(|_| LETTERS[pick(LETTERS.len())] as char).collect()).collect();
    let compared = compare_with_lua(&subjects, &patterns, &[]);
    eprintln!("{} made-up patterns on {} subjects, {compared} results compared", patterns.len(), subjects.len());
}

#[test]
fn everyday_patterns_still_work_on_long_texts() {
    let got = answer_soon("everyday", r#"
        local big = string.rep("word  ", 100000)
        local k, v = ("  name = Telescope  "):match("^%s*(.-)%s*=%s*(.-)%s*$")
        local words = 0
        for _ in string.gmatch(string.rep("a b ", 500), "%a+") do words = words + 1 end
        local squeezed, n = big:gsub("%s+", " ")
        local plain = string.find(big, "d  w", 1, true)
        local year = ("2024-05-06"):match("^(%d+)-(%d+)-(%d+)$")
        local malformed = pcall(string.find, "x", "[a")
        local never = string.find(string.rep("a", 100000), "a*a*b", 1, true) == nil
        local long_pattern = string.find("x", string.rep("a", 200)) == nil
        local line = (string.rep("x", 200000) .. "\n"):match("^(.-)\n$")
        return { k, v, words, #squeezed, n, plain, year, malformed, never, long_pattern, #line }
    "#).unwrap();
    assert_eq!(got, json!(["name", "Telescope", 1000, 500000, 100000, 4, "2024", false, true, true, 200000]));
}

#[test]
fn a_get_config_that_fails_is_not_an_empty_section() {
    let folder = plugin("shaky", "local broken = false\n\
        return { get_config = function() if broken then error('no') end return { zoom = 2 } end,\n\
                 set_config = function(c) broken = c.fail end }\n", json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.config(), json!({"zoom": 2}));
    t.set_config(json!({"fail": true})).unwrap();
    assert!(!t.config().is_object(), "a hook that threw gives nothing, so the host keeps what it saved");
    let folder = plugin("plain", "return {}\n", json!({}));
    assert_eq!(PluginTest::open(&folder).unwrap().config(), json!({}), "a plugin with no settings has an empty section");
}

#[test]
fn a_plugin_that_takes_too_much_memory_is_stopped() {
    let folder = plugin("hungry", "return { setup = function() local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 4096) .. i end end }\n", json!({}));
    let err = PluginTest::open(&folder).unwrap().call("setup", vec![]).unwrap_err();
    assert!(err.contains("memory"), "{err}");
}

#[test]
fn host_methods_need_their_permission_and_a_host() {
    let folder = plugin("asks", r#"return {
        setup = function() return host.is_streaming() end,
        quit = function() host.quit_app() end,
        card = function() host.is_streaming() end,
        later = function() host.set_ui_props("card", { on = true }) end,
    }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.call("setup", vec![]).unwrap(), json!(false));
    assert!(t.call("quit", vec![]).unwrap_err().contains("\"app\""));
    assert!(t.call_without_host("card", vec![]).unwrap_err().contains("runs without the host"));
    t.call_without_host("later", vec![]).unwrap();
    assert!(t.host.calls.is_empty(), "waits for a hook with the host");
    t.call("setup", vec![]).unwrap();
    assert_eq!(t.take_calls(), ["set_ui_props card {\"on\":true}"]);
}

#[test]
fn a_set_config_that_raises_rejects_the_settings_and_the_rest_still_load() {
    let folder = plugin("picky", r#"
        local cfg = { size = 3, on = false }
        return {
          get_config = function() return cfg end,
          set_config = function(c)
            if c.size ~= nil and math.type(c.size) ~= "integer" then error("size has to be a whole number") end
            cfg = c
          end,
        }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(t.set_config(json!({"size": "big"})).unwrap_err().contains("whole number"));
    load_config_tolerantly(&mut t.plugin, &json!({"size": 3, "on": false}), &json!({"size": "big", "on": true}));
    assert_eq!(t.config(), json!({"size": 3, "on": true}));
}

#[test]
fn a_plugin_emits_its_own_events_and_hears_them() {
    let folder = plugin("talker", r#"
        local heard = {}
        return {
          events = { ["talker.ping"] = function(e) heard[#heard + 1] = e.data.n end, stream_stopped = function() host.emit("talker.ping", { n = 2 }) end },
          heard = function() return heard end,
        }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    t.event(Event::StreamStopped);
    let sent = t.take_events();
    assert_eq!(sent, [Event::Custom { name: "talker.ping".into(), data: json!({"n": 2}) }]);
    t.event(sent[0].clone());
    assert_eq!(t.call("heard", vec![]).unwrap(), json!([2]));
}

#[test]
fn a_metamethod_on_the_plugin_table_cant_hang_hook_lookup() {
    let folder = plugin("sly", r#"return setmetatable({ real = function() return 1 end }, { __index = function() while true do end end })"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.call("real", vec![]).unwrap(), json!(1));
    assert!(t.call("missing", vec![]).is_err());
}

#[test]
fn a_plugin_that_inherits_its_hooks_and_handlers_from_a_base_module_still_has_them() {
    let folder = plugin("heir", r#"
        local base = require("base")
        return setmetatable({ own = function() return "own" end }, { __index = base })"#, json!({}));
    std::fs::write(folder.join("base.lua"), r#"return {
        inherited = function() return "inherited" end,
        events = { stream_stopped = function() host.emit("heir.stopped", {}) end },
    }"#).unwrap();
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(t.defines("inherited"));
    assert_eq!(t.call("inherited", vec![]).unwrap(), json!("inherited"));
    t.event(Event::StreamStopped);
    assert_eq!(t.take_events(), [Event::Custom { name: "heir.stopped".into(), data: json!({}) }]);
}

#[test]
fn a_looping_inherited_lookup_is_stopped_for_hooks_and_events_alike() {
    let folder = plugin("loopy", r#"return setmetatable({}, { __index = function() while true do end end })"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(!t.defines("anything"));
    t.event(Event::StreamStopped);
}

#[test]
fn a_table_that_shares_itself_into_billions_of_nodes_is_refused() {
    let folder = plugin("blowup", r#"return {
        bomb = function() local t = "x"; for i = 1, 30 do t = { t, t } end; return json.encode(t) end,
        small = function() return json.encode({ { 1, 2 }, { 1, 2 } }) end,
    }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(t.call("bomb", vec![]).unwrap_err().contains("too big"));
    assert_eq!(t.call("small", vec![]).unwrap(), json!("[[1,2],[1,2]]"));
}

// ── The examples ────────────────────────────────────────────────────────────

#[test]
fn black_and_white_has_a_card_a_setting_and_a_shader() {
    let mut t = PluginTest::open(example("black_and_white")).unwrap();
    t.setup();
    let card = t.card().unwrap();
    assert_eq!((card.region.as_str(), card.ui.component.as_str()), ("right", "BlackAndWhiteCard"));
    assert_eq!(card.ui.props, json!({"on": false, "strength": 100}));
    assert!(matches!(&card.ui.source, SlintSource::File(path) if path.ends_with("card.slint")));
    assert!(t.shader().is_none(), "off: frames pass untouched");

    t.ui("card", "toggled", vec![json!(true)]);
    assert_eq!(t.take_calls(), ["refresh_frame_step", "schedule_save"]);
    let step = t.shader().unwrap();
    assert_eq!(step.uniforms, json!({"strength": 1.0}));
    validate_shader(&step, "shaders/grey.wgsl").unwrap();
    t.ui("card", "strength-moved", vec![json!(42.4)]);
    assert_eq!(t.take_calls(), ["refresh_frame_step", "schedule_save"]);
    assert_eq!(t.shader().unwrap().uniforms, json!({"strength": 0.42}));
    assert_eq!(t.config(), json!({"on": true, "strength": 42}));

    t.set_config(json!({"on": false, "strength": "lots"})).unwrap();
    assert_eq!(t.config(), json!({"on": false, "strength": 100}), "a value from an older version or a hand edit falls back");
    t.event(Event::StreamStopped);
    let calls = t.take_calls();
    assert_eq!(calls.len(), 2, "done once a hook has the host: {calls:?}");
    let props: Value = serde_json::from_str(calls[0].strip_prefix("set_ui_props card ").unwrap()).unwrap();
    assert_eq!((props, calls[1].as_str()), (json!({"on": false, "strength": 100}), "refresh_frame_step"));
}

#[test]
fn stream_alerts_follows_events_and_calls_the_host() {
    let mut t = PluginTest::open(example("stream_alerts")).unwrap();
    t.start_stream("phone");
    assert!(t.take_calls().is_empty());
    t.event(Event::StreamLost { source_id: "phone".into() });
    t.event(Event::StreamLost { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream dropped"], "once per drop");
    t.event(Event::StreamConnected { source_id: "phone".into() });
    t.event(Event::StreamConnected { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream is back"]);
    assert_eq!(t.plugin.diagnostics(), [("Drops this run".to_string(), "1".to_string())]);
    let actions = t.plugin.actions();
    assert_eq!(actions[0].id, "stream_alerts.test");
    assert_eq!(t.plugin.run_action(&mut t.host, "stream_alerts.test", &json!({})).as_deref(), Some("Stream alerts: test sent"));
    t.set_config(json!({"when_back": false})).unwrap();
    t.event(Event::StreamLost { source_id: "phone".into() });
    t.event(Event::StreamConnected { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream alerts", "send_notification Stream dropped"]);
}

#[test]
fn stream_alerts_needs_its_permission() {
    let folder = scratch_dir("lua").join("stream_alerts");
    std::fs::create_dir(&folder).unwrap();
    std::fs::copy(example("stream_alerts").join("main.lua"), folder.join("main.lua")).unwrap();
    std::fs::write(folder.join("plugin.json"), json!({"name": "stream_alerts", "api": 2}).to_string()).unwrap();
    let mut t = PluginTest::open(&folder).unwrap();
    let lost = serde_json::to_value(Event::StreamLost { source_id: "phone".into() }).unwrap();
    assert!(t.call("on_event", vec![lost]).unwrap_err().contains("\"notifications\""));
}

// ── check-plugin (test_check_plugin.py) ─────────────────────────────────────

/// check-plugin run as an author runs it: its exit code and everything it printed.
fn checker(folder: &Path) -> (Option<i32>, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_check-plugin")).arg(folder).output().unwrap();
    (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn the_example_plugins_pass() {
    for name in ["black_and_white", "stream_alerts"] {
        let (code, out) = checker(&example(name));
        assert_eq!(code, Some(0), "{out}");
        assert!(!out.contains("[FAIL]") && out.ends_with("All good.\n"), "{out}");
    }
}

#[test]
fn a_broken_plugin_gets_each_problem_with_where_it_is() {
    let folder = plugin("bad_one", "local plugin = {}\n\
        function plugin.frame_step() return { shader = 'fx.wgsl' } end\n\
        function plugin.card() return { file = 'card.slint', component = 'Card' } end\n\
        function plugin.get_config() return { when = function() end } end\n\
        function plugin.on_stream_start(stream)\n  error('no lens')\nend\n\
        return plugin\n", json!({}));
    std::fs::write(folder.join("fx.wgsl"), "fn effect(uv: vec2<f32>) -> vec4<f32> {\n    return sample(uv) * nope;\n}\n").unwrap();
    let (code, text) = checker(&folder);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("4 problems to fix"), "{text}");
    assert!(text.contains("fx.wgsl:2"), "the shader's error points into its file: {text}");
    assert!(text.contains("main.lua:6: no lens"), "{text}");
    assert!(text.contains("the plugin has no card.slint"), "{text}");
    assert!(text.contains("JSON"), "settings that can't be saved say why: {text}");
    // Only the plugin's own places, none of the checker's
    assert!(![".rs:", "check-plugin", "stack traceback"].iter().any(|ours| text.contains(ours)), "{text}");
}

#[test]
fn a_plugin_that_wont_load_says_why() {
    let folder = plugin("broken", "require('not_a_module_anywhere')\nreturn {}\n", json!({}));
    let (code, out) = checker(&folder);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("1 problem to fix") && out.contains("not_a_module_anywhere"), "{out}");
}

#[test]
fn repo_is_checked() {
    let cases = [
        (json!({"repo": "github.com/me/extra"}), "[FAIL] plugin.json\n       \"repo\" has to be an https:// address"),
        (json!({"repo": "https://github.com/me/extra", "version": "1.0"}), "Telescope can tell people when a new release"),
        (json!({"repo": "https://github.com/me/extra"}), "Give it a \"version\""),
        (json!({"repo": "https://codeberg.org/me/extra"}), "only follows https://github.com/owner/repo"),
        (json!({}), ""),
    ];
    for (extra, expect) in cases {
        let report = check(&plugin("extra", "return {}\n", extra.clone()));
        let text = report.text();
        let about_repo = report.notes.len() + report.steps.iter().filter(|(w, r)| w == "plugin.json" && r.is_err()).count();
        assert!(text.contains(expect) && (about_repo > 0) == !expect.is_empty(), "{extra}: {text}");
    }
}

// ── The plugin API, frozen (test_plugin_api.py) ─────────────────────────────

/// Every hook GuestPlugin really calls when Telescope uses it as a Plugin: (hook, how many arguments, whether the host is there).
fn hooks_called() -> Vec<(String, usize, bool)> {
    struct Recorder(Arc<Mutex<Vec<(String, usize, bool)>>>);

    impl Guest for Recorder {
        fn call(&mut self, calls: &mut HostCalls<'_>, hook: &str, args: Vec<Value>) -> Result<Value, String> {
            self.0.lock().unwrap().push((hook.to_string(), args.len(), calls.has_host()));
            Ok(Value::Null)
        }
        fn defines(&self, _hook: &str) -> bool {
            true
        }
        fn listens(&self, _event: &str) -> bool {
            true
        }
    }

    let called: Arc<Mutex<Vec<(String, usize, bool)>>> = Arc::default();
    let info = PluginInfo { name: "recorder".into(), title: "Recorder".into(), ..Default::default() };
    let mut p = GuestPlugin::new(PluginContext::new(info, ".", Vec::new()), Recorder(called.clone()));
    let (mut host, h) = (FakeHost::new(), &json!({}));
    let stream = StreamInfo { source_id: "s".into(), url: "u".into(), session: 1 };
    p.setup(&mut host);
    p.on_plugins_ready(&mut host);
    p.shutdown(&mut host);
    p.on_event(&mut host, &Event::StreamStopped);
    p.card();
    p.header_widget();
    p.settings_sections();
    p.tray_actions();
    p.on_tray_action(&mut host, "t");
    p.actions();
    p.run_action(&mut host, "a", h);
    p.release_action(&mut host, "a", h);
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "c".into(), args: Vec::new() });
    p.on_issue_action(&mut host, &added_issue_key("recorder", "k"), "i");
    p.on_stream_starting(&mut host);
    p.on_stream_start(&mut host, &stream);
    p.on_stream_stop(&mut host);
    p.on_focus_gained(&mut host, &stream);
    p.on_focus_lost(&mut host);
    p.on_camera_off(&mut host);
    p.on_camera_on(&mut host);
    p.on_phone_state(&mut host, h);
    p.frame_step();
    p.get_config();
    p.set_config(h).unwrap();
    p.apply_preset(&mut host, h).unwrap();
    p.diagnostics();
    let mut called = called.lock().unwrap().clone();
    called.sort();
    called
}

/// Everything a plugin's Lua code finds in its globals and their tables, but host, which HOST_API lists: asked of a real sandbox.
fn lua_globals() -> Vec<String> {
    let folder = plugin("globals", r#"
        local plugin = {}
        function plugin.globals()
          local found = {}
          for name, value in pairs(_G) do
            if name == "_G" or name == "host" then
              found[#found + 1] = name
            elseif type(value) == "table" then
              for member in pairs(value) do found[#found + 1] = name .. "." .. member end
            else
              found[#found + 1] = name
            end
          end
          return found
        end
        return plugin
    "#, json!({}));
    let found = PluginTest::open(&folder).unwrap().call("globals", vec![]).unwrap();
    found.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

/// What a JSON value holds, as "field: kind" lines under name: the fields a plugin reads and hands back.
fn shape(name: &str, value: &Value, lines: &mut Vec<String>) {
    let kind = match value {
        Value::Object(fields) => {
            for (field, v) in fields {
                shape(&format!("{name}.{field}"), v, lines);
            }
            return;
        }
        Value::Array(items) => match items.first() {
            Some(first) => return shape(&format!("{name}[]"), first, lines),
            None => "list",
        },
        Value::String(_) => "text",
        Value::Number(_) => "number",
        Value::Bool(_) => "true or false",
        Value::Null => "nothing",
    };
    lines.push(format!("payload {name}: {kind}"));
}

fn current_api() -> String {
    let mut lines = vec![format!("PLUGIN_API = {PLUGIN_API}"), format!("OLDEST_PLUGIN_API = {OLDEST_PLUGIN_API}")];
    for m in HOST_API {
        let permission = m.permission.map(|p| format!(" [{p}]")).unwrap_or_default();
        let later = if m.deferrable { " [waits for the host]" } else { "" };
        lines.push(format!("host.{}({}){permission}{later}", m.name, m.params.join(", ")));
    }
    for (hook, args, with_host) in HOOKS {
        // Lua plugins don't define on_event: each event goes to the function of its name in plugin.events
        let name = if *hook == "on_event" { "events.<event>" } else { hook };
        lines.push(format!("plugin.{name}({args}){}", if *with_host { "" } else { " [no host]" }));
    }
    lines.extend(lua_globals().iter().filter(|g| *g != "_G" && *g != "host").map(|g| format!("global {g}")));
    // An event's fields and types, as plain text whatever spacing this compiler's stringify! gives
    for (event, fields) in Event::FIELDS {
        let fields: Vec<String> = fields.iter().map(|(name, ty)| format!("{name}: {}", ty.replace(' ', ""))).collect();
        lines.push(format!("event {event}({})", fields.join(", ")));
    }
    // What's inside those fields and the hooks' arguments, the way a plugin gets it
    let point = [0.5f32, 0.5];
    let pick = Pick { kind: PickKind::Point, shown: vec![point], full: vec![point], in_picture: vec![point], picture: None };
    let view = View { rotation: Some(90), ..View::default() };
    let samples = [
        ("LensBox", to_value(LensBox { label: "x".into(), x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 })),
        ("SourceName", to_value(SourceName { id: "x".into(), name: "x".into() })),
        ("RememberedSource", to_value(RememberedSource { id: "x".into(), name: "x".into(), detail: "x".into() })),
        ("View", to_value(view)),
        ("Pick", to_value(pick)),
        ("StreamInfo", to_value(StreamInfo { source_id: "x".into(), url: "x".into(), session: 1 })),
        ("UiEvent", to_value(UiEvent { ui: "x".into(), callback: "x".into(), args: vec![json!(1)] })),
    ];
    for (name, sample) in samples {
        shape(name, &sample.unwrap(), &mut lines);
    }
    lines.extend(telescope_plugin_host::manifest::PERMISSIONS.iter().map(|(p, _)| format!("permission {p}")));
    lines.push(format!("shader entry {}", telescope_core::shader::ENTRY));
    lines.push(format!("shader params {}", telescope_core::shader::PARAMS));
    lines.sort();
    lines.join("\n") + "\n"
}

// HOOKS and HOST_API are tables, so what they say is held to what the code does: the hooks Telescope calls and the arguments call_host reads
#[test]
fn the_api_tables_are_what_the_code_does() {
    let mut table: Vec<(String, usize, bool)> = HOOKS.iter()
        .map(|(hook, args, host)| (hook.to_string(), if args.is_empty() { 0 } else { args.split(", ").count() }, *host)).collect();
    table.sort();
    let mut called = hooks_called();
    called.dedup();
    assert_eq!(table, called, "HOOKS against what GuestPlugin calls");

    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/guest.rs")).unwrap();
    let body = &source[source.find("pub fn call_host").unwrap()..];
    let body = &body[..body.find("other => return Err").unwrap()];
    let mut arms: Vec<(String, String)> = Vec::new();
    for line in body.lines().map(str::trim) {
        match line.strip_prefix('"').and_then(|l| l.split_once("\" => ")) {
            Some((method, rest)) => arms.push((method.to_string(), rest.to_string())),
            None => if let Some(arm) = arms.last_mut() { arm.1.push_str(line) },
        }
    }
    assert_eq!(arms.len(), HOST_API.len());
    for (method, code) in arms {
        let mut read: Vec<String> = Vec::new();
        for pattern in ["(args, \"", "args.get(\"", "s(\""] {
            read.extend(code.match_indices(pattern).map(|(at, _)| code[at + pattern.len()..].split('"').next().unwrap().to_string()));
        }
        if code.contains("src)") || code.contains("src,") {
            read.push("source_id".into());
        }
        read.sort();
        let entry = HOST_API.iter().find(|m| m.name == method).unwrap_or_else(|| panic!("{method} isn't in HOST_API"));
        let mut params: Vec<String> = entry.params.iter().map(|p| p.trim_end_matches('?').to_string()).collect();
        params.sort();
        assert_eq!(read, params, "host.{method}'s arguments");
    }
    let found = lua_globals();
    for global in GLOBALS {
        assert!(*global == "host" || found.iter().any(|g| g == global), "{global} is gone from the sandbox");
    }
}

#[test]
fn the_plugin_api_only_changes_on_purpose() {
    let frozen = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugin_api.txt");
    let now = current_api();
    if std::env::var_os("UPDATE_PLUGIN_API").is_some() {
        std::fs::write(&frozen, &now).unwrap();
    }
    let was = std::fs::read_to_string(&frozen).unwrap_or_default();
    if now == was {
        return;
    }
    let (old, new): (Vec<&str>, Vec<&str>) = (was.lines().collect(), now.lines().collect());
    let gone: Vec<&&str> = old.iter().filter(|l| !new.contains(l)).collect();
    let added: Vec<&&str> = new.iter().filter(|l| !old.contains(l)).collect();
    let advice = if gone.is_empty() {
        "Only additions, which plugins don't mind. Write the new list with UPDATE_PLUGIN_API=1."
    } else {
        "Something plugins may use changed or went away, which breaks the ones using it. Keep the old one working next to the new one if you \
         can. Only if you can't, raise PLUGIN_API in manifest.rs. Then write the new list with UPDATE_PLUGIN_API=1."
    };
    panic!("The plugin API changed.\n{}{}\n\n{advice}", gone.iter().map(|l| format!("\n  - {l}")).collect::<String>(),
           added.iter().map(|l| format!("\n  + {l}")).collect::<String>());
}

#[test]
fn a_plugin_ticks_with_core_timers() {
    let folder = plugin("ticker", r#"
        local ticks = 0
        local plugin = { events = {} }
        function plugin.setup() host.schedule(1000, "tick") end
        function plugin.events.timer_fired(e)
          if e.token ~= "tick" then return end
          ticks = ticks + 1
          if ticks < 3 then host.schedule(1000, "tick") end
        end
        function plugin.on_stream_stop() host.cancel_schedule("tick") end
        function plugin.ticks() return ticks end
        return plugin
    "#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    t.setup();
    assert_eq!(t.host.scheduled, [(1000, "tick".to_string())]);
    let fired = Event::TimerFired { owner: "ticker".into(), token: "tick".into() };
    t.host.scheduled.clear();
    t.event(fired.clone());
    assert!(t.host.is_scheduled("tick"), "scheduling again repeats it");
    t.event(Event::TimerFired { owner: "someone_else".into(), token: "tick".into() });
    assert_eq!(t.call("ticks", vec![]).unwrap(), json!(1), "another plugin's timer isn't this one's");
    t.stop_stream();
    assert!(!t.host.is_scheduled("tick"));
    t.event(fired.clone());
    t.host.scheduled.clear();
    t.event(fired);
    assert_eq!(t.call("ticks", vec![]).unwrap(), json!(3));
    assert!(!t.host.is_scheduled("tick"), "it stops after three");
}

// ── Limits a plugin's own code could dodge ──────────────────────────────────

/// What `run` of a plugin with this body answers with the hook limit cut to a fifth of a second, or a failure if it hasn't answered in a few.
fn stopped_soon(name: &str, body: &str) -> Result<Value, String> {
    let folder = plugin(name, &format!("return {{ run = function() {body} end }}\n"), json!({}));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(PluginTest::open(&folder).and_then(|mut t| {
            t.set_hook_limit(std::time::Duration::from_millis(200));
            t.call("run", vec![])
        }));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the plugin's code outran the time limit")
}

#[test]
fn a_coroutine_is_held_to_the_time_limit() {
    for (name, body) in [
        ("co_resume", "coroutine.resume(coroutine.create(function() while true do end end))"),
        ("co_wrap", "coroutine.wrap(function() while true do end end)()"),
        ("co_nested", "coroutine.wrap(function() coroutine.wrap(function() while true do end end)() end)()"),
    ] {
        let err = stopped_soon(name, body).unwrap_err();
        assert!(err.contains("took too long"), "{name}: {err}");
    }
}

#[test]
fn a_catch_cant_swallow_the_time_limit() {
    for (name, body) in [
        ("catch_pcall", "while true do pcall(function() while true do end end) end"),
        ("catch_xpcall", "while true do xpcall(function() while true do end end, function() end) end"),
        ("catch_resume", "local co = coroutine.create(function() while true do end end) while true do coroutine.resume(co) end"),
    ] {
        let err = stopped_soon(name, body).unwrap_err();
        assert!(err.contains("took too long"), "{name}: {err}");
    }
    assert_eq!(stopped_soon("catch_fine", "return select(2, pcall(error, 'x', 0)) .. tostring((pcall(error)))").unwrap(), json!("xfalse"), "an ordinary catch still works");
}

#[test]
fn a_finaliser_cant_be_set() {
    // Lua runs a finaliser with its hooks off, so one that looped would hang the window or the unloading
    let err = stopped_soon("gc_set", "setmetatable({}, { __gc = function() while true do end end })").unwrap_err();
    assert!(err.contains("__gc"), "{err}");
    // Added after the metatable is set, Lua never marks the table for finalising, so nothing runs
    let late = "local mt = {} local t = setmetatable({}, mt) mt.__gc = function() while true do end end t = nil collectgarbage() collectgarbage() return 'fine'";
    assert_eq!(stopped_soon("gc_late", late).unwrap(), json!("fine"));
    assert_eq!(stopped_soon("gc_none", "local t = setmetatable({}, { __index = function() return 1 end }) return t.x").unwrap(), json!(1));
}

#[test]
fn what_a_plugin_hands_the_host_is_held_to_a_size_in_text_too() {
    // The same 6 MB text a few times over is more than the host will copy out, though Lua holds it once
    let big = "local big = string.rep('x', 6 * 1024 * 1024) ";
    let err = stopped_soon("bytes_json", &format!("{big} return json.encode({{ big, big, big }})")).unwrap_err();
    assert!(err.contains("too big"), "{err}");
    assert_eq!(stopped_soon("bytes_one", &format!("{big} return #json.encode({{ big }})")).unwrap(), json!(6 * 1024 * 1024 + 4));
    // and a call's arguments together
    let err = stopped_soon("bytes_call", &format!("local big = string.rep('x', 9 * 1024 * 1024) host.set_ui_props(big, {{ big }})")).unwrap_err();
    assert!(err.contains("too big"), "{err}");
    let err = stopped_soon("bytes_decode", "return json.decode(string.rep('1,', 9 * 1024 * 1024))").unwrap_err();
    assert!(err.contains("too big"), "{err}");
}

#[test]
fn a_hook_cant_pile_up_calls_for_the_host() {
    let folder = plugin("piles", r#"
        return { flood = function(n) for i = 1, n do host.schedule(1, "t") end end,
                 emits = function(n) for i = 1, n do host.emit("piles.x", {}) end end }
    "#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    let err = t.call("emits", vec![json!(telescope_plugin_host::guest::MAX_CALLS_PER_HOOK + 1)]).unwrap_err();
    assert!(err.contains("too much"), "{err}");
    t.call("emits", vec![json!(100)]).unwrap();
    // A hook with no host leaves its calls waiting, and the next ones add to them
    let each = telescope_plugin_host::guest::MAX_CALLS_PER_HOOK / 2 + 1;
    t.call_without_host("flood", vec![json!(each)]).unwrap();
    let err = t.call_without_host("flood", vec![json!(each)]).unwrap_err();
    assert!(err.contains("waiting for the host"), "{err}");
    t.call("emits", vec![json!(1)]).unwrap();
    t.call_without_host("flood", vec![json!(each)]).unwrap();
}

// ── Banners belong to the plugin that showed them ───────────────────────────

const BANNERS: &str = r#"
    local heard = {}
    return {
      show = function(key) host.show_issue(key, { title = "From me", actions = { { label = "Go", does = { kind = "plugin", id = "go" } } } }) end,
      clear = function(key) host.clear_issue(key) end,
      on_issue_action = function(key, id) heard[#heard + 1] = key .. "/" .. id end,
      heard = function() return heard end,
    }
"#;

#[test]
fn a_plugins_banner_keys_are_its_own() {
    let mut t = PluginTest::open(&plugin("banner_a", BANNERS, json!({}))).unwrap();
    // The app's banner, under a key the plugin knows
    t.host.show_issue("start", telescope_core::ui::Issue { title: "App's".into(), text: String::new(), actions: vec![], kind: "err".into(), details: String::new(), tell_dismiss: false });
    t.call("show", vec![json!("start")]).unwrap();
    let keys: Vec<&str> = t.host.issues.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["start", added_issue_key("banner_a", "start").as_str()], "the plugin's \"start\" is another banner than the app's");
    t.call("clear", vec![json!("start")]).unwrap();
    assert_eq!(t.host.issues.len(), 1, "it took down its own, not the app's");
    t.call("show", vec![json!("one")]).unwrap();
    t.call("show", vec![json!("two")]).unwrap();
    t.call("clear", vec![]).unwrap();
    let keys: Vec<&str> = t.host.issues.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["start"], "with no key it clears its own banners, not everyone's");
}

#[test]
fn a_banner_button_reaches_only_the_plugin_that_showed_it() {
    let mut t = PluginTest::open(&plugin("banner_b", BANNERS, json!({}))).unwrap();
    for (key, id) in [("switch_auto", "x"), (added_issue_key("someone_else", "k").as_str(), "switch_auto"), (added_issue_key("banner_b", "k").as_str(), "go")] {
        t.plugin.on_issue_action(&mut t.host, key, id);
    }
    assert_eq!(t.call("heard", vec![]).unwrap(), json!(["k/go"]), "the app's and another plugin's banners aren't its to hear about");
}
