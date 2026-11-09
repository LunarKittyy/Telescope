//! tests/test_camera_control.py, test for test.

use super::*;
use telescope_core::testing::FakeHost;

struct Rig {
    cam: CameraControl,
    host: FakeHost,
}

impl Rig {
    /// A card with a phone paired, as the Python fixture's phones_changed(1).
    fn new() -> Rig {
        let mut r = Rig { cam: CameraControl::new(), host: FakeHost::new() };
        r.cam.on_event(&mut r.host, &Event::PhonesChanged { count: 1 });
        r.host.calls.clear();
        r
    }

    /// plugin._ctrl = ctrl: a stream to send to, without starting one.
    fn streaming(&mut self) {
        self.cam.stream = Some("url".into());
    }

    /// What the handlers queued goes to the host.
    fn flush(&mut self) {
        self.cam.refresh(&mut self.host);
    }

    fn sent(&self) -> Vec<Value> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("phone_control None ")).map(|b| serde_json::from_str(b).unwrap()).collect()
    }

    fn actions_sent(&self) -> Vec<String> {
        self.sent().iter().map(|b| b["action"].as_str().unwrap().to_string()).collect()
    }

    fn clear(&mut self) {
        self.host.calls.clear();
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    fn start(&mut self) {
        self.cam.on_stream_start(&mut self.host, &StreamInfo { source_id: String::new(), url: "url".into(), session: 0 });
    }

    fn gained(&mut self) {
        self.cam.on_focus_gained(&mut self.host, &StreamInfo { source_id: String::new(), url: "url".into(), session: 0 });
    }

    fn state(&mut self, state: Value) {
        self.cam.on_phone_state(&mut self.host, &state);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.cam.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn event(&mut self, event: Event) {
        self.cam.on_event(&mut self.host, &event);
    }

    fn props(&self) -> Value {
        self.cam.props()
    }

    fn modes(&self) -> Vec<String> {
        self.host.events.iter().filter_map(|e| match e { Event::FocusModeChanged { mode } => Some(mode.clone()), _ => None }).collect()
    }

    fn picked_modes(&self) -> Vec<String> {
        self.host.events.iter().filter_map(|e| match e { Event::FocusModePicked { mode } => Some(mode.clone()), _ => None }).collect()
    }

    fn focus_available(&self) -> Vec<bool> {
        self.host.events.iter().filter_map(|e| match e { Event::FocusPointAvailable { available } => Some(*available), _ => None }).collect()
    }

    fn meter_available(&self) -> Vec<bool> {
        self.host.events.iter().filter_map(|e| match e { Event::MeterPointAvailable { available } => Some(*available), _ => None }).collect()
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs()
}

#[test]
fn the_card_is_greyed_out_until_a_phone_is_paired() {
    let mut r = Rig { cam: CameraControl::new(), host: FakeHost::new() };
    assert_eq!(r.props()["active"], false);
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!(r.props()["active"], true);
    r.event(Event::PhonesChanged { count: 0 });
    assert_eq!(r.props()["active"], false);
}

#[test]
fn derive_camera_control_view_returns_none_for_empty_state() {
    assert!(derive_camera_control_view(&json!({})).is_none());
    assert!(derive_camera_control_view(&Value::Null).is_none());
}

#[test]
fn derive_camera_control_view_maps_auto_and_manual_flags() {
    let view = derive_camera_control_view(&json!({"cameras": [], "auto": false, "wb_manual": true, "focus_mode": "manual"})).unwrap();
    assert!(view.manual_exposure && view.manual_wb && view.manual_focus);
}

#[test]
fn derive_camera_control_view_picks_current_camera_and_its_ranges() {
    let view = derive_camera_control_view(&json!({
        "cameras": [
            {"id": "0", "current": false, "aeCompMin": -8, "aeCompMax": 8},
            {"id": "1", "current": true, "aeCompMin": -3, "aeCompMax": 3, "aeCompStep": 0.5},
        ],
        "auto": true,
    })).unwrap();
    assert_eq!(view.current_camera.unwrap()["id"], "1");
    assert_eq!(view.ae_comp_range, (-3, 3));
    assert_eq!(view.ae_comp_step, 0.5);
}

#[test]
fn derive_camera_control_view_defaults_ae_range_without_current_camera() {
    let view = derive_camera_control_view(&json!({"cameras": [], "auto": true})).unwrap();
    assert!(view.current_camera.is_none());
    assert_eq!((view.ae_comp_range, view.ae_comp_step), ((-8, 8), 0.167));
}

#[test]
fn derive_camera_control_view_maps_nr_and_edge_mode_indices() {
    let view = derive_camera_control_view(&json!({"cameras": [], "auto": true, "nr_mode": 2, "edge_mode": 0})).unwrap();
    assert_eq!((view.nr_mode_index, view.edge_mode_index), (2, 0));
    // An unrecognized mode value falls back to index 1 ("Fast")
    assert_eq!(derive_camera_control_view(&json!({"cameras": [], "auto": true, "nr_mode": 99})).unwrap().nr_mode_index, 1);
}

#[test]
fn kelvin_gains_are_symmetric_and_tint_is_clamped() {
    let neutral = kelvin_to_rggb(5500.0, 0.0);
    let cool = kelvin_to_rggb(2000.0, 0.0);
    let warm = kelvin_to_rggb(10000.0, 0.0);
    let magenta = kelvin_to_rggb(5500.0, 10_000.0);
    let green = kelvin_to_rggb(5500.0, -10_000.0);
    assert_eq!(neutral, (2.0, 1.0, 1.0, 2.0));
    assert!(cool.0 < neutral.0 && cool.3 > neutral.3);
    assert!(warm.0 > neutral.0 && warm.3 < neutral.3);
    assert_eq!((magenta.1, magenta.2), (0.5, 0.5));
    assert_eq!((green.1, green.2), (2.5, 2.5));
}

#[test]
fn diopters_label() {
    for (d, expected) in [(0.0, "∞"), (0.01, "∞"), (0.5, "2.00 m"), (2.0, "0.50 m")] {
        assert_eq!(diopters_to_label(d), expected);
    }
}

#[test]
fn stream_start_pushes_all_default_settings() {
    let mut r = Rig::new();
    r.start();
    assert_eq!(r.actions_sent(), ["auto", "wb_auto", "ois", "focus_mode", "ae_comp", "nr_mode", "edge_mode", "black_level_lock"]);
    assert_eq!(r.props()["lens-placeholder"], "Loading lenses...");
}

#[test]
fn manual_handlers_send_controls_and_schedule_saves() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.set_exp(true);
    r.cam.on_exp_mode();
    r.cam.on_iso_changed(321.9);
    r.cam.on_shutter_changed(2_000_000.9);
    r.cam.set_wb_mode(true);
    r.cam.on_wb_mode();
    r.cam.on_wb_changed(6200);
    r.cam.on_tint_changed(25);
    r.flush();
    let actions = r.actions_sent();
    assert_eq!(actions[..2], ["iso", "shutter"]);
    assert!(r.sent().contains(&json!({"action": "iso", "value": 321})));
    assert!(r.sent().contains(&json!({"action": "shutter", "value": 2_000_000})));
    assert_eq!(actions.iter().filter(|a| *a == "wb_gains").count(), 3);
    assert!(r.saves() >= 6);
}

#[test]
fn auto_mode_handlers_send_reset_commands() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.set_exp(false);
    r.cam.on_exp_mode();
    r.cam.set_wb_mode(false);
    r.cam.on_wb_mode();
    r.cam.focus.index = 0;
    r.cam.on_focus_mode();
    r.flush();
    assert_eq!(r.actions_sent(), ["auto", "wb_auto", "focus_mode"]);
    assert_eq!(r.sent().last().unwrap()["value"], "continuous");
}

