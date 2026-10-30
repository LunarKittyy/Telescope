//! What the phone's /v1/state says, checked before anything uses it (telescope.models): a payload that doesn't have the shape the
//! desktop expects is refused whole, not half applied.

use serde_json::Value;

/// A /v1/state payload of the wrong shape, and what's wrong with it.
#[derive(Clone, Debug, PartialEq)]
pub struct PhoneStateError(pub String);

impl std::fmt::Display for PhoneStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PhoneStateError {}

fn error<T>(text: String) -> Result<T, PhoneStateError> {
    Err(PhoneStateError(text))
}

/// The type's name as Python says it in the messages.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn is_int(value: &Value) -> bool {
    value.as_number().is_some_and(|n| n.is_i64() || n.is_u64())
}

/// A required field of one type. A bool is never a number here, though Python's bool is an int.
fn require<'a>(raw: &'a serde_json::Map<String, Value>, key: &str, what: &str, fits: impl Fn(&Value) -> bool) -> Result<&'a Value, PhoneStateError> {
    let Some(value) = raw.get(key) else { return error(format!("{what}: missing '{key}'")) };
    if !fits(value) {
        return error(format!("{what}: '{key}' has wrong type ({})", kind(value)));
    }
    Ok(value)
}

fn require_str(raw: &serde_json::Map<String, Value>, key: &str, what: &str) -> Result<String, PhoneStateError> {
    require(raw, key, what, Value::is_string).map(|v| v.as_str().unwrap_or_default().to_string())
}

fn require_bool(raw: &serde_json::Map<String, Value>, key: &str, what: &str) -> Result<bool, PhoneStateError> {
    require(raw, key, what, Value::is_boolean).map(|v| v.as_bool().unwrap_or_default())
}

fn require_int(raw: &serde_json::Map<String, Value>, key: &str, what: &str) -> Result<i64, PhoneStateError> {
    // A whole number too big for i64 is a number all the same
    require(raw, key, what, is_int).map(|v| v.as_i64().unwrap_or_else(|| v.as_u64().map_or(i64::MAX, |u| u.min(i64::MAX as u64) as i64)))
}

fn require_number(raw: &serde_json::Map<String, Value>, key: &str, what: &str) -> Result<f64, PhoneStateError> {
    require(raw, key, what, Value::is_number).map(|v| v.as_f64().unwrap_or_default())
}

fn optional_number(raw: &serde_json::Map<String, Value>, key: &str) -> Result<Option<f64>, PhoneStateError> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_number() => Ok(v.as_f64()),
        Some(v) => error(format!("'{key}' has wrong type ({})", kind(v))),
    }
}

fn optional_int(raw: &serde_json::Map<String, Value>, key: &str) -> Result<Option<i64>, PhoneStateError> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if is_int(v) => Ok(Some(v.as_i64().unwrap_or(i64::MAX))),
        Some(v) => error(format!("'{key}' has wrong type ({})", kind(v))),
    }
}

fn optional_numbers(raw: &serde_json::Map<String, Value>, key: &str) -> Result<Vec<f64>, PhoneStateError> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) if list.iter().all(Value::is_number) => Ok(list.iter().filter_map(Value::as_f64).collect()),
        Some(_) => error(format!("'{key}' must be a list of numbers")),
    }
}

/// What one of the phone's cameras can do.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraCapabilities {
    pub id: String,
    pub logical_id: Option<String>,
    pub label: String,
    pub current: bool,
    pub has_ois: bool,
    pub iso_min: i64,
    pub iso_max: i64,
    pub shutter_min_ns: i64,
    pub shutter_max_ns: i64,
    pub supports_manual_sensor: bool,
    pub supports_manual_wb: bool,
    pub supports_manual_focus: bool,
    pub min_focus_distance: f64,
    pub ae_comp_min: i64,
    pub ae_comp_max: i64,
    pub ae_comp_step: f64,
    pub supports_flash: bool,
    pub hw_level: String,
    /// Newer phone apps only.
    pub supports_focus_point: bool,
    pub supports_meter_point: bool,
    /// How far the lens zooms itself (CONTROL_ZOOM_RATIO), 1 for an older phone app.
    pub zoom_ratio_max: f64,
    /// How far it crops on top of that (SCALER_CROP_REGION).
    pub crop_zoom_max: f64,
    /// Whether that crop can sit off-centre.
    pub freeform_crop: bool,
    /// The zoom ratios where its longer lenses take over.
    pub lens_zooms: Vec<f64>,
}

