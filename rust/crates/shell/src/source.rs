//! Reading a plugin's .slint before the window is built from it: its texts in the current language, the properties and
//! callbacks its exported component declares, and plugin data (JSON) to and from Slint values.

use serde_json::{Map, Value as Json};
use slint::{Model, ModelRc, SharedString, VecModel};
use i_slint_compiler::langtype::{ConstantExpression, Struct as StructType, Type};
use slint_interpreter::{ComponentInstance, Struct, Value};
use telescope_core::i18n::{tr, tr_ctx};

/// What the shell can reach on a plugin's component: what it may set and the callbacks it forwards as UiEvents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interface {
    /// `in` and `in-out` properties, in the order declared.
    pub props: Vec<String>,
    /// `out` properties, which the window can read but not set.
    pub outs: Vec<String>,
    pub callbacks: Vec<String>,
}

/// Every `@tr("...")` and `@tr("context" => "...")` swapped for its translation, so the interpreter needs no gettext of its
/// own. Plurals are left alone (no card has one: `trn` is for texts built in Rust).
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
        if after.starts_with('|') {
            continue;
        }
        if let Some(text) = after.strip_prefix("=>") {
            // @tr("ctx" => "text"): the text after the arrow, looked up under its context, comes out without one
            let text = text.trim_start();
            let Some((english, text_len)) = string_literal(text) else { continue };
            if text[text_len..].trim_start().starts_with('|') {
                continue;
            }
            out.push_str(&quote(&tr_ctx(&msgid, &english)));
            rest = &text[text_len..];
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

/// The English of the title `component` sets on itself (`title: @tr("Phone");`), None when it isn't a literal.
pub fn heading(source: &str, component: &str) -> Option<String> {
    let text = blank(source);
    let start = find_component(&text, component)?;
    let open = start + text[start..].find('{')?;
    let (mut depth, mut from) = (0, open);
    for (i, c) in text[open..].char_indices().map(|(i, c)| (i + open, c)) {
        match c {
            '{' => {
                depth += 1;
                from = i + 1;
            }
            '}' => {
                depth -= 1;
                from = i + 1;
                if depth == 0 {
                    break;
                }
            }
            ';' if depth == 1 => {
                if let Some(value) = text[from..i].trim_start().strip_prefix("title").map(str::trim_start).and_then(|t| t.strip_prefix(':')) {
                    return title_text(source[i - value.len()..i].trim());
                }
                from = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// The decoded English of a title's value: `"Text"`, `@tr("Text")` or `@tr("context" => "Text")`.
fn title_text(value: &str) -> Option<String> {
    let value = value.strip_prefix("@tr(").map_or(value, str::trim_start);
    let (first, len) = string_literal(value)?;
    match value[len..].trim_start().strip_prefix("=>") {
        Some(text) => string_literal(text.trim_start()).map(|(english, _)| english),
        None => Some(first),
    }
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
        "property" if first == "out" => {
            let after = stmt.split_once("property").map_or("", |x| x.1);
            let after = match after.trim_start().strip_prefix('<') {
                Some(t) => skip_type(t),
                None => after,
            };
            if let Some(name) = ident(after) {
                iface.outs.push(name);
            }
        }
        "property" if settable && first != "property" => {
            // Skip the <type>, which may hold spaces
            let after = stmt.split_once("property").map_or("", |x| x.1);
            let after = match after.trim_start().strip_prefix('<') {
                Some(t) => skip_type(t),
                None => after,
            };
            if let Some(name) = ident(after) {
                iface.props.push(name);
            }
        }
        "callback" => {
            let after = stmt.split_once("callback").map_or("", |x| x.1);
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

/// Plugin data for the property `name` of `ui`. Lists, structs and null are filled out to the property's declared type, so a
/// row that leaves a field out (or sends null) gets that field's default: Slint panics on a field a struct doesn't have.
pub fn to_property(ui: &ComponentInstance, name: &str, json: &Json) -> Value {
    let like = ui.get_property(name).unwrap_or(Value::Void);
    if matches!(json, Json::Array(_) | Json::Object(_) | Json::Null) {
        let same = |n: &str| n.replace('_', "-") == name.replace('_', "-");
        if let Some((_, (ty, _))) = ui.definition().properties_and_callbacks().find(|(n, _)| same(n)) {
            return fill(json, &ty, &like);
        }
    }
    to_slint(json, &like)
}

/// Plugin data as a value of exactly the type `ty`: every field its structs declare is there, a missing or null one is
/// the default, one of the wrong kind is the default too, and one it doesn't declare is dropped. `like` is the property's
/// current value: a struct sent partly keeps its other fields, a colour that doesn't parse stays, a picture stays.
fn fill(json: &Json, ty: &Type, like: &Value) -> Value {
    match (ty, json) {
        (Type::Bool, Json::Bool(b)) => Value::Bool(*b),
        (Type::String, Json::String(s)) => Value::String(SharedString::from(s.as_str())),
        (Type::Float32 | Type::Int32 | Type::Duration | Type::Angle | Type::PhysicalLength | Type::LogicalLength | Type::Rem | Type::Percent | Type::UnitProduct(_), Json::Number(n)) => {
            Value::Number(n.as_f64().unwrap_or(0.0))
        }
        (Type::Color | Type::Brush, Json::String(s)) => match (parse_color(s), like) {
            (Some(c), _) => Value::Brush(slint::Brush::SolidColor(c)),
            (None, Value::Brush(_)) => like.clone(),
            _ => default_of(ty),
        },
        // JSON has no pictures: they come through Host::set_ui_image
        (Type::Image, json) if !json.is_null() && matches!(like, Value::Image(_)) => like.clone(),
        (Type::Enumeration(e), Json::String(name)) => {
            let hit = e.values.iter().find(|v| v.replace('_', "-") == name.replace('_', "-"));
            hit.map_or_else(|| default_of(ty), |v| Value::EnumerationValue(e.name.to_string(), v.to_string()))
        }
        (Type::Enumeration(e), Json::Number(n)) => {
            let hit = n.as_u64().and_then(|i| e.values.get(i as usize));
            hit.map_or_else(|| default_of(ty), |v| Value::EnumerationValue(e.name.to_string(), v.to_string()))
        }
        (Type::Array(inner), Json::Array(items)) => Value::Model(ModelRc::new(VecModel::from(items.iter().map(|i| fill(i, inner, &Value::Void)).collect::<Vec<_>>()))),
        (Type::Model, json @ Json::Array(_)) => from_json(json),
        (Type::Struct(s), Json::Object(map)) => {
            let current = match like {
                Value::Struct(c) => Some(c),
                _ => None,
            };
            let fields = s.fields.iter().map(|(name, field_ty)| {
                let before = current.and_then(|c| c.get_field(name)).cloned();
                let sent = map.get(name.as_str()).or_else(|| map.get(&name.replace('-', "_")));
                let value = match (sent, before) {
                    (Some(json), before) => fill(json, field_ty, &before.unwrap_or_default()),
                    (None, Some(before)) => before,
                    (None, None) => field_default(s, name, field_ty),
                };
                (name.to_string(), value)
            });
            Value::Struct(fields.collect::<Struct>())
        }
        _ => default_of(ty),
    }
}

/// What a property or struct field of type `ty` holds before anything is set.
fn default_of(ty: &Type) -> Value {
    match ty {
        Type::Float32 | Type::Int32 | Type::Duration | Type::Angle | Type::PhysicalLength | Type::LogicalLength | Type::Rem | Type::Percent | Type::UnitProduct(_) => Value::Number(0.0),
        Type::String => Value::String(SharedString::new()),
        Type::Color | Type::Brush => Value::Brush(slint::Brush::default()),
        Type::Bool => Value::Bool(false),
        Type::Image => Value::Image(slint::Image::default()),
        Type::Struct(s) => Value::Struct(s.fields.iter().map(|(name, field_ty)| (name.to_string(), field_default(s, name, field_ty))).collect::<Struct>()),
        Type::Array(_) | Type::Model => Value::Model(ModelRc::default()),
        Type::Enumeration(e) => Value::EnumerationValue(e.name.to_string(), e.values.get(e.default_value).map(|v| v.to_string()).unwrap_or_default()),
        _ => Value::Void,
    }
}

/// A struct field's default: the one the struct declares (`checked: bool = true`) when it is a plain literal, else its type's.
fn field_default(s: &StructType, name: &str, ty: &Type) -> Value {
    match s.field_defaults.get(name) {
        Some(ConstantExpression::StringLiteral(t)) => Value::String(SharedString::from(t.as_str())),
        Some(ConstantExpression::NumberLiteral(n, _)) => Value::Number(*n),
        Some(ConstantExpression::BoolLiteral(b)) => Value::Bool(*b),
        Some(ConstantExpression::EnumerationValue(v)) => Value::EnumerationValue(v.enumeration.name.to_string(), v.to_string()),
        _ => default_of(ty),
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
    if !hex.is_ascii() {
        return None;
    }
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
    use slint_interpreter::ComponentHandle;

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

    // A plural under a context is left alone like one without, so what the card says still compiles
    #[test]
    fn a_plural_with_a_context_compiles() {
        let source = r#"export component Counted { in property <int> count; out property <string> says: @tr("counts" => "{n} phone" | "{n} phones" % root.count); }"#;
        let out = translate(source);
        assert_eq!(out, source);
        let compiled = crate::compile::spin_on(slint_interpreter::Compiler::default().build_from_source(out, "/counted.slint".into()));
        assert!(compiled.component("Counted").is_some(), "{:?}", compiled.diagnostics().map(|d| d.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn texts_go_through_the_translator() {
        telescope_core::i18n::set_translator(|m| match m {
            "Camera" => Some("Kamera \"x\"".to_string()),
            "ctx\u{4}Camera" => Some("Kamera (ctx)".to_string()),
            _ => None,
        });
        let out = translate(CARD);
        assert!(out.contains(r#"title: @tr("Kamera \"x\"");"#), "{out}");
        // A text with a context is looked up under it, and one with nothing for that context stays English
        assert_eq!(translate(r#"@tr("ctx" => "Camera") @tr("other" => "Camera") @tr("{n} a" | "{n} b" % n)"#), r#"@tr("Kamera (ctx)") @tr("Camera") @tr("{n} a" | "{n} b" % n)"#);
        telescope_core::i18n::set_translator(|_| None);
    }

    #[test]
    fn plugin_data_round_trips() {
        let v = json!({"id": "0", "number": "24", "n": 2.5, "on": true, "list": [1, 2]});
        assert_eq!(to_json(&from_json(&v)), v);
        let like = Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(0, 0, 0)));
        assert_eq!(to_json(&to_slint(&json!("#7fd3c4"), &like)), json!("#7fd3c4ff"));
    }

    const SPARSE: &str = r#"
        export enum Mood { calm, loud }
        export struct Inner { n: int, label: string }
        export struct Item { name: string, on: bool, size: length, ratio: float, tags: [string], inner: Inner, mood: Mood, tint: color, fill: brush, pic: image, subs: [Inner], seen: bool }
        export component Probe inherits Window {
            width: 200px;
            height: 200px;
            in property <[Item]> items;
            in property <Item> one;
            in property <string> note;
            VerticalLayout {
                for item in root.items: Rectangle {
                    background: item.tint;
                    Text { text: item.name + item.inner.label + (item.mood == Mood.loud ? "!" : "") + item.tags.length + item.subs.length + item.inner.n; }
                    Image { source: item.pic; width: item.size; opacity: item.ratio; }
                    Rectangle { background: item.fill; visible: item.on || item.seen; }
                }
            }
        }
    "#;

    fn probe() -> slint_interpreter::ComponentInstance {
        crate::offscreen::install();
        let def = crate::compile::compile("probe.slint", SPARSE, vec![], "Probe").expect("the probe compiles");
        let ui = def.create().unwrap();
        ui.show().unwrap();
        ui
    }

    fn field(row: &Value, name: &str) -> Value {
        let Value::Struct(s) = row else { panic!("not a struct: {row:?}") };
        s.get_field(name).cloned().unwrap_or_else(|| panic!("no {name} in {s:?}"))
    }

    #[test]
    fn a_colour_with_non_ascii_text_is_no_colour() {
        for text in ["#\u{e9}a", "#\u{e9}ab", "#\u{e9}abcde", "#abc\u{e9}", "#\u{e9}"] {
            assert!(parse_color(text).is_none(), "{text}");
        }
        assert!(parse_color("#0f8").is_some());
    }

    #[test]
    fn rows_that_leave_fields_out_get_their_types_defaults() {
        let ui = probe();
        let sent = json!([{"name": "a"}, {"on": true, "inner": {"label": "x"}, "mood": "loud", "tint": "#ff0000"}, {"name": null, "size": null, "tags": null, "inner": null, "mood": 7, "extra": 1}, {}]);
        ui.set_property("items", to_property(&ui, "items", &sent)).unwrap();
        // Bindings run over every row: a field a row lacked would panic here
        assert!(crate::offscreen::draw(ui.window(), 200.0, 200.0, 1.0).is_some());
        let Ok(Value::Model(rows)) = ui.get_property("items") else { panic!("not a list") };
        let row = |i| slint::Model::row_data(&rows, i).unwrap();
        assert_eq!(slint::Model::row_count(&rows), 4);
        assert_eq!(field(&row(0), "name"), Value::String("a".into()));
        assert_eq!(field(&row(0), "on"), Value::Bool(false));
        assert_eq!(field(&row(0), "size"), Value::Number(0.0));
        assert_eq!(field(&row(0), "mood"), Value::EnumerationValue("Mood".into(), "calm".into()));
        assert_eq!(field(&field(&row(0), "inner"), "n"), Value::Number(0.0));
        assert!(matches!(field(&row(0), "tags"), Value::Model(m) if slint::Model::row_count(&m) == 0));
        assert!(matches!(field(&row(0), "pic"), Value::Image(_)) && matches!(field(&row(0), "tint"), Value::Brush(_)));
        assert_eq!(field(&field(&row(1), "inner"), "label"), Value::String("x".into()));
        assert_eq!(field(&row(1), "mood"), Value::EnumerationValue("Mood".into(), "loud".into()));
        // Null, a value of the wrong kind and an undeclared field all end up as defaults or nothing
        assert_eq!(field(&row(2), "name"), Value::String("".into()));
        assert_eq!(field(&row(2), "mood"), Value::EnumerationValue("Mood".into(), "calm".into()));
        let Value::Struct(third) = row(2) else { panic!("not a struct") };
        assert!(third.get_field("extra").is_none());
        assert_eq!(field(&row(3), "ratio"), Value::Number(0.0));
    }

    #[test]
    fn a_struct_sent_in_part_keeps_the_rest_of_what_it_had() {
        let ui = probe();
        ui.set_property("one", to_property(&ui, "one", &json!({"name": "first", "on": true}))).unwrap();
        ui.set_property("one", to_property(&ui, "one", &json!({"name": "second"}))).unwrap();
        assert_eq!(field(&ui.get_property("one").unwrap(), "name"), Value::String("second".into()));
        assert_eq!(field(&ui.get_property("one").unwrap(), "on"), Value::Bool(true));
        // Null is the default, and something else where a list belongs is an empty list
        ui.set_property("one", to_property(&ui, "one", &json!(null))).unwrap();
        assert_eq!(field(&ui.get_property("one").unwrap(), "on"), Value::Bool(false));
        ui.set_property("items", to_property(&ui, "items", &json!({"not": "a list"}))).unwrap();
        assert!(matches!(ui.get_property("items"), Ok(Value::Model(m)) if slint::Model::row_count(&m) == 0));
    }

    #[test]
    fn a_heading_is_the_english_of_the_components_own_title() {
        let text = r#"
            component Other inherits Card { title: @tr("Not this"); }
            export component Phone inherits Card {
                in property <string> title-like: "no";
                // title: "a comment";
                callback picked(int);
                icon: Icons.phone;
                title: @tr("camera section" => "Light");
                Button { title: "a child's"; }
            }
            export component Quoted inherits Card { title: @tr("Say \"hello\""); }
            export component Bound inherits Card { title: root.who; }
            export component Plain inherits Card { title: "Telescope"; }
            export component Untitled inherits Card { Text { title: "a child's"; } }
        "#;
        assert_eq!(heading(text, "Phone").as_deref(), Some("Light"));
        assert_eq!(heading(text, "Plain").as_deref(), Some("Telescope"));
        assert_eq!(heading(text, "Quoted").as_deref(), Some("Say \"hello\""));
        assert_eq!(heading(text, "Bound"), None);
        assert_eq!(heading(text, "Untitled"), None);
        assert_eq!(heading(text, "Absent"), None);
    }
}