#[test]
fn manual_controls_are_progressively_disclosed() {
    let mut r = Rig::new();
    let shown = |p: &Value| (p["show-manual-exposure"].clone(), p["show-manual-wb"].clone(), p["show-manual-focus"].clone());
    assert_eq!(shown(&r.props()), (json!(false), json!(false), json!(false)));
    r.ui("exposure-picked", vec![json!(1)]);
    r.ui("wb-picked", vec![json!(1)]);
    r.ui("focus-picked", vec![json!(2)]);
    assert_eq!(shown(&r.props()), (json!(true), json!(true), json!(true)));
}

#[test]
fn discrete_control_handlers_send_selected_values() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.on_ois(false);
    r.cam.on_ae_comp_changed(3);
    r.cam.on_nr_mode_changed(2);
    r.cam.on_edge_mode_changed(0);
    r.cam.on_bll_changed(true);
    r.cam.on_torch_toggled(true);
    r.flush();
    assert_eq!(r.sent(), [
        json!({"action": "ois", "value": "0"}),
        json!({"action": "ae_comp", "value": 3}),
        json!({"action": "nr_mode", "value": 2}),
        json!({"action": "edge_mode", "value": 0}),
        json!({"action": "black_level_lock", "value": "1"}),
        json!({"action": "torch", "value": "1"}),
    ]);
    assert_eq!(r.saves(), 5); // torch is intentionally live-only, not persisted
}

#[test]
fn phone_state_populates_camera_capabilities_and_controls() {
    let mut r = Rig::new();
    r.state(json!({
        "cameras": [{
            "id": "tele", "label": "Tele", "current": true, "isoMin": 100, "isoMax": 1600, "shutterMinNs": 200_000,
            "shutterMaxNs": 500_000_000, "supportsManualSensor": true, "supportsManualWB": true, "supportsManualFocus": true,
            "minFocusDistance": 5.0, "supportsFlash": true, "hasOis": false, "hwLevel": "FULL", "aeCompMin": -3, "aeCompMax": 4,
            "aeCompStep": 0.5,
        }],
        "auto": false, "iso": 400, "shutter_ns": 10_000_000, "wb_manual": true, "ois": false, "focus_mode": "manual",
        "focus_distance": 2.5, "ae_comp": 2, "nr_mode": 2, "edge_mode": 0, "black_level_lock": true, "torch": true,
    }));
    let c = &r.cam;
    assert_eq!(c.lenses.checked, Some(0));
    assert!(c.manual_exp && c.manual_wb && c.manual_focus);
    assert!(close(c.iso.get_value(), 400.0, 0.01));
    assert!(close(c.shutter.get_value(), 10_000_000.0, 0.01));
    assert_eq!((c.ae.min, c.ae.max), (-3, 4));
    assert_eq!(c.ae_text, "+1.0 EV");
    assert_eq!(c.focus_slider.value, 500);
    assert!(!c.ois.enabled && c.torch.enabled);
    assert_eq!(c.info_text, "Full manual control");
}

#[test]
fn empty_phone_state_and_stop_clear_camera_ui() {
    let mut r = Rig::new();
    r.state(json!({}));
    assert_eq!(r.cam.lenses.placeholder, "Unavailable");
    r.streaming();
    r.cam.on_stream_stop(&mut r.host);
    assert!(r.cam.stream.is_none() && r.cam.lenses.cameras.is_empty());
    assert_eq!(r.props()["lens-placeholder"], "Start streaming to load lenses");
    assert_eq!(r.props()["info"], ""); // an empty note doesn't show
}

#[test]
fn capability_line_names_only_what_the_lens_supports() {
    let mut r = Rig::new();
    r.cam.update_cam_info(&json!({"hwLevel": "LEVEL_3", "supportsManualSensor": true, "supportsManualWB": false,
                                   "supportsManualFocus": true, "hasOis": false}));
    assert_eq!(r.cam.info_text, "Limited manual control");
    let tip = &r.cam.info_tip;
    assert!(tip.contains("Camera level: Level 3") && tip.contains("Supports: Manual exposure, Manual focus"), "{tip}");
    assert!(tip.contains("Not supported: Manual WB, Point focus, OIS"), "{tip}");
}

#[test]
fn capability_line_is_rebuilt_not_appended() {
    let mut r = Rig::new();
    let cam = json!({"hwLevel": "FULL", "supportsManualSensor": true, "supportsManualWB": true, "supportsManualFocus": true});
    r.cam.update_cam_info(&cam);
    r.cam.update_cam_info(&cam);
    assert_eq!(r.cam.info_text, "Full manual control");
    assert_eq!(r.cam.info_tip.matches("Supports:").count(), 1);
}

#[test]
fn saved_point_focus_shortcuts_accept_the_previous_name() {
    let mut r = Rig::new();
    r.event(Event::PhonesChanged { count: 1 });
    r.cam.focus.enabled[1] = true;
    assert!(r.cam.run_action(&mut r.host, "camera.focus_mode", &json!({"op": "set", "option": "Point"})).is_some());
    assert_eq!(r.cam.focus.index, 1);
}

#[test]
fn unknown_hardware_level_passes_through_verbatim() {
    let mut r = Rig::new();
    r.cam.update_cam_info(&json!({"hwLevel": "WHO_KNOWS"}));
    assert!(r.cam.info_tip.contains("Camera level: WHO_KNOWS"));
}

#[test]
fn capability_gating_forces_unsupported_modes_off() {
    let mut r = Rig::new();
    r.cam.set_exp(true);
    r.cam.set_wb_mode(true);
    r.cam.focus.index = 2;
    r.cam.update_camera_caps(Caps { manual_sensor: false, manual_wb: false, manual_focus: false, min_focus_distance: 0.0, flash: false,
                                    ois: false });
    let c = &r.cam;
    assert!(!c.exp.enabled[1] && c.exp.index == 0);
    assert!(!c.wb.enabled[1] && !c.focus.enabled[2]);
    assert!(!c.torch.enabled && !c.ois.enabled);
    assert_eq!(c.focus_max_diopters, 10.0);
}

#[test]
fn lens_selection_updates_ranges_caps_and_sends_camera() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.on_lens_selected(&json!({"id": "wide", "label": "Wide", "isoMin": 80, "isoMax": 800, "shutterMinNs": 300_000,
                                    "shutterMaxNs": 300_000_000, "aeCompMin": -2, "aeCompMax": 2, "aeCompStep": 1.0}));
    r.flush();
    assert_eq!(r.sent()[0], json!({"action": "camera", "id": "wide"}));
    assert_eq!((r.cam.iso.v_min, r.cam.iso.v_max), (80.0, 800.0));
    assert_eq!((r.cam.ae.min, r.cam.ae.max), (-2, 2));
}

