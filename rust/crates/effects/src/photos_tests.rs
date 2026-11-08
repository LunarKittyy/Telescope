use super::*;
use telescope_core::testing::FakeHost;

fn caps(v: Value) -> StillCaps {
    serde_json::from_value(v).unwrap()
}

fn plain() -> StillCaps {
    caps(json!({"size": [1920, 1080]})) // an emulator: JPEG and nothing else
}

fn full_json() -> Value {
    json!({"formats": ["jpeg", "heic", "ultrahdr", "raw"], "size": [4080, 3072], "max_res": [8160, 6144], "max_res_raw": true,
           "extensions": ["night", "hdr", "bokeh"], "exposure_max_ns": 30_000_000_000i64, "bracketing": true, "focus_bracketing": true,
           "noise_reduction_hq": true, "sharpening_hq": true, "hot_pixel_hq": true, "distortion": true, "burst_max": 60})
}

fn full() -> StillCaps {
    caps(full_json())
}

fn choices(changes: Value) -> Choices {
    let mut c = defaults();
    c.extend(changes.as_object().unwrap().clone());
    c
}

// ── What's offered ───────────────────────────────────────────────────────────

#[test]
fn a_plain_lens_offers_only_a_single_jpeg() {
    let can = offered(&plain(), &choices(json!({})));
    assert_eq!((can.formats, can.modes, can.shots, can.exposures), (vec!["jpeg"], vec!["normal"], vec!["single"], vec![0.0]));
}

#[test]
fn a_flagship_offers_everything_it_has() {
    let can = offered(&full(), &choices(json!({})));
    assert_eq!(can.formats, ["jpeg", "heic", "ultrahdr", "raw", "raw+jpeg"]);
    assert_eq!(can.modes, ["normal", "max_res", "night", "hdr", "bokeh"]);
    assert_eq!(can.shots, ["single", "burst", "bracket", "focus"]);
    assert!(can.exposures.last() == Some(&30.0) && !can.exposures.contains(&60.0));
}

#[test]
fn full_resolution_only_saves_jpeg_and_raw() {
    assert_eq!(offered(&full(), &choices(json!({"mode": "max_res"}))).formats, ["jpeg", "raw", "raw+jpeg"]);
    let no_raw = caps(json!({"formats": ["jpeg", "raw"], "max_res": [8000, 6000], "max_res_raw": false}));
    assert_eq!(offered(&no_raw, &choices(json!({"mode": "max_res"}))).formats, ["jpeg"]);
}

#[test]
fn the_makers_modes_take_jpeg_singles_or_bursts_at_their_own_exposure() {
    let can = offered(&full(), &choices(json!({"mode": "night"})));
    assert_eq!((can.formats, can.shots, can.exposures), (vec!["jpeg"], vec!["single", "burst"], vec![0.0]));
}

#[test]
fn a_mode_this_lens_lacks_falls_back_to_normal() {
    assert_eq!(offered(&plain(), &choices(json!({"mode": "night", "format": "raw"}))).formats, ["jpeg"]);
}

#[test]
fn bracketing_has_no_long_exposure_of_its_own() {
    assert_eq!(offered(&full(), &choices(json!({"shots": "bracket"}))).exposures, [0.0]);
}

// ── The options sent ─────────────────────────────────────────────────────────

#[test]
fn a_plain_jpeg() {
    assert_eq!(build_options(&plain(), &choices(json!({"quality": 88})), 0), json!({"format": "jpeg", "rotation": 0, "quality": 88}));
}

#[test]
fn picks_a_lens_cant_do_are_left_out_rather_than_sent() {
    let picks = choices(json!({"format": "raw+jpeg", "mode": "night", "shots": "bracket", "exposure_s": 8, "distortion": "off"}));
    assert_eq!(build_options(&plain(), &picks, 0), json!({"format": "jpeg", "rotation": 0, "quality": 95}));
}

#[test]
fn raw_alone_has_no_quality() {
    let opts = build_options(&full(), &choices(json!({"format": "raw"})), 0);
    assert!(opts["format"] == "none" && opts["raw"] == 1 && opts.get("quality").is_none());
}

#[test]
fn raw_and_jpeg_at_full_resolution_with_a_long_exposure_and_the_best_processing() {
    let picks = choices(json!({"format": "raw+jpeg", "mode": "max_res", "exposure_s": 4, "timer": 10, "distortion": "off"}));
    assert_eq!(build_options(&full(), &picks, 90), json!({
        "format": "jpeg", "raw": 1, "quality": 95, "max_res": 1, "exposure_ns": 4_000_000_000i64, "timer_s": 10, "nr_hq": 1, "edge_hq": 1,
        "hot_pixel_hq": 1, "distortion": 0, "rotation": 90,
    }));
}

#[test]
fn night_mode_bursts() {
    let picks = choices(json!({"mode": "night", "shots": "burst", "burst": 3, "best": true, "distortion": "on"}));
    assert_eq!(build_options(&full(), &picks, 0), json!({"format": "jpeg", "quality": 95, "extension": "night", "count": 3, "rotation": 0}));
}

#[test]
fn brackets_and_focus_steps() {
    assert_eq!(build_options(&full(), &choices(json!({"shots": "bracket", "bracket": "5x1"})), 0)["bracket_ev"], "-2,-1,0,1,2");
    assert_eq!(build_options(&full(), &choices(json!({"shots": "focus", "focus_steps": 8})), 0)["focus_steps"], 8);
    let few = caps(json!({"burst_max": 4, "focus_bracketing": true}));
    assert_eq!(build_options(&few, &choices(json!({"shots": "focus", "focus_steps": 8})), 0)["focus_steps"], 4);
    assert_eq!(build_options(&full(), &choices(json!({"shots": "bracket", "bracket": "nope"})), 0)["bracket_ev"], "-1,0,1");
}

