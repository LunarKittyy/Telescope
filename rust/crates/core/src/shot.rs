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
    // A phone that takes one focus for a photo says so; an older one only takes focus brackets, which aren't a `shot` key
    let near = caps.get("focus_near_max").and_then(Value::as_f64).filter(|d| d.is_finite() && *d > 0.0);
    if let (Some(true), Some(near)) = (caps.get("focus_distance").and_then(Value::as_bool), near) {
        shot.insert("focus".into(), json!({"near_diopters": near}));
    }
    shot.insert("freezes_from_ns".into(), json!(FREEZES_FROM_NS));
    Value::Object(shot)
}

/// A camera's numeric shutter entry, excluding Bulb and non-finite values.
pub fn dslr_shutter_seconds(raw: &str) -> Option<f64> {
    let text = raw.trim().trim_end_matches(['s', 'S', '"']).replace(',', ".");
    let seconds = match text.split_once('/') {
        Some((n, d)) => n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok().filter(|d| *d != 0.0)?,
        None => text.parse::<f64>().ok()?,
    };
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// A numeric ISO entry. Automatic and the maker's named boost modes have no numeric equivalent.
pub fn dslr_iso(raw: &str) -> Option<f64> {
    raw.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0)
}

/// Discrete photo settings, retaining the raw labels and current entry alongside numeric values.
pub fn dslr_ranges(caps: &Value) -> Value {
    let mut shot = Map::new();
    for (key, values_key, parse) in [
        ("shutter", "shutter_values_s", dslr_shutter_seconds as fn(&str) -> Option<f64>),
        ("iso", "iso_values", dslr_iso as fn(&str) -> Option<f64>),
    ] {
        let Some(menu) = caps.get(key) else { continue };
        let Some(raw) = menu.get("values").and_then(Value::as_array) else { continue };
        let entries: Vec<(usize, f64)> = raw.iter().enumerate().filter_map(|(i, value)| Some((i, parse(value.as_str()?)?))).collect();
        if entries.is_empty() { continue; }
        let values: Vec<f64> = entries.iter().map(|(_, n)| *n).collect();
        let labels: Vec<&str> = entries.iter().filter_map(|(i, _)| raw[*i].as_str()).collect();
        let current = menu.get("current").and_then(Value::as_u64).and_then(|i| entries.iter().position(|(raw_i, _)| *raw_i == i as usize));
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let range = if key == "shutter" {
            json!({"values": values, "labels": labels, "current": current, "min_ns": (min * 1e9).round() as i64, "max_ns": (max * 1e9).round() as i64})
        } else {
            json!({"values": values, "labels": labels, "current": current, "min": min as i64, "max": max as i64})
        };
        shot.insert(values_key.into(), if key == "iso" { json!(values.iter().map(|v| *v as i64).collect::<Vec<_>>()) } else { json!(values) });
        shot.insert(key.into(), range);
    }
    shot.insert("freezes_from_ns".into(), json!(0));
    Value::Object(shot)
}

