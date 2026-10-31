use super::*;
use telescope_core::testing::FakeHost;
use telescope_core::view::{view_to_shown, PhoneZoom};

struct Rig {
    plugin: TransformsPlugin,
    host: FakeHost,
}

impl Rig {
    /// A card with a phone paired and the saved settings loaded, as the window does at startup.
    fn new() -> Rig {
        let mut rig = Rig { plugin: TransformsPlugin::new(), host: FakeHost::new() };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.set_config(&json!({})).unwrap();
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn zoom(&mut self, steps: i32) {
        self.ui("zoom-moved", vec![json!(steps as f64 / 100.0)]);
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// The newest value the card was given for a property.
    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn run(&mut self, id: &str, params: Value) -> Option<String> {
        self.plugin.run_action(&mut self.host, id, &params)
    }

    fn view_status(&mut self, status: &ViewStatus) {
        self.event(Event::ViewStatus { status: serde_json::to_value(status).unwrap() });
    }
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn the_card_is_greyed_out_until_a_phone_is_paired() {
    let mut p = TransformsPlugin::new();
    let mut host = FakeHost::new();
    p.setup(&mut host);
    let card = p.card().unwrap();
    assert_eq!(card.ui.props["active"], json!(false));
    p.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    assert!(host.calls.iter().any(|c| c.contains("\"active\":true")));
}

#[test]
fn it_is_a_pinned_built_in_that_rides_in_presets_per_phone() {
    let info = TransformsPlugin::new().info();
    assert!(info.pinned && info.in_presets && info.device_local && !info.optional && !info.required && info.category == "camera");
}

#[test]
fn visible_rotation_and_pan_controls_keep_their_shortcuts() {
    let mut r = Rig::new();
    r.zoom(200);
    assert!(r.run("transforms.rotation", json!({"op": "set", "option": "90 CW"})).is_some());
    assert!(r.run("transforms.pan_x", json!({"op": "set", "value": 50})).is_some());
    assert!(r.run("transforms.pan_y", json!({"op": "set", "value": -25})).is_some());
    assert_eq!(r.plugin.rotation(), Some(90));
    assert_eq!(r.plugin.pan(), (0.5, -0.25));
}

#[test]
fn pan_and_zoom_shortcuts_have_nothing_to_do_while_they_cant_be_used() {
    let mut r = Rig::new();
    assert!(r.run("transforms.pan_x", json!({"op": "set", "value": 50})).is_none(), "unzoomed there's nothing to pan");
    r.zoom(200);
    r.event(Event::ViewTaken { owner: "Auto framing".into() });
    assert!(r.run("transforms.zoom", json!({"op": "up", "amount": 1})).is_none());
    assert!(r.run("transforms.pan_x", json!({"op": "up", "amount": 1})).is_none());
    assert!(r.run("transforms.stretch_x", json!({"op": "set", "value": 120})).is_some(), "stretch isn't taken");
    let mut unpaired = TransformsPlugin::new();
    let mut host = FakeHost::new();
    assert!(unpaired.run_action(&mut host, "transforms.reset", &json!({})).is_none());
}

#[test]
fn the_pan_pad_works_from_the_keyboard() {
    let mut r = Rig::new();
    r.zoom(200);
    r.ui("pad-key", vec![json!("right"), json!(false)]);
    r.ui("pad-key", vec![json!("down"), json!(true)]);
    assert!(r.plugin.pan().0 > 0.0 && r.plugin.pan().1 == 0.1);
    r.ui("pad-key", vec![json!("home"), json!(false)]);
    assert_eq!(r.plugin.pan(), (0.0, 0.0));
}

#[test]
fn the_pad_hands_over_values_the_sliders_quantise() {
    let mut r = Rig::new();
    r.zoom(200);
    r.ui("pad-pointer", vec![json!(0.5033), json!(-1.7)]);
    assert_eq!(r.plugin.pan(), (100.0 / 200.0, -1.0));
    assert_eq!(r.shown("pan-text"), json!("H +50%\nV -100%"));
    r.ui("pad-reset", vec![]);
    assert_eq!(r.plugin.pan(), (0.0, 0.0));
    r.zoom(100);
    assert_eq!(r.shown("pan-enabled"), json!(false));
}

#[test]
fn handlers_update_runtime_state_and_schedule_a_save() {
    let mut r = Rig::new();
    r.ui("flip-toggled", vec![json!(0), json!(true)]);
    r.ui("rotation-picked", vec![json!(1)]);
    r.zoom(250);
    r.plugin.pan_steps = [100, -50];
    r.plugin.on_pan_changed(&mut r.host);
    assert!(r.plugin.flips().0 && r.plugin.rotation() == Some(90) && r.plugin.zoom() == 2.5 && r.plugin.pan() == (0.5, -0.25));
    assert_eq!(r.shown("pan-enabled"), json!(true));
    assert!(r.saves() >= 4);
    assert!(r.host.view.flip_h && r.host.view.rotation == Some(90) && r.host.view.zoom == 2.5);
}

#[test]
fn returning_zoom_to_one_resets_and_disables_pan() {
    let mut r = Rig::new();
    r.zoom(200);
    r.ui("pad-pointer", vec![json!(0.8), json!(-0.8)]);
    r.zoom(100);
    assert_eq!(r.plugin.pan(), (0.0, 0.0));
    assert_eq!(r.shown("pan-enabled"), json!(false));
    assert_eq!(r.host.view.pan_x, 0.0);
}

#[test]
fn the_view_is_what_the_card_says() {
    let mut r = Rig::new();
    r.ui("flip-toggled", vec![json!(0), json!(true)]);
    r.ui("rotation-picked", vec![json!(1)]);
    r.ui("stretch-moved", vec![json!(0), json!(150)]);
    r.ui("stretch-moved", vec![json!(1), json!(80)]);
    let v = &r.host.view;
    assert!(v.flip_h && v.rotation == Some(90) && approx(v.stretch_x as f64, 1.5) && approx(v.stretch_y as f64, 0.8));
    assert_eq!(r.shown("stretch-x-text"), json!("150%"));
}

#[test]
fn config_round_trip_updates_the_card() {
    let mut r = Rig::new();
    let cfg = json!({"flip_h": true, "flip_v": true, "rotation": "180", "zoom": 3.0, "pan_x": 0.4, "pan_y": -0.6, "stretch_x": 1.5, "stretch_y": 0.75});
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    r.event(Event::StreamStopped);
    assert_eq!(r.shown("pan-enabled"), json!(true));
    assert_eq!(r.shown("rotation"), json!(2));
}

#[test]
fn the_settings_keep_the_keys_and_order_python_saved() {
    let r = Rig::new();
    let cfg = r.plugin.get_config();
    let keys: Vec<&String> = cfg.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["flip_h", "flip_v", "rotation", "zoom", "pan_x", "pan_y", "stretch_x", "stretch_y"]);
    assert_eq!(cfg, json!({"flip_h": false, "flip_v": false, "rotation": "None", "zoom": 1.0, "pan_x": 0.0, "pan_y": 0.0, "stretch_x": 1.0, "stretch_y": 1.0}));
}

#[test]
fn a_saved_zoom_loads_exactly() {
    for zoom in [1.15, 2.03, 2.3, 4.1] {
        let mut r = Rig::new();
        r.plugin.set_config(&json!({"zoom": zoom})).unwrap();
        assert!(approx(r.plugin.get_config()["zoom"].as_f64().unwrap(), zoom), "{zoom}");
    }
}

#[test]
fn saved_pan_config_updates_the_runtime_state() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"zoom": 2.0, "pan_x": 0.75, "pan_y": -0.5})).unwrap();
    assert_eq!(r.plugin.pan(), (0.75, -0.5));
    let pan = r.plugin.pan_props();
    assert!(approx(pan["pan-x"].as_f64().unwrap(), 0.75));
}

