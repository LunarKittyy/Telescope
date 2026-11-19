//! The neutral `shot` object of take_still's options (shutter, ISO, focus: every key optional, absent means like the camera) and what a
//! device reports of it. One place turns it into the options of the device that takes the photo, fitted to the ranges it reports.

use crate::i18n::{tr, Fields};
use serde_json::{json, Map, Value};

/// Exposures at least this long pause the video, as the phone's own `pausesVideo` has it.
pub const FREEZES_FROM_NS: i64 = 500_000_000;

/// Why a shot can't be taken: a code the photo's RequestDone carries and a sentence in the person's language.
#[derive(Clone, Debug, PartialEq)]
pub struct ShotRefused {
    pub code: &'static str,
    pub why: String,
}

fn int(caps: &Value, key: &str) -> i64 {
    caps.get(key).and_then(Value::as_i64).unwrap_or(0).max(0)
}

/// What a phone's still caps (the link crate's) say about `shot`: the ranges of what it can set on one photo. A key the phone can't set
/// is absent. Plugins read this as `still_caps().shot`.
pub fn phone_ranges(caps: &Value) -> Value {
    let (min, max) = (int(caps, "exposure_min_ns"), int(caps, "exposure_max_ns"));
    let mut shot = Map::new();
    // Manual exposure is what shutter and ISO both need. A phone that doesn't say its fastest shutter gets the long ones only
    if max > 0 {
        let fastest = if min > 0 { min.min(max) } else { 1_000_000_000.min(max) };
        shot.insert("shutter".into(), json!({"min_ns": fastest, "max_ns": max}));
        let (iso_min, iso_max) = (int(caps, "iso_min"), int(caps, "iso_max"));
        if iso_max > 0 {
            shot.insert("iso".into(), json!({"min": iso_min.min(iso_max), "max": iso_max}));
        }
    }
    shot.insert("freezes_from_ns".into(), json!(FREEZES_FROM_NS));
    Value::Object(shot)
}

fn number(shot: &Map<String, Value>, key: &str) -> Result<Option<f64>, ShotRefused> {
    match shot.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|n| n.is_finite()).map(Some).ok_or_else(|| ShotRefused { code: "bad_option", why: tr("The photo's settings aren't numbers.") }),
    }
}