// ── The card ─────────────────────────────────────────────────────────────────

struct Rig {
    plugin: PhotosPlugin,
    host: FakeHost,
}

impl Rig {
    fn new(caps: Option<Value>, streaming: bool) -> Rig {
        let mut rig = Rig { plugin: PhotosPlugin::new(), host: FakeHost::new() };
        rig.host.still_caps = caps;
        rig.host.streaming = streaming;
        rig.plugin.setup(&mut rig.host);
        rig.event(Event::PhonesChanged { count: 1 });
        rig.plugin.set_config(&json!({"folder": "/tmp/photos"})).unwrap();
        rig.event(Event::PhoneStateUpdated { state: json!({}) });
        rig
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    /// The card's properties as everything said about them leaves them.
    fn props(&self) -> Value {
        let mut all = Map::new();
        for call in self.host.calls.iter().filter(|c| c.starts_with("set_ui_props card ")) {
            let Value::Object(props) = serde_json::from_str::<Value>(&call["set_ui_props card ".len()..]).unwrap() else { panic!() };
            all.extend(props);
        }
        Value::Object(all)
    }
}

/// A control as the card shows it: (its list, where it is in it), or None when the card has nothing to pick there (it is greyed out).
fn tile(props: &Value, key: &str) -> Option<(Vec<String>, i64)> {
    let (prefix, there) = match key {
        "iso" | "shutter" => (key, !props[format!("{key}-options")].as_array()?.is_empty()),
        "exposure_s" => ("exposure", props["exposure-options"].as_array()?.len() > 1),
        "mode" | "format" => (key, props[format!("{key}-enabled")] == true),
        "distortion" => (key, props["distortion-enabled"] == true),
        "burst" | "focus_steps" | "bracket" => ("slot", props["slot-key"] == key),
        _ => return None,
    };
    if !there {
        return None;
    }
    let list = props[format!("{prefix}-options")].as_array()?.iter().map(|o| o.as_str().unwrap().to_string()).collect();
    Some((list, props[format!("{prefix}-index")].as_i64()?))
}

/// The controls with something to pick, in the order the card has them.
fn keys(props: &Value) -> Vec<String> {
    let offered = props["shots-offered"].as_array().map_or(0, |s| s.iter().filter(|o| **o == true).count());
    let slot = props["slot-key"].as_str().unwrap_or("");
    ["iso", "shutter", "mode", "format", "shots", "burst", "focus_steps", "bracket", "exposure_s", "distortion"]
        .into_iter()
        .filter(|key| match *key {
            "shots" => offered > 1,
            "burst" | "focus_steps" | "bracket" => slot == *key,
            _ => tile(props, key).is_some(),
        })
        .map(String::from)
        .collect()
}

#[test]
fn without_photos_the_button_is_off_and_the_card_says_why() {
    let mut rig = Rig::new(None, false);
    assert_eq!(rig.props()["take-enabled"], false);
    assert!(rig.props()["take-tip"].as_str().unwrap().contains("Start streaming"));
    rig.host.streaming = true;
    rig.caps_changed();
    assert!(rig.props()["take-tip"].as_str().unwrap().contains("Update the app"), "the reason is the disabled button's tooltip, not a sentence on the card");
}

impl Rig {
    fn caps_changed(&mut self) {
        self.event(Event::PhoneStateUpdated { state: json!({}) });
    }
}

#[test]
fn the_card_shows_only_what_the_lens_has() {
    let mut rig = Rig::new(Some(json!({"size": [1920, 1080]})), true);
    let p = rig.props();
    assert!(p["take-enabled"] == true);
    assert!(keys(&p).is_empty() && p["best-enabled"] == false, "a plain lens has the timer, the folder and nothing else to pick");
    assert_eq!((p["mode-options"].clone(), p["format-options"].clone()), (json!(["Normal"]), json!(["JPEG"])), "greyed out in their places");
    assert_eq!(p["shots-offered"], json!([true, false, false, false]));
    rig.host.still_caps = Some(full_json());
    rig.caps_changed();
    let p = rig.props();
    assert_eq!((tile(&p, "format").unwrap().0.len(), tile(&p, "mode").unwrap().0.len()), (5, 5));
    assert_eq!(keys(&p), ["mode", "format", "shots", "exposure_s", "distortion"]);
}

#[test]
fn diagnostics_give_each_modes_own_size() {
    let mut with_night = full_json();
    with_night["extensions"] = json!(["night"]);
    with_night["extension_sizes"] = json!([["night", [3840, 2160]]]);
    let rig = Rig::new(Some(with_night), true);
    let d: std::collections::HashMap<_, _> = rig.plugin.diagnostics().into_iter().collect();
    assert_eq!(d["Photo sizes"], "normal 4080x3072, max_res 8160x6144, night 3840x2160");
    assert_eq!(d["Photos"], "jpeg, heic, ultrahdr, raw, full resolution, night");
    assert_eq!(Rig::new(None, false).plugin.diagnostics(), [("Photos".to_string(), "not available".to_string())]);
}

#[test]
fn a_lens_switch_brings_that_lenss_options() {
    let mut rig = Rig::new(Some(full_json()), true);
    assert!(tile(&rig.props(), "mode").unwrap().0.contains(&"Full resolution".to_string()));
    rig.host.still_caps = Some(json!({"size": [1000, 1000]}));
    rig.event(Event::CameraSwitched { camera: json!({"id": "4"}) });
    assert!(keys(&rig.props()).is_empty());
}

#[test]
fn picking_a_night_mode_hides_what_it_cant_do() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("tile-picked", vec![json!("mode"), json!(2)]);
    let p = rig.props();
    assert!(tile(&p, "format").is_none() && tile(&p, "exposure_s").is_none() && p["best-enabled"] == false);
    assert_eq!(tile(&p, "shots").unwrap().0, ["Single", "Burst"]);
    assert_eq!(rig.plugin.get_config()["mode"], "night");
}