#[test]
fn stretch_saves_loads_and_resets() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"stretch_x": 1.25, "stretch_y": 0.7})).unwrap();
    r.event(Event::StreamStopped);
    assert!(approx(r.plugin.get_config()["stretch_x"].as_f64().unwrap(), 1.25) && approx(r.plugin.get_config()["stretch_y"].as_f64().unwrap(), 0.7));
    assert!(approx(r.host.view.stretch_y as f64, 0.7));
    r.plugin.set_config(&json!({"stretch_x": "junk", "stretch_y": 9})).unwrap();
    assert_eq!(r.plugin.stretch(), (1.0, 2.0));
    r.ui("reset-clicked", vec![]);
    assert_eq!((r.host.view.stretch_x, r.host.view.stretch_y), (1.0, 1.0));
    assert_eq!(r.plugin.get_config()["stretch_x"], json!(1.0));
}

#[test]
fn loading_a_stretch_sets_the_view_once() {
    let mut r = Rig::new();
    r.host.calls.clear();
    // every phone switch, Transforms being per phone
    r.plugin.set_config(&json!({"stretch_x": 1.25, "stretch_y": 0.7})).unwrap();
    r.event(Event::StreamStopped);
    r.event(Event::StreamStopped);
    let sets: Vec<&String> = r.host.calls.iter().filter(|c| c.starts_with("set_view") && c.contains("stretch_x: Some(1.25)")).collect();
    assert_eq!(sets.len(), 1, "{:?}", r.host.calls);
}

