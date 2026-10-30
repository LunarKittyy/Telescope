//! Setup's canvas sizes (CANVAS_PRESETS and canvas_dims in plugins/setup.py): Setup saves them, and the window and the virtual
//! camera read them from Setup's settings.

use crate::i18n::n_;
use serde_json::Value;

/// The preset that takes its size from the stream's first frame.
pub const AUTO: &str = n_("Auto (from first frame)");
/// The preset that uses custom_canvas_w and custom_canvas_h.
pub const CUSTOM: &str = n_("Custom...");

/// (label, size) for each canvas preset in the order the list shows them. The English label is what's saved; None is Auto
/// or Custom (told apart by the label).
pub const PRESETS: &[(&str, Option<(u32, u32)>)] = &[
    (AUTO, None),
    ("1080p 16:9 - 1920 x 1080", Some((1920, 1080))),
    (n_("1080p 16:9 Portrait - 1080 x 1920"), Some((1080, 1920))),
    ("720p 16:9 - 1280 x 720", Some((1280, 720))),
    (n_("720p 16:9 Portrait - 720 x 1280"), Some((720, 1280))),
    ("4K 16:9 - 3840 x 2160", Some((3840, 2160))),
    (n_("4K 16:9 Portrait - 2160 x 3840"), Some((2160, 3840))),
    ("XGA 4:3 - 1024 x 768", Some((1024, 768))),
    ("UXGA 4:3 - 1600 x 1200", Some((1600, 1200))),
    (CUSTOM, None),
];

/// The largest custom canvas, and the smallest side.
pub const CUSTOM_MAX: (u32, u32) = (7680, 4320);
pub const CUSTOM_MIN: u32 = 64;
/// What a custom canvas is without a usable saved size.
pub const CUSTOM_DEFAULT: (u32, u32) = (1920, 1080);

/// value rounded up to even, within top: on Linux the camera's I420 frames need an even width and height.
pub fn even(value: u32, top: u32) -> u32 {
    (value + (value & 1)).min(top - (top & 1))
}

/// A whole number saved as one (not a boolean, not 1920.0), as Python's isinstance(v, int) checks.
fn whole(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(n.as_i64().unwrap_or(i64::MAX)),
        _ => None,
    }
}

/// The custom size a Setup config holds, made even; the default for one outside the dialog's ranges (_custom_size).
pub fn custom_size(cfg: &Value) -> (u32, u32) {
    let (w, h) = (whole(cfg.get("custom_canvas_w")), whole(cfg.get("custom_canvas_h")));
    match (w, h) {
        (Some(w), Some(h)) if (64..=7680).contains(&w) && (64..=4320).contains(&h) => {
            (even(w as u32, CUSTOM_MAX.0), even(h as u32, CUSTOM_MAX.1))
        }
        _ => CUSTOM_DEFAULT,
    }
}

/// The size a preset label stands for: None for Auto or a label that isn't one, Some(None) for Custom.
pub fn preset_size(label: &str) -> Option<Option<(u32, u32)>> {
    PRESETS.iter().find(|(l, _)| *l == label).and_then(|(l, size)| match (size, *l == CUSTOM) {
        (Some(size), _) => Some(Some(*size)),
        (None, true) => Some(None),
        (None, false) => None,
    })
}

/// The canvas a Setup config asks for, or (None, None) for Auto.
pub fn canvas_dims(cfg: &Value) -> (Option<u32>, Option<u32>) {
    match cfg.get("canvas_preset").and_then(Value::as_str).and_then(preset_size) {
        Some(Some((w, h))) => (Some(w), Some(h)),
        Some(None) => {
            let (w, h) = custom_size(cfg);
            (Some(w), Some(h))
        }
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // test_plugins.py::test_setup_plugin_canvas_config's cases
    #[test]
    fn setup_plugin_canvas_config() {
        let cases = [
            (json!({}), (None, None)),
            (json!({"canvas_preset": "1280 × 720"}), (None, None)),
            (json!({"canvas_preset": "720p 16:9 - 1280 x 720"}), (Some(1280), Some(720))),
            (json!({"canvas_preset": "Custom...", "custom_canvas_w": 1112, "custom_canvas_h": 778}), (Some(1112), Some(778))),
            (json!({"canvas_preset": "Custom...", "custom_canvas_w": 1111, "custom_canvas_h": 777}), (Some(1112), Some(778))),
            (json!({"canvas_preset": "Custom...", "custom_canvas_w": 7680, "custom_canvas_h": 4319}), (Some(7680), Some(4320))),
            (json!({"canvas_preset": "Custom...", "custom_canvas_w": "1920", "custom_canvas_h": 1080}), (Some(1920), Some(1080))),
            (json!({"canvas_preset": "Custom...", "custom_canvas_w": 0, "custom_canvas_h": 720}), (Some(1920), Some(1080))),
        ];
        for (cfg, want) in cases {
            assert_eq!(canvas_dims(&cfg), want, "{cfg}");
        }
    }

    #[test]
    fn a_custom_size_is_even_and_inside_its_range() {
        assert_eq!(even(1921, 7680), 1922);
        assert_eq!(even(7680, 7680), 7680);
        assert_eq!(custom_size(&json!({"custom_canvas_w": 1920.0, "custom_canvas_h": 1080})), CUSTOM_DEFAULT);
        assert_eq!(custom_size(&json!({"custom_canvas_w": true, "custom_canvas_h": 1080})), CUSTOM_DEFAULT);
        assert_eq!(custom_size(&json!({"custom_canvas_w": 64, "custom_canvas_h": 4321})), CUSTOM_DEFAULT);
    }
}
