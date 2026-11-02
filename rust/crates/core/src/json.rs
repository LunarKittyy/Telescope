//! JSON read and written the way Python's json module does it, so the settings file comes out byte for byte the same.

use serde_json::{Map, Number, Value};

/// Python's own recursion limit, which json.loads runs into on a file nested this deep (RecursionError).
const MAX_DEPTH: usize = 1000;
/// Python refuses to turn a longer run of digits into an int (sys.get_int_max_str_digits).
const MAX_INT_DIGITS: usize = 4300;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.position)
    }
}

impl std::error::Error for ParseError {}

/// json.loads: NaN and Infinity read as null and a lone surrogate as U+FFFD, since a serde_json Value holds neither.
pub fn loads(text: &str) -> Result<Value, ParseError> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("Extra data"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> ParseError {
        ParseError { message: message.into(), position: self.i }
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        let Some(&c) = self.s.get(self.i) else { return Err(self.err("Expecting value")) };
        match c {
            b'{' | b'[' if depth >= MAX_DEPTH => Err(self.err("maximum recursion depth exceeded")),
            b'{' => self.object(depth + 1),
            b'[' => self.array(depth + 1),
            b'"' => Ok(Value::String(self.string()?)),
            b'n' if self.eat("null") => Ok(Value::Null),
            b't' if self.eat("true") => Ok(Value::Bool(true)),
            b'f' if self.eat("false") => Ok(Value::Bool(false)),
            b'N' if self.eat("NaN") => Ok(Value::Null),
            b'I' if self.eat("Infinity") => Ok(Value::Null),
            b'-' if self.eat("-Infinity") => Ok(Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.err("Expecting value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.i += 1;
        let mut map = Map::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return Err(self.err("Expecting property name enclosed in double quotes"));
            }
            let key = self.string()?;
            self.ws();
            if self.s.get(self.i) != Some(&b':') {
                return Err(self.err("Expecting ':' delimiter"));
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth)?;
            // A repeated key keeps its first place and its last value, like a Python dict
            map.insert(key, v);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth)?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("Invalid \\uXXXX escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.err("Invalid \\uXXXX escape"))?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.err("Invalid \\uXXXX escape"));
        }
        self.i += 4;
        Ok(u32::from_str_radix(text, 16).unwrap())
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && !matches!(self.s[self.i], b'"' | b'\\') && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            // The input is a &str, so any run between ASCII stops is whole UTF-8
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).unwrap());
            match self.s.get(self.i) {
                None => return Err(self.err("Unterminated string starting at")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else { return Err(self.err("Unterminated string starting at")) };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xd800..0xdc00).contains(&code) && self.s[self.i..].starts_with(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                let low = self.hex4()?;
                                if (0xdc00..0xe000).contains(&low) {
                                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                                } else {
                                    self.i = save;
                                }
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.err("Invalid \\escape"));
                        }
                    }
                }
                Some(_) => return Err(self.err("Invalid control character")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        let digit = |p: &Self| p.s.get(p.i).is_some_and(u8::is_ascii_digit);
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        if self.s.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else if digit(self) {
            while digit(self) {
                self.i += 1;
            }
        } else {
            self.i = start;
            return Err(self.err("Expecting value"));
        }
        let int_end = self.i;
        let mut is_float = false;
        if self.s.get(self.i) == Some(&b'.') && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
            while digit(self) {
                self.i += 1;
            }
            is_float = true;
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digit(self) {
                while digit(self) {
                    self.i += 1;
                }
                is_float = true;
            } else {
                self.i = save;
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        if is_float {
            let f: f64 = text.parse().map_err(|_| self.err("bad number"))?;
            return Ok(Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null));
        }
        if int_end - start > MAX_INT_DIGITS {
            return Err(self.err("Exceeds the limit for integer string conversion"));
        }
        if let Ok(n) = text.parse::<i64>() {
            return Ok(Value::Number(n.into()));
        }
        if let Ok(n) = text.parse::<u64>() {
            return Ok(Value::Number(n.into()));
        }
        // Bigger than 64 bits: Python keeps the exact int, this keeps the nearest float
        let f: f64 = text.parse().map_err(|_| self.err("bad number"))?;
        Ok(Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null))
    }
}