#[test]
fn focus_slider_conversion_and_manual_send() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.focus_max_diopters = 8.0;
    r.cam.manual_focus = true;
    r.cam.on_focus_slider(250);
    r.flush();
    assert_eq!(r.cam.slider_to_diopters(250), 2.0);
    assert_eq!(r.cam.focus_text, "0.50 m");
    assert_eq!(r.sent().last().unwrap(), &json!({"action": "focus_distance", "value": 2.0}));
    r.cam.set_focus_slider_value(100.0);
    assert_eq!(r.cam.focus_slider.value, 1000);
}

#[test]
fn camera_config_round_trip_for_supported_values() {
    let mut r = Rig::new();
    r.cam.focus_max_diopters = 10.0;
    let cfg = json!({"exp_manual": true, "iso": 500, "shutter_ns": 12_000_000, "ois": false, "focus_manual": true, "focus_diopters": 3,
                     "wb_manual": true, "wb_kelvin": 6500, "wb_tint": -20, "ae_comp": 2, "nr_mode": 2, "edge_mode": 0, "bll": true});
    r.cam.set_config(&cfg).unwrap();
    let got = r.cam.get_config();
    for key in ["exp_manual", "ois", "focus_manual", "wb_manual", "wb_kelvin", "wb_tint", "ae_comp", "nr_mode", "edge_mode", "bll"] {
        assert_eq!(got[key], cfg[key], "{key}");
    }
    for key in ["iso", "shutter_ns", "focus_diopters"] {
        assert!(close(got[key].as_f64().unwrap(), cfg[key].as_f64().unwrap(), 0.01), "{key}");
    }
}

#[test]
fn saved_manual_modes_are_pushed_to_phone_on_stream_start() {
    let mut r = Rig::new();
    r.cam.set_config(&json!({"exp_manual": true, "iso": 400, "shutter_ns": 5_000_000, "focus_manual": true, "focus_diopters": 2})).unwrap();
    r.start();
    let actions = r.actions_sent();
    assert!(actions.contains(&"iso".to_string()) && actions.contains(&"shutter".to_string()));
    assert!(r.sent().contains(&json!({"action": "focus_mode", "value": "manual"})));
}

#[test]
fn default_config_resets_boolean_camera_settings() {
    let mut r = Rig::new();
    r.cam.set_config(&json!({"exp_manual": true, "focus_manual": true, "wb_manual": true, "bll": true})).unwrap();
    r.cam.set_config(&json!({"exp_manual": false, "focus_manual": false, "wb_manual": false, "bll": false})).unwrap();
    let cfg = r.cam.get_config();
    assert_eq!((cfg["exp_manual"].clone(), cfg["focus_manual"].clone(), cfg["wb_manual"].clone(), cfg["bll"].clone()),
               (json!(false), json!(false), json!(false), json!(false)));
}

fn point_state(focus_mode: &str, supports: bool, meter: bool) -> Value {
    json!({
        "cameras": [{"id": "0", "label": "Main", "current": true, "supportsManualSensor": true, "supportsManualWB": true,
                     "supportsManualFocus": true, "minFocusDistance": 10.0, "supportsFocusPoint": supports, "supportsMeterPoint": meter}],
        "auto": true, "focus_mode": focus_mode,
    })
}

#[test]
fn focus_on_a_point_from_the_preview_or_the_point_button() {
    let mut r = Rig::new();
    r.start();
    r.state(point_state("continuous", true, false));
    assert!(r.cam.focus.enabled[1] && r.focus_available().last() == Some(&true));

    r.clear();
    r.event(Event::FocusPoint { x: 0.25, y: 0.75 });
    assert_eq!(r.sent(), [json!({"action": "focus_point", "x": 0.25, "y": 0.75})]);
    assert_eq!(r.cam.focus.index, 1);

    r.host.events.clear();
    r.ui("focus-picked", vec![json!(0)]);
    assert_eq!(r.sent().last().unwrap(), &json!({"action": "focus_mode", "value": "continuous"}));
    r.ui("focus-picked", vec![json!(1)]);
    // the centre of what's shown, mapped by transforms like a click
    let picked: Vec<(f32, f32)> = r.host.events.iter().filter_map(|e| match e { Event::FocusPointPicked { x, y } => Some((*x, *y)), _ => None }).collect();
    assert_eq!(picked, [(0.5, 0.5)]);
}

#[test]
fn point_focus_follows_the_phone_and_the_lens() {
    let mut r = Rig::new();
    r.start();
    r.state(point_state("point", true, false));
    assert_eq!(r.cam.focus.index, 1);
    r.state(point_state("continuous", false, false));
    assert!(!r.cam.focus.enabled[1] && r.cam.focus.index == 0);
    r.clear();
    r.event(Event::FocusPoint { x: 0.5, y: 0.5 });
    assert!(r.sent().is_empty()); // this lens can't
    r.cam.on_stream_stop(&mut r.host);
    assert!(!r.cam.focus.enabled[1]);
}

#[test]
fn a_lens_that_can_only_meter_takes_a_meter_point_and_tap_a_point_stays_off() {
    let mut r = Rig::new();
    r.start();
    r.state(point_state("continuous", false, true));
    assert!(r.focus_available().last() == Some(&false) && r.meter_available().last() == Some(&true) && !r.cam.focus.enabled[1]);
    r.clear();
    r.event(Event::MeterPoint { x: 0.25, y: 0.75 });
    assert_eq!(r.sent(), [json!({"action": "meter_point", "x": 0.25, "y": 0.75})]);
    assert_eq!(r.cam.focus.index, 0);
    r.clear();
    r.event(Event::FocusPoint { x: 0.25, y: 0.75 });
    assert!(r.sent().is_empty()); // Tap a point works as before: not on this lens
    r.state(point_state("continuous", false, false)); // an older phone doesn't say
    r.event(Event::MeterPoint { x: 0.5, y: 0.5 });
    assert!(r.sent().is_empty() && r.meter_available().last() == Some(&false));
    r.state(point_state("continuous", false, true));
    r.cam.on_stream_stop(&mut r.host);
    assert_eq!(r.meter_available().last(), Some(&false));
}

#[test]
fn compensation_moves_freely_and_all_three_reset_on_a_double_click() {
    // Brightness doesn't snap to 0 (its centre line shows where 0 is); all three have a default a double-click goes back to (the slider's own job in ui-kit)
    let card = include_str!("../../ui/cards/camera.slint");
    let brightness = &card[card.find("shortcut: \"camera.compensation\";").unwrap()..];
    let brightness = &brightness[..brightness.find("moved(v)").unwrap()];
    assert!(!brightness.contains("marks:") && brightness.contains("default-value: 0;"));
    assert_eq!(card.matches("has-default: true;").count(), 3);
    assert_eq!(card.matches("marks:").count(), 0);
    let mut r = Rig::new();
    r.streaming();
    r.cam.kelvin.enabled = true;
    r.cam.tint.enabled = true;
    r.ui("ae-moved", vec![json!(3)]);
    r.ui("tint-moved", vec![json!(40)]);
    r.ui("kelvin-moved", vec![json!(3300)]);
    r.clear();
    // A double-click comes back as a move to the default, through the normal handlers
    r.ui("ae-moved", vec![json!(0)]);
    r.ui("kelvin-moved", vec![json!(5500)]);
    r.ui("tint-moved", vec![json!(0)]);
    assert_eq!((r.cam.ae.value, r.cam.kelvin.value, r.cam.tint.value), (0, 5500, 0));
    assert!(r.sent().contains(&json!({"action": "ae_comp", "value": 0})));
}

