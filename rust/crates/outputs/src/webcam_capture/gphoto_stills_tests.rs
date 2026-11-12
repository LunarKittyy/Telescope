use super::*;
use crate::webcam_capture::gphoto::Timing;
use crate::webcam_capture::gphoto_lib::{gather, Said};
use crate::webcam_capture::gphoto_lib::{open_with, Connect};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::channel;

#[derive(Default)]
struct State {
    settings: HashMap<String, (bool, Vec<String>, String)>,
    log: Vec<String>,
    previews: u32,
    /// What capture() makes: (name, bytes) per file, or the error it fails with, and how long it takes.
    made: Vec<(String, Vec<u8>)>,
    capture_error: Option<String>,
    /// capture() brings the first file and fails for the second.
    partial: bool,
    capture_takes: Duration,
    captures: u32,
    /// Live view as capture() found it.
    live_view_at_capture: Option<String>,
    expected: Vec<usize>,
    closed: bool,
}

fn setting(readonly: bool, choices: &[&str], value: &str) -> (bool, Vec<String>, String) {
    (readonly, choices.iter().map(|c| c.to_string()).collect(), value.to_string())
}

/// A D5100 as the probe found it: in M everything can be set, in a scene mode ISO and shutter speed are read-only.
fn d5100(manual: bool) -> Arc<Mutex<State>> {
    let ro = !manual;
    let mut s = State { capture_takes: Duration::from_millis(5), ..State::default() };
    let quality = ["JPEG Basic", "JPEG Normal", "JPEG Fine", "NEF (Raw)", "NEF+Basic", "NEF+Normal", "NEF+Fine"];
    s.settings.insert("imagequality".into(), setting(false, &quality, "NEF+Fine"));
    s.settings.insert("iso".into(), setting(ro, &["100", "200", "400", "800"], "400"));
    s.settings.insert("shutterspeed".into(), setting(ro, &["0,0010s", "0,0250s", "1,0000s", "30,0000s", "Bulb"], "0,0250s"));
    s.settings.insert("shutterspeed2".into(), setting(ro, &["1/1000", "1/40", "1", "30", "Bulb"], "1/40"));
    s.settings.insert("capturetarget".into(), setting(false, &["Internal RAM", "Memory card"], "Memory card"));
    s.settings.insert("viewfinder".into(), (false, vec![], "false".into()));
    s.made = vec![("DSC_0001.NEF".into(), vec![1; 8]), ("DSC_0002.JPG".into(), vec![2; 4])];
    Arc::new(Mutex::new(s))
}

struct Fake(Arc<Mutex<State>>);

impl Rig for Fake {
    fn preview(&mut self) -> Result<Vec<u8>, String> {
        let mut s = self.0.lock().unwrap();
        s.previews += 1;
        drop(s);
        std::thread::sleep(Duration::from_millis(2));
        Ok(vec![0xFF, 0xD8, 0xFF, 0xD9])
    }
    fn read(&mut self, name: &str) -> Option<Setting> {
        let (ro, choices, value) = self.0.lock().unwrap().settings.get(name)?.clone();
        let kind = if choices.is_empty() { Kind::Toggle } else { Kind::Choice };
        Some(Setting { kind, readonly: ro, value, choices })
    }
    fn write(&mut self, name: &str, value: &str) -> Result<(), String> {
        let mut s = self.0.lock().unwrap();
        s.log.push(format!("{name}={value}"));
        match s.settings.get_mut(name) {
            Some((false, _, v)) => {
                *v = value.into();
                Ok(())
            }
            Some(_) => Err("Access Denied".into()),
            None => Err("no such setting".into()),
        }
    }
    fn restart_live_view(&mut self) {}
    fn capture(&mut self, expect: usize) -> Result<Vec<Shot>, Incomplete> {
        let (takes, error) = {
            let mut s = self.0.lock().unwrap();
            s.log.push("capture".into());
            s.captures += 1;
            s.expected.push(expect);
            s.live_view_at_capture = s.settings.get("viewfinder").map(|v| v.2.clone());
            (s.capture_takes, s.capture_error.clone())
        };
        std::thread::sleep(takes);
        if let Some(why) = error {
            return Err(why.into());
        }
        let s = self.0.lock().unwrap();
        if s.partial {
            return Err(Incomplete { why: "1 of 2 files came".into(), shots: s.made.iter().take(1).map(|(name, data)| Shot { name: name.clone(), data: data.clone() }).collect() });
        }
        Ok(s.made.iter().take(expect).map(|(name, data)| Shot { name: name.clone(), data: data.clone() }).collect())
    }
    fn close(&mut self) {
        self.0.lock().unwrap().closed = true;
    }
}