#[test]
fn a_damaged_saved_zoom_or_pan_falls_back_instead_of_breaking_frames() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"zoom": 2.0, "pan_x": "nan", "pan_y": 7})).unwrap();
    assert_eq!(r.plugin.pan(), (0.0, 1.0));
    r.plugin.set_config(&json!({"zoom": "big"})).unwrap();
    assert_eq!(r.plugin.get_config()["zoom"], json!(1.0));
}

#[test]
fn a_preset_is_applied_at_once() {
    let mut r = Rig::new();
    r.host.calls.clear();
    r.plugin.apply_preset(&mut r.host, &json!({"zoom": 2.0, "rotation": "180"})).unwrap();
    assert!(r.host.view.zoom == 2.0 && r.host.view.rotation == Some(180));
}

#[test]
fn a_reset_clears_everything() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"flip_h": true, "rotation": "90 CW", "zoom": 3.0, "pan_x": 0.4, "stretch_x": 1.5})).unwrap();
    r.event(Event::StreamStopped);
    r.run("transforms.reset", json!({}));
    assert_eq!(r.plugin.get_config(), TransformsPlugin::new().get_config());
    assert!(!r.host.view.flip_h && r.host.view.rotation.is_none() && r.host.view.zoom == 1.0);
}

#[test]
fn the_zoom_slider_follows_max_zoom() {
    let mut r = Rig::new();
    assert_eq!(r.shown("zoom-max"), json!(10.0));
    r.zoom(800);
    r.event(Event::MaxZoomChanged { max_zoom: 5 });
    assert_eq!(r.shown("zoom-max"), json!(5.0));
    assert_eq!(r.plugin.zoom(), 5.0, "clamped, through the normal handler");
    r.event(Event::MaxZoomChanged { max_zoom: 20 });
    r.zoom(2000);
    assert_eq!(r.plugin.zoom(), 20.0);
}

#[test]
fn the_zoom_action_follows_max_zoom() {
    let mut r = Rig::new();
    let top = |r: &mut Rig| {
        let zoom = r.plugin.actions().into_iter().find(|a| a.id == "transforms.zoom").unwrap();
        zoom.params.iter().find(|p| p["name"] == "value").unwrap()["maximum"].clone()
    };
    assert_eq!(top(&mut r), json!(10.0));
    r.host.calls.clear();
    r.event(Event::MaxZoomChanged { max_zoom: 20 });
    assert_eq!(top(&mut r), json!(20.0));
    assert!(r.host.calls.iter().any(|c| c == "refresh_actions"), "the shortcut list asks for the actions again");
}

// ── What the lens dot says ───────────────────────────────────────────