#[test]
fn a_pick_for_another_lens_is_kept_when_this_one_cant_do_it() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("tile-picked", vec![json!("format"), json!(3)]);
    assert_eq!(rig.plugin.get_config()["format"], "raw");
    rig.host.still_caps = Some(json!({"size": [1000, 1000]}));
    rig.caps_changed();
    assert!(tile(&rig.props(), "format").is_none());
    rig.host.still_caps = Some(full_json());
    rig.caps_changed();
    let p = rig.props();
    assert_eq!((tile(&p, "format").map(|t| t.1), rig.plugin.get_config()["format"].as_str()), (Some(3), Some("raw")));
}

#[test]
fn take_photo_sends_the_options_with_the_views_rotation() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.host.view.rotation = Some(270);
    rig.ui("take-clicked", vec![]);
    let (options, folder) = rig.host.stills_taken[0].clone();
    assert!(folder == "/tmp/photos" && options["rotation"] == 270 && options["format"] == "jpeg");
    let id = *rig.plugin.taking.last().unwrap();
    rig.event(Event::RequestProgress { request: id, data: json!({"state": "waiting", "countdown_ms": 2400}) });
    assert!(rig.props()["status"].as_str().unwrap().contains("3 s"));
    rig.event(Event::RequestProgress { request: id, data: json!({"state": "capturing", "shots": 5, "taken": 1}) });
    assert!(rig.props()["status"].as_str().unwrap().contains("2 of 5"));
    rig.event(Event::RequestProgress { request: id, data: json!({"state": "capturing", "paused": true}) });
    assert_eq!(rig.props()["status"], "Taking the photo, the video is paused…");
    rig.event(Event::RequestDone { request: id, ok: true, message: String::new(), data: json!({"ok": true, "files": ["/tmp/photos/IMG_1.jpg", "/tmp/photos/IMG_1.dng"]}) });
    let p = rig.props();
    assert!(p["status"].as_str().unwrap().contains("IMG_1.jpg") && p["status"].as_str().unwrap().contains("1 more"));
    assert_eq!((p["status-kind"].as_str(), p["open-enabled"].as_bool()), (Some("status_ok"), Some(true)));
    // Taken from a shortcut with another app in front
    assert!(rig.host.calls.iter().any(|c| c == "send_notification Photo saved"));
}

#[test]
fn a_photo_taken_with_the_window_in_front_sends_no_notification() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.host.window_active = true;
    rig.ui("take-clicked", vec![]);
    let id = *rig.plugin.taking.last().unwrap();
    rig.event(Event::RequestDone { request: id, ok: true, message: String::new(), data: json!({"ok": true, "files": ["/tmp/photos/IMG_1.jpg"]}) });
    assert_eq!(rig.props()["status"], "Saved IMG_1.jpg");
    assert!(!rig.host.calls.iter().any(|c| c.starts_with("send_notification")));
}

// The card's half; app/tests/cards2_parity.rs runs it from the phone's error code
#[test]
fn a_failed_photo_shows_why() {
    let mut rig = Rig::new(Some(json!({"size": [1920, 1080]})), true);
    rig.ui("take-clicked", vec![]);
    let id = *rig.plugin.taking.last().unwrap();
    rig.event(Event::RequestDone { request: id, ok: false, message: String::new(), data: json!({"ok": false, "files": [], "error": "The phone's camera is off."}) });
    assert!(rig.props()["status"].as_str().unwrap().contains("camera is off"));
    assert_eq!(rig.props()["status-kind"], "status_err");
    // With no reason at all
    rig.ui("take-clicked", vec![]);
    let id = *rig.plugin.taking.last().unwrap();
    rig.event(Event::RequestDone { request: id, ok: false, message: String::new(), data: json!({"ok": false, "files": []}) });
    assert_eq!(rig.props()["status"], "The phone couldn't take the photo.");
}

#[test]
fn a_photo_the_host_wont_start_says_the_phone_is_busy() {
    let mut rig = Rig::new(Some(json!({"size": [1920, 1080]})), true);
    // The phone becomes busy between the check and the start
    rig.host.still_busy = true;
    rig.plugin.busy = false;
    rig.plugin.take(&mut rig.host);
    assert!(rig.host.stills_taken.is_empty(), "nothing is asked of a busy phone");
}

#[test]
fn the_button_waits_while_any_plugin_takes_a_photo() {
    let mut rig = Rig::new(Some(json!({"size": [1920, 1080]})), true);
    rig.host.still_busy = true;
    rig.event(Event::StillBusyChanged { source_id: "phone-1".into(), busy: true });
    assert_eq!(rig.props()["take-enabled"], false);
    rig.host.still_busy = false;
    rig.event(Event::StillBusyChanged { source_id: "phone-1".into(), busy: false });
    assert_eq!(rig.props()["take-enabled"], true);
}

