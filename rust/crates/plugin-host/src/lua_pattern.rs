//! string.find, match, gmatch and gsub for plugins: a port of Lua 5.4's pattern matcher (lstrlib.c, MIT licensed, Lua.org and PUC-Rio) with a step budget.
//!
//! Lua's own matcher backtracks without ever running a Lua instruction, so the deadline hook can't stop a pattern that takes hours. This one
//! counts its steps and raises a Lua error when a call has used them up. Results for everything that finishes are the ones Lua 5.4 gives
//! (tests/lua.rs compares both on a few hundred patterns). gsub calling a Lua function is covered by the deadline hook as usual.

use crate::lua::MEMORY_LIMIT;
use mlua::{Function, Lua, MultiValue, Table, Value};

/// How much work of the matcher one call may do: a call of find or match, a whole gsub, one step of a gmatch loop. About 0.3 s at worst.
pub const STEP_BUDGET: u64 = 70_000_000;
/// What one try at matching the rest of the pattern costs (a call of Lua's `match`), against 1 for each character a loop looks at.
const MATCH_COST: u64 = 8;

const MAX_CAPTURES: usize = 32;
/// How deep Lua lets the matcher recurse before it calls a pattern too complex.
const MAX_DEPTH: u32 = 200;
const UNFINISHED: isize = -1;
const POSITION: isize = -2;
const SPECIALS: &[u8] = b"^$*+?.([%-";

/// Why a match stopped: Lua's own error text, the budget, or the memory a result may take.
enum Fail {
    Message(Box<String>),
    Budget,
    Memory,
}

/// Why a call of the string functions stops. Lua code must see what native Lua would raise, and a Rust callback can only raise mlua's error
/// userdata, so these are handed back to a small Lua wrapper that raises them with error().
enum Stop {
    /// A plain string error, with its position in front where Lua puts one.
    Raise(String),
    /// An error value that came out of the plugin's own replacement function or table, passed on as it was.
    Rethrow(Value),
    /// Anything else, which the host deals with as before (the deadline hook's error, say).
    Other(mlua::Error),
}

impl From<mlua::Error> for Stop {
    fn from(e: mlua::Error) -> Self {
        match e {
            mlua::Error::MemoryError(_) => Stop::Raise(NO_MEMORY.into()),
            other => Stop::Other(other),
        }
    }
}

type Run<T> = Result<T, Stop>;

/// What Lua's allocator raises when it can't give more.
const NO_MEMORY: &str = "not enough memory";

type Found = Result<Option<usize>, Fail>;

fn bad<T>(message: impl Into<String>) -> Result<T, Fail> {
    Err(Fail::Message(Box::new(message.into())))
}

#[derive(Clone, Copy)]
struct Cap {
    init: usize,
    len: isize,
}

/// One capture's value: a piece of the subject (start, end) or a position.
enum Capture {
    Text(usize, usize),
    Position(usize),
}

struct Matcher<'a> {
    src: &'a [u8],
    pat: &'a [u8],
    depth: u32,
    level: usize,
    caps: [Cap; MAX_CAPTURES],
    left: u64,
}

fn match_class(c: u8, class: u8) -> bool {
    let found = match class.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => matches!(c, b' ' | 9..=13),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        b'z' => c == 0,
        _ => return class == c,
    };
    if class.is_ascii_lowercase() { found } else { !found }
}

impl<'a> Matcher<'a> {
    fn new(src: &'a [u8], pat: &'a [u8], left: u64) -> Self {
        Matcher { src, pat, depth: MAX_DEPTH, level: 0, caps: [Cap { init: 0, len: 0 }; MAX_CAPTURES], left }
    }

    fn reset(&mut self) {
        self.depth = MAX_DEPTH;
        self.level = 0;
    }

    fn tick(&mut self, cost: u64) -> Result<(), Fail> {
        spend(&mut self.left, cost)
    }

    /// The pattern's byte at p, with the 0 Lua's strings end in after the last one.
    fn p(&self, i: usize) -> u8 {
        self.pat.get(i).copied().unwrap_or(0)
    }

    fn s(&self, i: usize) -> u8 {
        self.src.get(i).copied().unwrap_or(0)
    }

    fn check_capture(&self, l: u8) -> Result<usize, Fail> {
        let l = i32::from(l) - i32::from(b'1');
        if l < 0 || l as usize >= self.level || self.caps[l as usize].len == UNFINISHED {
            return bad(format!("invalid capture index %{}", l + 1));
        }
        Ok(l as usize)
    }