fn tele() -> PhoneZoomCaps {
    PhoneZoomCaps { ratio_max: 10.0, crop_max: 4.0, freeform: true, lens_zooms: vec![3.7] }
}

#[test]
fn lens_note_says_nothing_on_the_default_camera() {
    assert_eq!(lens_note(2.0, 1.0, Some(&tele()), "~24mm OIS", "~24mm OIS", "", true), ("", String::new()));
    assert_eq!(lens_note(3.0, 1.0, None, "", "", "", true), ("", String::new()));
}

#[test]
fn lens_note_says_when_the_phone_switched() {
    let (level, note) = lens_note(4.0, 3.7, Some(&tele()), "Tele ~85mm", "~24mm OIS", "Tele ~85mm", true);
    assert_eq!(level, "on");
    assert!(note.starts_with("Switched to Tele ~85mm") && note.contains("wobble for a moment"));
}

#[test]
fn lens_note_says_when_panning_fell_back_to_the_main_camera() {
    let (level, note) = lens_note(4.0, 1.0, Some(&tele()), "~24mm OIS", "~24mm OIS", "Tele ~85mm", true);
    assert_eq!(level, "off");
    assert!(note.contains("Panned past what Tele ~85mm can see, so it's on ~24mm OIS"));
    assert!(lens_note(4.0, 1.0, Some(&tele()), "", "", "", true).1.contains("the telephoto can see"));
}

#[test]
fn lens_note_says_when_the_phone_stayed_on_the_main_camera_by_itself() {
    let (level, note) = lens_note(4.0, 3.7, Some(&tele()), "~24mm OIS", "~24mm OIS", "Tele ~85mm", true);
    assert_eq!(level, "off");
    assert!(note.contains("The phone stayed on ~24mm OIS by itself"));
    // not while a switch may still be under way, nor when the phone doesn't say which lens it's on
    assert_eq!(lens_note(4.0, 3.7, Some(&tele()), "~24mm OIS", "~24mm OIS", "", false), ("", String::new()));
    assert_eq!(lens_note(4.0, 3.7, Some(&tele()), "", "", "", true), ("", String::new()));
}

#[test]
fn red_wins_when_the_phone_is_off_the_lens_after_switching() {
    let (level, _) = lens_note(4.0, 1.0, Some(&tele()), "Tele ~85mm", "~24mm OIS", "Tele ~85mm", true);
    assert_eq!(level, "off", "still on the tele, but the pan already asked for the main camera");
}