#[test]
fn config_round_trips_and_odd_values_go_back_to_defaults() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.plugin.set_config(&json!({"format": "heic", "timer": 3, "best": false, "folder": "/x"})).unwrap();
    let cfg = rig.plugin.get_config();
    assert!(cfg["format"] == "heic" && cfg["folder"] == "/x" && cfg["timer"] == 3 && cfg["best"] == false);
    rig.plugin.set_config(&json!({"timer": 7, "best": "yes", "burst": true, "folder": 5})).unwrap();
    let cfg = rig.plugin.get_config();
    assert_eq!((&cfg["timer"], &cfg["best"], &cfg["burst"]), (&json!(0), &json!(true), &json!(5)));
    assert_eq!(cfg["folder"], default_folder().as_str());
    // The keys come in the order the file has always had them
    let keys: Vec<&String> = cfg.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["format", "quality", "mode", "shots", "burst", "bracket", "focus_steps", "exposure_s", "timer", "best", "distortion", "folder"]);
    assert!(rig.plugin.set_config(&json!([1])).is_err());
}

#[test]
fn the_shortcuts_take_a_photo_pick_a_mode_and_set_the_timer() {
    let mut rig = Rig::new(Some(full_json()), true);
    let actions = rig.plugin.actions();
    assert_eq!(actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["photos.take", "photos.mode", "photos.timer"]);
    assert_eq!(actions[1].params[1]["dynamic"], true);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.take", &json!({})).as_deref(), Some("Take photo"));
    assert_eq!(rig.host.stills_taken.len(), 1);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.mode", &json!({"op": "next"})).as_deref(), Some("Photo mode: Full resolution"));
    assert_eq!(rig.plugin.get_config()["mode"], "max_res");
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.mode", &json!({"op": "set", "option": "night"})).as_deref(), Some("Photo mode: Night"));
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.timer", &json!({"op": "next"})).as_deref(), Some("Photo timer: 3 s"));
    assert_eq!(rig.plugin.get_config()["timer"], 3);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.timer", &json!({"op": "set", "option": "Off"})).as_deref(), Some("Photo timer: Off"));
    // A button that isn't ready does nothing: the photo is being taken
    rig.host.still_busy = true;
    rig.caps_changed();
    assert!(rig.plugin.run_action(&mut rig.host, "photos.take", &json!({})).is_none());
}

#[test]
fn the_shortcuts_do_nothing_without_a_phone() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.event(Event::PhonesChanged { count: 0 });
    assert!(rig.plugin.run_action(&mut rig.host, "photos.take", &json!({})).is_none());
    assert!(rig.plugin.run_action(&mut rig.host, "photos.timer", &json!({})).is_none());
    // A plain lens has one mode, so there's no drop-down to pick in
    let mut plain = Rig::new(Some(json!({"size": [1, 1]})), true);
    assert!(plain.plugin.run_action(&mut plain.host, "photos.mode", &json!({})).is_none());
}

#[test]
fn the_card_hides_for_a_source_that_isnt_a_phone() {
    let mut rig = Rig::new(Some(full_json()), true);
    assert_eq!(rig.props()["shown"], true);
    rig.event(Event::SourceSelected { source_id: "browser".into() });
    assert_eq!(rig.props()["shown"], false);
    rig.event(Event::SourceSelected { source_id: String::new() });
    assert_eq!(rig.props()["shown"], true);
}

#[test]
fn quality_burst_and_the_other_boxes_are_picks_too() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("quality-moved", vec![json!(80.0)]);
    assert_eq!((rig.plugin.get_config()["quality"].clone(), rig.props()["quality-text"].clone()), (json!(80), json!("80")));
    rig.ui("tile-picked", vec![json!("shots"), json!(1)]);
    assert!(tile(&rig.props(), "burst").is_some());
    // The dial runs from 2 to the most the lens takes, and only a place on it is taken
    rig.ui("tile-picked", vec![json!("burst"), json!(10)]);
    rig.ui("tile-picked", vec![json!("burst"), json!(999)]);
    assert_eq!(rig.plugin.get_config()["burst"], 12, "only what's on the dial is taken");
    let (list, at) = tile(&rig.props(), "burst").unwrap();
    assert_eq!((list.first().map(String::as_str), list.last().map(String::as_str), at), (Some("2"), Some("60"), 10));
    rig.ui("timer-picked", vec![json!(2)]);
    rig.ui("best-toggled", vec![json!(false)]);
    rig.ui("tile-picked", vec![json!("exposure_s"), json!(2)]);
    let cfg = rig.plugin.get_config();
    assert_eq!((&cfg["timer"], &cfg["best"], &cfg["exposure_s"]), (&json!(10), &json!(false), &json!(1)));
    let saves = rig.host.calls.iter().filter(|c| *c == "schedule_save").count();
    rig.ui("timer-picked", vec![json!(2)]);
    assert_eq!(rig.host.calls.iter().filter(|c| *c == "schedule_save").count(), saves, "the same pick again isn't a change");
}

#[cfg(unix)]
#[test]
fn the_folder_is_picked_in_a_dialog_and_opened_from_the_last_photo() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("folder-clicked", vec![]);
    assert!(rig.host.calls.iter().any(|c| c == "pick_file Folder Save photos in"));
    let asked = rig.plugin.asked.unwrap();
    // Cancelled: nothing changes
    rig.event(Event::RequestDone { request: asked, ok: false, message: String::new(), data: json!({}) });
    assert_eq!(rig.plugin.get_config()["folder"], "/tmp/photos");
    rig.ui("folder-clicked", vec![]);
    let asked = rig.plugin.asked.unwrap();
    rig.event(Event::RequestDone { request: asked, ok: true, message: String::new(), data: json!({"path": "/home/me/My Pictures"}) });
    assert_eq!(rig.plugin.get_config()["folder"], "/home/me/My Pictures");
    assert_eq!(rig.props()["folder-name"], "My Pictures");
    rig.ui("open-clicked", vec![]);
    assert!(rig.host.calls.iter().any(|c| c == "open_url file:///home/me/My%20Pictures"));
}