#[test]
fn a_lens_switch_keeps_iso_and_shutter_clamped_to_the_new_lens() {
    let mut r = Rig::new();
    r.streaming();
    r.cam.on_lens_selected(&json!({"id": "0", "label": "Main", "isoMin": 50, "isoMax": 6400, "shutterMinNs": 100_000,
                                    "shutterMaxNs": 1_000_000_000}));
    r.cam.iso.set_value(400.0);
    r.cam.shutter.set_value(16_666_667.0);
    r.cam.on_lens_selected(&json!({"id": "2", "label": "Wide", "isoMin": 100, "isoMax": 3200, "shutterMinNs": 20_000,
                                    "shutterMaxNs": 10_000_000}));
    assert!(close(r.cam.iso.get_value(), 400.0, 0.02));
    assert_eq!(r.cam.iso.spin, 400.0);
    assert!(close(r.cam.shutter.get_value(), 10_000_000.0, 0.02)); // past the new maximum
}

#[test]
fn manual_focus_at_infinity_loads_from_a_preset() {
    let mut r = Rig::new();
    r.cam.focus_max_diopters = 10.0;
    r.cam.set_config(&json!({"focus_manual": true, "focus_diopters": 2.0})).unwrap();
    r.cam.set_config(&json!({"focus_manual": true, "focus_diopters": 0.0})).unwrap();
    assert_eq!(r.cam.focus_slider.value, 0);
}

#[test]
fn low_light_shows_only_once_the_phone_says_it_takes_it() {
    let mut r = Rig::new();
    let state = json!({"cameras": [{"id": "0", "label": "Main", "current": true}], "auto": true});
    let with = |extra: Value| {
        let mut s = state.clone();
        s.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        s
    };
    r.state(state.clone());
    let tile = |r: &Rig| (r.props()["low-light"]["enabled"].clone(), r.props()["low-light"]["tip"].as_str().unwrap().to_string());
    assert_eq!(tile(&r).0, false); // an older phone app, or not streaming yet: greyed out, with why
    assert!(!tile(&r).1.is_empty());
    r.state(with(json!({"low_light_toggle": true})));
    assert_eq!(tile(&r).0, true);
    assert!(!r.cam.low_light.checked);
    r.streaming();
    r.clear();
    r.state(with(json!({"low_light_toggle": true, "low_light": true})));
    assert!(r.cam.low_light.checked);
    assert!(r.sent().is_empty()); // following the phone isn't a new request
    r.cam.on_stream_stop(&mut r.host);
    assert_eq!(tile(&r).0, false); // until the next phone says it takes it
}

#[test]
fn low_light_shortcut_toggles_only_on_a_phone_that_takes_it() {
    let mut r = Rig::new();
    r.streaming();
    assert_eq!(r.cam.run_action(&mut r.host, "camera.low_light", &json!({})), None); // hidden: an older phone app doesn't know it
    r.state(json!({"cameras": [{"id": "0", "label": "Main", "current": true}], "auto": true, "low_light_toggle": true}));
    r.clear();
    assert_eq!(r.cam.run_action(&mut r.host, "camera.low_light", &json!({})).as_deref(), Some("Low light: on"));
    assert_eq!(r.sent(), [json!({"action": "low_light", "value": "1"})]);
}

#[test]
fn low_light_toggle_sends_and_saves() {
    let mut r = Rig::new();
    r.streaming();
    r.ui("low-light-toggled", vec![json!(true)]);
    r.ui("low-light-toggled", vec![json!(false)]);
    assert_eq!(r.sent(), [json!({"action": "low_light", "value": "1"}), json!({"action": "low_light", "value": "0"})]);
    assert_eq!(r.saves(), 2);
}

#[test]
fn low_light_is_saved_and_sent_at_stream_start_only_when_on() {
    let mut r = Rig::new();
    assert_eq!(r.cam.get_config()["low_light"], false);
    r.cam.set_config(&json!({"low_light": true})).unwrap();
    assert_eq!(r.cam.get_config()["low_light"], true);
    r.start();
    assert_eq!(r.sent().last().unwrap(), &json!({"action": "low_light", "value": "1"}));
    r.cam.on_stream_stop(&mut r.host);
    r.cam.set_config(&json!({"low_light": false})).unwrap();
    r.clear();
    r.start();
    assert!(!r.actions_sent().contains(&"low_light".to_string()));
}

#[test]
fn back_on_a_tile_nothing_is_sent_but_after_a_real_stop_everything_is() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": [{"id": "0", "label": "Back", "current": true}], "auto": true}));
    r.cam.on_focus_lost(&mut r.host);
    r.clear();
    r.gained();
    assert!(r.sent().is_empty()); // the phone still has its settings
    r.cam.on_stream_stop(&mut r.host);
    r.gained();
    assert!(!r.sent().is_empty()); // nothing kept from a stream that ended
}

#[test]
fn a_hand_over_behind_the_panels_sends_nothing_and_a_new_start_sends_everything() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": [{"id": "0", "label": "Back", "current": true}], "auto": true}));
    r.cam.on_focus_lost(&mut r.host);
    r.clear();
    r.cam.on_focus_gained(&mut r.host, &StreamInfo { source_id: String::new(), url: "moved".into(), session: 0 });
    assert!(r.sent().is_empty(), "the url moved with the route: the phone still has its settings");
    r.cam.on_focus_lost(&mut r.host);
    r.cam.on_focus_gained(&mut r.host, &StreamInfo { source_id: String::new(), url: "moved".into(), session: 1 });
    assert!(!r.sent().is_empty(), "the same url, but it started again");
}

#[test]
fn the_focus_mode_is_published_however_it_changes() {
    let mut r = Rig::new();
    r.cam.on_plugins_ready(&mut r.host);
    assert_eq!(r.modes(), ["auto"]);
    r.cam.set_config(&json!({"focus_manual": true})).unwrap(); // a saved Manual focus coming back
    r.flush(); // set_config has no host: it's said with the next call that has one
    assert_eq!(r.modes().last().unwrap(), "manual");
    assert!(r.picked_modes().is_empty());
    r.start();
    assert_eq!(r.modes()[r.modes().len() - 2..], ["manual", "manual"]); // said again for the new stream
    r.ui("focus-picked", vec![json!(0)]);
    assert_eq!(r.modes().last().unwrap(), "auto");
    assert_eq!(r.picked_modes(), ["auto"]);
}

#[test]
fn lens_labels_become_tiles() {
    assert_eq!(split_lens_label("Auto ~24mm OIS"), ("24".into(), "mm".into(), "Auto · OIS".into()));
    assert_eq!(split_lens_label("2x"), ("2".into(), "x".into(), String::new()));
    assert_eq!(split_lens_label("Main"), ("Main".into(), String::new(), String::new()));
}