fn wish(format: Format) -> Wish {
    Wish { format, iso: None, shutter: None, timer_s: 0 }
}

fn log(state: &Arc<Mutex<State>>) -> Vec<String> {
    state.lock().unwrap().log.clone()
}

// ── What the camera offers ───────────────────────────────────────────────────

#[test]
fn quality_choices_are_told_apart_by_what_they_make() {
    for (choice, class) in [("JPEG Fine", Class::Jpeg), ("NEF (Raw)", Class::Raw), ("NEF+Fine", Class::Both), ("Large Fine JPEG", Class::Jpeg),
                            ("RAW", Class::Raw), ("cRAW", Class::Raw), ("RAW + Large Fine JPEG", Class::Both), ("Smaller JPEG", Class::Jpeg),
                            ("Medium Normal", Class::Jpeg), ("TIFF", Class::Other)] {
        assert_eq!(classify(choice), class, "{choice}");
    }
}

#[test]
fn a_format_keeps_the_cameras_own_choice_when_it_makes_that_kind_and_else_takes_the_finest() {
    let mut caps = read_photo_caps(&mut Fake(d5100(true)));
    let q = caps.quality.as_mut().unwrap();
    assert_eq!(q.target(Format::RawJpeg).map(String::as_str), Some("NEF+Fine"), "already on one");
    assert_eq!(q.target(Format::Jpeg).map(String::as_str), Some("JPEG Fine"));
    assert_eq!(q.target(Format::Raw).map(String::as_str), Some("NEF (Raw)"));
    q.current = "JPEG Normal".into();
    assert_eq!(q.target(Format::Jpeg).map(String::as_str), Some("JPEG Normal"), "the maker's settings stay");
    // A Canon lists the largest first and says size in words
    let canon = Quality { name: "imageformat", writable: true, current: "RAW".into(), choices: ["Large Fine JPEG", "Large Normal JPEG", "Medium Fine JPEG", "Smaller JPEG", "RAW + Large Fine JPEG", "RAW"].map(String::from).to_vec() };
    assert_eq!(canon.target(Format::Jpeg).map(String::as_str), Some("Large Fine JPEG"));
    assert_eq!(canon.target(Format::RawJpeg).map(String::as_str), Some("RAW + Large Fine JPEG"));
    // Read-only: only what it is on
    let stuck = Quality { writable: false, ..canon };
    assert_eq!((stuck.target(Format::Jpeg), stuck.target(Format::Raw).map(String::as_str)), (None, Some("RAW")));
}

#[test]
fn shutter_speeds_read_as_cameras_show_them_and_bulb_isnt_offered() {
    for (raw, shown) in [("0,0250s", "1/40"), ("1/40", "1/40"), ("10/25", "0.4 s"), ("30", "30 s"), ("30,0000s", "30 s"), ("1,3000s", "1.3 s"), ("2", "2 s")] {
        assert_eq!(shutter_label(raw).as_deref(), Some(shown), "{raw}");
    }
    assert_eq!(shutter_label("Bulb"), None);
    let caps = read_photo_caps(&mut Fake(d5100(true)));
    let shutter = caps.shutter.unwrap();
    assert_eq!(shutter.values, ["0,0010s", "0,0250s", "1,0000s", "30,0000s"]);
    assert_eq!(shutter.labels, ["1/1000", "1/40", "1 s", "30 s"]);
    assert_eq!(shutter.current, Some(1));
}

#[test]
fn the_card_hears_the_cameras_own_lists() {
    let json = read_photo_caps(&mut Fake(d5100(true))).json();
    assert_eq!(json["formats"], json!(["jpeg", "raw"]));
    assert_eq!((json["camera"].clone(), json["burst_max"].clone()), (json!(true), json!(1)));
    assert_eq!(json["iso"], json!({"values": ["100", "200", "400", "800"], "labels": ["100", "200", "400", "800"], "current": 2}));
    assert_eq!(json["shutter"]["labels"], json!(["1/1000", "1/40", "1 s", "30 s"]));
}

#[test]
fn a_mode_without_iso_or_shutter_offers_neither() {
    let json = read_photo_caps(&mut Fake(d5100(false))).json();
    assert!(json.get("iso").is_none() && json.get("shutter").is_none(), "{json}");
    assert_eq!(json["formats"], json!(["jpeg", "raw"]));
}

// ── The shooting ─────────────────────────────────────────────────────────────

