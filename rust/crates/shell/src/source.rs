//! Reading a plugin's .slint before the window is built from it: its texts in the current language, the properties and
//! callbacks its exported component declares, and plugin data (JSON) to and from Slint values.

use serde_json::{Map, Value as Json};
use slint::{Model, ModelRc, SharedString, VecModel};
use slint_interpreter::{Struct, Value};
use telescope_core::i18n::tr;

/// What the shell can reach on a plugin's component: what it may set and the callbacks it forwards as UiEvents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interface {
    /// `in` and `in-out` properties, in the order declared.
    pub props: Vec<String>,
    pub callbacks: Vec<String>,
}

/// Every `@tr("...")` msgid swapped for its translation, so the interpreter needs no gettext of its own. Contexts and
/// plurals are left alone.
pub fn translate(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find("@tr(") {
        out.push_str(&rest[..at + 4]);
        rest = &rest[at + 4..];
        let trimmed = rest.trim_start();
        out.push_str(&rest[..rest.len() - trimmed.len()]);
        rest = trimmed;
        let Some((msgid, len)) = string_literal(rest) else { continue };
        let after = rest[len..].trim_start();
        if after.starts_with("=>") || after.starts_with('|') {
            continue;
        }
        out.push_str(&quote(&tr(&msgid)));
        rest = &rest[len..];
    }
    out.push_str(rest);
    out
}

/// A Slint string literal at the start of s: its text and how many bytes it took. None when there isn't one, or it
/// holds an interpolation (`\{...}`).
fn string_literal(s: &str) -> Option<(String, usize)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut text = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((text, i + 1)),
            '\\' => match chars.next()?.1 {
                'n' => text.push('\n'),
                '{' => return None,
                other => text.push(other),
            },
            c => text.push(c),
        }
    }
    None
}

/// Text as a Slint string literal.
pub fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '{' => out.push_str("\\{"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Source with comments and string contents blanked (same length), so a scan doesn't trip over braces in them.
fn blank(source: &str) -> String {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                out[i] = if b[i] == b'\n' { b'\n' } else { b' ' };
                i += 1;
            }
            for _ in 0..2 {
                if i < b.len() {
                    out[i] = b' ';
                    i += 1;
                }
            }
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    out[i] = b' ';
                    i += 1;
                }
                if i < b.len() {
                    out[i] = b' ';
                    i += 1;
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// The properties and callbacks `component` declares itself (not what it inherits). Empty if it isn't in source.
pub fn scan(source: &str, component: &str) -> Interface {
    let text = blank(source);
    let mut iface = Interface::default();
    let Some(start) = find_component(&text, component) else { return iface };
    let Some(open) = text[start..].find('{').map(|o| start + o) else { return iface };
    let mut depth = 0;
    let mut stmt = String::new();
    for c in text[open..].chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 2 {
                    declare(&stmt, &mut iface);
                }
                stmt.clear();
            }
            '}' => {
                depth -= 1;
                stmt.clear();
                if depth == 0 {
                    break;
                }
            }
            ';' if depth == 1 => {
                declare(&stmt, &mut iface);
                stmt.clear();
            }
            c if depth == 1 => stmt.push(c),
            _ => {}
        }
    }
    iface
}

fn find_component(text: &str, component: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = text[from..].find("component") {
        let at = from + at;
        let after = text[at + 9..].trim_start();
        let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        let before_ok = at == 0 || !text.as_bytes()[at - 1].is_ascii_alphanumeric();
        if before_ok && name == component {
            return Some(at);
        }
        from = at + 9;
    }
    None
}

/// One statement in a component body: note it if it declares a property or callback the shell can reach.
fn declare(stmt: &str, iface: &mut Interface) {
    let stmt = stmt.trim();
    let mut words = stmt.split_whitespace().peekable();
    let first = words.next().unwrap_or("");
    let (kind, settable) = match first {
        "in" | "in-out" => (words.next().unwrap_or(""), true),
        "out" | "private" => (words.next().unwrap_or(""), false),
        "pure" => (words.next().unwrap_or(""), true),
        other => (other, other == "callback"),
    };
    match kind {
        "property" if settable && first != "property" => {
            // Skip the <type>, which may hold spaces
            let after = stmt.splitn(2, "property").nth(1).unwrap_or("");
            let after = match after.trim_start().strip_prefix('<') {
                Some(t) => skip_type(t),
                None => after,
            };
            if let Some(name) = ident(after) {
                iface.props.push(name);
            }
        }
        "callback" => {
            let after = stmt.splitn(2, "callback").nth(1).unwrap_or("");
            if let Some(name) = ident(after) {
                iface.callbacks.push(name);
            }
        }
        _ => {}
    }
}