// tests/test_widgets.py::test_ns_to_display
#[test]
fn ns_to_display_reads_exposure_times() {
    for (value, expected) in [(0, "?"), (-1, "?"), (1_000_000_000, "1.0 s"), (2_500_000_000, "2.5 s"), (500_000_000, "1/2"), (1_000_000, "1/1,000")] {
        assert_eq!(ns_to_display(value), expected, "{value}");
    }
}

// tests/test_widgets.py::test_integer_log_slider_syncs_slider_spin_and_signal
#[test]
fn integer_log_slider_syncs_slider_spin_and_signal() {
    let mut row = LogRow::new(10.0, 1000.0, 1.0, false, false);
    row.set_value(100.0);
    assert!(close(row.get_value(), 100.0, 0.01));
    assert_eq!(row.spin, 100.0);
    assert_eq!(row.text(), "ISO 100");
    row.pos = LOG_STEPS;
    assert!(close(row.get_value(), 1000.0, 0.001));
    assert_eq!(row.to_spin(row.get_value()), 1000.0);
}

// tests/test_widgets.py::test_double_log_slider_converts_scaled_spin_value
#[test]
fn double_log_slider_converts_scaled_spin_value() {
    let mut row = LogRow::new(100_000.0, 1_000_000_000.0, 1e-6, true, true);
    let val = row.typed("20.5").unwrap();
    assert!(close(val, 20_500_000.0, 1e-9), "{val}");
    assert!(close(row.get_value(), 20_500_000.0, 0.01));
    assert_eq!((row.spin, row.spin_text().as_str()), (20.5, "20.50"));
    assert_eq!(row.text(), ns_to_display(20_500_000));
    row.typed("0,5");
    assert_eq!(row.spin, 0.5, "a comma is a point too");
    row.typed("9999");
    assert_eq!(row.spin, 1000.0, "held to the box's range");
    row.typed("0.0044");
    assert!(close(row.spin, 0.1, 1e-9), "and to its range after two decimals");
    assert_eq!(row.typed("soon"), None);
    assert!(close(row.spin, 0.1, 1e-9), "not a number: nothing moves");
    let mut iso = LogRow::new(50.0, 6400.0, 1.0, false, false);
    assert_eq!(iso.typed("400.4"), Some(400.0));
    assert_eq!(iso.spin_text(), "400");
}

// tests/test_widgets.py::test_log_slider_range_and_enabled_state
#[test]
fn log_slider_range_and_enabled_state() {
    let mut row = LogRow::new(10.0, 1000.0, 1.0, false, false);
    row.set_value(100.0);
    row.set_range(20.0, 2000.0);
    row.enabled = false;
    assert_eq!((row.v_min, row.v_max), (20.0, 2000.0));
    assert_eq!((row.to_spin(1.0), row.to_spin(1e9)), (20.0, 2000.0));
    assert!(!row.enabled);
    assert!(close(row.get_value(), 100.0, 0.01));
}

// tests/test_widgets.py::test_empty_lenses_show_a_placeholder
#[test]
fn empty_lenses_show_a_placeholder() {
    let mut lenses = Lenses::new();
    lenses.load(&[]);
    assert!(lenses.cameras.is_empty());
    assert_eq!(lenses.placeholder, "No cameras available");
}

// tests/test_widgets.py::test_lens_panel_load_select_placeholder_and_clear
#[test]
fn lens_panel_load_select_placeholder_and_clear() {
    let mut lenses = Lenses::new();
    let cameras = [json!({"id": "wide", "label": "Wide", "current": false}), json!({"id": "tele", "label": "Tele", "current": true})];
    lenses.load(&cameras);
    assert_eq!(lens_label(&lenses.cameras[0]), "Wide");
    assert_eq!(lenses.checked, Some(1));
    assert_eq!(lenses.select_id("wide"), Some(cameras[0].clone()));
    assert_eq!(lenses.checked, Some(0));
    assert_eq!(lenses.select_id("none"), None);
    lenses.set_placeholder("Unavailable".into());
    assert_eq!(lenses.placeholder, "Unavailable");
    lenses.clear();
    assert!(lenses.cameras.is_empty());
    assert_eq!(lenses.placeholder, "Start streaming to load lenses");
}

// tests/test_widgets.py::test_lens_labels_keep_the_zoom_factor_when_shortened
#[test]
fn lens_labels_keep_the_zoom_factor_when_shortened() {
    assert_eq!(shorten_lens_label("Back Telephoto 3x [phys]", true), "Tele 3x");
    assert_eq!(shorten_lens_label("Back ~24mm OIS [auto]", true), "Auto ~24mm OIS");
}

// tests/test_widgets.py::test_lens_tiles_split_the_focal_length_from_the_rest_and_stay_in_even_rows
#[test]
fn lens_tiles_split_the_focal_length_from_the_rest_and_stay_in_even_rows() {
    assert_eq!(split_lens_label("Auto ~24mm OIS"), ("24".into(), "mm".into(), "Auto · OIS".into()));
    assert_eq!(split_lens_label("F/~25mm"), ("25".into(), "mm".into(), "Front".into()));
    assert_eq!(split_lens_label("Tele 3x"), ("3".into(), "x".into(), "Tele".into()));
    assert_eq!(split_lens_label("Wide"), ("Wide".into(), String::new(), String::new()));
    assert_eq!([1, 3, 5, 6, 7, 10, 11].map(lens_columns), [1, 3, 5, 3, 4, 5, 4]);
    assert_eq!(lens_columns(0), 1);
}

#[test]
fn the_card_compiles_and_has_every_property_the_plugin_sets() {
    let def = crate::compile::compile("camera.slint", include_str!("../../ui/cards/camera.slint"), vec![], "CameraCard").unwrap();
    let names: Vec<String> = def.properties().map(|(name, _)| name.replace('_', "-")).collect();
    for key in Rig::new().props().as_object().unwrap().keys() {
        assert!(names.contains(key), "camera.slint has no {key}");
    }
    let callbacks: Vec<String> = def.callbacks().map(|c| c.replace('_', "-")).collect();
    for cb in ["lens-picked", "exposure-picked", "iso-picked", "shutter-picked", "ae-moved", "wb-picked", "kelvin-moved", "tint-moved",
               "focus-picked", "focus-moved", "ois-toggled", "low-light-toggled", "bll-toggled", "torch-toggled", "nr-picked", "edge-picked",
               "iso-fine", "shutter-fine", "iso-typed", "shutter-typed"] {
        assert!(callbacks.contains(&cb.to_string()), "camera.slint has no {cb}");
    }
}

// ── tests/test_presets.py, the ones about the Camera card ──

fn cams() -> Value {
    json!([{"id": "0", "label": "Back camera", "current": true}, {"id": "2", "label": "Back camera (wide)", "current": false}])
}

fn wide_range_cams() -> Value {
    json!([
        {"id": "0", "label": "Back camera", "current": true, "supportsManualFocus": true, "minFocusDistance": 5.0, "aeCompMin": -8, "aeCompMax": 8},
        {"id": "2", "label": "Back camera (macro)", "current": false, "supportsManualFocus": true, "minFocusDistance": 10.0,
         "aeCompMin": -12, "aeCompMax": 12},
    ])
}