#[test]
fn a_photo_sets_what_was_asked_sends_the_picture_to_the_computer_and_brings_live_view_back() {
    let state = d5100(true);
    let shared = Shared::default();
    *shared.pending.lock().unwrap() = {
        let (reply, waiter) = handover();
        std::mem::forget(waiter);
        Some(Job { wish: Wish { format: Format::RawJpeg, iso: Some("800".into()), shutter: Some("1,0000s".into()), timer_s: 0 }, reply, late: None })
    };
    assert!(shared.run_pending(&mut Fake(state.clone())));
    assert_eq!(log(&state), ["viewfinder=0", "iso=800", "shutterspeed=1,0000s", "capturetarget=Internal RAM", "capture", "viewfinder=1"]);
    assert_eq!(state.lock().unwrap().live_view_at_capture.as_deref(), Some("0"), "live view was off for the shot");
    assert_eq!(state.lock().unwrap().expected, [2], "RAW and JPEG are two files");
    assert!(!shared.run_pending(&mut Fake(state)), "nothing left to do");
}

#[test]
fn a_jpeg_changes_the_quality_from_a_raw_one_and_waits_for_one_file() {
    let state = d5100(true);
    let shots = take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).unwrap();
    assert_eq!(shots.len(), 1);
    assert_eq!(log(&state), ["viewfinder=0", "imagequality=JPEG Fine", "capturetarget=Internal RAM", "capture"]);
    // Already on RAM and on a JPEG choice: nothing to change
    let again = take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(log(&state)[4..], ["viewfinder=0", "capture"]);
}

#[test]
fn a_mode_that_refuses_iso_fails_the_photo_before_the_shutter_opens() {
    let state = d5100(false);
    let asked = Wish { iso: Some("800".into()), ..wish(Format::Jpeg) };
    assert_eq!(take_photo(&mut Fake(state.clone()), &asked).unwrap_err(), Failure::Setting("ISO"));
    assert_eq!(state.lock().unwrap().captures, 0);
    // Without asking for one it takes the photo at whatever the camera is set to
    assert!(take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).is_ok());
}

#[test]
fn a_value_the_camera_doesnt_list_is_refused_and_a_refusal_from_the_camera_is_too() {
    let state = d5100(true);
    let off_list = Wish { iso: Some("51200".into()), ..wish(Format::Jpeg) };
    assert_eq!(take_photo(&mut Fake(state.clone()), &off_list).unwrap_err(), Failure::Setting("ISO"));
    // A dial turned since the list was read: the write is refused
    let mut rig = Fake(state.clone());
    let caps = read_photo_caps(&mut rig);
    state.lock().unwrap().settings.get_mut("iso").unwrap().0 = true;
    assert_eq!(put(&mut rig, caps.iso.as_ref(), "800", "ISO").unwrap_err(), Failure::Setting("ISO"));
    assert_eq!(state.lock().unwrap().captures, 0);
}

#[test]
fn a_camera_that_fails_or_is_busy_says_which() {
    let state = d5100(true);
    state.lock().unwrap().capture_error = Some("Could not capture".into());
    assert_eq!(take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).unwrap_err(), Failure::Camera("Could not capture".into()));
    state.lock().unwrap().capture_error = Some("PTP Device Busy".into());
    assert_eq!(take_photo(&mut Fake(state), &wish(Format::Jpeg)).unwrap_err(), Failure::Busy);
}

#[test]
fn a_camera_on_bulb_has_to_be_given_a_speed() {
    let state = d5100(true);
    state.lock().unwrap().settings.get_mut("shutterspeed").unwrap().2 = "Bulb".into();
    assert_eq!(take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).unwrap_err(), Failure::Bulb);
    assert_eq!(state.lock().unwrap().captures, 0);
    let asked = Wish { shutter: Some("0,0250s".into()), ..wish(Format::Jpeg) };
    assert!(take_photo(&mut Fake(state), &asked).is_ok());
}

#[test]
fn a_camera_with_no_raw_cant_be_asked_for_one() {
    let state = d5100(true);
    state.lock().unwrap().settings.insert("imagequality".into(), setting(false, &["JPEG Normal", "JPEG Fine"], "JPEG Fine"));
    assert_eq!(read_photo_caps(&mut Fake(state.clone())).json()["formats"], json!(["jpeg"]));
    assert_eq!(take_photo(&mut Fake(state), &wish(Format::Raw)).unwrap_err(), Failure::Unsupported);
}