fn lens_state(active: &str) -> Value {
    json!({"cameras": [{"id": "1", "current": false}, {"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true, "lensZooms": [3.7]},
                       {"id": "2", "current": false, "label": "Back ~24mm OIS [phys]"}, {"id": "4", "current": false, "label": "Back Telephoto ~85mm OIS [phys]"}],
           "active_lens": active})
}

/// What the window's view controller would say about the zoom, for a phone on a camera with a 3.7x lens.
fn status(sent_ratio: f64, at: f64) -> ViewStatus {
    ViewStatus { streaming: true, caps: Some(tele()), sent: Some(PhoneZoom { ratio: sent_ratio, crop: 1.0, x: 0.5, y: 0.5 }), sent_ratio_at: at, desktop_zoom: 1.0 }
}

fn stream() -> StreamInfo {
    StreamInfo { source_id: "a".into(), url: "http://phone/v1/video".into(), session: 0 }
}

#[test]
fn the_dot_shows_only_when_the_phone_switched_or_fell_back() {
    let mut r = Rig::new();
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, 1.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    assert_eq!(r.shown("dot-level"), json!(""));
    r.zoom(400);
    r.view_status(&status(3.7, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("4"));
    assert_eq!(r.shown("dot-level"), json!("on"));
    assert!(r.shown("dot-tip").as_str().unwrap().starts_with("Switched to Tele ~85mm OIS"));
    r.plugin.pan_steps = [200, 0];
    r.plugin.on_pan_changed(&mut r.host);
    r.view_status(&status(1.0, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    assert!(r.shown("dot-tip").as_str().unwrap().contains("Panned past what Tele ~85mm OIS can see"));
    assert_eq!(r.shown("dot-level"), json!("off"));
    r.zoom(200);
    assert_eq!(r.shown("dot-level"), json!(""));
    assert!(r.shown("zoom-tip").as_str().unwrap().starts_with("Zoomed on the phone's sensor\nDots mark"));
}

#[test]
fn the_dot_goes_red_once_the_phone_had_time_to_switch_and_didnt() {
    let mut r = Rig::new();
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    r.zoom(400);
    // just asked: give it a moment
    r.view_status(&status(3.7, now_s()));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    assert_eq!(r.shown("dot-level"), json!(""));
    assert!(r.host.scheduled.iter().any(|(_, t)| t == "settle"), "checked again once it had time");
    r.view_status(&status(3.7, now_s() - 3.0));
    r.event(Event::TimerFired { owner: NAME.into(), token: "settle".into() });
    assert_eq!(r.shown("dot-level"), json!("off"));
    assert!(r.shown("dot-tip").as_str().unwrap().contains("stayed on ~24mm OIS by itself"));
}

#[test]
fn sweeping_the_zoom_under_a_lens_and_back_doesnt_make_the_telephoto_the_unzoomed_lens() {
    let mut r = Rig::new();
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    r.zoom(500);
    r.view_status(&status(3.7, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("4"));
    for _ in 0..3 {
        // quick back and forth, dipping under 3.7x: the phone is still on the tele meanwhile
        r.zoom(350);
        r.view_status(&status(1.0, now_s()));
        r.plugin.on_phone_state(&mut r.host, &lens_state("4"));
        r.zoom(600);
        r.view_status(&status(3.7, now_s()));
        r.plugin.on_phone_state(&mut r.host, &lens_state("4"));
    }
    r.view_status(&status(3.7, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("4"));
    assert_eq!(r.shown("dot-level"), json!("on"));
    assert!(r.shown("dot-tip").as_str().unwrap().starts_with("Switched to Tele ~85mm OIS"));
}

#[test]
fn the_zoom_slider_marks_each_lens() {
    let mut r = Rig::new();
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, 1.0));
    assert_eq!(r.shown("zoom-marks"), json!([3.7]));
    r.view_status(&ViewStatus::default());
    assert_eq!(r.shown("zoom-marks"), json!([]));
}

#[test]
fn a_zoom_right_on_a_lens_mark_gets_that_lens() {
    for lens in [1.51, 2.0, 3.7, 3.9, 5.0, 6.3] {
        let caps = PhoneZoomCaps { ratio_max: 10.0, crop_max: 4.0, freeform: true, lens_zooms: vec![lens] };
        let zoom = lens_step(lens) as f64 / 100.0;
        assert_eq!(view::split_zoom(zoom, 0.0, 0.0, Some(&caps), 1.0).phone.unwrap().ratio, lens);
        // and so the dot doesn't claim it fell back (it did at 3.9x: 1/(1/3.9) is a hair under 3.9)
        assert_eq!(lens_note(zoom, lens, Some(&caps), "Tele", "Main", "Tele", true).0, "on");
    }
}

#[test]
fn back_on_a_tile_the_lenses_come_back_but_after_a_real_stop_they_dont() {
    let mut r = Rig::new();
    r.host.streaming = true;
    r.host.focused = Some("a".into());
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    r.plugin.on_focus_lost(&mut r.host);
    assert_eq!(r.shown("dot-level"), json!(""));
    r.plugin.on_focus_gained(&mut r.host, &stream());
    assert_eq!(r.plugin.default_lens, "~24mm OIS");
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_stream_stop(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream());
    assert_eq!(r.plugin.default_lens, "~24mm OIS", "kept by the stream the panels left only until it stops");
}

#[test]
fn a_hand_over_behind_the_panels_keeps_the_lenses_and_a_new_start_doesnt() {
    let mut r = Rig::new();
    r.host.streaming = true;
    r.host.focused = Some("a".into());
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, now_s() - 3.0));
    r.plugin.on_phone_state(&mut r.host, &lens_state("2"));
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.default_lens = String::new();
    let moved = StreamInfo { url: "http://127.0.0.1:41000/v1/video".into(), ..stream() };
    r.plugin.on_focus_gained(&mut r.host, &moved);
    assert_eq!(r.plugin.default_lens, "~24mm OIS");
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.default_lens = String::new();
    r.plugin.on_focus_gained(&mut r.host, &StreamInfo { session: 1, ..stream() });
    assert_eq!(r.plugin.default_lens, "");
}

// ── Dragging and scrolling the preview ───────────────────────────────

fn drag_rig(rot: &str, flip_h: bool) -> Rig {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"flip_h": flip_h, "rotation": rot, "zoom": 2.5})).unwrap();
    r.event(Event::StreamStopped);
    r
}