fn with(cfg: Value, extra: Value) -> Value {
    let mut cfg = cfg;
    cfg.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    cfg
}

fn preset(r: &mut Rig, cfg: Value) {
    r.cam.apply_preset(&mut r.host, &cfg).unwrap();
}

#[test]
fn camera_saves_the_lens_and_applying_switches_it_before_sending_settings() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": cams(), "auto": true}));
    assert_eq!(r.cam.get_config()["lens"], "0");
    let saved = with(r.cam.get_config(), json!({"lens": "2", "exp_manual": true, "iso": 400, "shutter_ns": 10_000_000}));
    r.clear();
    preset(&mut r, saved);
    let sent = r.sent();
    assert_eq!(sent[0], json!({"action": "camera", "id": "2"}));
    let iso = sent.iter().find(|m| m["action"] == "iso").unwrap()["value"].as_f64().unwrap();
    assert!(close(iso, 400.0, 0.02)); // the log slider's step
    assert!(!sent.contains(&json!({"action": "auto"})));
    assert_eq!(r.cam.get_config()["lens"], "2");
    assert_eq!(r.cam.exp.index, 1);
}

#[test]
fn a_preset_for_another_lens_sends_its_own_values_not_the_live_lens_limits() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": wide_range_cams(), "auto": true}));
    r.clear();
    let cfg = with(r.cam.get_config(), json!({"lens": "2", "focus_manual": true, "focus_diopters": 8.0, "ae_comp": 12}));
    preset(&mut r, cfg);
    let sent = r.sent();
    let focus: Vec<f64> = sent.iter().filter(|m| m["action"] == "focus_distance").map(|m| m["value"].as_f64().unwrap()).collect();
    let ae: Vec<i64> = sent.iter().filter(|m| m["action"] == "ae_comp").map(|m| m["value"].as_i64().unwrap()).collect();
    assert!((focus.last().unwrap() - 8.0).abs() <= 0.05 && *ae.last().unwrap() == 12, "{sent:?}");
}

#[test]
fn switching_lens_keeps_the_manual_focus_distance() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": wide_range_cams(), "auto": true}));
    let cfg = with(r.cam.get_config(), json!({"focus_manual": true, "focus_diopters": 2.0}));
    r.cam.set_config(&cfg).unwrap();
    r.cam.on_lens_selected(&wide_range_cams()[1]);
    assert!((r.cam.get_config()["focus_diopters"].as_f64().unwrap() - 2.0).abs() <= 0.05);
}

#[test]
fn camera_preset_for_the_live_lens_sends_no_switch() {
    let mut r = Rig::new();
    r.start();
    r.state(json!({"cameras": cams(), "auto": true}));
    r.clear();
    let cfg = with(r.cam.get_config(), json!({"lens": "0"}));
    preset(&mut r, cfg);
    let cfg = with(r.cam.get_config(), json!({"lens": "gone"}));
    preset(&mut r, cfg);
    assert!(r.sent().iter().all(|m| m["action"] != "camera"));
    assert!(r.sent().contains(&json!({"action": "auto"})));
}

#[test]
fn camera_preset_while_idle_only_loads() {
    let mut r = Rig::new();
    let cfg = with(r.cam.get_config(), json!({"wb_manual": true, "wb_kelvin": 3200, "lens": "2"}));
    preset(&mut r, cfg);
    assert_eq!(r.cam.wb.index, 1);
    assert_eq!(r.cam.get_config()["lens"], "2");
    assert!(r.sent().is_empty());
}

#[test]
fn a_lens_from_a_preset_applied_while_idle_goes_out_when_the_stream_starts() {
    let mut r = Rig::new();
    let cfg = with(r.cam.get_config(), json!({"lens": "2"}));
    preset(&mut r, cfg);
    r.start();
    r.state(json!({"cameras": cams(), "auto": true}));
    r.state(json!({"cameras": cams(), "auto": true})); // a later state from the old lens doesn't switch again
    let switches: Vec<Value> = r.sent().into_iter().filter(|m| m["action"] == "camera").collect();
    assert_eq!(switches, [json!({"action": "camera", "id": "2"})]);
}

// test_browser_camera.py::test_phone_only_cards_hide_while_a_source_is_picked, the Camera card's part (the mic's is the
// Microphone card's)
#[test]
fn phone_only_cards_hide_while_a_source_is_picked() {
    let mut r = Rig { cam: CameraControl::new(), host: FakeHost::new() };
    assert_eq!(r.props()["active"], false); // nothing paired yet
    r.event(Event::SourceSelected { source_id: "browser".into() });
    assert_eq!(r.props()["phone-only-hidden"], true);
    r.event(Event::SourceSelected { source_id: String::new() });
    assert_eq!((r.props()["phone-only-hidden"].clone(), r.props()["active"].clone()), (json!(false), json!(false)));
}

// Sol's review, finding 1: the panels go A, B, A through the real window state, which tells the plugins about a focus
// change before it updates its own picture of what's shown (streams.rs syncs it once the flow is through)
#[test]
fn back_on_a_after_b_nothing_is_sent_and_a_keeps_its_own_range() {
    use crate::stream_flow::Hook;
    let path = telescope_core::testing::scratch_dir("camera-focus").join("telescope_config.json");
    let mut state = crate::shell::State::new(telescope_core::config::ConfigFile::at(&path));
    state.register(Box::new(CameraControl::new())).unwrap();
    let names = vec!["camera_control".to_string()];
    let info = |sid: &str| StreamInfo { source_id: sid.into(), url: format!("http://{sid}/"), session: 0 };
    let lens = |max: i64| json!({"cameras": [{"id": "0", "label": "Back", "current": true, "isoMin": 50, "isoMax": max}],
                                 "auto": false, "iso": max});
    let i = state.index("camera_control").unwrap();
    (state.streams.streaming, state.streams.source_id) = (true, Some("A".into()));
    state.call_hook(&names, &Hook::StreamStart(info("A")));
    state.call(i, |p, host| p.on_phone_state(host, &lens(10000)));
    // A to B: the window's snapshot still says A while both hooks run
    state.call_hook(&names, &Hook::FocusLost);
    state.call_hook(&names, &Hook::FocusGained(info("B")));
    state.streams.source_id = Some("B".into());
    state.call(i, |p, host| p.on_phone_state(host, &lens(6400)));
    // B to A, the same way round
    state.call_hook(&names, &Hook::FocusLost);
    state.effects.clear();
    state.call_hook(&names, &Hook::FocusGained(info("A")));
    state.streams.source_id = Some("A".into());
    let sent: Vec<String> = state.effects.iter().filter_map(|e| match e {
        crate::shell::Effect::PhoneControl { body, .. } => Some(body.to_string()),
        _ => None,
    }).collect();
    assert!(sent.is_empty(), "A still has its settings, yet these went: {sent:?}");
    let iso = state.plugin_config("camera_control").unwrap()["iso"].as_f64().unwrap();
    assert!(iso > 9000.0, "A's ISO came back clamped to B's range: {iso}");
}