#[test]
fn the_cards_options_become_a_wish() {
    assert_eq!(Wish::from_options(&json!({"format": "jpeg", "rotation": 0, "quality": 95})), wish(Format::Jpeg));
    assert_eq!(Wish::from_options(&json!({"format": "none", "raw": 1})).format, Format::Raw);
    assert_eq!(Wish::from_options(&json!({"format": "jpeg", "raw": 1, "quality": 90})).format, Format::RawJpeg);
    let w = Wish::from_options(&json!({"format": "jpeg", "iso": "800", "shutter": "1/60", "timer_s": 10}));
    assert_eq!((w.iso.as_deref(), w.shutter.as_deref(), w.timer_s), (Some("800"), Some("1/60"), 10));
    assert_eq!(Wish::from_options(&json!({"timer_s": 9999})).timer_s, 60);
}

// ── Through the camera's thread ──────────────────────────────────────────────

fn fast() -> Timing {
    Timing { restart: Duration::from_millis(5), restart_max: Duration::from_millis(20), give_up_runs: 5, silent: Duration::from_secs(5), grace: Duration::from_secs(1) }
}

fn connecting(state: &Arc<Mutex<State>>) -> Connect {
    let state = state.clone();
    Box::new(move |_| {
        state.lock().unwrap().closed = false;
        Ok(Box::new(Fake(state.clone())) as Box<dyn Rig>)
    })
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-dslr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Starts a photo and gives (the progress states heard, the end) once it's over.
struct Taken {
    states: Arc<Mutex<Vec<Value>>>,
    done: std::sync::mpsc::Receiver<Value>,
}

fn take(stills: &GphotoStills, options: Value, folder: &Path, alive: Arc<AtomicBool>) -> Option<Taken> {
    let states = Arc::new(Mutex::new(Vec::new()));
    let (tx, done) = channel();
    let heard = states.clone();
    let started = stills.take(&options, &folder.display().to_string(), Box::new(move |p| heard.lock().unwrap().push(p)),
                              Box::new(move |r| tx.send(r).unwrap()), Box::new(move || alive.load(Ordering::SeqCst)));
    started.then_some(Taken { states, done })
}

fn yes() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

/// Reads frames while a photo is taken, since the stream's reader is what keeps the camera's thread running on its own.
fn read_until_done(cap: &mut Box<dyn crate::webcam_capture::Capture>, taken: &Taken) -> Value {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(done) = taken.done.try_recv() {
            return done;
        }
        assert!(Instant::now() < until, "the photo never ended");
        cap.read().expect("the live view carries on");
    }
}