/// Validate neutral overrides against DSLR caps without replacing the legacy persistent settings.
pub fn apply_to_dslr(options: &mut Map<String, Value>, ranges: &Value) -> Result<Value, ShotRefused> {
    let Some(value) = options.get("shot").filter(|v| !v.is_null()) else { return Ok(Value::Null) };
    let Some(shot) = value.as_object() else {
        return Err(ShotRefused { code: "bad_option", why: tr("The photo's settings aren't numbers.") });
    };
    let mut applied = Map::new();
    for (key, range_key) in [("shutter_s", "shutter"), ("iso", "iso")] {
        let Some(wanted) = number(shot, key)? else { continue };
        let nearest = ranges.get(range_key).and_then(|r| r.get("values")).and_then(Value::as_array)
            .into_iter().flatten().filter_map(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0)
            .min_by(|a, b| (a - wanted).abs().total_cmp(&(b - wanted).abs()));
        let Some(nearest) = nearest else {
            let what = if key == "iso" { tr("the ISO") } else { tr("the shutter speed") };
            return Err(ShotRefused { code: "unsupported", why: tr("This camera can't set {what} for one photo.").field("what", what) });
        };
        applied.insert(key.into(), if key == "iso" { json!(nearest as i64) } else { json!(nearest) });
    }
    for (key, what) in [("focus_diopters", tr("the focus")), ("nr", tr("noise reduction")), ("edge", tr("sharpening"))] {
        if shot.get(key).is_some_and(|v| !v.is_null()) {
            return Err(ShotRefused { code: "unsupported", why: tr("This camera can't set {what} for one photo.").field("what", what) });
        }
    }
    Ok(Value::Object(applied))
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
    if let Some(diopters) = number(&shot, "focus_diopters")? {
        let Some(near) = ranges.get("focus").and_then(|f| f.get("near_diopters")).and_then(Value::as_f64) else {
            return Err(ShotRefused { code: "unsupported", why: tr("This phone can't set {what} for one photo.").field("what", tr("the focus")) });
        };
        let diopters = diopters.clamp(0.0, near);
        options.insert("focus_diopters".into(), json!(diopters));
        applied.insert("focus_diopters".into(), json!(diopters));
    }
    // The phone takes no processing choice for one photo yet
    for (key, what) in [("nr", tr("noise reduction")), ("edge", tr("sharpening"))] {
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
    fn a_phone_that_takes_one_focus_gets_it_fitted_to_the_lens() {
        let older = phone_ranges(&json!({"focus_near_max": 10.0}));
        assert!(older.get("focus").is_none(), "an older phone only takes focus brackets");
        let ranges = phone_ranges(&json!({"focus_near_max": 10.0, "focus_distance": true}));
        assert_eq!(ranges["focus"], json!({"near_diopters": 10.0}));
        let mut o = options(json!({"focus_diopters": 0.0}));
        assert_eq!(apply_to_phone(&mut o, &ranges).unwrap(), json!({"focus_diopters": 0.0}));
        assert_eq!(o["focus_diopters"], json!(0.0));
        let mut o = options(json!({"focus_diopters": 25.0}));
        assert_eq!(apply_to_phone(&mut o, &ranges).unwrap(), json!({"focus_diopters": 10.0}));
        assert_eq!(apply_to_phone(&mut options(json!({"focus_diopters": 1.0})), &older).unwrap_err().code, "unsupported");
    }

    #[test]
    fn options_without_a_shot_are_left_alone() {
        let mut o = Map::new();
        o.insert("format".into(), json!("jpeg"));
        assert_eq!(apply_to_phone(&mut o, &Value::Null), Ok(Value::Null));
        assert_eq!(o.len(), 1);
    }

    fn dslr() -> Value {
        json!({"camera": true, "iso": {"values": ["Auto", "100", "200", "800", "Hi 1"], "current": 2},
            "shutter": {"values": ["Bulb", "1/4000", "0,0250s", "1", "10", "30"], "current": 2}})
    }

    #[test]
    fn dslr_caps_keep_numeric_stops_raw_labels_and_current_index() {
        let caps = dslr_ranges(&dslr());
        assert_eq!(caps["shutter"]["values"], json!([0.00025, 0.025, 1.0, 10.0, 30.0]));
        assert_eq!(caps["shutter"]["labels"], json!(["1/4000", "0,0250s", "1", "10", "30"]));
        assert_eq!(caps["shutter"]["current"], 1);
        assert_eq!(caps["iso"]["current"], 1);
        assert_eq!(caps["iso_values"], json!([100, 200, 800]));
        assert_eq!(caps["shutter"]["max_ns"], json!(30_000_000_000i64));
        assert!(caps.get("focus").is_none());
    }

    #[test]
    fn dslr_mapping_selects_nearest_numeric_entries_and_retains_legacy_strings() {
        let mut o = options(json!({"shutter_s": 8.0, "iso": 640}));
        o.insert("iso".into(), json!("200"));
        o.insert("shutter".into(), json!("1"));
        let before = o.clone();
        assert_eq!(apply_to_dslr(&mut o, &dslr_ranges(&dslr())).unwrap(), json!({"shutter_s": 10.0, "iso": 800}));
        assert_eq!(o, before, "fresh camera validation still sees the original request and persistent picks");
        assert_eq!(apply_to_dslr(&mut options(json!({"iso": 1, "shutter_s": 300.0})), &dslr_ranges(&dslr())).unwrap(), json!({"iso": 100, "shutter_s": 30.0}));
    }

    #[test]
    fn dslr_refuses_unavailable_malformed_and_unsupported_overrides() {
        for shot in [json!({"shutter_s": 10}), json!({"iso": 800})] {
            assert_eq!(apply_to_dslr(&mut options(shot), &dslr_ranges(&json!({}))).unwrap_err().code, "unsupported");
        }
        for shot in [json!({"focus_diopters": 0}), json!({"nr": "off"}), json!({"edge": "off"})] {
            assert_eq!(apply_to_dslr(&mut options(shot), &dslr_ranges(&dslr())).unwrap_err().code, "unsupported");
        }
        for shot in [json!({"shutter_s": "Bulb"}), json!({"iso": "800"}), json!(12)] {
            assert_eq!(apply_to_dslr(&mut options(shot), &dslr_ranges(&dslr())).unwrap_err().code, "bad_option");
        }
    }

    #[test]
    fn dslr_parsers_exclude_automatic_bulb_boost_and_non_finite_values() {
        for raw in ["Bulb", "Time", "0", "NaN", "inf", "1/0", "-2"] {
            assert_eq!(dslr_shutter_seconds(raw), None, "{raw}");
        }
        assert_eq!(dslr_shutter_seconds("10/25"), Some(0.4));
        assert_eq!(dslr_shutter_seconds(" 2,5s "), Some(2.5));
        for raw in ["Auto", "Hi 1", "0", "NaN", "100.5"] {
            assert_eq!(dslr_iso(raw), None, "{raw}");
        }
        assert_eq!(dslr_iso(" 800 "), Some(800.0));
    }

    #[test]
    fn dslr_empty_shot_and_legacy_only_requests_need_no_manual_caps() {
        assert_eq!(apply_to_dslr(&mut options(json!({})), &Value::Null), Ok(json!({})));
        let mut legacy = json!({"iso": "Auto", "shutter": "1/60"}).as_object().unwrap().clone();
        let before = legacy.clone();
        assert_eq!(apply_to_dslr(&mut legacy, &Value::Null), Ok(Value::Null));
        assert_eq!(legacy, before);
    }

}