fn skip_type(t: &str) -> &str {
    let mut depth = 1;
    for (i, c) in t.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &t[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn ident(s: &str) -> Option<String> {
    let name: String = s.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// Plugin data as a Slint value, shaped like `like` (the property's current value) where JSON can't say: a colour
/// string for a brush, and so on.
pub fn to_slint(json: &Json, like: &Value) -> Value {
    match (json, like) {
        (Json::String(s), Value::Brush(_)) => parse_color(s).map(|c| Value::Brush(slint::Brush::SolidColor(c))).unwrap_or(like.clone()),
        (Json::Number(n), Value::EnumerationValue(..)) => Value::Number(n.as_f64().unwrap_or(0.0)),
        (Json::String(s), Value::EnumerationValue(e, _)) => Value::EnumerationValue(e.clone(), s.clone()),
        (Json::Array(items), Value::Model(model)) => {
            let first = model.row_data(0).unwrap_or(Value::Void);
            Value::Model(ModelRc::new(VecModel::from(items.iter().map(|i| to_slint(i, &first)).collect::<Vec<_>>())))
        }
        (Json::Object(map), Value::Struct(s)) => {
            let mut out = s.clone();
            for (k, v) in map {
                let field = s.get_field(k).cloned().unwrap_or(Value::Void);
                out.set_field(k.clone(), to_slint(v, &field));
            }
            Value::Struct(out)
        }
        (json, _) => from_json(json),
    }
}

/// Plugin data as a Slint value with nothing to go by.
pub fn from_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::Void,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Value::String(SharedString::from(s.as_str())),
        Json::Array(items) => Value::Model(ModelRc::new(VecModel::from(items.iter().map(from_json).collect::<Vec<_>>()))),
        Json::Object(map) => Value::Struct(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect::<Struct>()),
    }
}

/// A Slint value (a callback's argument, a property read back) as plugin data.
pub fn to_json(value: &Value) -> Json {
    match value {
        Value::Void => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9e15 { Json::from(*n as i64) } else { serde_json::Number::from_f64(*n).map_or(Json::Null, Json::Number) }
        }
        Value::String(s) => Json::String(s.to_string()),
        Value::Model(m) => Json::Array(m.iter().map(|v| to_json(&v)).collect()),
        Value::Struct(s) => Json::Object(s.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect::<Map<_, _>>()),
        Value::EnumerationValue(_, v) => Json::String(v.clone()),
        Value::Brush(slint::Brush::SolidColor(c)) => {
            Json::String(format!("#{:02x}{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue(), c.alpha()))
        }
        _ => Json::Null,
    }
}

/// "#rgb", "#rrggbb" or "#rrggbbaa".
pub fn parse_color(s: &str) -> Option<slint::Color> {
    let hex = s.strip_prefix('#')?;
    let n = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
            Some(slint::Color::from_rgb_u8(d(0)?, d(1)?, d(2)?))
        }
        6 => Some(slint::Color::from_rgb_u8(n(0)?, n(2)?, n(4)?)),
        8 => Some(slint::Color::from_argb_u8(n(6)?, n(0)?, n(2)?, n(4)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CARD: &str = r#"
import { Card } from "@ui-kit/ui-kit.slint";
export struct Lens { id: string, number: string }
// a comment with { braces } and property <int> nope;
export component CameraCard inherits Card {
    in property <bool> active;
    in property <[Lens]> lenses;
    in-out property <int> current-lens;
    out property <int> readout;
    property <float> private-one: 3;
    callback lens-picked(string);
    callback control(string, float);
    title: @tr("Camera");
    if active: Rectangle { property <int> inner; callback nested; }
}
"#;

    #[test]
    fn the_interface_is_what_the_component_declares_for_others() {
        let iface = scan(CARD, "CameraCard");
        assert_eq!(iface.props, ["active", "lenses", "current-lens"]);
        assert_eq!(iface.callbacks, ["lens-picked", "control"]);
        assert_eq!(scan(CARD, "Missing"), Interface::default());
    }

    #[test]
    fn texts_go_through_the_translator() {
        telescope_core::i18n::set_translator(|m| (m == "Camera").then(|| "Kamera \"x\"".to_string()));
        let out = translate(CARD);
        assert!(out.contains(r#"title: @tr("Kamera \"x\"");"#), "{out}");
        assert_eq!(translate(r#"@tr("ctx" => "Camera") @tr("{n} a" | "{n} b" % n)"#), r#"@tr("ctx" => "Camera") @tr("{n} a" | "{n} b" % n)"#);
        telescope_core::i18n::set_translator(|_| None);
    }

    #[test]
    fn plugin_data_round_trips() {
        let v = json!({"id": "0", "number": "24", "n": 2.5, "on": true, "list": [1, 2]});
        assert_eq!(to_json(&from_json(&v)), v);
        let like = Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(0, 0, 0)));
        assert_eq!(to_json(&to_slint(&json!("#7fd3c4"), &like)), json!("#7fd3c4ff"));
    }
}
