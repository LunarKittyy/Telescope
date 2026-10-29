//! What both cards need: reading numbers out of saved settings the way Python's float() does, and knowing when to dim until a
//! phone is paired.

use serde_json::Value;
use telescope_core::Event;

/// float(value) as Python does it for what a settings file can hold: numbers, booleans and numeric strings.
pub fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::String(s) => {
            let t = s.trim().to_ascii_lowercase();
            let body = t.trim_start_matches(['+', '-']);
            if matches!(body, "nan" | "inf" | "infinity") {
                t.parse().ok()
            } else if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-' | '_')) {
                t.replace('_', "").parse::<f64>().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A saved number held to lo..hi; the default for anything that isn't one (and for NaN). A hand-edited or damaged value
/// becomes the default instead of breaking every frame.
pub fn number_in(value: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    match value.and_then(py_float) {
        Some(v) if !v.is_nan() => v.clamp(lo, hi),
        _ => default,
    }
}

/// dim_until_paired: a card is out of use until a phone is paired (or a source that isn't a phone picked).
#[derive(Clone, Debug, Default)]
pub struct Paired {
    phones: u32,
    source: String,
}

impl Paired {
    /// The new state when this event changes it.
    pub fn hear(&mut self, event: &Event) -> Option<bool> {
        match event {
            Event::PhonesChanged { count } => self.phones = *count,
            Event::SourceSelected { source_id } => self.source = source_id.clone(),
            _ => return None,
        }
        Some(self.on())
    }

    pub fn on(&self) -> bool {
        self.phones > 0 || !self.source.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_are_read_as_python_reads_them() {
        assert_eq!(py_float(&json!(3)), Some(3.0));
        assert_eq!(py_float(&json!(true)), Some(1.0));
        assert_eq!(py_float(&json!(" 1.5 ")), Some(1.5));
        assert_eq!(py_float(&json!("hot")), None);
        assert_eq!(py_float(&json!(null)), None);
        assert_eq!(py_float(&json!([1])), None);
        assert!(py_float(&json!("nan")).unwrap().is_nan());
        assert_eq!(py_float(&json!("inf")), Some(f64::INFINITY));
        assert_eq!(number_in(Some(&json!(99)), 1.0, 0.0, 2.0), 2.0);
        assert_eq!(number_in(Some(&json!("hot")), 1.0, 0.0, 2.0), 1.0);
        assert_eq!(number_in(None, 1.0, 0.0, 2.0), 1.0);
        assert_eq!(number_in(Some(&json!("nan")), 1.0, 0.0, 2.0), 1.0);
    }

    #[test]
    fn a_card_wakes_for_a_phone_or_another_source() {
        let mut p = Paired::default();
        assert!(!p.on());
        assert_eq!(p.hear(&Event::PhonesChanged { count: 1 }), Some(true));
        assert_eq!(p.hear(&Event::PhonesChanged { count: 0 }), Some(false));
        assert_eq!(p.hear(&Event::SourceSelected { source_id: "browser".into() }), Some(true));
        assert_eq!(p.hear(&Event::StreamStopped), None);
    }
}
