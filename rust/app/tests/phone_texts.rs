// Texts the phone words in English next to a code: the desktop words them itself, and still shows the English from a phone that sends no
// code or one it doesn't know (tests/test_phone_texts.py, docs/protocol.md "Texts the desktop words itself"). Swedish is the real catalog.
use serde_json::json;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use telescope_core::i18n;
use telescope_core::lens::{lens_label, lens_name};
use telescope_link::audio::open_audio;
use telescope_link::https::PhoneAuth;
use telescope_outputs::stream_output::codec_error_text;
use telescope_shell::plugins::camera_control::split_lens_label;
use telescope_shell::sources::camera_error_text;

static LANGUAGE: Mutex<()> = Mutex::new(());

/// Swedish is loaded while this is held, and English again afterwards (the fixture of the Python tests).
struct Swedish(#[allow(dead_code)] MutexGuard<'static, ()>);

fn swedish() -> Swedish {
    let guard = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    i18n::use_folder(Path::new(env!("CARGO_MANIFEST_DIR")).join("../locale"));
    assert_eq!(i18n::load(Some("sv")), "sv");
    Swedish(guard)
}

impl Drop for Swedish {
    fn drop(&mut self) {
        i18n::load(Some("en"));
    }
}

// test_phone_texts.py::test_lens_label_from_fields_matches_the_one_cut_from_the_english_label
#[test]
fn lens_label_from_fields_matches_the_one_cut_from_the_english_label() {
    let new = json!({"facing": "back", "focalMm": 24, "hasOis": true, "lensKind": "auto", "label": "Back ~24mm OIS [auto]"});
    assert_eq!(lens_label(&new), "Auto ~24mm OIS");
    let mut other = new.clone();
    other["label"] = json!("something else");
    assert_eq!(lens_label(&other), "Auto ~24mm OIS");
    assert_eq!(lens_label(&json!({"label": "Back ~24mm OIS [auto]"})), "Auto ~24mm OIS");
    assert_eq!(lens_label(&json!({"facing": "front", "focalMm": 25, "hasOis": false, "label": "Front ~25mm"})), "F/~25mm");
    assert_eq!(lens_label(&json!({"facing": "external", "focalMm": 0, "hasOis": false})), "Ext ?");
    assert_eq!(lens_label(&json!({"facing": "back", "focalMm": 13, "hasOis": false, "lensKind": "physical"})), "~13mm");
}

// test_phone_texts.py::test_lens_names_are_worded_in_the_language_but_keep_their_english_key
#[test]
fn lens_names_are_worded_in_the_language_but_keep_their_english_key() {
    let _sv = swedish();
    assert_eq!(lens_label(&json!({"facing": "front", "focalMm": 25})), "F/~25mm");
    assert_eq!(split_lens_label("F/~25mm"), ("25".into(), "mm".into(), "Fram".into()));
    assert_eq!(split_lens_label("Auto ~24mm OIS"), ("24".into(), "mm".into(), "Auto · OIS".into()));
    assert_eq!(lens_name("F/~25mm"), "Fram ~25mm");
}

// test_phone_texts.py::test_codec_error_is_worded_from_the_code_and_size
#[test]
fn codec_error_is_worded_from_the_code_and_size() {
    let _sv = swedish();
    let state = json!({"codec_error": "English", "codec_error_code": "camera_cant_feed", "codec_error_width": 3840, "codec_error_height": 2160});
    assert_eq!(codec_error_text(&state), "Kameran kan inte mata H.264 vid 3840x2160 på den här telefonen");
    assert_eq!(codec_error_text(&json!({"codec_error": "The phone's H.264 encoder stopped", "codec_error_code": "encoder_stopped"})), "Telefonens H.264-kodare slutade fungera");
}

// test_phone_texts.py::test_codec_error_falls_back_to_the_english_text
#[test]
fn codec_error_falls_back_to_the_english_text() {
    let _sv = swedish();
    assert_eq!(codec_error_text(&json!({"codec_error": "Old phone text"})), "Old phone text");
    assert_eq!(codec_error_text(&json!({"codec_error": "New code", "codec_error_code": "from_the_future"})), "New code");
    assert_eq!(codec_error_text(&json!({"codec_error": "No size", "codec_error_code": "camera_stopped"})), "No size");
}

// test_phone_texts.py::test_camera_error_is_worded_from_the_code
#[test]
fn camera_error_is_worded_from_the_code() {
    let _sv = swedish();
    assert!(camera_error_text(&json!({"camera_error": "English", "camera_error_code": "camera_in_use"})).starts_with("Kameran slogs"));
    assert_eq!(camera_error_text(&json!({"camera_error": "English", "camera_error_code": "x"})), "English");
    assert_eq!(camera_error_text(&json!({"camera_error": "English"})), "English");
}

/// The phone's /v1/audio answering 403 with this body; what the Microphone card says about it.
fn refused_with(body: serde_json::Value) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let text = body.to_string();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let _ = conn.read(&mut [0u8; 2048]);
        let reply = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
        conn.write_all(reply.as_bytes()).unwrap();
    });
    let result = open_audio(&format!("http://127.0.0.1:{port}/v1/audio"), &PhoneAuth::new("t", ""));
    server.join().unwrap();
    match result {
        Err(e) => e,
        Ok(_) => panic!("the phone refused, so no stream"),
    }
}

// test_phone_texts.py::test_mic_refusal_is_worded_from_the_code
#[test]
fn mic_refusal_is_worded_from_the_code() {
    let _sv = swedish();
    assert_eq!(refused_with(json!({"error": "English", "code": "mic_in_use"})), "Telefonens mikrofon används");
    assert_eq!(refused_with(json!({"error": "Only English"})), "Only English");
    assert_eq!(refused_with(json!({"error": "Unknown", "code": ["not", "a", "string"]})), "Unknown");
}