#[test]
fn dragging_the_preview_moves_the_picture_with_the_mouse() {
    for (rot, flip_h) in [("None", false), ("90 CW", false), ("180", true), ("90 CCW", true)] {
        let mut r = drag_rig(rot, flip_h);
        let spot = shown_to_view(0.4, 0.55, &r.plugin.geometry());
        r.event(Event::ViewDragged { du: 0.1, dv: -0.05 });
        let g = r.plugin.geometry();
        let back = view_to_shown(spot.0, spot.1, &g);
        assert!((back.0 - 0.5).abs() < 1e-5 && (back.1 - 0.5).abs() < 1e-5, "{rot}: {back:?}");
        assert!(r.saves() >= 1);
        assert_eq!(r.plugin.get_config()["pan_x"].as_f64().unwrap(), r.plugin.pan().0, "kept finer than the slider's steps");
    }
}

#[test]
fn dragging_stops_at_the_edge_and_does_nothing_unzoomed() {
    let mut r = drag_rig("None", false);
    r.event(Event::ViewDragged { du: -5.0, dv: 0.0 });
    assert_eq!(r.plugin.pan().0, 1.0);
    assert_eq!(r.shown("pan-text").as_str().unwrap().lines().next().unwrap(), "H +100%");
    r.zoom(100);
    r.event(Event::ViewDragged { du: 0.3, dv: 0.3 });
    assert_eq!(r.plugin.pan(), (0.0, 0.0));
}

#[test]
fn scrolling_zooms_around_the_mouse() {
    for rot in ["None", "90 CW"] {
        let mut r = drag_rig(rot, false);
        let spot = shown_to_view(0.6, 0.45, &r.plugin.geometry());
        r.event(Event::ViewScrolled { factor: 1.1, u: 0.6, v: 0.45 });
        assert_eq!(r.plugin.zoom(), 2.75);
        let back = view_to_shown(spot.0, spot.1, &r.plugin.geometry());
        assert!((back.0 - 0.6).abs() < 0.01 && (back.1 - 0.45).abs() < 0.01, "the slider's 0.01x steps: {back:?}");
        r.event(Event::ViewScrolled { factor: 1.0 / 1.1f32.powi(20), u: 0.6, v: 0.45 });
        assert!(r.plugin.zoom() == 1.0 && r.plugin.pan().0 == 0.0);
        for _ in 0..40 {
            r.event(Event::ViewScrolled { factor: 1.1, u: 0.5, v: 0.5 });
        }
        assert_eq!(r.plugin.zoom(), 10.0);
    }
}

#[test]
fn small_scrolls_add_up_and_stop_on_a_lens() {
    let mut r = Rig::new();
    r.plugin.on_stream_start(&mut r.host, &stream());
    r.view_status(&status(1.0, 1.0));
    r.zoom(350);
    for _ in 0..10 {
        // a trackpad: well under a slider step each
        r.event(Event::ViewScrolled { factor: 1.002, u: 0.5, v: 0.5 });
    }
    assert!(r.plugin.zoom() > 3.5);
    r.event(Event::ViewScrolled { factor: 1.1, u: 0.5, v: 0.5 });
    assert_eq!(r.plugin.zoom(), 3.7, "stopped on the lens");
    r.event(Event::ViewScrolled { factor: 1.1, u: 0.5, v: 0.5 });
    assert!(r.plugin.zoom() > 3.7);
}