    fn capture_to_close(&self) -> Result<usize, Fail> {
        (0..self.level).rev().find(|&l| self.caps[l].len == UNFINISHED).map_or_else(|| bad("invalid pattern capture"), Ok)
    }

    fn class_end(&self, mut p: usize) -> Result<usize, Fail> {
        let c = self.p(p);
        p += 1;
        match c {
            b'%' => {
                if p == self.pat.len() {
                    return bad("malformed pattern (ends with '%')");
                }
                Ok(p + 1)
            }
            b'[' => {
                if self.p(p) == b'^' {
                    p += 1;
                }
                loop {
                    if p == self.pat.len() {
                        return bad("malformed pattern (missing ']')");
                    }
                    let c = self.p(p);
                    p += 1;
                    if c == b'%' && p < self.pat.len() {
                        p += 1;
                    }
                    if self.p(p) == b']' {
                        return Ok(p + 1);
                    }
                }
            }
            _ => Ok(p),
        }
    }

    /// Whether c is in the set that starts at the `[` at p and whose `]` is at ec.
    fn match_bracket_class(&self, c: u8, mut p: usize, ec: usize) -> bool {
        let mut sig = true;
        if self.p(p + 1) == b'^' {
            sig = false;
            p += 1;
        }
        loop {
            p += 1;
            if p >= ec {
                break;
            }
            if self.p(p) == b'%' {
                p += 1;
                if match_class(c, self.p(p)) {
                    return sig;
                }
            } else if self.p(p + 1) == b'-' && p + 2 < ec {
                p += 2;
                if self.p(p - 2) <= c && c <= self.p(p) {
                    return sig;
                }
            } else if self.p(p) == c {
                return sig;
            }
        }
        !sig
    }

    /// Whether the subject's byte at s is in the class at p..ep. A bracket class costs as many units as it has bytes, anything else 1.
    fn single_match(&mut self, s: usize, p: usize, ep: usize) -> Result<bool, Fail> {
        self.tick(if self.p(p) == b'[' { (ep - p) as u64 } else { 1 })?;
        if s >= self.src.len() {
            return Ok(false);
        }
        let c = self.src[s];
        Ok(match self.p(p) {
            b'.' => true,
            b'%' => match_class(c, self.p(p + 1)),
            b'[' => self.match_bracket_class(c, p, ep - 1),
            other => other == c,
        })
    }

