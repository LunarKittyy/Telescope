//! A Photos run against rust/tools/fake_phone.py: the phone's /v1/still, the real photo client and Stargazer's stack.
#![cfg(unix)]

use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use telescope_core::stack::{StackMode, Stacker};
use telescope_core::testing::scratch_dir;
use telescope_effects::stargazer_photos;
use telescope_link::https::{PhoneAuth, Request};
use telescope_link::stills::{StillCaps, Stills};

/// How long to wait on the fake phone: a loaded CI runner can stall a Python process and a photo for far longer than they take.
const LONG: Duration = Duration::from_secs(120);

/// The fake phone on free local ports; the process ends with this.
struct FakePhone {
    child: Child,
    /// Held open: the phone's second status line fails on a closed pipe and takes the whole process down with it
    _stdout: BufReader<ChildStdout>,
    session: u16,
    stream: u16,
    auth: PhoneAuth,
}

impl Drop for FakePhone {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Two different free ports: both are held until each is known, so the second can't be the first again.
fn free_ports() -> (u16, u16) {
    let (a, b) = (std::net::TcpListener::bind("127.0.0.1:0").unwrap(), std::net::TcpListener::bind("127.0.0.1:0").unwrap());
    (a.local_addr().unwrap().port(), b.local_addr().unwrap().port())
}

fn start() -> FakePhone {
    let tools = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools");
    let clip = scratch_dir("fake-phone-clip").join("clip.h264");
    std::fs::write(&clip, b"not a video: nothing here streams it").unwrap();
    let (session, stream) = free_ports();
    let mut child = Command::new("python3")
        .arg("-I").arg(tools.join("fake_phone.py")).arg(&clip)
        .args(["--host", "127.0.0.1", "--token", "test-token", "--legacy-presence", "--session-port", &session.to_string(), "--stream-port", &stream.to_string()])
        .stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().expect("python3 runs");
    let mut first = String::new();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut first).unwrap();
    let entry: Value = serde_json::from_str(&first).expect("the fake phone's entry");
    let pin = entry["cert_sha256"].as_str().unwrap();
    FakePhone { child, _stdout: stdout, session, stream, auth: PhoneAuth::new("test-token", pin) }
}

fn post(phone: &FakePhone, port: u16, path: &str, body: Value) -> Value {
    let url = format!("https://127.0.0.1:{port}/v1{path}");
    let body = body.to_string();
    let mut resp = phone.auth.open_any(Request::post(&url, body.as_bytes(), LONG)).unwrap();
    serde_json::from_slice(&resp.read_capped(1 << 20).unwrap()).unwrap()
}

#[test]
fn a_photos_run_against_the_fake_phone_saves_a_photo_that_stacks() {
    let phone = start();
    // Photos has nothing to offer a phone that isn't streaming
    assert_eq!(post(&phone, phone.stream, "/still", json!({"action": "take", "format": "jpeg"}))["code"], "camera_off");
    assert_eq!(post(&phone, phone.session, "/session", json!({"action": "start"})), json!({"ok": true}));

    let url = format!("https://127.0.0.1:{}/v1/state", phone.stream);
    let mut resp = phone.auth.open(Request::get(&url, LONG)).unwrap();
    let state: Value = serde_json::from_slice(&resp.read_capped(1 << 20).unwrap()).unwrap();
    let caps = StillCaps::from_state(Some(&state)).expect("the phone takes photos");
    assert_eq!(caps.size_for("normal"), Some((160, 120)));

    let folder = scratch_dir("fake-phone-photos");
    let (tx, rx) = mpsc::channel();
    let options: Map<String, Value> = json!({"format": "jpeg", "quality": 95}).as_object().unwrap().clone();
    assert!(Stills::new().take(Some("fake"), &format!("https://127.0.0.1:{}/v1", phone.stream), phone.auth.clone(), options, folder.clone(),
                               Box::new(move |r| tx.send(r).unwrap()), None, None));
    let result = rx.recv_timeout(LONG).expect("the photo never finished");
    assert!(result.ok, "{result:?}");
    assert_eq!(result.files.len(), 1);

    let path = std::path::Path::new(&result.files[0]);
    let header = stargazer_photos::probe(path).unwrap();
    assert_eq!(header.size, (160, 120));
    let frame = stargazer_photos::read(path, header).unwrap().decode(1).unwrap();
    let mut stack = Stacker::new(StackMode::Mean);
    stack.add(&frame);
    assert_eq!(stack.count(), 1);
}