#[test]
fn exposures_read_like_the_python_app() {
    assert_eq!((exposure_text(0.0).as_str(), exposure_text(0.5).as_str(), exposure_text(8.0).as_str()), ("Like the video", "0.5 s", "8 s"));
}

#[test]
fn a_second_phones_photo_does_not_cost_the_first_its_end() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("take-clicked", vec![]);
    let first = *rig.plugin.taking.last().unwrap();
    // Another phone is shown now and takes its own photo while the first is still going
    rig.ui("take-clicked", vec![]);
    let second = *rig.plugin.taking.last().unwrap();
    assert_ne!(first, second);
    rig.event(Event::RequestProgress { request: first, data: json!({"state": "capturing", "shots": 5, "taken": 1}) });
    assert!(rig.props()["status"].as_str().unwrap().contains("2 of 5"));
    rig.event(Event::RequestDone { request: first, ok: true, message: String::new(), data: json!({"ok": true, "files": ["/tmp/photos/A.jpg"]}) });
    assert_eq!(rig.props()["status"], "Saved A.jpg");
    assert!(rig.host.calls.iter().any(|c| c == "send_notification Photo saved"));
    rig.event(Event::RequestDone { request: second, ok: true, message: String::new(), data: json!({"ok": true, "files": ["/tmp/photos/B.jpg"]}) });
    assert_eq!(rig.props()["status"], "Saved B.jpg");
    assert!(rig.plugin.taking.is_empty());
}

// Python (QStandardPaths.PicturesLocation) under a made-up home, for each user-dirs.dirs
#[cfg(unix)]
#[test]
fn the_pictures_folder_is_read_the_way_qt_reads_it() {
    let home = Path::new("/home/me");
    let folder = |text: Option<&str>| pictures_in(home, text).display().to_string();
    assert_eq!(folder(None), "/home/me/Pictures");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"$HOME/Bilder\"\n")), "/home/me/Bilder");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"$HOME\"\n")), "/home/me");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"$HOME/\"\n")), "/home/me");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"/data/pics\"\n")), "/data/pics");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"/data/pics/\"\n")), "/data/pics");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"\"\n")), "\"\"");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"pics\"\n")), "pics");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=$HOME/Q\n")), "/home/me/Q");
    assert_eq!(folder(Some("  XDG_PICTURES_DIR=\"$HOME/Ind\"\n")), "/home/me/Pictures");
    assert_eq!(folder(Some("# XDG_PICTURES_DIR=\"$HOME/No\"\nXDG_PICTURES_DIR=\"$HOME/Yes\"\n")), "/home/me/Yes");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"$HOME/One\"\nXDG_PICTURES_DIR=\"$HOME/Two\"\n")), "/home/me/Two");
    assert_eq!(folder(Some("XDG_PICTURES_DIR=\"$HOMEx/y\"\n")), "/home/mex/y");
    assert_eq!(folder(Some("XDG_PICTURES_DIR = \"$HOME/Sp\"\n")), "/home/me/Pictures");
    assert_eq!(folder(Some("XDG_MUSIC_DIR=\"$HOME/Tunes\"\n")), "/home/me/Pictures");
}

// ── A camera of its own (a DSLR) ─────────────────────────────────────────────

fn dslr_json() -> Value {
    json!({"formats": ["jpeg", "raw"], "camera": true, "burst_max": 1,
           "iso": {"values": ["100", "200", "400", "800"], "labels": ["100", "200", "400", "800"], "current": 2},
           "shutter": {"values": ["0,0010s", "0,0250s", "30,0000s"], "labels": ["1/1000", "1/40", "30 s"], "current": 1}})
}

/// No phone paired, a DSLR picked and streaming.
fn dslr(caps: Value) -> Rig {
    let mut rig = Rig::new(Some(caps), true);
    rig.event(Event::PhonesChanged { count: 0 });
    rig.event(Event::SourceSelected { source_id: "webcam:cam".into() });
    rig
}

#[test]
fn the_card_shows_for_a_camera_that_takes_photos_and_not_for_other_sources() {
    let mut rig = dslr(dslr_json());
    let p = rig.props();
    assert_eq!((p["shown"].clone(), p["active"].clone(), p["take-enabled"].clone()), (json!(true), json!(true), json!(true)));
    // The browser camera has no stills service: the card goes
    rig.host.still_caps = None;
    rig.event(Event::SourceSelected { source_id: "browser".into() });
    assert_eq!(rig.props()["shown"], false);
    // ...and comes back with the camera's own
    rig.host.still_caps = Some(dslr_json());
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert_eq!(rig.props()["shown"], true);
}