    fn match_balance(&mut self, mut s: usize, p: usize) -> Found {
        if p + 1 >= self.pat.len() {
            return bad("malformed pattern (missing arguments to '%b')");
        }
        if self.s(s) != self.p(p) {
            return Ok(None);
        }
        let (b, e) = (self.p(p), self.p(p + 1));
        let mut cont = 1usize;
        loop {
            s += 1;
            if s >= self.src.len() {
                return Ok(None);
            }
            self.tick(1)?;
            if self.src[s] == e {
                cont -= 1;
                if cont == 0 {
                    return Ok(Some(s + 1));
                }
            } else if self.src[s] == b {
                cont += 1;
            }
        }
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> Found {
        let mut i = 0usize;
        while self.single_match(s + i, p, ep)? {
            i += 1;
        }
        loop {
            if let Some(res) = self.do_match(s + i, ep + 1)? {
                return Ok(Some(res));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> Found {
        loop {
            if let Some(res) = self.do_match(s, ep + 1)? {
                return Ok(Some(res));
            }
            if !self.single_match(s, p, ep)? {
                return Ok(None);
            }
            s += 1;
        }
    }

    fn start_capture(&mut self, s: usize, p: usize, what: isize) -> Found {
        if self.level >= MAX_CAPTURES {
            return bad("too many captures");
        }
        self.caps[self.level] = Cap { init: s, len: what };
        self.level += 1;
        let res = self.do_match(s, p)?;
        if res.is_none() {
            self.level -= 1;
        }
        Ok(res)
    }

    fn end_capture(&mut self, s: usize, p: usize) -> Found {
        let l = self.capture_to_close()?;
        self.caps[l].len = (s - self.caps[l].init) as isize;
        let res = self.do_match(s, p)?;
        if res.is_none() {
            self.caps[l].len = UNFINISHED;
        }
        Ok(res)
    }

    fn match_capture(&mut self, s: usize, l: u8) -> Found {
        let l = self.check_capture(l)?;
        let cap = self.caps[l];
        // A position capture has no text, so it never matches (Lua reads its length as a huge unsigned number)
        let Ok(len) = usize::try_from(cap.len) else { return Ok(None) };
        self.tick(len as u64)?;
        if s <= self.src.len() && self.src.len() - s >= len && self.src[cap.init..cap.init + len] == self.src[s..s + len] {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    /// Lua's `match`: the end of the match of the pattern from p at s, if there is one.
    fn do_match(&mut self, mut s: usize, mut p: usize) -> Found {
        self.tick(MATCH_COST)?;
        if self.depth == 0 {
            return bad("pattern too complex");
        }
        self.depth -= 1;
        let found = loop {
            if p == self.pat.len() {
                break Some(s);
            }
            match self.p(p) {
                b'(' => {
                    break if self.p(p + 1) == b')' { self.start_capture(s, p + 2, POSITION)? } else { self.start_capture(s, p + 1, UNFINISHED)? };
                }
                b')' => break self.end_capture(s, p + 1)?,
                b'$' if p + 1 == self.pat.len() => break (s == self.src.len()).then_some(s),
                b'%' if self.p(p + 1) == b'b' => match self.match_balance(s, p + 2)? {
                    Some(next) => {
                        s = next;
                        p += 4;
                    }
                    None => break None,
                },
                b'%' if self.p(p + 1) == b'f' => {
                    p += 2;
                    if self.p(p) != b'[' {
                        return bad("missing '[' after '%f' in pattern");
                    }
                    let ep = self.class_end(p)?;
                    self.tick(2 * (ep - p) as u64)?;
                    let previous = if s == 0 { 0 } else { self.src[s - 1] };
                    if !self.match_bracket_class(previous, p, ep - 1) && self.match_bracket_class(self.s(s), p, ep - 1) {
                        p = ep;
                    } else {
                        break None;
                    }
                }
                b'%' if self.p(p + 1).is_ascii_digit() => match self.match_capture(s, self.p(p + 1))? {
                    Some(next) => {
                        s = next;
                        p += 2;
                    }
                    None => break None,
                },
                _ => {
                    let ep = self.class_end(p)?;
                    self.tick((ep - p - 1) as u64)?;
                    let suffix = self.p(ep);
                    if !self.single_match(s, p, ep)? {
                        if matches!(suffix, b'*' | b'?' | b'-') {
                            p = ep + 1;
                            continue;
                        }
                        break None;
                    }
                    match suffix {
                        b'?' => match self.do_match(s + 1, ep + 1)? {
                            Some(res) => break Some(res),
                            None => p = ep + 1,
                        },
                        b'+' => break self.max_expand(s + 1, p, ep)?,
                        b'*' => break self.max_expand(s, p, ep)?,
                        b'-' => break self.min_expand(s, p, ep)?,
                        _ => {
                            s += 1;
                            p = ep;
                        }
                    }
                }
            }
        };
        self.depth += 1;
        Ok(found)
    }

    fn one_capture(&self, i: usize, whole: (usize, usize)) -> Result<Capture, Fail> {
        if i >= self.level {
            if i != 0 {
                return bad(format!("invalid capture index %{}", i + 1));
            }
            return Ok(Capture::Text(whole.0, whole.1));
        }
        let cap = self.caps[i];
        match cap.len {
            UNFINISHED => bad("unfinished capture"),
            POSITION => Ok(Capture::Position(cap.init + 1)),
            len => Ok(Capture::Text(cap.init, cap.init + len as usize)),
        }
    }

    /// All captures of a match, or the whole match when the pattern has none and `whole_if_none` says so.
    fn captures(&self, whole: (usize, usize), whole_if_none: bool) -> Result<Vec<Capture>, Fail> {
        let n = if self.level == 0 && whole_if_none { 1 } else { self.level };
        (0..n).map(|i| self.one_capture(i, whole)).collect()
    }
}

fn spend(left: &mut u64, n: u64) -> Result<(), Fail> {
    match left.checked_sub(n) {
        Some(rest) => {
            *left = rest;
            Ok(())
        }
        None => Err(Fail::Budget),
    }
}

/// Lua's plain search, counting what it compares so a long needle that nearly matches everywhere can't run for ever either.
fn plain_find(hay: &[u8], needle: &[u8], left: &mut u64) -> Result<Option<usize>, Fail> {
    if needle.is_empty() {
        return Ok(Some(0));
    }
    if needle.len() > hay.len() {
        return Ok(None);
    }
    let last = hay.len() - needle.len();
    let mut at = 0;
    while at <= last {
        let Some(skip) = hay[at..=last].iter().position(|&b| b == needle[0]) else {
            spend(left, (last - at) as u64 / 8 + 1)?;
            return Ok(None);
        };
        at += skip;
        spend(left, skip as u64 / 8 + 1 + needle.len() as u64 / 16)?;
        if hay[at + 1..at + needle.len()] == needle[1..] {
            return Ok(Some(at));
        }
        at += 1;
    }
    Ok(None)
}

// ── The Lua side ────────────────────────────────────────────────────────────

/// Every function below runs inside a C function made by `guarded`, so the Lua code that called it is two levels up and the C function, which
/// Lua names by how it was called, one.
const CALLER: usize = 2;
const WRAPPER: usize = 1;

/// The C function that wraps a Rust one (its upvalue): calls it and turns the (true, results...) or (false, error value) it answers with into
/// its results or a plain error, as luaL_error gives. Being a C function, it keeps its caller's frame and name even when tail-called.
unsafe extern "C-unwind" fn guard(state: *mut mlua::ffi::lua_State) -> std::os::raw::c_int {
    use mlua::ffi::*;
    // SAFETY: only Lua's own stack functions, on the stack Lua gave this function, and nothing with a destructor alive when lua_error unwinds
    unsafe {
        let given = lua_gettop(state);
        lua_pushvalue(state, lua_upvalueindex(1));
        lua_insert(state, 1);
        lua_call(state, given, LUA_MULTRET);
        if lua_toboolean(state, 1) != 0 {
            return lua_gettop(state) - 1;
        }
        lua_pushvalue(state, 2);
        lua_error(state)
    }
}

/// `inner` (a function that answers as `reply` does) as a function that raises its errors as Lua's string functions do.
fn guarded(lua: &Lua, inner: Function) -> mlua::Result<Function> {
    // SAFETY: the closure leaves one value on the stack, the C function with `inner` as its upvalue
    unsafe { lua.exec_raw::<Function>(inner, |state| mlua::ffi::lua_pushcclosure(state, guard, 1)) }
}

/// Where the Lua code that called the running function is, as "file:line: " (what luaL_error puts in front of its message).
fn whereabouts(lua: &Lua) -> String {
    lua.inspect_stack(CALLER, |d| d.current_line().map(|line| format!("{}:{line}: ", d.source().short_src.as_deref().unwrap_or("?")))).flatten().unwrap_or_default()
}

fn raise(lua: &Lua, message: impl std::fmt::Display) -> Stop {
    Stop::Raise(format!("{}{message}", whereabouts(lua)))
}

fn failed(lua: &Lua, function: &str, fail: Fail) -> Stop {
    match fail {
        Fail::Message(m) => raise(lua, *m),
        Fail::Budget => raise(
            lua,
            format!("string.{function}: this pattern took too many steps on this text and was stopped. Use a simpler pattern (fewer *, +, - and ? with more pattern after them), cut the text down first, or look for plain text with string.find(text, what, 1, true)"),
        ),
        Fail::Memory => Stop::Raise(NO_MEMORY.into()),
    }
}

/// What a function hands to its wrapper: (true, results...) or (false, the error to raise).
fn reply(lua: &Lua, run: Run<MultiValue>) -> mlua::Result<MultiValue> {
    match run {
        Ok(mut values) => {
            values.push_front(Value::Boolean(true));
            Ok(values)
        }
        Err(Stop::Raise(message)) => Ok(MultiValue::from_vec(vec![Value::Boolean(false), Value::String(lua.create_string(message)?)])),
        Err(Stop::Rethrow(value)) => Ok(MultiValue::from_vec(vec![Value::Boolean(false), value])),
        Err(Stop::Other(e)) => Err(e),
    }
}

/// luaL_argerror: "bad argument #n to 'name' (what)".
fn arg_error(lua: &Lua, function: &str, mut arg: usize, what: &str) -> Stop {
    let (name, method) = lua
        .inspect_stack(WRAPPER, |d| {
            let names = d.names();
            (names.name.map(|n| n.to_string()), names.name_what == Some("method"))
        })
        .unwrap_or((None, false));
    let name = name.unwrap_or_else(|| format!("string.{function}"));
    if method {
        arg -= 1;
        if arg == 0 {
            return raise(lua, format!("calling '{name}' on bad self ({what})"));
        }
    }
    raise(lua, format!("bad argument #{arg} to '{name}' ({what})"))
}

fn type_name(v: Option<&Value>) -> &'static str {
    v.map_or("no value", Value::type_name)
}

/// luaL_checklstring: a string, or a number as the text Lua shows it as.
fn check_str(lua: &Lua, args: &MultiValue, i: usize, function: &str) -> Run<mlua::LuaString> {
    match args.get(i) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(v @ (Value::Integer(_) | Value::Number(_))) => Ok(lua.coerce_string(v.clone())?.expect("a number has a text")),
        other => Err(arg_error(lua, function, i + 1, &format!("string expected, got {}", type_name(other)))),
    }
}

/// luaL_optinteger.
fn opt_integer(lua: &Lua, args: &MultiValue, i: usize, function: &str, default: i64) -> Run<i64> {
    let Some(v) = args.get(i).filter(|v| !matches!(v, Value::Nil)) else { return Ok(default) };
    if let Some(n) = lua.coerce_integer(v.clone())? {
        return Ok(n);
    }
    let number = matches!(v, Value::Number(_)) || (matches!(v, Value::String(_)) && lua.coerce_number(v.clone())?.is_some());
    let what = if number { "number has no integer representation".to_string() } else { format!("number expected, got {}", type_name(Some(v))) };
    Err(arg_error(lua, function, i + 1, &what))
}

/// A 1-based position that may count from the end, as the first byte to look at (1 or more).
fn start_position(pos: i64, len: usize) -> usize {
    if pos > 0 {
        pos as usize
    } else if pos == 0 || pos < -(len as i64) {
        1
    } else {
        len - (-pos) as usize + 1
    }
}

fn capture_value(lua: &Lua, src: &[u8], c: Capture) -> mlua::Result<Value> {
    Ok(match c {
        Capture::Text(a, b) => Value::String(lua.create_string(&src[a..b])?),
        Capture::Position(p) => Value::Integer(p as i64),
    })
}

fn find_aux(lua: &Lua, args: MultiValue, find: bool) -> Run<MultiValue> {
    let function = if find { "find" } else { "match" };
    let (s, p) = (check_str(lua, &args, 0, function)?, check_str(lua, &args, 1, function)?);
    let (src, pat) = (s.as_bytes(), p.as_bytes());
    let (src, pat): (&[u8], &[u8]) = (&src, &pat);
    let init = start_position(opt_integer(lua, &args, 2, function, 1)?, src.len()) - 1;
    let fail = MultiValue::from_vec(vec![Value::Nil]);
    if init > src.len() {
        return Ok(fail);
    }
    let mut left = STEP_BUDGET;
    let plain = args.get(3).is_some_and(|v| !matches!(v, Value::Nil | Value::Boolean(false)));
    if find && (plain || !pat.iter().any(|c| SPECIALS.contains(c))) {
        return match plain_find(&src[init..], pat, &mut left).map_err(|f| failed(lua, function, f))? {
            Some(at) => Ok(MultiValue::from_vec(vec![Value::Integer((init + at + 1) as i64), Value::Integer((init + at + pat.len()) as i64)])),
            None => Ok(fail),
        };
    }
    let anchor = pat.first() == Some(&b'^');
    let mut m = Matcher::new(src, &pat[usize::from(anchor)..], left);
    let mut s1 = init;
    loop {
        m.reset();
        if let Some(end) = m.do_match(s1, 0).map_err(|f| failed(lua, function, f))? {
            let mut out = Vec::new();
            if find {
                out.push(Value::Integer(s1 as i64 + 1));
                out.push(Value::Integer(end as i64));
            }
            for c in m.captures((s1, end), !find).map_err(|f| failed(lua, function, f))? {
                out.push(capture_value(lua, src, c)?);
            }
            return Ok(MultiValue::from_vec(out));
        }
        if s1 >= src.len() || anchor {
            return Ok(fail);
        }
        s1 += 1;
    }
}

/// One call of a gmatch loop's iterator: the next match at or after `at` that isn't the empty match right after the last one.
fn gmatch_step(lua: &Lua, s: &mlua::LuaString, p: &mlua::LuaString, at: &mut usize, last_match: &mut Option<usize>) -> Run<MultiValue> {
    let (src, pat) = (s.as_bytes(), p.as_bytes());
    let (src, pat): (&[u8], &[u8]) = (&src, &pat);
    let mut m = Matcher::new(src, pat, STEP_BUDGET);
    for start in *at..=src.len() {
        m.reset();
        if let Some(end) = m.do_match(start, 0).map_err(|f| failed(lua, "gmatch", f))? {
            if Some(end) != *last_match {
                *at = end;
                *last_match = Some(end);
                let values = m.captures((start, end), true).map_err(|f| failed(lua, "gmatch", f))?;
                return Ok(values.into_iter().map(|c| capture_value(lua, src, c)).collect::<mlua::Result<MultiValue>>()?);
            }
        }
    }
    Ok(MultiValue::new())
}

fn gmatch(lua: &Lua, args: MultiValue) -> Run<MultiValue> {
    let (s, p) = (check_str(lua, &args, 0, "gmatch")?, check_str(lua, &args, 1, "gmatch")?);
    let len = s.as_bytes().len();
    let mut at = (start_position(opt_integer(lua, &args, 2, "gmatch", 1)?, len) - 1).min(len + 1);
    let mut last_match = None;
    let step = lua.create_function_mut(move |lua, _: MultiValue| reply(lua, gmatch_step(lua, &s, &p, &mut at, &mut last_match)))?;
    Ok(MultiValue::from_vec(vec![Value::Function(guarded(lua, step)?)]))
}

/// Adds bytes to a gsub's result, unless that would make it more than Lua's memory limit allows, and without aborting if the allocator can't.
fn append(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Fail> {
    if bytes.len() > MEMORY_LIMIT.saturating_sub(out.len()) {
        return Err(Fail::Memory);
    }
    out.try_reserve(bytes.len()).map_err(|_| Fail::Memory)?;
    out.extend_from_slice(bytes);
    Ok(())
}

/// A replacement string's `%0` to `%9` and `%%` filled in. Costs a unit for each byte of it and one for 16 bytes copied.
fn add_replacement(m: &mut Matcher, out: &mut Vec<u8>, news: &[u8], whole: (usize, usize)) -> Result<(), Fail> {
    m.tick(news.len() as u64)?;
    let mut i = 0;
    while i < news.len() {
        let c = news[i];
        i += 1;
        if c != b'%' {
            append(out, &[c])?;
            continue;
        }
        let d = news.get(i).copied().unwrap_or(0);
        i += 1;
        if d == b'%' {
            append(out, b"%")?;
        } else if d == b'0' {
            m.tick((whole.1 - whole.0) as u64 / 16)?;
            append(out, &m.src[whole.0..whole.1])?;
        } else if d.is_ascii_digit() {
            match m.one_capture(usize::from(d - b'1'), whole)? {
                Capture::Text(a, b) => {
                    m.tick((b - a) as u64 / 16)?;
                    append(out, &m.src[a..b])?;
                }
                Capture::Position(p) => append(out, p.to_string().as_bytes())?,
            }
        } else {
            return bad("invalid use of '%' in replacement string");
        }
    }
    Ok(())
}

/// Calls f the way Lua's own gsub does, but keeps what it raises (any value, not only text) to be raised again as it was. Gives its first result.
fn call_kept(pcall: &Function, f: &Function, args: MultiValue) -> Run<Value> {
    let mut all = args;
    all.push_front(Value::Function(f.clone()));
    let mut answer = pcall.call::<MultiValue>(all)?;
    let ok = matches!(answer.pop_front(), Some(Value::Boolean(true)));
    let first = answer.pop_front().unwrap_or(Value::Nil);
    if ok { Ok(first) } else { Err(Stop::Rethrow(first)) }
}

/// What gsub calls of the plugin's own: pcall to keep what a replacement function raises, and a Lua function that indexes a table (it may have an __index).
struct Calls {
    pcall: Function,
    index: Function,
}

fn gsub(lua: &Lua, args: MultiValue, calls: &Calls) -> Run<MultiValue> {
    let (s, p) = (check_str(lua, &args, 0, "gsub")?, check_str(lua, &args, 1, "gsub")?);
    let (src, pat) = (s.as_bytes(), p.as_bytes());
    let (src, pat): (&[u8], &[u8]) = (&src, &pat);
    let max = opt_integer(lua, &args, 3, "gsub", src.len() as i64 + 1)?;
    let repl = match args.get(2) {
        Some(Value::String(r)) => Some(r.clone()),
        Some(v @ (Value::Integer(_) | Value::Number(_))) => lua.coerce_string(v.clone())?,
        Some(Value::Function(_) | Value::Table(_)) => None,
        other => return Err(arg_error(lua, "gsub", 3, &format!("string/function/table expected, got {}", type_name(other)))),
    };
    let repl_bytes = repl.as_ref().map(|r| r.as_bytes());
    let anchor = pat.first() == Some(&b'^');
    let mut m = Matcher::new(src, &pat[usize::from(anchor)..], STEP_BUDGET);
    let mut out: Vec<u8> = Vec::new();
    let (mut at, mut last_match, mut n, mut changed) = (0usize, None, 0i64, false);
    while n < max {
        m.reset();
        let found = m.do_match(at, 0).map_err(|f| failed(lua, "gsub", f))?;
        match found.filter(|&end| Some(end) != last_match) {
            Some(end) => {
                n += 1;
                let whole = (at, end);
                if let Some(news) = &repl_bytes {
                    add_replacement(&mut m, &mut out, news, whole).map_err(|f| failed(lua, "gsub", f))?;
                    changed = true;
                } else {
                    let value = match args.get(2) {
                        Some(Value::Function(f)) => {
                            let caps = m.captures(whole, true).map_err(|f| failed(lua, "gsub", f))?;
                            let values = caps.into_iter().map(|c| capture_value(lua, src, c)).collect::<mlua::Result<MultiValue>>()?;
                            call_kept(&calls.pcall, f, values)?
                        }
                        Some(Value::Table(t)) => {
                            let first = m.one_capture(0, whole).map_err(|f| failed(lua, "gsub", f))?;
                            let first = capture_value(lua, src, first)?;
                            call_kept(&calls.pcall, &calls.index, MultiValue::from_vec(vec![Value::Table(t.clone()), first]))?
                        }
                        _ => unreachable!("the replacement is a function or a table here"),
                    };
                    match value {
                        Value::Nil | Value::Boolean(false) => append(&mut out, &src[at..end]).map_err(|f| failed(lua, "gsub", f))?,
                        v @ (Value::String(_) | Value::Integer(_) | Value::Number(_)) => {
                            append(&mut out, &lua.coerce_string(v)?.expect("a string or number has a text").as_bytes()).map_err(|f| failed(lua, "gsub", f))?;
                            changed = true;
                        }
                        other => return Err(raise(lua, format!("invalid replacement value (a {})", other.type_name()))),
                    }
                }
                at = end;
                last_match = Some(end);
            }
            None if at < src.len() => {
                append(&mut out, &src[at..=at]).map_err(|f| failed(lua, "gsub", f))?;
                at += 1;
            }
            None => break,
        }
        if anchor {
            break;
        }
    }
    let result = if changed {
        append(&mut out, &src[at..]).map_err(|f| failed(lua, "gsub", f))?;
        Value::String(lua.create_string(&out)?)
    } else {
        Value::String(s.clone())
    };
    Ok(MultiValue::from_vec(vec![result, Value::Integer(n)]))
}

/// Puts the budgeted find, match, gmatch and gsub into a Lua state's string table.
pub fn install(lua: &Lua, string: &Table) -> mlua::Result<()> {
    let calls = Calls { pcall: lua.globals().get("pcall")?, index: lua.load("local t, k = ... return t[k]").set_name("=string").into_function()? };
    let put = |name: &str, f: Function| -> mlua::Result<()> { string.set(name, guarded(lua, f)?) };
    put("find", lua.create_function(|lua, args: MultiValue| reply(lua, find_aux(lua, args, true)))?)?;
    put("match", lua.create_function(|lua, args: MultiValue| reply(lua, find_aux(lua, args, false)))?)?;
    put("gmatch", lua.create_function(|lua, args: MultiValue| reply(lua, gmatch(lua, args)))?)?;
    put("gsub", lua.create_function(move |lua, args: MultiValue| reply(lua, gsub(lua, args, &calls)))?)?;
    Ok(())
}