/// How json.dumps was called.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub indent: Option<usize>,
    pub sort_keys: bool,
    pub ensure_ascii: bool,
}

impl Style {
    /// json.dumps(cfg, indent=2), how config.py saves the settings file.
    pub const CONFIG: Style = Style { indent: Some(2), sort_keys: false, ensure_ascii: true };
    /// json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False), how the config fixtures are written.
    pub const FIXTURE: Style = Style { indent: Some(2), sort_keys: true, ensure_ascii: false };
}

/// json.dumps with these arguments.
pub fn dumps(value: &Value, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    out
}

fn newline(out: &mut String, style: Style, level: usize) {
    if let Some(n) = style.indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * level));
    }
}

fn write_value(out: &mut String, value: &Value, style: Style, level: usize) {
    let item_sep = if style.indent.is_some() { "," } else { ", " };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s, style.ensure_ascii),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, style, level + 1);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if style.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, style, level + 1);
                write_string(out, k, style.ensure_ascii);
                out.push_str(": ");
                write_value(out, v, style, level + 1);
            }
            newline(out, style, level);
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &Number) {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else {
        out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
    }
}

/// Python's repr() of a float: the shortest digits that read back the same, in exponent form outside 1e-4..1e16.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Rust's {:e} gives the same shortest digits, as d.ddde<exp>
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if f < 0.0 { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let rest = &digits[1..];
        let mant = if rest.is_empty() { digits[..1].to_string() } else { format!("{}.{}", &digits[..1], rest) };
        return format!("{sign}{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let point = exp + 1;
    let body = if point <= 0 {
        format!("0.{}{}", "0".repeat((-point) as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
    } else {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    };
    format!("{sign}{body}")
}

fn write_string(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && !(' '..='~').contains(&c) => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_like_python() {
        // python -c "print([repr(x) for x in (1.0, 0.1, 1e16, 1e15, 1e-5, 0.0001, 1793208492.747, -2.5e-07, 123456789012345680.0)])"
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1793208492.747, "1793208492.747"),
            (-2.5e-7, "-2.5e-07"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (-0.0, "-0.0"),
        ];
        for (f, want) in cases {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn dumps_matches_python_indent() {
        // python -c 'import json; print(json.dumps({"a": [], "b": {}, "c": [1, {"d": "é\n"}], "e": 2.0}, indent=2))'
        let v = json!({"a": [], "b": {}, "c": [1, {"d": "é\n"}], "e": 2.0});
        let want = "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    {\n      \"d\": \"\\u00e9\\n\"\n    }\n  ],\n  \"e\": 2.0\n}";
        assert_eq!(dumps(&v, Style::CONFIG), want);
    }

    #[test]
    fn astral_characters_escape_as_surrogate_pairs() {
        assert_eq!(dumps(&json!("😀\u{7f}"), Style::CONFIG), "\"\\ud83d\\ude00\\u007f\"");
        assert_eq!(dumps(&json!("😀\u{1}"), Style::FIXTURE), "\"😀\\u0001\"");
    }

    #[test]
    fn loads_keeps_order_and_types() {
        let v = loads("{\"z\": 1, \"a\": 1.0, \"m\": -0, \"z\": 3}").unwrap();
        assert_eq!(dumps(&v, Style::CONFIG), "{\n  \"z\": 3,\n  \"a\": 1.0,\n  \"m\": 0\n}");
    }

    #[test]
    fn loads_refuses_what_python_refuses() {
        for bad in ["", "{", "[1,]", "01", "{'a': 1}", "\"tab\there\"", "1 2", "\"\\x\"", "tru"] {
            assert!(loads(bad).is_err(), "{bad:?}");
        }
        assert!(loads(&"[".repeat(1001)).is_err());
        assert!(loads(&format!("{}{}", "[".repeat(500), "]".repeat(500))).is_ok());
    }

    #[test]
    fn loads_reads_escapes() {
        assert_eq!(loads(r#""\ud83d\ude00 \u00e9 \/""#).unwrap(), json!("😀 é /"));
        assert_eq!(loads(r#""\ud800x""#).unwrap(), json!("\u{fffd}x"));
    }
}
