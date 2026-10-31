//! On Air, QR codes and Photos against what the Python app does. crates/effects/tests/fixtures holds what Python made
//! (make_cards2_fixtures.py): what each plugin saves after loading odd settings, the webhook each address, method and payload makes,
//! the options each Photos pick sends, and what the QR reader finds in a set of pictures.

use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use telescope_core::event::Event;
use telescope_core::host::RequestId;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_effects::on_air::{self, Method};
use telescope_effects::on_air_plugin::OnAirPlugin;
use telescope_effects::photos::{self, PhotosPlugin, StillCaps};
use telescope_effects::qr_reader::{self, QrReaderPlugin};
use telescope_link::stills::StillResult;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/effects/tests/fixtures")
}

fn json_of(name: &str) -> Value {
    telescope_core::json::loads(&std::fs::read_to_string(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))).unwrap()
}

/// A saved number is the same number, whether it was written 100 or 100.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same(va, vb)),
        _ => a == b,
    }
}

#[test]
fn settings_load_and_save_as_python_does() {
    let mut checked = 0;
    for case in json_of("cards2_configs.json").as_array().unwrap() {
        let (mut plugin, name): (Box<dyn Plugin>, _) = match case["plugin"].as_str().unwrap() {
            "on_air" => (Box::new(OnAirPlugin::new(std::sync::Arc::new(telescope_core::testing::FakeWeb::default()), None)), "on_air"),
            "qr_reader" => (Box::new(QrReaderPlugin::new()), "qr_reader"),
            "photos" => (Box::new(PhotosPlugin::new()), "photos"),
            other => panic!("{other}"),
        };
        plugin.set_config(&case["input"]).unwrap();
        let mut got = plugin.get_config();
        // A link made fresh is random, and the default folder depends on the computer
        if name == "on_air" && got["token"] != case["input"]["token"] {
            got["token"] = json!("<token>");
        }
        if name == "photos" && got["folder"] == photos::default_folder().as_str() {
            got["folder"] = json!("<default>");
        }
        assert!(same(&got, &case["output"]), "{name} given {}:\n  rust   {got}\n  python {}", case["input"], case["output"]);
        checked += 1;
    }
    assert!(checked > 25);
}

#[test]
fn webhook_addresses_are_judged_as_python_does() {
    for case in json_of("on_air.json")["problems"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        assert_eq!(!on_air::webhook_problem(url).is_empty(), case["problem"].as_bool().unwrap(), "{url:?}");
    }
}

#[test]
fn webhooks_are_built_as_python_builds_them() {
    let all = json_of("on_air.json");
    let requests = all["requests"].as_array().unwrap();
    assert!(requests.len() > 500);
    for case in requests {
        let (url, method, payload, on) = (case["url"].as_str().unwrap(), case["method"].as_str().unwrap(), case["payload"].as_str().unwrap(), case["on"].as_bool().unwrap());
        let got = on_air::webhook_request(url, method, payload, on);
        let what = format!("{method} {url:?} {payload:?}");
        // The fragment is never sent, so where Python keeps it on the request it makes no difference
        assert_eq!(got.url, case["full_url"].as_str().unwrap().split('#').next().unwrap(), "{what}: the address");
        assert_eq!(if got.method == Method::Get { "GET" } else { "POST" }, case["http_method"], "{what}");
        assert_eq!(got.body.as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).as_deref(), case["data"].as_str(), "{what}: the body");
        assert_eq!(got.content_type.as_deref(), case["content_type"].as_str(), "{what}: the content type");
    }
}

#[test]
fn photo_options_are_chosen_as_python_chooses_them() {
    let all = json_of("photos.json");
    let lenses: std::collections::HashMap<String, StillCaps> = all["lenses"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), serde_json::from_value(v.clone()).unwrap())).collect();
    let cases = all["cases"].as_array().unwrap();
    assert!(cases.len() > 300);
    for case in cases {
        let caps = &lenses[case["lens"].as_str().unwrap()];
        let mut choices = photos::defaults();
        choices.extend(case["choices"].as_object().unwrap().clone());
        let can = photos::offered(caps, &choices);
        let want = &case["offered"];
        let what = format!("{} given {}", case["lens"], case["choices"]);
        assert!(same(&json!({"formats": can.formats, "modes": can.modes, "shots": can.shots, "exposures": can.exposures}), want), "{what}: offered");
        let options = photos::build_options(caps, &choices, case["rotation"].as_i64().unwrap() as i32);
        assert!(same(&options, &case["options"]), "{what}:\n  rust   {options}\n  python {}", case["options"]);
    }
    for (lens, sizes) in all["sizes"].as_object().unwrap() {
        for (mode, size) in sizes.as_object().unwrap() {
            let got = lenses[lens].size_for(mode).map(|(w, h)| json!([w, h]));
            assert_eq!(got.unwrap_or(Value::Null), *size, "{lens} {mode}");
        }
    }
}