#[test]
fn the_card_offers_the_cameras_own_iso_and_shutter_lists_and_no_quality() {
    let rig = dslr(dslr_json());
    let p = rig.props();
    let (iso, shutter) = (tile(&p, "iso").unwrap(), tile(&p, "shutter").unwrap());
    assert_eq!((iso.0, iso.1), (["100", "200", "400", "800"].map(String::from).to_vec(), 2));
    assert_eq!((shutter.0, shutter.1), (["1/1000", "1/40", "30 s"].map(String::from).to_vec(), 1));
    assert_eq!(p["quality-enabled"], false, "a camera keeps its own JPEG quality");
    assert!(p["quality-tip"].as_str().unwrap().contains("own JPEG quality"), "and the tooltip says so");
    assert_eq!(tile(&p, "format").unwrap().0, ["JPEG", "RAW (DNG)", "RAW and JPEG"]);
    assert_eq!(keys(&p), ["iso", "shutter", "format"], "the dials first, then the named choice");
    assert_eq!(p["best-enabled"], false);
    // A phone's card has neither list
    let phone = Rig::new(Some(full_json()), true);
    assert!(tile(&phone.props(), "iso").is_none() && tile(&phone.props(), "shutter").is_none());
}

#[test]
fn a_mode_without_iso_or_shutter_hides_both_and_forgets_what_was_picked() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-picked", vec![json!("iso"), json!(3)]);
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3);
    let mut scene = dslr_json();
    scene.as_object_mut().unwrap().remove("iso");
    scene.as_object_mut().unwrap().remove("shutter");
    rig.host.still_caps = Some(scene);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    let p = rig.props();
    assert!(tile(&p, "iso").is_none() && tile(&p, "shutter").is_none());
    // Back in M the camera's own value shows, not the pick from before the dial turned
    rig.host.still_caps = Some(dslr_json());
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 2);
}

#[test]
fn picks_from_the_cameras_lists_go_with_the_photo_without_a_quality_and_are_not_saved() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-picked", vec![json!("iso"), json!(3)]);
    rig.ui("tile-picked", vec![json!("shutter"), json!(2)]);
    rig.ui("tile-picked", vec![json!("format"), json!(2)]);
    assert!(rig.plugin.get_config().get("iso").is_none() && rig.plugin.get_config().get("shutter").is_none());
    rig.ui("take-clicked", vec![]);
    let (options, folder) = rig.host.stills_taken[0].clone();
    assert_eq!(folder, "/tmp/photos");
    assert_eq!(options, json!({"format": "jpeg", "raw": 1, "rotation": 0, "iso": "800", "shutter": "30,0000s"}));
    // Without a pick nothing is asked of the camera but the format
    let mut quiet = dslr(dslr_json());
    quiet.ui("take-clicked", vec![]);
    assert_eq!(quiet.host.stills_taken[0].0, json!({"format": "jpeg", "rotation": 0}));
}

#[test]
fn a_pick_stands_until_the_camera_reports_the_same() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-picked", vec![json!("iso"), json!(3)]);
    // The camera still says 400: the pick shows
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3);
    let mut moved = dslr_json();
    moved["iso"]["current"] = json!(3);
    rig.host.still_caps = Some(moved);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3);
    // Now the camera is the one that decides: a turn of its dial shows
    let mut turned = dslr_json();
    turned["iso"]["current"] = json!(0);
    rig.host.still_caps = Some(turned);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 0);
}

#[test]
fn a_camera_says_camera_where_a_phone_says_phone() {
    let mut rig = dslr(dslr_json());
    rig.ui("take-clicked", vec![]);
    let id = *rig.plugin.taking.last().unwrap();
    rig.event(Event::RequestDone { request: id, ok: false, message: String::new(), data: json!({"ok": false, "files": []}) });
    assert_eq!(rig.props()["status"], "The camera couldn't take the photo.");
    // The host won't start a second one while one is on
    rig.host.still_busy = true;
    rig.plugin.take(&mut rig.host);
    assert_eq!(rig.host.stills_taken.len(), 1, "nothing more started");
    // The phone's wording is unchanged
    let mut phone = Rig::new(Some(full_json()), true);
    phone.ui("take-clicked", vec![]);
    let id = *phone.plugin.taking.last().unwrap();
    phone.event(Event::RequestDone { request: id, ok: false, message: String::new(), data: json!({"ok": false, "files": []}) });
    assert_eq!(phone.props()["status"], "The phone couldn't take the photo.");
}

#[test]
fn the_shortcuts_work_for_a_camera_and_not_for_a_source_without_photos() {
    let mut rig = dslr(dslr_json());
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.take", &json!({})).as_deref(), Some("Take photo"));
    assert_eq!(rig.host.stills_taken.len(), 1);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "photos.timer", &json!({"op": "next"})).as_deref(), Some("Photo timer: 3 s"));
    rig.host.still_caps = None;
    rig.event(Event::SourceSelected { source_id: "browser".into() });
    assert!(rig.plugin.run_action(&mut rig.host, "photos.take", &json!({})).is_none());
}

#[test]
fn the_self_timer_goes_with_a_camera_photo() {
    let mut rig = dslr(dslr_json());
    rig.ui("timer-picked", vec![json!(2)]);
    rig.ui("take-clicked", vec![]);
    assert_eq!(rig.host.stills_taken[0].0["timer_s"], 10);
}

#[test]
fn camera_options_drop_the_quality_and_keep_only_picks_that_were_made() {
    let mut options = json!({"format": "jpeg", "rotation": 0, "quality": 95});
    camera_options(&mut options, None, Some("1/60"));
    assert_eq!(options, json!({"format": "jpeg", "rotation": 0, "shutter": "1/60"}));
}

