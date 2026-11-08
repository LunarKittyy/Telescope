//! What both cards need: reading numbers out of saved settings the way Python's float() does, and knowing when to dim until a
//! phone is paired.

use serde_json::Value;
use std::sync::Arc;
use telescope_core::frame::{Frame, FrameContext};
use telescope_core::media::Media;
use telescope_core::Event;

/// The person mask of this frame as a picture of its own size, when the stream has one that fits.
pub(crate) fn mask_of(ctx: &dyn FrameContext, frame: &Frame) -> Option<crate::imgops::Gray> {
    let m = ctx.person_mask(frame)?;
    (m.len() == (frame.width * frame.height) as usize).then(|| crate::imgops::Gray { w: frame.width as usize, h: frame.height as usize, data: m.to_vec() })
}

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

/// A hue, saturation and brightness (each 0 to 1, the hue going once round the wheel) as a colour's red, green, blue.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let (h, s, v) = (h.clamp(0.0, 1.0), s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    // the red end and the violet end are one colour: 1.0 is the same as 0.0
    let sector = (h * 6.0) % 6.0;
    let (i, f) = (sector.floor(), sector - sector.floor());
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as u8 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|c| (c * 255.0).round() as u8)
}

/// A colour's hue, saturation and brightness, each 0 to 1. A grey has no hue and a black no saturation either: `keep` is what
/// they get then, so the pad's handle stays where the person left it when they drag to a corner.
pub fn rgb_to_hsv(rgb: [u8; 3], keep: (f32, f32)) -> (f32, f32, f32) {
    let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    if max <= 0.0 {
        return (keep.0, keep.1, 0.0);
    }
    let s = delta / max;
    if delta <= 0.0 {
        return (keep.0, s, max);
    }
    let sector = if max == r { ((g - b) / delta).rem_euclid(6.0) } else if max == g { (b - r) / delta + 2.0 } else { (r - g) / delta + 4.0 };
    (sector / 6.0, s, max)
}

/// A colour typed as hex: "#rrggbb" or "rrggbb", or the short "#rgb", in any case and with spaces round it.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().trim_start_matches('#');
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pair = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    match digits.len() {
        6 => Some([pair(0)?, pair(2)?, pair(4)?]),
        3 => {
            let one = |i: usize| u8::from_str_radix(&digits[i..i + 1], 16).ok().map(|n| n * 17);
            Some([one(0)?, one(1)?, one(2)?])
        }
        _ => None,
    }
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
    fn every_colour_comes_back_from_the_pad_unchanged() {
        // a colour as the pad's three numbers and back is the colour it was
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(15) {
                    let rgb = [r as u8, g as u8, b as u8];
                    let (h, s, v) = rgb_to_hsv(rgb, (0.0, 0.0));
                    assert_eq!(hsv_to_rgb(h, s, v), rgb, "{rgb:?} through {h} {s} {v}");
                }
            }
        }
    }

    #[test]
    fn the_pad_and_the_strip_make_the_colours_they_show() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv_to_rgb(1.0 / 3.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv_to_rgb(2.0 / 3.0, 1.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv_to_rgb(1.0, 1.0, 1.0), [255, 0, 0], "the strip's two ends are one colour");
        assert_eq!(hsv_to_rgb(0.4, 0.0, 1.0), [255, 255, 255]);
        assert_eq!(hsv_to_rgb(0.4, 1.0, 0.0), [0, 0, 0]);
        assert_eq!(hsv_to_rgb(0.5, 0.5, 0.5), [64, 128, 128]);
    }

    #[test]
    fn a_grey_or_a_black_keeps_the_hue_and_saturation_it_had() {
        assert_eq!(rgb_to_hsv([128, 128, 128], (0.7, 0.9)), (0.7, 0.0, 128.0 / 255.0));
        assert_eq!(rgb_to_hsv([0, 0, 0], (0.7, 0.9)), (0.7, 0.9, 0.0));
        assert_eq!(rgb_to_hsv([255, 0, 0], (0.7, 0.9)), (0.0, 1.0, 1.0));
    }

    #[test]
    fn typed_hex_is_read_loosely() {
        assert_eq!(parse_hex("#1E2228"), Some([0x1e, 0x22, 0x28]));
        assert_eq!(parse_hex(" 1e2228 "), Some([0x1e, 0x22, 0x28]));
        assert_eq!(parse_hex("#f80"), Some([0xff, 0x88, 0x00]));
        for bad in ["", "#", "12345", "1234567", "gg0000", "#12 456", "é12345"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
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
