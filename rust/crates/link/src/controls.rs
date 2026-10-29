//! The /v1/control bodies the window and the plugins send, built the way the Python app builds them (same keys, same
//! order, same types), so whoever sends one through PhoneControl::send puts the same bytes on the wire.

use serde_json::{json, Value};

/// round(v, 4), as the Camera card rounds a picked point.
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The camera going off while the mic streams on, and back (app.py, sources.py).
pub fn camera_on(on: bool) -> Value {
    json!({"action": "camera_on", "value": if on { 1 } else { 0 }})
}

/// Asks for a keyframe now (app.py, after a decoder lost its place).
pub fn keyframe() -> Value {
    json!({"action": "keyframe"})
}

/// Focus on a point, 0..1 in the phone's own frame (camera_control.py).
pub fn focus_point(x: f64, y: f64) -> Value {
    json!({"action": "focus_point", "x": round4(x), "y": round4(y)})
}

/// Meter exposure on a point, focus left alone (camera_control.py).
pub fn meter_point(x: f64, y: f64) -> Value {
    json!({"action": "meter_point", "x": round4(x), "y": round4(y)})
}

/// The phone's low light mode; the phone takes it as the strings "1" and "0".
pub fn low_light(on: bool) -> Value {
    json!({"action": "low_light", "value": if on { "1" } else { "0" }})
}

/// Manual white balance as the four Bayer gains.
pub fn wb_gains(r: f64, ge: f64, go: f64, b: f64) -> Value {
    json!({"action": "wb_gains", "r": r, "ge": ge, "go": go, "b": b})
}

/// The teleprompter's text on the phone ("" clears it).
pub fn notes(text: &str) -> Value {
    json!({"action": "notes", "text": text})
}

pub fn notes_scroll(by: i64) -> Value {
    json!({"action": "notes_scroll", "by": by})
}

/// speed 0 and playing 0 unless it scrolls by itself.
pub fn notes_style(size: i64, speed: i64, playing: bool) -> Value {
    json!({"action": "notes_style", "size": size, "speed": speed, "playing": if playing { 1 } else { 0 }})
}

/// Bits per second; -1 is Dynamic, 0 Auto (stream_output.py).
pub fn bitrate(bps: i64) -> Value {
    json!({"action": "bitrate", "value": bps})
}

pub fn fps_target(fps: i64) -> Value {
    json!({"action": "fps_target", "value": fps})
}

pub fn jpeg_quality(quality: i64) -> Value {
    json!({"action": "jpeg_quality", "value": quality})
}

pub fn resolution(width: i64, height: i64) -> Value {
    json!({"action": "resolution", "width": width, "height": height})
}

/// The phone's zoom: a centred ratio, then a 1/crop box around (x, y) of that view (view.PhoneZoom, in field order).
pub fn zoom(ratio: f64, crop: f64, x: f64, y: f64) -> Value {
    json!({"action": "zoom", "ratio": ratio, "crop": crop, "x": x, "y": y})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::python_json_utf8;

    #[test]
    fn bodies_go_out_byte_for_byte_as_python_writes_them() {
        // Each string is json.dumps(..., ensure_ascii=False) of the dict the Python app sends, as phone_client does
        let cases = [
            (camera_on(false), r#"{"action": "camera_on", "value": 0}"#),
            (keyframe(), r#"{"action": "keyframe"}"#),
            (focus_point(0.123456, 0.5), r#"{"action": "focus_point", "x": 0.1235, "y": 0.5}"#),
            (meter_point(0.123456, 0.5), r#"{"action": "meter_point", "x": 0.1235, "y": 0.5}"#),
            (wb_gains(2.1, 1.0, 1.0, 1.85), r#"{"action": "wb_gains", "r": 2.1, "ge": 1.0, "go": 1.0, "b": 1.85}"#),
            (notes("Hej då ☃"), r#"{"action": "notes", "text": "Hej då ☃"}"#),
            (notes_style(28, 0, false), r#"{"action": "notes_style", "size": 28, "speed": 0, "playing": 0}"#),
            (notes_scroll(-3), r#"{"action": "notes_scroll", "by": -3}"#),
            (zoom(2.0, 1.5, 0.25, 0.75), r#"{"action": "zoom", "ratio": 2.0, "crop": 1.5, "x": 0.25, "y": 0.75}"#),
            (resolution(1920, 1080), r#"{"action": "resolution", "width": 1920, "height": 1080}"#),
            (bitrate(-1), r#"{"action": "bitrate", "value": -1}"#),
            (fps_target(30), r#"{"action": "fps_target", "value": 30}"#),
            (jpeg_quality(85), r#"{"action": "jpeg_quality", "value": 85}"#),
            (low_light(true), r#"{"action": "low_light", "value": "1"}"#),
        ];
        for (body, python) in cases {
            assert_eq!(python_json_utf8(&body), python);
        }
    }
}
