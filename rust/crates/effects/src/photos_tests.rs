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

#[test]
fn without_photos_the_button_is_off_and_the_card_says_why() {
    let mut rig = Rig::new(None, false);
    assert_eq!(rig.props()["take-enabled"], false);
    assert!(rig.props()["info"].as_str().unwrap().contains("Start streaming"));
    rig.host.streaming = true;
    rig.caps_changed();
    assert!(rig.props()["info"].as_str().unwrap().contains("Update the app"));
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
    assert!(p["take-enabled"] == true && p["info-shown"] == false);
    for row in ["format-shown", "mode-shown", "shots-shown", "exposure-shown", "best-shown", "distortion-shown"] {
        assert_eq!(p[row], false, "{row}");
    }
    rig.host.still_caps = Some(full_json());
    rig.caps_changed();
    let p = rig.props();
    assert!(p["format-shown"] == true && p["mode-shown"] == true);
    assert_eq!((p["format-items"].as_array().unwrap().len(), p["mode-items"].as_array().unwrap().len()), (5, 5));
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
    assert!(rig.props()["mode-shown"] == true);
    rig.host.still_caps = Some(json!({"size": [1000, 1000]}));
    rig.event(Event::CameraSwitched { camera: json!({"id": "4"}) });
    let p = rig.props();
    assert!(p["mode-shown"] == false && p["format-shown"] == false);
}

#[test]
fn picking_a_night_mode_hides_what_it_cant_do() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("choice-picked", vec![json!(0), json!(2)]);
    let p = rig.props();
    assert!(p["format-shown"] == false && p["exposure-shown"] == false && p["best-shown"] == false);
    assert_eq!(p["shots-items"], json!(["Single", "Burst"]));
    assert_eq!(rig.plugin.get_config()["mode"], "night");
}

#[test]
fn a_pick_for_another_lens_is_kept_when_this_one_cant_do_it() {
    let mut rig = Rig::new(Some(full_json()), true);
    rig.ui("choice-picked", vec![json!(1), json!(3)]);
    assert_eq!(rig.plugin.get_config()["format"], "raw");
    rig.host.still_caps = Some(json!({"size": [1000, 1000]}));
    rig.caps_changed();
    assert_eq!(rig.props()["format-shown"], false);
    rig.host.still_caps = Some(full_json());
    rig.caps_changed();
    let p = rig.props();
    assert_eq!((p["format-current"].as_i64(), rig.plugin.get_config()["format"].as_str()), (Some(3), Some("raw")));
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
    assert_eq!((p["status-kind"].as_str(), p["open-shown"].as_bool()), (Some("status_ok"), Some(true)));
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
    rig.ui("choice-picked", vec![json!(2), json!(1)]);
    assert_eq!(rig.props()["burst-shown"], true);
    rig.ui("number-edited", vec![json!(0), json!("12")]);
    rig.ui("number-edited", vec![json!(0), json!("1")]);
    rig.ui("number-edited", vec![json!(0), json!("999")]);
    rig.ui("number-edited", vec![json!(0), json!("abc")]);
    assert_eq!(rig.plugin.get_config()["burst"], 12, "only what's a number in range is taken");
    rig.ui("timer-picked", vec![json!(2)]);
    rig.ui("best-toggled", vec![json!(false)]);
    rig.ui("choice-picked", vec![json!(3), json!(2)]);
    let cfg = rig.plugin.get_config();
    assert_eq!((&cfg["timer"], &cfg["best"], &cfg["exposure_s"]), (&json!(10), &json!(false), &json!(1)));
    let saves = rig.host.calls.iter().filter(|c| *c == "schedule_save").count();
    rig.ui("timer-picked", vec![json!(2)]);
    assert_eq!(rig.host.calls.iter().filter(|c| *c == "schedule_save").count(), saves, "the same pick again isn't a change");
}

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