// Sol's review, finding 6: ISO and shutter run over LogSliderRow's 2000 positions and manual focus over 1000, one at a
// time as in Qt (the step also quantises a drag in ui-kit's Slider)
#[test]
fn iso_shutter_and_focus_move_a_position_at_a_time() {
    let card = include_str!("../../ui/cards/camera.slint");
    for label in ["shortcut: \"camera.focus\";"] {
        let at = card.find(label).unwrap_or_else(|| panic!("no {label}"));
        let slider = &card[at..at + card[at..].find("moved(v)").unwrap()];
        assert!(slider.contains("step: 1;"), "{label}: {slider}");
    }
}

// Sol's review, finding 7: the readout says the value it was given (LogSliderRow.set_value), not the one the nearest
// of its 2000 positions comes to
#[test]
fn the_log_readouts_say_the_value_they_were_given() {
    let mut r = Rig::new();
    r.cam.iso.set_value(1234.0);
    assert_eq!(r.cam.iso.text(), "ISO 1234");
    r.cam.shutter.set_value(33_333_333.0);
    assert_eq!(r.cam.shutter.text(), "1/30");
    // Turned to the last stop by hand, it says that stop
    r.cam.iso.enabled = true;
    r.cam.manual_exp = true;
    r.ui("iso-picked", vec![json!(1000)]);
    assert_eq!(r.cam.iso.text(), "ISO 6400");
}

#[test]
fn a_keys_range_and_choices_move_with_the_lens() {
    let mut r = Rig::new();
    r.streaming();
    r.flush();
    assert!(!r.host.calls.iter().any(|c| c == "refresh_actions"), "nothing moved yet");
    r.cam.on_lens_selected(&json!({"id": "wide", "label": "Wide", "isoMin": 80, "isoMax": 800, "shutterMinNs": 300_000, "shutterMaxNs": 500_000_000}));
    r.flush();
    assert_eq!(r.host.calls.iter().filter(|c| *c == "refresh_actions").count(), 1, "{:?}", r.host.calls);
    let iso = r.cam.actions().into_iter().find(|a| a.id == "camera.iso").unwrap();
    assert!(iso.params.iter().any(|p| p["maximum"] == 800.0), "{:?}", iso.params);
    r.clear();
    r.flush();
    assert!(!r.host.calls.iter().any(|c| c == "refresh_actions"), "only when something moved");
}

// shortcuts.py gives every action its widgets: the control is where a right-click offers it
#[test]
fn every_camera_action_is_offered_on_its_control() {
    let mut r = Rig::new();
    let card = include_str!("../../ui/cards/camera.slint");
    for action in r.cam.actions() {
        assert!(card.contains(&format!("shortcut: \"{}\"", action.id)) || card.contains(&format!("action: \"{}\"", action.id)), "{}", action.id);
    }
}

// ── The dials: a lens's ISO and shutter times as stops ──

#[test]
fn the_iso_dial_has_the_stops_inside_the_lens_range() {
    let stops = dial_stops(false, 50.0, 1600.0);
    let names: Vec<&str> = stops.iter().map(|s| s.name.as_str()).collect();
    assert_eq!((names[0], names[names.len() - 1], names.contains(&"400")), ("50", "1600", true));
    assert!(stops.windows(2).all(|w| w[0].value < w[1].value), "low to high");
    // A lens that stops a hair under a stop still gets it, held to its own end
    assert_eq!(dial_stops(false, 50.0, 3199.0).last().unwrap().name, "3200");
    assert!(dial_stops(false, 7.0, 9.0).is_empty(), "no stop in a range this small");
}

#[test]
fn the_shutter_dial_goes_from_slow_to_fast() {
    let stops = dial_stops(true, 100_000.0, 1_000_000_000.0);
    assert!(stops.windows(2).all(|w| w[0].value > w[1].value), "slow to fast, so right is faster");
    let names: Vec<&str> = stops.iter().map(|s| s.name.as_str()).collect();
    assert_eq!((names[0], names[names.len() - 1]), ("1 s", "1/8000"));
    assert!(names.contains(&"1/60") && names.contains(&"1/4"));
    assert_eq!(nearest_stop(&stops, 16_666_667.0), names.iter().position(|n| *n == "1/60").unwrap());
}

#[test]
fn turning_a_dial_sends_that_stop_and_a_disabled_dial_sends_nothing() {
    let mut r = Rig::new();
    r.streaming();
    let stops = dial_stops(false, r.cam.iso.v_min, r.cam.iso.v_max);
    let at = stops.iter().position(|s| s.name == "800").unwrap();
    r.ui("iso-picked", vec![json!(at)]);
    assert!(r.sent().is_empty(), "auto exposure: the dial is off");
    r.ui("exposure-picked", vec![json!(1)]);
    r.clear();
    r.ui("iso-picked", vec![json!(at)]);
    assert!(r.sent().contains(&json!({"action": "iso", "value": 800})));
    assert_eq!(r.props()["iso-index"], at);
    let slow = dial_stops(true, r.cam.shutter.v_min, r.cam.shutter.v_max);
    let eighth = slow.iter().position(|s| s.name == "1/125").unwrap();
    r.clear();
    r.ui("shutter-picked", vec![json!(eighth)]);
    assert!(r.sent().contains(&json!({"action": "shutter", "value": 8_000_000})));
    assert_eq!(r.props()["shutter-stops"][eighth], "1/125");
}

#[test]
fn every_third_stop_is_a_full_stop_and_the_dial_says_where_the_first_one_is() {
    let iso = dial_stops(false, 50.0, 1600.0);
    let majors: Vec<&str> = iso.iter().filter(|s| s.major).map(|s| s.name.as_str()).collect();
    assert_eq!(majors, ["50", "100", "200", "400", "800", "1600"], "a doubling each");
    assert_eq!(first_major(&iso), 0);
    // A range that starts on a third: the first full stop is later
    let from_64 = dial_stops(false, 64.0, 1600.0);
    assert_eq!((from_64[0].name.as_str(), first_major(&from_64)), ("64", 2));
    let shutter = dial_stops(true, 100_000.0, 1_000_000_000.0);
    let majors: Vec<&str> = shutter.iter().filter(|s| s.major).map(|s| s.name.as_str()).collect();
    assert_eq!(majors, ["1 s", "0.5 s", "1/4", "1/8", "1/15", "1/30", "1/60", "1/125", "1/250", "1/500", "1/1000", "1/2000", "1/4000", "1/8000"], "{majors:?}");
}