/// Takes `shot` out of a photo's options and puts what it asks for in the phone's own: `shutter_s` becomes `exposure_ns` and `iso` stays
/// `iso`, each fitted to `ranges` (`phone_ranges`). Returns what was really used, for the photo's result (`applied`).
pub fn apply_to_phone(options: &mut Map<String, Value>, ranges: &Value) -> Result<Value, ShotRefused> {
    let Some(Value::Object(shot)) = options.remove("shot") else { return Ok(Value::Null) };
    let mut applied = Map::new();
    if let Some(seconds) = number(&shot, "shutter_s")? {
        let Some(range) = ranges.get("shutter") else {
            return Err(ShotRefused { code: "unsupported", why: tr("This phone can't set the shutter speed.") });
        };
        let (min, max) = (int(range, "min_ns"), int(range, "max_ns"));
        let ns = ((seconds * 1e9).round() as i64).clamp(min, max.max(min));
        options.insert("exposure_ns".into(), json!(ns));
        applied.insert("shutter_s".into(), json!(ns as f64 / 1e9));
    }
    if let Some(iso) = number(&shot, "iso")? {
        let Some(range) = ranges.get("iso") else {
            return Err(ShotRefused { code: "unsupported", why: tr("This phone can't set the ISO.") });
        };
        let value = (iso.round() as i64).clamp(int(range, "min"), int(range, "max").max(int(range, "min")));
        options.insert("iso".into(), json!(value));
        applied.insert("iso".into(), json!(value));
    }
    // The phone takes no focus distance or processing choice for one photo yet
    for (key, what) in [("focus_diopters", tr("the focus")), ("nr", tr("noise reduction")), ("edge", tr("sharpening"))] {
        if shot.get(key).is_some_and(|v| !v.is_null()) {
            return Err(ShotRefused { code: "unsupported", why: tr("This phone can't set {what} for one photo.").field("what", what) });
        }
    }
    Ok(Value::Object(applied))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Value {
        json!({"exposure_min_ns": 62_500, "exposure_max_ns": 10_000_000_000i64, "iso_min": 50, "iso_max": 3200})
    }

    fn options(shot: Value) -> Map<String, Value> {
        let Value::Object(map) = json!({"format": "jpeg", "shot": shot}) else { unreachable!() };
        map
    }

    #[test]
    fn ranges_come_from_what_the_phone_reports() {
        let r = phone_ranges(&caps());
        assert_eq!(r["shutter"], json!({"min_ns": 62_500, "max_ns": 10_000_000_000i64}));
        assert_eq!(r["iso"], json!({"min": 50, "max": 3200}));
        assert_eq!(r["freezes_from_ns"], json!(FREEZES_FROM_NS));
        assert!(r.get("focus").is_none() && r.get("nr").is_none());
    }

    #[test]
    fn a_phone_without_manual_exposure_has_no_shutter_or_iso() {
        let r = phone_ranges(&json!({"exposure_max_ns": 0, "iso_max": 3200}));
        assert!(r.get("shutter").is_none() && r.get("iso").is_none());
    }

    #[test]
    fn a_phone_that_doesnt_say_its_fastest_shutter_gets_the_long_ones() {
        let r = phone_ranges(&json!({"exposure_max_ns": 30_000_000_000i64, "iso_max": 800}));
        assert_eq!(r["shutter"]["min_ns"], json!(1_000_000_000));
        assert_eq!(r["iso"]["min"], json!(0));
    }

    #[test]
    fn shutter_and_iso_become_the_phones_own_options_fitted_to_the_ranges() {
        let mut o = options(json!({"shutter_s": 10.0, "iso": 800}));
        let applied = apply_to_phone(&mut o, &phone_ranges(&caps())).unwrap();
        assert_eq!((o["exposure_ns"].clone(), o["iso"].clone()), (json!(10_000_000_000i64), json!(800)));
        assert!(!o.contains_key("shot") && o["format"] == "jpeg");
        assert_eq!(applied, json!({"shutter_s": 10.0, "iso": 800}));
    }

    #[test]
    fn values_past_the_ends_are_clamped_and_reported() {
        let mut o = options(json!({"shutter_s": 30.0, "iso": 12800}));
        let applied = apply_to_phone(&mut o, &phone_ranges(&caps())).unwrap();
        assert_eq!(applied, json!({"shutter_s": 10.0, "iso": 3200}));
        let mut o = options(json!({"shutter_s": 0.000001, "iso": 1}));
        assert_eq!(apply_to_phone(&mut o, &phone_ranges(&caps())).unwrap(), json!({"shutter_s": 0.0000625, "iso": 50}));
    }

    #[test]
    fn a_key_left_out_stays_like_the_camera() {
        let mut o = options(json!({"iso": 400}));
        apply_to_phone(&mut o, &phone_ranges(&caps())).unwrap();
        assert!(!o.contains_key("exposure_ns") && o["iso"] == json!(400));
    }

    #[test]
    fn what_the_phone_cant_set_is_refused() {
        let none = phone_ranges(&json!({}));
        for shot in [json!({"shutter_s": 1.0}), json!({"iso": 100}), json!({"focus_diopters": 0.0}), json!({"nr": "off"}), json!({"edge": "off"})] {
            let refused = apply_to_phone(&mut options(shot), &none).unwrap_err();
            assert_eq!(refused.code, "unsupported");
        }
        assert_eq!(apply_to_phone(&mut options(json!({"iso": "high"})), &phone_ranges(&caps())).unwrap_err().code, "bad_option");
    }

    #[test]
    fn options_without_a_shot_are_left_alone() {
        let mut o = Map::new();
        o.insert("format".into(), json!("jpeg"));
        assert_eq!(apply_to_phone(&mut o, &Value::Null), Ok(Value::Null));
        assert_eq!(o.len(), 1);
    }
}
