//! What Python's int(), float(), bool() and isinstance(v, int) make of a JSON value, for settings and phone states read the
//! way the Python plugins read them.

use serde_json::Value;

/// isinstance(v, int) and not isinstance(v, bool): a whole number written as one.
pub fn whole(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.is_i64() => n.as_i64(),
        Value::Number(n) if n.is_u64() => Some(i64::MAX),
        _ => None,
    }
}

/// bool(v): false for null, false, 0, "", [] and {}.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The text int() and float() read: surrounding whitespace dropped, and underscores allowed between digits.
fn number_text(s: &str) -> Option<String> {
    let t = s.trim();
    if t.starts_with('_') || t.ends_with('_') || t.contains("__") {
        return None;
    }
    Some(t.replace('_', ""))
}

/// int(v), or None where Python raises (a list, null, "1.5", a float that isn't finite).
pub fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(i),
            None => n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64),
        },
        Value::String(s) => {
            let t = number_text(s)?;
            let digits = t.strip_prefix(['+', '-']).unwrap_or(&t);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some(t.parse::<i64>().unwrap_or(if t.starts_with('-') { i64::MIN } else { i64::MAX }))
        }
        _ => None,
    }
}

/// float(v), or None where Python raises.
pub fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = number_text(s)?;
            let lower = t.to_ascii_lowercase();
            let bare = lower.strip_prefix(['+', '-']).unwrap_or(&lower);
            if matches!(bare, "inf" | "infinity" | "nan") {
                return lower.parse::<f64>().ok().or(Some(if lower.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
            }
            if bare.is_empty() || !bare.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')) {
                return None;
            }
            t.parse::<f64>().ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conversions_follow_python() {
        assert_eq!(int(&json!("  30 ")), Some(30));
        assert_eq!(int(&json!("1_000")), Some(1000));
        assert_eq!(int(&json!("1.5")), None);
        assert_eq!(int(&json!(30.9)), Some(30));
        assert_eq!(int(&json!(true)), Some(1));
        assert_eq!(int(&json!(null)), None);
        assert_eq!(float(&json!("30")), Some(30.0));
        assert_eq!(float(&json!(null)), None);
        assert_eq!(whole(&json!(true)), None);
        assert_eq!(whole(&json!(12.0)), None);
        assert!(!truthy(&json!(0)) && truthy(&json!("x")));
    }
}