#[test]
fn a_value_between_two_ticks_is_a_place_on_the_dial_and_back() {
    let stops = dial_stops(false, 50.0, 1600.0);
    let at = |name: &str| stops.iter().position(|s| s.name == name).unwrap();
    assert_eq!(dial_place(&stops, 400.0), (at("400"), 0.0));
    assert_eq!(dial_place(&stops, 400.2), (at("400"), 0.0), "a rounding of the phone's is still the tick");
    // 450 is on the log scale about half way from 400 to 500
    let (i, off) = dial_place(&stops, 447.0);
    assert_eq!(i, at("400"));
    assert!(off > 0.4 && off <= 0.5, "{off}");
    let (i, off) = dial_place(&stops, 360.0);
    assert_eq!(i, at("400"));
    assert!(off < 0.0 && off >= -0.5, "{off}");
    // And back: the place gives the value again
    let (i, off) = dial_place(&stops, 430.0);
    let back = dial_value(&stops, i, off).unwrap();
    assert!((back - 430.0).abs() < 1.0, "{back}");
    // Past the last tick it is held to half a tick
    let (i, off) = dial_place(&stops, 1700.0);
    assert_eq!(i, stops.len() - 1);
    assert!(off > 0.0 && off <= 0.5);
    // Shutter times run the other way, and the value comes back in nanoseconds
    let slow = dial_stops(true, 100_000.0, 1_000_000_000.0);
    let thirty = slow.iter().position(|s| s.name == "1/30").unwrap();
    let (i, off) = dial_place(&slow, 1e9 / 33.0);
    assert_eq!(i, thirty, "1/33 is nearer 1/30 than 1/40");
    assert!(off > 0.0, "towards the faster one: {off}");
    let back = dial_value(&slow, i, off).unwrap();
    assert!((back - 1e9 / 33.0).abs() < 1e5, "{back}");
}

#[test]
fn exposure_times_and_isos_are_read_as_they_are_written() {
    assert_eq!(parse_shutter("1/250"), Some(4_000_000.0));
    assert_eq!(parse_shutter(" 1/33 "), Some((1e9_f64 / 33.0).round()));
    assert_eq!(parse_shutter("250"), Some(4_000_000.0), "a bare number from 1 up is the n of 1/n");
    assert_eq!(parse_shutter("0.5"), Some(500_000_000.0));
    assert_eq!(parse_shutter("0,5 s"), Some(500_000_000.0));
    assert_eq!(parse_shutter("2s"), Some(2_000_000_000.0));
    assert_eq!(parse_shutter("30\""), Some(30_000_000_000.0));
    assert_eq!(parse_shutter("8 ms"), Some(8_000_000.0));
    for bad in ["", "fast", "1/", "1/0", "-3", "1/x", "s"] {
        assert_eq!(parse_shutter(bad), None, "{bad:?}");
    }
    assert_eq!(parse_iso("400"), Some(400.0));
    assert_eq!(parse_iso("ISO 640"), Some(640.0));
    assert_eq!(parse_iso("412.4"), Some(412.0));
    for bad in ["", "iso", "-100", "0", "lots"] {
        assert_eq!(parse_iso(bad), None, "{bad:?}");
    }
}

#[test]
fn a_dial_between_ticks_sends_that_value_and_reads_it_in_its_own_words() {
    let mut r = Rig::new();
    r.streaming();
    r.ui("exposure-picked", vec![json!(1)]);
    let stops = dial_stops(false, r.cam.iso.v_min, r.cam.iso.v_max);
    let at = stops.iter().position(|s| s.name == "400").unwrap();
    r.clear();
    r.ui("iso-fine", vec![json!(at), json!(0.25)]);
    let sent = r.sent();
    let value = sent.iter().find(|b| b["action"] == "iso").map(|b| b["value"].as_i64().unwrap()).expect("an ISO was sent");
    assert!((400..500).contains(&value) && value != 400, "a quarter of the way from 400 to 500: {value}");
    let p = r.props();
    assert_eq!(p["iso-index"], at);
    assert!(p["iso-between"].as_f64().unwrap() > 0.2 && p["iso-between"].as_f64().unwrap() < 0.3);
    assert_eq!(p["iso-text"], value.to_string());
    // On a tick again it reads as the tick
    r.ui("iso-picked", vec![json!(at)]);
    assert_eq!((r.props()["iso-between"].as_f64(), r.props()["iso-text"].as_str()), (Some(0.0), Some("")));
    // The shutter, in nanoseconds
    let slow = dial_stops(true, r.cam.shutter.v_min, r.cam.shutter.v_max);
    let thirty = slow.iter().position(|s| s.name == "1/30").unwrap();
    r.clear();
    r.ui("shutter-fine", vec![json!(thirty), json!(0.4)]);
    let ns = r.sent().iter().find(|b| b["action"] == "shutter").map(|b| b["value"].as_i64().unwrap()).expect("a shutter time was sent");
    assert!(ns < 33_333_333 && ns > 25_000_000, "between 1/30 and 1/40: {ns}");
    assert!(r.props()["shutter-text"].as_str().unwrap().starts_with("1/3") || r.props()["shutter-text"].as_str().unwrap().starts_with("1/4"));
}

#[test]
fn a_typed_value_is_set_clamped_to_the_range_and_a_bad_one_counts_for_the_shake() {
    let mut r = Rig::new();
    r.streaming();
    r.ui("exposure-picked", vec![json!(1)]);
    r.clear();
    r.ui("shutter-typed", vec![json!("1/250")]);
    assert!(r.sent().contains(&json!({"action": "shutter", "value": 4_000_000})));
    assert_eq!(r.props()["shutter-text"], "");
    r.clear();
    r.ui("shutter-typed", vec![json!("1/33")]);
    assert!(r.sent().contains(&json!({"action": "shutter", "value": 30_303_030})));
    assert_eq!(r.props()["shutter-text"], "1/33");
    r.clear();
    r.ui("iso-typed", vec![json!("400")]);
    assert!(r.sent().contains(&json!({"action": "iso", "value": 400})));
    // Past the lens's range it is held to it
    r.ui("iso-typed", vec![json!("999999")]);
    assert!(r.sent().contains(&json!({"action": "iso", "value": r.cam.iso.v_max as i64})));
    let before = r.props()["shutter-rejected"].as_i64().unwrap();
    r.clear();
    r.ui("shutter-typed", vec![json!("fast")]);
    assert!(r.sent().is_empty(), "nothing changed");
    assert_eq!(r.props()["shutter-rejected"].as_i64().unwrap(), before + 1);
}

#[test]
fn a_dial_that_is_off_takes_nothing_typed_or_between() {
    let mut r = Rig::new();
    r.streaming();
    r.clear();
    r.ui("iso-typed", vec![json!("400")]);
    r.ui("iso-fine", vec![json!(5), json!(0.2)]);
    assert!(r.sent().is_empty(), "auto exposure: the dial is off");
    assert_eq!(r.props()["iso-rejected"], 0, "and it does not shake: it is not asking");
}

#[test]
fn the_noise_and_sharpening_rows_say_which_row_they_are_on_hover() {
    let r = Rig::new();
    let p = r.props();
    assert_eq!(p["noise-tip-details"][2], "Reduce noise as much as possible, even if the frame rate drops.");
    assert_eq!(p["sharpening-tip-details"][0], "");
}

#[test]
fn brightness_readout_moves_by_the_phones_step_with_every_unit() {
    for step in [0.167, 0.333, 0.5, 1.0] {
        let mut r = Rig::new();
        r.cam.ae_step = step;
        let mut last: Option<f64> = None;
        for units in -6..=6_i64 {
            r.cam.on_ae_comp_changed(units);
            let shown: f64 = r.cam.ae_text.trim_end_matches(" EV").parse().unwrap();
            if let Some(before) = last {
                assert!((shown - before - step).abs() < 0.011, "step {step}: {before} to {shown} at {units} is not one step ({})", r.cam.ae_text);
            }
            last = Some(shown);
        }
    }
}
