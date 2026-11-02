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

/// What a line edit with setMaxLength keeps when new, its text after an edit, is over the cap: only what the edit put in
/// is cut, from its end. old is its text before.
pub fn fit_to_cap(old: &str, new: &str, cap: usize) -> String {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - start;
    let end = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let put_in = &new[start..new.len() - end];
    let keeps = cap.saturating_sub(start + end).min(put_in.len());
    new[..start].iter().chain(&put_in[..keeps]).chain(&new[new.len() - end..]).collect()
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

/// A whole number typed into a box, for a value that's about to be held to its limits: None for anything else. More digits
/// than an i64 holds come back as the largest or smallest one, so a huge number still ends at the limit.
pub fn whole_number(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let negative = text.starts_with('-');
    Some(text.trim_start_matches('+').parse::<i64>().unwrap_or(if negative { i64::MIN } else { i64::MAX }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_typed_whole_number() {
        use super::whole_number;
        assert_eq!(whole_number(" 12 "), Some(12));
        assert_eq!(whole_number("+6"), Some(6));
        assert_eq!(whole_number("-30"), Some(-30));
        assert_eq!(whole_number("99999999999999999999999"), Some(i64::MAX));
        assert_eq!(whole_number("-99999999999999999999999"), Some(i64::MIN));
        for bad in ["", " ", "-", "+", "abc", "1.5", "1e3", "NaN", "12 px"] {
            assert_eq!(whole_number(bad), None, "{bad}");
        }
    }

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
    fn a_paste_over_the_cap_is_cut_where_it_went_in() {
        assert_eq!(fit_to_cap("ab", "abcdefgh", 5), "abcde");
        assert_eq!(fit_to_cap("ab", "a12345678b", 5), "a123b", "the text after the cursor stays");
        assert_eq!(fit_to_cap("abcd", "a12345678d", 5), "a123d", "a selection goes first");
        assert_eq!(fit_to_cap("abcde", "abxxcde", 5), "abcde", "no room: nothing goes in");
        assert_eq!(fit_to_cap("aaa", "aaaaaaaa", 5), "aaaaa");
        assert_eq!(fit_to_cap("éé", "éééééééé", 5), "ééééé", "characters, not bytes");
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
