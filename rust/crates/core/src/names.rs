//! Names people or phones give things (a phone, a preset), as safe display text (session_client.clean_name).

use crate::json::float_repr;
use serde_json::Value;

/// The most characters clean_name keeps.
pub const MAX_NAME_CHARS: usize = 64;

/// Python's str.isspace(): Unicode whitespace plus the separators \x1c to \x1f.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Format and private-use characters, which Python's isprintable() leaves out (a right-to-left override, say).
fn is_format_or_private(c: char) -> bool {
    matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x8E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F | 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)
}

/// A name as safe display text: no control characters, no < or > (a label would draw it as HTML), runs of white space
/// as one space, capped at MAX_NAME_CHARS.
pub fn clean_name(raw: &str) -> String {
    let text: String = raw
        .chars()
        .filter(|&c| (is_space(c) || !(c.is_control() || is_format_or_private(c))) && c != '<' && c != '>')
        .map(|c| if is_space(c) { ' ' } else { c })
        .collect();
    text.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").chars().take(MAX_NAME_CHARS).collect()
}

/// str(value) as Python writes it for what a settings file can hold, for the fields it reads with str().
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => float_repr(n.as_f64().unwrap_or(0.0)),
        },
        // A list or a dict prints with Python's own quoting, which no name in a real settings file has
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_cleaned_into_plain_display_text() {
        assert_eq!(clean_name("Sam's Pixel"), "Sam's Pixel");
        assert_eq!(clean_name("  <b>Pixel\n8</b> "), "bPixel 8/b");
        assert_eq!(clean_name("<b>Your bank</b>\n\x07phone"), "bYour bank/b phone");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(clean_name("  \t "), "");
        assert_eq!(clean_name("a\u{202e}b\u{1c}c"), "ab c");
        assert_eq!(clean_name("Des\x00k\n<b>"), "Desk b");
    }

    #[test]
    fn what_a_settings_file_holds_prints_as_python_prints_it() {
        assert_eq!(py_str(&json!("a")), "a");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_str(&json!(1e20)), "1e+20");
    }
}