#[test]
fn diagnostics_say_what_the_camera_offers_now() {
    let rig = dslr(dslr_json());
    let d: std::collections::HashMap<_, _> = rig.plugin.diagnostics().into_iter().collect();
    assert_eq!((d["Photos"].as_str(), d["Photo settings"].as_str()), ("camera, jpeg, raw", "ISO, shutter speed"));
    let scene = dslr(json!({"formats": ["jpeg"], "camera": true}));
    let d: std::collections::HashMap<_, _> = scene.plugin.diagnostics().into_iter().collect();
    assert_eq!((d["Photos"].as_str(), d["Photo settings"].as_str()), ("camera, jpeg", "none"));
}

// ── The tiles ────────────────────────────────────────────────────────────────

#[test]
fn a_dslr_has_its_dials_then_the_format_and_a_phone_its_named_choices() {
    let rig = dslr(dslr_json());
    let p = rig.props();
    assert_eq!(keys(&p), ["iso", "shutter", "format"]);
    assert_eq!((p["iso-options"].as_array().unwrap().len(), p["shutter-options"].as_array().unwrap().len()), (4, 3));
    let mut phone = Rig::new(Some(full_json()), true);
    phone.ui("tile-picked", vec![json!("shots"), json!(2)]);
    let p = phone.props();
    assert_eq!(keys(&p), ["mode", "format", "shots", "bracket", "distortion"]);
    assert_eq!((p["slot-key"].clone(), p["shots-index"].clone()), (json!("bracket"), json!(2)));
    assert_eq!(tile(&p, "bracket").unwrap().0[0], "3 photos, 1 EV apart");
    // A shot the lens doesn't have can't be picked, whatever the card sends
    let mut plain = Rig::new(Some(json!({"size": [1920, 1080]})), true);
    plain.ui("tile-picked", vec![json!("shots"), json!(3)]);
    assert_eq!(plain.props()["shots-index"], json!(0));
    let long = {
        phone.ui("tile-picked", vec![json!("shots"), json!(0)]);
        tile(&phone.props(), "exposure_s").unwrap().0
    };
    assert_eq!((long[0].as_str(), long[1].as_str(), long.last().map(String::as_str)), ("Like the video", "0.5 s", Some("30 s")));
}

#[test]
fn a_dial_steps_along_its_list_and_stops_at_the_ends() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-step", vec![json!("iso"), json!(1)]);
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3);
    rig.ui("tile-step", vec![json!("iso"), json!(1)]);
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3, "800 is the last");
    for _ in 0..5 {
        rig.ui("tile-step", vec![json!("iso"), json!(-1)]);
    }
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 0);
    // What it stepped to goes with the photo, as a pick from the list does
    rig.ui("take-clicked", vec![]);
    assert_eq!(rig.host.stills_taken[0].0["iso"], "100");
    // A named choice isn't a dial: the arrows aren't there and a step does nothing
    rig.ui("tile-step", vec![json!("format"), json!(1)]);
    assert_eq!(tile(&rig.props(), "format").unwrap().1, 0);
}

#[test]
fn a_step_keeps_the_hold_and_a_new_list_lets_it_go() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-hold-down", vec![json!("shutter"), json!(-1)]);
    assert!(rig.plugin.held.is_some());
    rig.ui("tile-step", vec![json!("shutter"), json!(1)]);
    assert!(rig.plugin.held.is_some(), "a step along the same lists leaves the arrow held");
    // A new list of options does let go: the arrow would step along a list it no longer has
    let mut scene = dslr_json();
    scene.as_object_mut().unwrap().remove("iso");
    rig.host.still_caps = Some(scene);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert!(rig.plugin.held.is_none());
}

fn hold_tick(rig: &mut Rig) {
    rig.host.cancel_schedule(T_HOLD); // a timer that has fired is no longer waiting
    rig.event(Event::TimerFired { owner: NAME.into(), token: T_HOLD.into() });
}

fn long_dslr() -> Value {
    let values: Vec<String> = (0..45).map(|i| i.to_string()).collect();
    let mut caps = dslr_json();
    caps["shutter"] = json!({"values": values, "labels": values, "current": 0});
    caps
}

#[test]
fn a_held_arrow_steps_at_once_then_repeats_and_takes_bigger_steps_on_a_long_list() {
    let mut rig = dslr(long_dslr());
    rig.ui("tile-hold-down", vec![json!("shutter"), json!(1)]);
    assert_eq!(tile(&rig.props(), "shutter").unwrap().1, 1, "one step as it goes down");
    assert!(rig.host.is_scheduled(T_HOLD));
    // Nothing more through the first pause, so a click is one step
    for _ in 0..8 {
        hold_tick(&mut rig);
    }
    assert_eq!(tile(&rig.props(), "shutter").unwrap().1, 1);
    let mut seen = Vec::new();
    for _ in 0..60 {
        hold_tick(&mut rig);
        seen.push(tile(&rig.props(), "shutter").unwrap().1);
    }
    let jumps: Vec<i64> = seen.windows(2).map(|w| w[1] - w[0]).filter(|j| *j != 0).collect();
    assert!(jumps.contains(&1) && jumps.contains(&3), "single steps, then three at a time: {jumps:?}");
    assert!(jumps.iter().position(|j| *j == 3).unwrap() > jumps.iter().position(|j| *j == 1).unwrap(), "{jumps:?}");
    rig.ui("tile-hold-up", vec![]);
    let at = tile(&rig.props(), "shutter").unwrap().1;
    for _ in 0..20 {
        hold_tick(&mut rig);
    }
    assert_eq!(tile(&rig.props(), "shutter").unwrap().1, at, "let go, so nothing more");
    assert!(!rig.host.is_scheduled(T_HOLD));
}