#[test]
fn a_photo_is_taken_saved_and_the_live_view_comes_back() {
    let state = d5100(true);
    let mut cap = open_with("gphoto2:usb:S1", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S1");
    wait_for("caps", || stills.caps().is_some());
    assert!(!stills.busy());
    let dir = folder("saved");
    let taken = take(&stills, json!({"format": "jpeg", "raw": 1, "rotation": 0, "iso": "800", "shutter": "1,0000s"}), &dir, yes()).expect("starts");
    assert!(stills.busy(), "pressed twice is refused");
    assert!(take(&stills, json!({}), &dir, yes()).is_none());
    let before = state.lock().unwrap().previews;
    let done = read_until_done(&mut cap, &taken);
    assert_eq!(done["ok"], json!(true), "{done}");
    let files: Vec<String> = done["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    assert_eq!(files.len(), 2);
    let exts: Vec<&str> = files.iter().map(|f| Path::new(f).extension().unwrap().to_str().unwrap()).collect();
    assert_eq!(exts, ["nef", "jpg"], "named after the camera's own kinds, in lower case");
    assert_eq!(Path::new(&files[0]).file_stem(), Path::new(&files[1]).file_stem(), "a RAW and its JPEG share a name");
    assert_eq!(std::fs::read(&files[0]).unwrap(), vec![1; 8]);
    assert!(Path::new(&files[0]).file_name().unwrap().to_str().unwrap().starts_with("Telescope_"));
    assert!(taken.states.lock().unwrap().iter().any(|p| p["state"] == "capturing" && p["paused"] == true));
    wait_for("the busy flag to drop", || !stills.busy());
    // Live view came back and frames keep arriving, and the camera was left to shoot with the settings that were asked
    wait_for("frames again", || state.lock().unwrap().previews > before + 3);
    assert_eq!(state.lock().unwrap().settings["iso"].2, "800");
    assert_eq!(state.lock().unwrap().settings["viewfinder"].2, "1");
    drop(cap);
    assert!(state.lock().unwrap().closed);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_photo_the_camera_cant_take_ends_with_why_and_live_view_carries_on() {
    let state = d5100(true);
    state.lock().unwrap().capture_error = Some("Could not capture".into());
    let mut cap = open_with("gphoto2:usb:S2", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S2");
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("failed");
    let taken = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!((done["ok"].clone(), done["code"].clone(), done["files"].clone()), (json!(false), json!("failed"), json!([])));
    assert_eq!(done["error"], json!("The camera couldn't take the photo."));
    wait_for("the busy flag to drop", || !stills.busy());
    let before = state.lock().unwrap().previews;
    wait_for("frames again", || state.lock().unwrap().previews > before + 3);
    // A busy camera is worded as that, and can be tried again
    state.lock().unwrap().capture_error = Some("PTP Device Busy".into());
    let busy = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    assert_eq!(read_until_done(&mut cap, &busy)["error"], json!("The camera is busy. Try again in a moment."));
    wait_for("the busy flag to drop", || !stills.busy());
    state.lock().unwrap().capture_error = None;
    let again = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    assert_eq!(read_until_done(&mut cap, &again)["ok"], json!(true));
    drop(cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_long_exposure_doesnt_make_a_silent_camera() {
    let state = d5100(true);
    state.lock().unwrap().capture_takes = Duration::from_millis(900);
    let timing = Timing { silent: Duration::from_millis(300), ..fast() };
    let mut cap = open_with("gphoto2:usb:S3", timing, connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S3");
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("long");
    let taken = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    // Three times as long as the camera may stay silent, and the read still waits instead of ending the stream
    let done = read_until_done(&mut cap, &taken);
    assert_eq!(done["ok"], json!(true), "{done}");
    cap.read().expect("frames after it");
    drop(cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nothing_is_offered_or_taken_while_the_camera_isnt_streaming() {
    let stills = GphotoStills::new("gphoto2:usb:NOPE");
    assert!(stills.caps().is_none() && !stills.busy());
    assert!(take(&stills, json!({}), &folder("none"), yes()).is_none());
}

#[test]
fn a_turn_of_the_dial_and_the_end_of_the_stream_are_heard() {
    let state = d5100(true);
    let stills = GphotoStills::new("gphoto2:usb:S4");
    let heard = Arc::new(AtomicUsize::new(0));
    let counter = heard.clone();
    stills.set_on_change(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let mut cap = open_with("gphoto2:usb:S4", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    wait_for("the first caps", || heard.load(Ordering::SeqCst) >= 1 && stills.caps().is_some());
    assert!(stills.caps().unwrap().get("iso").is_some());
    let before = heard.load(Ordering::SeqCst);
    // The dial goes to a scene mode: ISO and shutter speed go read-only
    for name in ["iso", "shutterspeed", "shutterspeed2"] {
        state.lock().unwrap().settings.get_mut(name).unwrap().0 = true;
    }
    wait_for("the change", || stills.caps().is_some_and(|c| c.get("iso").is_none()));
    assert!(heard.load(Ordering::SeqCst) > before);
    let before = heard.load(Ordering::SeqCst);
    drop(cap);
    assert!(heard.load(Ordering::SeqCst) > before, "letting go is a change too");
    assert!(stills.caps().is_none());
}

#[test]
fn a_self_timer_counts_down_before_the_shutter_and_a_stream_that_stops_ends_it() {
    let state = d5100(true);
    let mut cap = open_with("gphoto2:usb:S5", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S5");
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("timer");
    let taken = take(&stills, json!({"format": "jpeg", "timer_s": 1}), &dir, yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!(done["ok"], json!(true), "{done}");
    let heard = taken.states.lock().unwrap().clone();
    assert_eq!(heard[0]["state"], json!("waiting"));
    assert!(heard[0]["countdown_ms"].as_u64().unwrap() > 0);
    assert!(heard.iter().position(|p| p["state"] == "capturing") > Some(0));
    wait_for("the busy flag to drop", || !stills.busy());
    // Stopped in the middle of counting
    let alive = yes();
    let cut = take(&stills, json!({"format": "jpeg", "timer_s": 30}), &dir, alive.clone()).unwrap();
    let captures = state.lock().unwrap().captures;
    alive.store(false, Ordering::SeqCst);
    let done = cut.done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((done["ok"].clone(), done["code"].clone()), (json!(false), json!("interrupted")));
    assert_eq!(state.lock().unwrap().captures, captures, "the shutter never opened");
    drop(cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_photo_that_cant_be_saved_says_so_and_keeps_what_was_saved() {
    let state = d5100(true);
    let mut cap = open_with("gphoto2:usb:S6", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S6");
    wait_for("caps", || stills.caps().is_some());
    // A file where the folder should be
    let blocker = folder("blocked");
    std::fs::write(&blocker, b"x").unwrap();
    let taken = take(&stills, json!({"format": "jpeg"}), &blocker.join("inside"), yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!((done["ok"].clone(), done["code"].clone(), done["error"].clone()), (json!(false), json!("save"), json!("Telescope couldn't save the photo.")));
    drop(cap);
    let _ = std::fs::remove_file(blocker);
}

#[test]
fn two_photos_in_the_same_second_dont_overwrite_each_other() {
    let dir = folder("names");
    std::fs::create_dir_all(&dir).unwrap();
    let shots = vec![Shot { name: "a.JPG".into(), data: vec![1] }];
    let first = save(&dir, &shots).unwrap();
    let second = save(&dir, &shots).unwrap();
    // Same second or not, the second never takes the first's name
    assert_ne!(first, second);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_raw_and_jpeg_photo_with_a_file_missing_says_so_and_keeps_the_file_that_came() {
    let state = d5100(true);
    state.lock().unwrap().partial = true;
    let mut cap = open_with("gphoto2:usb:S9", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:S9");
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("partial");
    let taken = take(&stills, json!({"format": "jpeg", "raw": 1, "rotation": 0}), &dir, yes()).expect("starts");
    let done = read_until_done(&mut cap, &taken);
    assert_eq!(done["ok"], json!(false), "{done}");
    assert_eq!(done["code"], json!("partial"));
    let files = done["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "the file that came is saved");
    assert_eq!(std::fs::read(files[0].as_str().unwrap()).unwrap(), vec![1; 8]);
    wait_for("the busy flag to drop", || !stills.busy());
    assert_eq!(state.lock().unwrap().log.last().map(String::as_str), Some("viewfinder=1"), "live view is back");
}

fn shot(n: u8) -> Shot {
    Shot { name: format!("DSC_{n}.JPG"), data: vec![n] }
}

#[test]
fn waiting_for_the_second_file_ends_at_the_deadline_however_busy_the_camera_is() {
    // Events of another kind every time, and the second file never comes: a count of quiet moments would never get to the end
    let started = Instant::now();
    let got = gather(vec![shot(1)], 2, Duration::from_millis(120), || {
        std::thread::sleep(Duration::from_millis(5));
        Ok(Said::<u8>::Other)
    }, |_| Ok(shot(2)));
    let err = got.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(err.shots, [shot(1)], "the first file is kept");
    assert!(err.why.contains("1 of 2"), "{}", err.why);
}

#[test]
fn the_second_file_is_fetched_when_the_camera_says_it_is_there() {
    let mut said = vec![Said::Other, Said::Quiet, Said::File(2u8)].into_iter();
    let got = gather(vec![shot(1)], 2, Duration::from_secs(1), || Ok(said.next().unwrap()), |n| Ok(shot(n))).unwrap();
    assert_eq!(got, [shot(1), shot(2)]);
    // One file asked for needs no waiting at all
    assert_eq!(gather(vec![shot(1)], 1, Duration::ZERO, || -> Result<Said<u8>, String> { panic!("no listening") }, |_| Ok(shot(2))).unwrap().len(), 1);
}

#[test]
fn a_second_file_that_fails_to_come_down_or_a_camera_done_early_is_an_incomplete_photo() {
    let fetch_fails = gather(vec![shot(1)], 2, Duration::from_secs(1), || Ok(Said::File(2u8)), |_| Err("download failed".into())).unwrap_err();
    assert_eq!((fetch_fails.shots.len(), fetch_fails.why.contains("download failed")), (1, true));
    let done_early = gather(vec![shot(1)], 2, Duration::from_secs(1), || Ok(Said::<u8>::Done), |_| Ok(shot(2))).unwrap_err();
    assert_eq!(done_early.shots.len(), 1);
    let lost = gather(vec![shot(1)], 2, Duration::from_secs(1), || Err::<Said<u8>, _>("gone".into()), |_| Ok(shot(2))).unwrap_err();
    assert_eq!(lost.shots.len(), 1);
}

#[test]
fn a_service_that_follows_its_camera_is_told_where_the_camera_is_now_and_not_where_it_was() {
    // The camera streams on a port, and the service for it is asked to listen while its device says another one
    let state = d5100(true);
    let location = Arc::new(Mutex::new("gphoto2:usb:F1".to_string()));
    let here = location.clone();
    let stills = GphotoStills::following(Arc::new(move || Some(here.lock().unwrap().clone())));
    let heard = Arc::new(AtomicUsize::new(0));
    let counter = heard.clone();
    stills.set_on_change(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(stills.caps().is_none(), "nothing streams on F1");
    let mut cap = open_with("gphoto2:usb:F2", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    assert_eq!(heard.load(Ordering::SeqCst), 0, "F2 isn't its camera's port yet");
    // The device moves to F2 (the camera came back there): the same service now has its camera, and hears what it does
    *location.lock().unwrap() = "gphoto2:usb:F2".into();
    assert!(stills.caps().is_some(), "found by where the device is now");
    notify_changed("usb:F2");
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    notify_changed("usb:F1");
    assert_eq!(heard.load(Ordering::SeqCst), 1, "a camera on another port isn't its business");
    // A service that is gone isn't kept: the camera ending tells nobody
    drop(stills);
    drop(cap);
    notify_changed("usb:F2");
    assert_eq!(heard.load(Ordering::SeqCst), 1);
}

// ── A long exposure, a photo that comes late and a quality the camera has locked ──

#[test]
fn the_wait_for_a_photo_follows_the_exposure() {
    let base = Duration::from_secs(240);
    assert_eq!(wait_for_photo(base, None), base);
    assert_eq!(wait_for_photo(base, shutter_seconds("1/40")), base, "an ordinary photo waits as it always did");
    assert_eq!(wait_for_photo(base, shutter_seconds("30,0000s")), base, "30 s with noise reduction fits");
    assert_eq!(wait_for_photo(base, shutter_seconds("300")), Duration::from_secs(660), "300 s is not cut off at 240");
    assert_eq!(wait_for_photo(base, shutter_seconds("900,0000s")), Duration::from_secs(1860));
    assert_eq!(wait_for_photo(base, shutter_seconds("Bulb")), base);
}

#[test]
fn the_exposure_is_the_speed_asked_for_else_the_one_the_camera_is_on() {
    let state = d5100(true);
    state.lock().unwrap().settings.insert("shutterspeed".into(), setting(false, &["0,0250s", "30,0000s", "300,0000s"], "300,0000s"));
    let shared = Shared::default();
    shared.set_caps(Some(read_photo_caps(&mut Fake(state))));
    assert_eq!(exposure_of(&shared, &wish(Format::Jpeg)), Some(300.0), "what the camera is on");
    assert_eq!(exposure_of(&shared, &Wish { shutter: Some("0,0250s".into()), ..wish(Format::Jpeg) }), Some(0.025));
    assert_eq!(exposure_of(&Shared::default(), &wish(Format::Jpeg)), None);
}

#[test]
fn a_photo_that_comes_after_the_wait_is_saved_and_the_card_is_told_it_was_late() {
    let state = d5100(true);
    state.lock().unwrap().capture_takes = Duration::from_millis(700);
    let mut cap = open_with("gphoto2:usb:L1", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let mut stills = GphotoStills::new("gphoto2:usb:L1");
    stills.longest = Duration::from_millis(100);
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("late");
    let taken = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!((done["ok"].clone(), done["code"].clone()), (json!(false), json!("timeout")), "{done}");
    assert!(done["error"].as_str().unwrap().contains("saved"), "it says the photo is kept if it still comes: {done}");
    // The camera finishes after that, and the files are not thrown away
    let until = Instant::now() + Duration::from_secs(10);
    let late = loop {
        if let Some(late) = taken.states.lock().unwrap().iter().find(|p| p["state"] == "late").cloned() {
            break late;
        }
        assert!(Instant::now() < until, "the late photo was never reported");
        cap.read().expect("the live view carries on");
    };
    let files: Vec<String> = late["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read(&files[0]).unwrap(), vec![1; 8], "the camera's file is on disk");
    drop(cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_photo_that_comes_late_with_a_file_missing_still_keeps_the_file_that_came() {
    let state = d5100(true);
    {
        let mut s = state.lock().unwrap();
        s.capture_takes = Duration::from_millis(700);
        s.partial = true;
    }
    let mut cap = open_with("gphoto2:usb:L2", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let mut stills = GphotoStills::new("gphoto2:usb:L2");
    stills.longest = Duration::from_millis(100);
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("late-partial");
    let taken = take(&stills, json!({"format": "jpeg", "raw": 1}), &dir, yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!(done["code"], json!("timeout"));
    let until = Instant::now() + Duration::from_secs(10);
    while !taken.states.lock().unwrap().iter().any(|p| p["state"] == "late") {
        assert!(Instant::now() < until, "the late file was never reported");
        cap.read().expect("the live view carries on");
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    drop(cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_photo_that_finishes_in_time_is_not_reported_late() {
    let shared = Shared::default();
    let (reply, waiter) = handover();
    let told = Arc::new(Mutex::new(0));
    let counted = told.clone();
    *shared.pending.lock().unwrap() = Some(Job { wish: wish(Format::Jpeg), reply, late: Some(Arc::new(move |_| *counted.lock().unwrap() += 1)) });
    assert!(shared.run_pending(&mut Fake(d5100(true))));
    assert!(matches!(waiter.wait(Duration::ZERO), Wait::Got(Ok(_))));
    assert_eq!(*told.lock().unwrap(), 0);
    // With nobody waiting any more, it is
    let (reply, waiter) = handover();
    drop(waiter);
    let counted = told.clone();
    *shared.pending.lock().unwrap() = Some(Job { wish: wish(Format::Jpeg), reply, late: Some(Arc::new(move |_| *counted.lock().unwrap() += 1)) });
    assert!(shared.run_pending(&mut Fake(d5100(true))));
    assert_eq!(*told.lock().unwrap(), 1);
}

#[test]
fn a_photo_is_saved_once_whichever_moment_it_comes_at_the_end_of_the_wait() {
    let shots = || vec![Shot { name: "a.jpg".into(), data: vec![1] }];
    // Sent just before the job gives up: the job still gets it
    let (reply, waiter) = handover();
    assert!(reply.send(Ok(shots())).is_ok());
    assert!(matches!(waiter.abandon(), Some(Ok(_))));
    assert!(waiter.abandon().is_none(), "and only once");
    // Sent after: it goes back to the camera's side to be saved late
    let (reply, waiter) = handover();
    assert!(waiter.abandon().is_none());
    assert!(reply.send(Ok(shots())).is_err());
    // A camera thread that ends without an answer is not a photo
    let (reply, waiter) = handover();
    drop(reply);
    assert!(matches!(waiter.wait(Duration::ZERO), Wait::Closed));
    assert!(waiter.abandon().is_none());
}

#[test]
fn a_waiting_job_wakes_when_the_photo_comes() {
    let (reply, waiter) = handover();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let _ = reply.send(Ok(Vec::new()));
    });
    assert!(matches!(waiter.wait(Duration::from_secs(10)), Wait::Got(Ok(_))));
    sender.join().unwrap();
}

#[test]
fn a_jpeg_is_refused_when_the_quality_is_locked_to_raw_and_nothing_is_shot() {
    let quality = ["JPEG Fine", "NEF (Raw)", "NEF+Fine"];
    for (locked_to, class) in [("NEF (Raw)", Class::Raw), ("NEF+Fine", Class::Both)] {
        let state = d5100(true);
        state.lock().unwrap().settings.insert("imagequality".into(), setting(true, &quality, locked_to));
        let err = take_photo(&mut Fake(state.clone()), &wish(Format::Jpeg)).unwrap_err();
        assert_eq!(err, Failure::Locked(class), "{locked_to}");
        assert_eq!(state.lock().unwrap().captures, 0, "the shutter never opened");
        assert_eq!(err.code(), "unsupported");
        assert!(err.text().contains("locked to RAW"), "{}", err.text());
    }
    // What it is locked to is what can be asked for
    let state = d5100(true);
    state.lock().unwrap().settings.insert("imagequality".into(), setting(true, &quality, "NEF (Raw)"));
    assert_eq!(take_photo(&mut Fake(state.clone()), &wish(Format::Raw)).unwrap().len(), 1);
    // A locked JPEG setting takes a JPEG
    let state = d5100(true);
    state.lock().unwrap().settings.insert("imagequality".into(), setting(true, &quality, "JPEG Fine"));
    assert_eq!(take_photo(&mut Fake(state), &wish(Format::Jpeg)).unwrap().len(), 1);
}

#[test]
fn a_locked_camera_ending_a_photo_through_the_service_says_why() {
    let state = d5100(true);
    state.lock().unwrap().settings.insert("imagequality".into(), setting(true, &["JPEG Fine", "NEF (Raw)"], "NEF (Raw)"));
    let mut cap = open_with("gphoto2:usb:L3", fast(), connecting(&state)).ok().unwrap();
    cap.read().unwrap();
    let stills = GphotoStills::new("gphoto2:usb:L3");
    wait_for("caps", || stills.caps().is_some());
    let dir = folder("locked");
    let taken = take(&stills, json!({"format": "jpeg"}), &dir, yes()).unwrap();
    let done = read_until_done(&mut cap, &taken);
    assert_eq!((done["ok"].clone(), done["code"].clone()), (json!(false), json!("unsupported")), "{done}");
    assert!(done["error"].as_str().unwrap().contains("locked to RAW"));
    assert_eq!(done["files"], json!([]), "nothing saved that wasn't asked for");
    drop(cap);
}