#[test]
fn scrolling_does_nothing_while_something_else_moves_the_view() {
    let mut r = drag_rig("None", false);
    r.event(Event::ViewTaken { owner: "Auto framing".into() });
    r.event(Event::ViewScrolled { factor: 1.5, u: 0.5, v: 0.5 });
    r.event(Event::ViewDragged { du: 0.2, dv: 0.0 });
    assert_eq!((r.plugin.zoom(), r.plugin.pan()), (2.5, (0.0, 0.0)));
    assert_eq!(r.shown("zoom-free"), json!(false));
    assert!(r.shown("zoom-tip").as_str().unwrap().starts_with("Auto framing is moving the zoom and pan"));
    r.event(Event::ViewTaken { owner: String::new() });
    assert_eq!(r.shown("zoom-free"), json!(true));
}

#[test]
fn dragging_the_preview_sticks_at_the_centre_then_lets_go() {
    let mut r = drag_rig("None", false);
    // 2.5x: a pan of 1 is 0.75 frames off centre
    r.event(Event::ViewDragged { du: -0.3, dv: 0.0 });
    assert!(approx(r.plugin.pan().0, 0.4));
    let mut seen = Vec::new();
    for _ in 0..60 {
        // back towards the centre and past it, 0.65% of the frame at a time
        r.event(Event::ViewDragged { du: 0.0065, dv: 0.0 });
        seen.push(r.plugin.pan().0);
    }
    let stuck: Vec<usize> = seen.iter().enumerate().filter(|(_, p)| **p == 0.0).map(|(i, _)| i + 1).collect();
    assert_eq!(stuck, (44..50).collect::<Vec<_>>(), "0.3 - 0.0065k within 2% of the frame either side of the centre");
    assert!((seen[49] - (-0.025 / 0.75)).abs() < 1e-6, "lets go right where the drag is, no jump: {}", seen[49]);
    assert_eq!(r.plugin.pan().1, 0.0);
    // moved another way: the next drag starts from there
    r.ui("pad-pointer", vec![json!(0.5), json!(0.0)]);
    r.event(Event::ViewDragged { du: -0.001, dv: 0.0 });
    assert!(r.plugin.pan().0 > 0.5);
}

#[test]
fn a_saved_half_step_rounds_to_even_like_pythons_round() {
    // 112.5 steps: Python's round() gives 112, not 113, and 137.5 gives 138
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"zoom": 1.125, "stretch_x": 1.125, "stretch_y": 1.375})).unwrap();
    let cfg = r.plugin.get_config();
    assert_eq!((cfg["zoom"].as_f64().unwrap(), cfg["stretch_x"].as_f64().unwrap(), cfg["stretch_y"].as_f64().unwrap()), (1.12, 1.12, 1.38));
    // and the pan read-out, which Python rounds the same way (12.5 gives 12, 13.5 gives 14)
    assert_eq!(TransformsPlugin::pan_label(0.125), TransformsPlugin::pan_label(0.12));
    assert_eq!(TransformsPlugin::pan_label(0.135), TransformsPlugin::pan_label(0.14));
}

#[test]
fn a_double_click_resets_zoom_and_pan() {
    // The pan pad puts itself back to the middle and the zoom slider back to 1 (the slider's default), each leaving the other alone
    let mut r = Rig::new();
    r.zoom(420);
    r.ui("pad-pointer", vec![json!(0.7), json!(0.5)]);
    assert!(r.plugin.pan().0 != 0.0);
    r.ui("pad-reset", vec![]);
    assert_eq!((r.plugin.pan().0, r.plugin.zoom()), (0.0, 4.2));
    r.ui("pad-pointer", vec![json!(0.7), json!(0.5)]);
    r.zoom(100);
    assert_eq!((r.plugin.zoom(), r.plugin.pan().0), (1.0, 0.0));
}