#[test]
fn a_held_arrow_stops_at_the_end_and_a_short_list_keeps_to_single_steps() {
    let mut rig = dslr(long_dslr());
    rig.ui("tile-hold-down", vec![json!("shutter"), json!(1)]);
    for _ in 0..400 {
        hold_tick(&mut rig);
    }
    assert_eq!(tile(&rig.props(), "shutter").unwrap().1, 44, "the last of 45");
    assert!(!rig.host.is_scheduled(T_HOLD), "nothing left to wait for");
    let mut short = dslr(dslr_json());
    short.ui("tile-hold-down", vec![json!("iso"), json!(-1)]);
    let mut seen = vec![tile(&short.props(), "iso").unwrap().1];
    for _ in 0..40 {
        hold_tick(&mut short);
        seen.push(tile(&short.props(), "iso").unwrap().1);
    }
    assert!(seen.windows(2).all(|w| (w[0] - w[1]).abs() <= 1) && seen.last() == Some(&0), "{seen:?}");
}

#[test]
fn a_hold_ends_when_its_tile_goes_or_the_plugin_stops() {
    let mut rig = dslr(dslr_json());
    rig.ui("tile-hold-down", vec![json!("iso"), json!(-1)]);
    assert!(rig.host.is_scheduled(T_HOLD));
    let mut scene = dslr_json();
    scene.as_object_mut().unwrap().remove("iso");
    rig.host.still_caps = Some(scene);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert!(!rig.host.is_scheduled(T_HOLD), "the dial under the hand is gone");
    rig.host.still_caps = Some(dslr_json());
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    rig.ui("tile-hold-down", vec![json!("iso"), json!(-1)]);
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.host.is_scheduled(T_HOLD));
    rig.ui("tile-hold-down", vec![json!("nothing"), json!(1)]);
    assert!(!rig.host.is_scheduled(T_HOLD), "nothing to hold");
}

#[test]
fn picks_made_on_one_camera_are_not_sent_with_another_cameras_photo() {
    let mut rig = dslr(dslr_json());
    rig.host.focused = Some("webcam:a".into());
    rig.event(Event::DeviceChanged { source_id: "webcam:a".into() });
    rig.ui("tile-picked", vec![json!("iso"), json!(3)]);
    rig.ui("tile-picked", vec![json!("shutter"), json!(2)]);
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, 3);
    // Another camera whose lists are word for word the same, so nothing in what it reports tells the two apart
    rig.host.focused = Some("webcam:b".into());
    rig.event(Event::DeviceChanged { source_id: "webcam:b".into() });
    assert_eq!((tile(&rig.props(), "iso").unwrap().1, tile(&rig.props(), "shutter").unwrap().1), (2, 1), "the camera's own values show");
    rig.ui("take-clicked", vec![]);
    assert_eq!(rig.host.stills_taken[0].0, json!({"format": "jpeg", "rotation": 0}), "nothing from the other camera goes with it");
    // The same camera again after the stream stops and starts keeps what was picked on it
    let mut same = dslr(dslr_json());
    same.host.focused = Some("webcam:a".into());
    same.event(Event::DeviceChanged { source_id: "webcam:a".into() });
    same.ui("tile-picked", vec![json!("iso"), json!(3)]);
    same.host.focused = None;
    same.event(Event::StreamStopped);
    same.host.focused = Some("webcam:a".into());
    same.event(Event::DeviceChanged { source_id: "webcam:a".into() });
    assert_eq!(tile(&same.props(), "iso").unwrap().1, 3);
}

#[test]
fn a_hold_ends_before_the_tiles_are_replaced_even_when_its_own_tile_stays() {
    let mut rig = dslr(long_dslr());
    rig.ui("tile-hold-down", vec![json!("shutter"), json!(1)]);
    rig.ui("tile-hold-up", vec![]);
    rig.ui("tile-hold-down", vec![json!("iso"), json!(1)]);
    assert!(rig.host.is_scheduled(T_HOLD));
    // The mode dial turns: the shutter tile goes and ISO stays, so a new list of tiles replaces the arrows
    let mut scene = long_dslr();
    scene.as_object_mut().unwrap().remove("shutter");
    rig.host.still_caps = Some(scene);
    rig.event(Event::StillCapsChanged { source_id: "webcam:cam".into() });
    assert!(tile(&rig.props(), "iso").is_some() && tile(&rig.props(), "shutter").is_none());
    assert!(!rig.host.is_scheduled(T_HOLD), "the arrow it was under is gone");
    let at = tile(&rig.props(), "iso").unwrap().1;
    for _ in 0..30 {
        hold_tick(&mut rig);
    }
    assert_eq!(tile(&rig.props(), "iso").unwrap().1, at, "nothing keeps stepping after the release that never came");
}

#[test]
fn a_camera_that_streams_again_brings_the_card_back_even_if_nobody_told_the_card_what_it_takes() {
    let mut rig = dslr(dslr_json());
    assert_eq!(rig.props()["shown"], true);
    // The stream drops and the camera takes nothing for now
    rig.host.still_caps = None;
    rig.event(Event::StreamLost { source_id: "webcam:cam".into() });
    assert_eq!((rig.props()["shown"].clone(), rig.props()["take-enabled"].clone()), (json!(false), json!(false)));
    // It is back, and the only notice is that the stream is: the card asks
    rig.host.still_caps = Some(dslr_json());
    rig.event(Event::StreamConnected { source_id: "webcam:cam".into() });
    assert_eq!((rig.props()["shown"].clone(), rig.props()["take-enabled"].clone()), (json!(true), json!(true)));
}