impl CameraCapabilities {
    pub fn from_value(raw: &Value) -> Result<CameraCapabilities, PhoneStateError> {
        let Some(raw) = raw.as_object() else { return error(format!("camera entry is not an object ({})", kind(raw))) };
        let w = "camera entry";
        let logical_id = match raw.get("logicalId") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return error(format!("{w}: 'logicalId' has wrong type")),
        };
        let flag = |key: &str| raw.get(key) == Some(&Value::Bool(true));
        // `x or 1.0`: nothing and zero are both 1
        let or_one = |v: Option<f64>| v.filter(|n| *n != 0.0).unwrap_or(1.0);
        Ok(CameraCapabilities {
            id: require_str(raw, "id", w)?,
            logical_id,
            label: require_str(raw, "label", w)?,
            current: require_bool(raw, "current", w)?,
            has_ois: require_bool(raw, "hasOis", w)?,
            iso_min: require_int(raw, "isoMin", w)?,
            iso_max: require_int(raw, "isoMax", w)?,
            shutter_min_ns: require_int(raw, "shutterMinNs", w)?,
            shutter_max_ns: require_int(raw, "shutterMaxNs", w)?,
            supports_manual_sensor: require_bool(raw, "supportsManualSensor", w)?,
            supports_manual_wb: require_bool(raw, "supportsManualWB", w)?,
            supports_manual_focus: require_bool(raw, "supportsManualFocus", w)?,
            min_focus_distance: require_number(raw, "minFocusDistance", w)?,
            ae_comp_min: require_int(raw, "aeCompMin", w)?,
            ae_comp_max: require_int(raw, "aeCompMax", w)?,
            ae_comp_step: require_number(raw, "aeCompStep", w)?,
            supports_flash: require_bool(raw, "supportsFlash", w)?,
            hw_level: require_str(raw, "hwLevel", w)?,
            supports_focus_point: flag("supportsFocusPoint"),
            supports_meter_point: flag("supportsMeterPoint"),
            zoom_ratio_max: or_one(optional_number(raw, "zoomRatioMax")?),
            crop_zoom_max: or_one(optional_number(raw, "cropZoomMax")?),
            freeform_crop: flag("freeformCrop"),
            lens_zooms: optional_numbers(raw, "lensZooms")?,
        })
    }
}

/// A decoded /v1/state response; raw is kept for what takes the payload as the phone sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct PhoneState {
    pub cameras: Vec<CameraCapabilities>,
    pub auto: bool,
    pub iso: Option<i64>,
    pub shutter_ns: Option<i64>,
    pub wb_manual: bool,
    pub ois: bool,
    pub focus_mode: String,
    pub focus_distance: f64,
    pub ae_comp: i64,
    pub nr_mode: i64,
    pub edge_mode: i64,
    pub black_level_lock: bool,
    pub torch: bool,
    pub battery: Option<i64>,
    pub charging: Option<bool>,
    pub battery_temp_c: Option<f64>,
    pub raw: Value,
}

/// Python's truthiness, for "an empty payload means no data".
fn falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

impl PhoneState {
    /// No data yet.
    pub fn empty() -> PhoneState {
        PhoneState {
            cameras: Vec::new(), auto: true, iso: None, shutter_ns: None, wb_manual: false, ois: true, focus_mode: "continuous".into(), focus_distance: 0.0,
            ae_comp: 0, nr_mode: 1, edge_mode: 1, black_level_lock: false, torch: false, battery: None, charging: None, battery_temp_c: None, raw: Value::Object(Default::default()),
        }
    }

    pub fn is_empty(&self) -> bool {
        falsy(&self.raw)
    }

