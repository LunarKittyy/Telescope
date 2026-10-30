//! What both cards need: reading numbers out of saved settings the way Python's float() does, and knowing when to dim until a
//! phone is paired.

use serde_json::Value;
use std::sync::Arc;
use telescope_core::frame::Frame;
use telescope_core::media::Media;
use telescope_core::Event;

/// Reads a picture file (a PNG, JPEG, WebP or BMP) into a frame. The binary makes one from its Media.
pub type PictureReader = Arc<dyn Fn(&str) -> Result<Frame, String> + Send + Sync>;

/// pictures.read_picture: a file's picture through this Media, or why not.
pub fn picture_reader(media: Arc<dyn Media>) -> PictureReader {
    Arc::new(move |path| media.decode_picture(&std::fs::read(path).map_err(|e| e.to_string())?))
}

/// A reader for a plugin made without a Media: every picture fails to open.
pub fn no_pictures() -> PictureReader {
    Arc::new(|_| Err("no picture reader".into()))
}

/// A colour as "#rrggbb".
pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

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
                // an underscore stands between two digits, one at a time
                let b = t.as_bytes();
                let lone = (0..b.len()).filter(|&i| b[i] == b'_').all(|i| i > 0 && b[i - 1].is_ascii_digit() && b.get(i + 1).is_some_and(u8::is_ascii_digit));
                if lone { t.replace('_', "").parse::<f64>().ok() } else { None }
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

    /// A source that isn't a phone is picked (a card for phones only goes away then).
    pub fn source(&self) -> bool {
        !self.source.is_empty()
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
        // underscores only between digits, as float() takes them
        assert_eq!((py_float(&json!("1_0")), py_float(&json!("1.5_5")), py_float(&json!("1e1_0"))), (Some(10.0), Some(1.55), Some(1e10)));
        for bad in ["1__0", "_1", "1_", "1_.5", "1._5", "1_e5", "1e_5", "1e5_"] {
            assert_eq!(py_float(&json!(bad)), None, "{bad}");
        }
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