#[test]
fn wifi_codes_and_links_are_read_as_python_reads_them() {
    let all = json_of("qr.json");
    for case in all["wifi"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got = qr_reader::parse_wifi(text).map(|w| json!({"ssid": w.ssid, "password": w.password, "security": w.security, "hidden": w.hidden}));
        assert_eq!(got.unwrap_or(Value::Null), case["parsed"], "{text:?}");
        let (put, line, note) = QrReaderPlugin::copy(text);
        assert_eq!(json!([put, line, note]), case["copy"], "{text:?}");
    }
    for case in all["links"].as_array().unwrap() {
        assert_eq!(qr_reader::is_link(case["text"].as_str().unwrap()), case["link"].as_bool().unwrap(), "{}", case["text"]);
    }
}

#[test]
fn quads_look_like_codes_as_python_judges() {
    for case in json_of("qr.json")["quads"].as_array().unwrap() {
        let points: Vec<[f64; 2]> = case["points"].as_array().unwrap().iter().map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]).collect();
        assert_eq!(qr_reader::plausible(&points), case["plausible"].as_bool().unwrap(), "{points:?}");
    }
}

/// How far a corner may be from where OpenCV puts it, as a fraction of the picture: the two find a code's corners by different means.
const CORNER_SLACK: f64 = 0.01;

#[test]
fn the_reader_finds_what_python_finds_in_the_same_pictures() {
    let mut missed = Vec::new();
    for frame in json_of("qr.json")["frames"].as_array().unwrap() {
        let (w, h) = (frame["width"].as_u64().unwrap() as usize, frame["height"].as_u64().unwrap() as usize);
        let mut grey = Vec::new();
        flate2::read::ZlibDecoder::new(&std::fs::read(fixtures().join(frame["file"].as_str().unwrap())).unwrap()[..]).read_to_end(&mut grey).unwrap();
        let found = qr_reader::read_code(&grey, w, h);
        let name = frame["name"].as_str().unwrap();
        let Some(want) = frame["found"].as_object() else {
            assert!(found.is_none(), "{name}: Python sees no code but Rust reads {found:?}");
            continue;
        };
        let want_text = want["text"].as_str().unwrap();
        match found {
            Some((text, corners)) if text == want_text => {
                let theirs: Vec<[f64; 2]> = want["corners"].as_array().unwrap().iter().map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]).collect();
                // The corners may start from another one of the four
                let start = (0..4).min_by(|a, b| {
                    let d = |s: usize| (0..4).map(|i| (corners[(i + s) % 4][0] - theirs[i][0]).abs() + (corners[(i + s) % 4][1] - theirs[i][1]).abs()).sum::<f64>();
                    d(*a).total_cmp(&d(*b))
                }).unwrap();
                let worst = (0..4).map(|i| (corners[(i + start) % 4][0] - theirs[i][0]).abs().max((corners[(i + start) % 4][1] - theirs[i][1]).abs())).fold(0.0, f64::max);
                eprintln!("{name:<12} read, corners off by at most {worst:.4}\n   rust   {corners:.3?}\n   python {theirs:.3?}");
                assert!(worst <= CORNER_SLACK, "{name}: corners off by {worst}");
            }
            other => missed.push(format!("{name}: Python reads {want_text:?}, Rust {:?}", other.map(|(t, _)| t))),
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");
}

// test_photos.py::test_a_failed_photo_shows_why
#[test]
fn a_failed_photo_shows_why() {
    let (mut plugin, mut host) = (PhotosPlugin::new(), FakeHost::new());
    host.still_caps = Some(json!({"size": [1920, 1080]}));
    host.streaming = true;
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    plugin.set_config(&json!({"folder": "/tmp/photos"})).unwrap();
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: json!({}) });
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "take-clicked".into(), args: vec![] });
    assert_eq!(host.stills_taken.len(), 1);
    let failed = serde_json::to_value(StillResult::failed("camera_off", "", &[])).unwrap();
    plugin.on_event(&mut host, &Event::RequestDone { request: RequestId(1), ok: false, message: String::new(), data: failed });
    let last = host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ") && c.contains("\"status\"")).unwrap();
    assert!(last.contains("camera is off"), "{last}");
}