    /// Decodes a /v1/state response. An empty payload means no data, which isn't an error; a shape that doesn't fit is.
    pub fn from_value(raw: &Value) -> Result<PhoneState, PhoneStateError> {
        if falsy(raw) {
            return Ok(PhoneState::empty());
        }
        let Some(map) = raw.as_object() else { return error(format!("state is not an object ({})", kind(raw))) };
        let w = "state";
        let cameras = match map.get("cameras") {
            None => Vec::new(),
            Some(Value::Array(list)) => list.iter().map(CameraCapabilities::from_value).collect::<Result<_, _>>()?,
            Some(other) => return error(format!("{w}: 'cameras' has wrong type ({})", kind(other))),
        };
        Ok(PhoneState {
            cameras,
            auto: require_bool(map, "auto", w)?,
            iso: optional_int(map, "iso")?,
            shutter_ns: optional_int(map, "shutter_ns")?,
            wb_manual: require_bool(map, "wb_manual", w)?,
            ois: require_bool(map, "ois", w)?,
            focus_mode: require_str(map, "focus_mode", w)?,
            focus_distance: require_number(map, "focus_distance", w)?,
            ae_comp: require_int(map, "ae_comp", w)?,
            nr_mode: require_int(map, "nr_mode", w)?,
            edge_mode: require_int(map, "edge_mode", w)?,
            black_level_lock: require_bool(map, "black_level_lock", w)?,
            torch: require_bool(map, "torch", w)?,
            battery: Some(require_int(map, "battery", w)?),
            charging: Some(require_bool(map, "charging", w)?),
            battery_temp_c: Some(require_number(map, "battery_temp_c", w)?),
            raw: raw.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn camera() -> Value {
        json!({
            "id": "0", "logicalId": null, "label": "Back", "current": true,
            "hasOis": true, "isoMin": 50, "isoMax": 3200,
            "shutterMinNs": 100_000, "shutterMaxNs": 1_000_000_000,
            "supportsManualSensor": true, "supportsManualWB": true,
            "supportsManualFocus": false, "minFocusDistance": 0.0,
            "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167,
            "supportsFlash": true, "hwLevel": "FULL",
        })
    }

    fn state() -> Value {
        json!({
            "cameras": [camera()], "auto": true, "wb_manual": false, "ois": true,
            "focus_mode": "continuous", "focus_distance": 0.0, "ae_comp": 0,
            "nr_mode": 1, "edge_mode": 1, "black_level_lock": false, "torch": false,
            "battery": 80, "charging": false, "battery_temp_c": 25.0,
        })
    }

    fn with(base: Value, changes: Value) -> Value {
        let mut base = base;
        for (k, v) in changes.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    fn without(base: Value, key: &str) -> Value {
        let mut base = base;
        base.as_object_mut().unwrap().remove(key);
        base
    }

    // test_models.py::test_camera_capabilities_from_dict_round_trips_all_fields
    #[test]
    fn camera_capabilities_from_dict_round_trips_all_fields() {
        let cam = CameraCapabilities::from_value(&camera()).unwrap();
        assert_eq!(cam.id, "0");
        assert_eq!(cam.logical_id, None);
        assert!(cam.has_ois);
        assert_eq!(cam.iso_min, 50);
        assert_eq!(cam.hw_level, "FULL");
        assert_eq!((cam.shutter_max_ns, cam.ae_comp_min, cam.ae_comp_step), (1_000_000_000, -8, 0.167));
    }

    // test_models.py::test_camera_capabilities_rejects_missing_field
    #[test]
    fn camera_capabilities_rejects_missing_field() {
        assert_eq!(CameraCapabilities::from_value(&without(camera(), "isoMin")), Err(PhoneStateError("camera entry: missing 'isoMin'".into())));
    }

    // test_models.py::test_camera_capabilities_rejects_wrong_type
    #[test]
    fn camera_capabilities_rejects_wrong_type() {
        let bad = with(camera(), json!({"isoMin": "50"})); // must be an int, not a string
        assert_eq!(CameraCapabilities::from_value(&bad), Err(PhoneStateError("camera entry: 'isoMin' has wrong type (str)".into())));
    }

    // test_models.py::test_camera_capabilities_rejects_bool_where_int_expected
    #[test]
    fn camera_capabilities_rejects_bool_where_int_expected() {
        let bad = with(camera(), json!({"isoMin": true}));
        assert_eq!(CameraCapabilities::from_value(&bad), Err(PhoneStateError("camera entry: 'isoMin' has wrong type (bool)".into())));
        // And a whole number where a flag is expected is no flag either
        assert!(CameraCapabilities::from_value(&with(camera(), json!({"current": 1}))).is_err());
        // A float where an int is wanted
        assert!(CameraCapabilities::from_value(&with(camera(), json!({"isoMax": 3200.0}))).is_err());
    }

    // test_models.py::test_camera_capabilities_reads_phone_zoom_fields_and_defaults_them
    #[test]
    fn camera_capabilities_reads_phone_zoom_fields_and_defaults_them() {
        let cam = CameraCapabilities::from_value(&with(camera(), json!({"zoomRatioMax": 10.0, "cropZoomMax": 4, "freeformCrop": true, "lensZooms": [3.7]}))).unwrap();
        assert_eq!((cam.zoom_ratio_max, cam.crop_zoom_max, cam.freeform_crop, cam.lens_zooms), (10.0, 4.0, true, vec![3.7]));
        let old = CameraCapabilities::from_value(&camera()).unwrap(); // an older phone app
        assert_eq!((old.zoom_ratio_max, old.crop_zoom_max, old.freeform_crop, old.lens_zooms), (1.0, 1.0, false, vec![]));
        // A zero stands in for nothing, as Python's `or 1.0` has it
        assert_eq!(CameraCapabilities::from_value(&with(camera(), json!({"zoomRatioMax": 0}))).unwrap().zoom_ratio_max, 1.0);
        // The newer phone apps' flags are only true when they say true
        let flags = CameraCapabilities::from_value(&with(camera(), json!({"supportsFocusPoint": true, "supportsMeterPoint": 1}))).unwrap();
        assert_eq!((flags.supports_focus_point, flags.supports_meter_point), (true, false));
    }

    // test_models.py::test_camera_capabilities_rejects_lens_zooms_that_arent_numbers
    #[test]
    fn camera_capabilities_rejects_lens_zooms_that_arent_numbers() {
        assert!(CameraCapabilities::from_value(&with(camera(), json!({"lensZooms": ["3.7"]}))).is_err());
        assert!(CameraCapabilities::from_value(&with(camera(), json!({"lensZooms": [true]}))).is_err());
        assert!(CameraCapabilities::from_value(&with(camera(), json!({"logicalId": 3}))).is_err());
    }

    // test_models.py::test_phone_state_empty_dict_is_not_an_error
    #[test]
    fn phone_state_empty_dict_is_not_an_error() {
        let decoded = PhoneState::from_value(&json!({})).unwrap();
        assert!(decoded.is_empty());
        assert_eq!(decoded, PhoneState::empty());
        // null is no data either, and a payload that isn't an object is an error
        assert_eq!(PhoneState::from_value(&Value::Null), Ok(PhoneState::empty()));
        assert_eq!(PhoneState::from_value(&json!([1])), Err(PhoneStateError("state is not an object (list)".into())));
    }

    // test_models.py::test_phone_state_decodes_full_valid_payload
    #[test]
    fn phone_state_decodes_full_valid_payload() {
        let decoded = PhoneState::from_value(&state()).unwrap();
        assert!(!decoded.is_empty());
        assert_eq!(decoded.cameras.len(), 1);
        assert_eq!(decoded.cameras[0].id, "0");
        assert_eq!(decoded.battery, Some(80));
        assert_eq!(decoded.raw, state());
    }

    // test_models.py::test_phone_state_rejects_missing_required_field
    #[test]
    fn phone_state_rejects_missing_required_field() {
        for key in ["auto", "wb_manual", "battery", "focus_mode"] {
            assert_eq!(PhoneState::from_value(&without(state(), key)), Err(PhoneStateError(format!("state: missing '{key}'"))), "{key}");
        }
    }

    // test_models.py::test_phone_state_rejects_non_list_cameras
    #[test]
    fn phone_state_rejects_non_list_cameras() {
        let bad = with(state(), json!({"cameras": "not-a-list"}));
        assert_eq!(PhoneState::from_value(&bad), Err(PhoneStateError("state: 'cameras' has wrong type (str)".into())));
    }

    // test_models.py::test_phone_state_rejects_malformed_camera_entry
    #[test]
    fn phone_state_rejects_malformed_camera_entry() {
        assert!(PhoneState::from_value(&with(state(), json!({"cameras": [{"id": "0"}]}))).is_err());
        assert_eq!(PhoneState::from_value(&with(state(), json!({"cameras": [5]}))), Err(PhoneStateError("camera entry is not an object (int)".into())));
    }

    // test_models.py::test_phone_state_optional_iso_and_shutter_default_to_none
    #[test]
    fn phone_state_optional_iso_and_shutter_default_to_none() {
        let decoded = PhoneState::from_value(&state()).unwrap();
        assert_eq!((decoded.iso, decoded.shutter_ns), (None, None));
        let manual = PhoneState::from_value(&with(state(), json!({"iso": 400, "shutter_ns": 8_000_000}))).unwrap();
        assert_eq!((manual.iso, manual.shutter_ns), (Some(400), Some(8_000_000)));
        assert!(PhoneState::from_value(&with(state(), json!({"iso": 400.5}))).is_err());
    }
}
