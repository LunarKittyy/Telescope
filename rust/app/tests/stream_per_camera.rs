// Settings that belong to one camera, with real plugins on two streams: effects, presets, and coming back to a stream.
mod rig;

use rig::{state_with, Listener, Rig};
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::plugin::Plugin;
use telescope_core::ui::UiEvent;
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::fun_pack::{FunPackPlugin, EFFECTS};
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::plugins::presets::PresetsPlugin;

fn with(name: &str, optional: Vec<Box<dyn Plugin>>) -> Rig {
    let r = Rig::with(name, |link, heard| {
        let list: Vec<Box<dyn Plugin>> = vec![Box::new(Listener::new("panel", heard)), Box::new(rig::Picker(link.clone()))];
        list
    });
    for plugin in optional {
        r.w.state.borrow_mut().register(plugin).unwrap(); // optional ones aren't loaded with the others
    }
    r
}

fn per_camera(name: &str) -> Rig {
    with(name, vec![Box::new(FunPackPlugin::new()), Box::new(ColourGradingPlugin::new()), Box::new(PresetsPlugin::new())])
}

fn ui(r: &Rig, plugin: &str, id: &str, callback: &str, args: Vec<Value>) {
    r.w.state.borrow_mut().ui_event(plugin, UiEvent { ui: id.into(), callback: callback.into(), args });
    r.w.process();
    r.pump();
}

fn config(r: &Rig, plugin: &str) -> Value {
    r.w.state.borrow().plugin_config(plugin).unwrap()
}

fn grade(r: &Rig, key: &str) -> f64 {
    config(r, "colour_grading")[key].as_f64().unwrap()
}

fn pick_effect(r: &Rig, effect: &str) {
    ui(r, "fun_pack", "card", "effect-picked", vec![json!(EFFECTS.iter().position(|(k, _)| *k == effect).unwrap())]);
}

fn effect(r: &Rig) -> Value {
    config(r, "fun_pack")["effect"].clone()
}

fn save_preset(r: &Rig, name: &str) {
    ui(r, "presets", "header", "save-clicked", vec![]);
    ui(r, "presets", "header", "name-accepted", vec![json!(name)]);
}

fn apply_preset(r: &Rig, name: &str) {
    ui(r, "presets", "header", "apply", vec![json!(name)]);
}

fn presets(r: &Rig) -> Vec<String> {
    config(r, "presets")["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect()
}

fn on(r: &Rig, id: &str) {
    r.w.with_flow(|flow, host| flow.focus_stream(host, id));
    r.pump();
}

fn second(r: &Rig) {
    r.add("oneplus");
    r.until("the second stream", |r| r.workers_made() == 2);
    r.pump();
}

/// What a worker runs now, by who gave each step.
fn running(r: &Rig, worker: usize) -> Vec<String> {
    let w = r.worker(worker);
    let steps = w.steps.lock().unwrap();
    steps.last().unwrap_or(&w.first_steps).clone()
}

fn runs(r: &Rig, worker: usize, owner: &str) -> bool {
    running(r, worker).iter().any(|o| o == owner)
}

struct Plain;

impl FrameContext for Plain {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A picture with something in it to change.
fn picture() -> Frame {
    let (w, h) = (64, 36);
    let mut data: Vec<u8> = (0..w * h).map(|i| (i % w * 255 / w) as u8).collect();
    data.extend(std::iter::repeat_n(90, w * h / 2));
    Frame::new(w as u32, h as u32, PixelFormat::I420, data)
}

/// picture() through the steps a worker runs now, as bytes. Each CPU step is real; a shader needs a graphics card, so a test
/// picks an effect that has none.
fn through(r: &Rig, worker: usize) -> Vec<u8> {
    let w = r.worker(worker);
    let mut frame = picture();
    for staged in w.staged.lock().unwrap().iter_mut() {
        match &mut staged.step {
            FrameStep::Cpu(step) => step.process(&mut frame, &Plain),
            FrameStep::Shader(_) | FrameStep::Either(..) => assert_ne!(staged.owner, "fun_pack", "pick a CPU effect"),
        }
    }
    frame.data.to_vec()
}

#[test]
fn a_fun_pack_effect_stays_on_the_camera_it_was_turned_on_for() {
    let r = per_camera("fun-stays");
    r.start();
    pick_effect(&r, "webcam");
    second(&r);
    assert_eq!(effect(&r), "off");
    assert_eq!(through(&r, 1), *picture().data, "the camera it wasn't turned on for");
    assert!(!runs(&r, 1, "fun_pack"));
    assert_ne!(through(&r, 0), *picture().data, "still on behind the panels");
    on(&r, "pixel");
    assert_eq!(effect(&r), "webcam");
    assert_ne!(through(&r, 0), *picture().data);
    assert_eq!(through(&r, 1), *picture().data);
    on(&r, "oneplus");
    assert_eq!(effect(&r), "off");
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn a_preset_saved_with_two_cameras_streaming_is_the_shown_ones() {
    let r = per_camera("preset-shown");
    r.start();
    second(&r);
    pick_effect(&r, "vhs");
    r.configure("colour_grading", json!({"temperature": 40}));
    save_preset(&r, "Retro");
    on(&r, "pixel");
    assert!(presets(&r).is_empty(), "presets are per phone");
    assert_eq!(effect(&r), "off");
    assert_eq!(grade(&r, "temperature"), 0.0);
    on(&r, "oneplus");
    pick_effect(&r, "off");
    r.configure("colour_grading", json!({}));
    apply_preset(&r, "Retro");
    assert_eq!(effect(&r), "vhs");
    assert_eq!(grade(&r, "temperature"), 40.0);
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn a_preset_applied_on_one_camera_leaves_the_others_alone() {
    let r = per_camera("preset-alone");
    r.start();
    save_preset(&r, "Plain");
    pick_effect(&r, "crt");
    save_preset(&r, "Telly");
    second(&r);
    on(&r, "pixel");
    apply_preset(&r, "Plain");
    assert_eq!(effect(&r), "off");
    assert!(!runs(&r, 1, "fun_pack"), "oneplus: nothing on");
    on(&r, "oneplus");
    assert_eq!(effect(&r), "off");
    assert!(presets(&r).is_empty());
    on(&r, "pixel");
    apply_preset(&r, "Telly");
    assert!(r.w.state.borrow_mut().save_now());
    let devices = r.w.state.borrow().file.load()["devices"].clone();
    assert_eq!(devices["pixel"]["plugin_configs"]["fun_pack"]["effect"], "crt");
    assert_eq!(devices["oneplus"]["plugin_configs"]["fun_pack"]["effect"], "off");
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn a_preset_from_before_per_camera_effects_applies_to_the_shown_camera() {
    let r = per_camera("preset-old");
    r.w.state.borrow().file.save(&mut json!({
        "plugin_configs": {"fun_pack": {"effect": "vhs"}},
        "selected_device": "pixel",
        "devices": {"pixel": {"plugin_configs": {"presets": {"presets": [{"name": "Old", "fun_pack": {"effect": "ascii"}, "colour_grading": {"tint": 30}}]}}}},
    }));
    r.w.state.borrow_mut().apply_saved_config();
    r.w.process();
    assert_eq!(effect(&r), "vhs");
    assert_eq!(presets(&r), ["Old"]);
    r.start();
    second(&r);
    assert_eq!(effect(&r), "off", "a phone with nothing saved starts plain");
    on(&r, "pixel");
    apply_preset(&r, "Old");
    assert_eq!(effect(&r), "ascii");
    assert_eq!(grade(&r, "tint"), 30.0);
    on(&r, "oneplus");
    assert_eq!(effect(&r), "off");
    assert_eq!(grade(&r, "tint"), 0.0);
    r.w.stop_all_streams();
    r.pump();
}

fn stays_on_its_camera(name: &str, plugin: Box<dyn Plugin>, id: &str, on_it: Value) {
    let r = with(name, vec![plugin]);
    let off = config(&r, id);
    let mut set = off.clone();
    set.as_object_mut().unwrap().extend(on_it.as_object().unwrap().clone());
    r.start();
    r.configure(id, set.clone());
    second(&r);
    assert_eq!(config(&r, id), off);
    on(&r, "pixel");
    assert_eq!(config(&r, id), set);
    r.configure(id, off); // Auto framing's timer would outlive the window
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn turning_a_camera_effect_on_leaves_the_other_cameras_alone() {
    stays_on_its_camera("own-background", Box::new(telescope_effects::background::BackgroundPlugin::new()), "background", json!({"mode": "blur"}));
    stays_on_its_camera("own-chroma", Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::new()), "chroma_key", json!({"on": true}));
    stays_on_its_camera("own-framing", Box::new(telescope_effects::framing::FramingPlugin::new()), "framing", json!({"on": true}));
    stays_on_its_camera("own-face", Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()), "face_exposure", json!({"on": true}));
    stays_on_its_camera("own-tag", Box::new(telescope_effects::name_tag::NameTagPlugin::new()), "name_tag", json!({"on": true, "name": "Luna"}));
}

// ── Coming back to a stream ──

fn camera_state(camera: Value, more: Value) -> Value {
    let mut lens = json!({
        "id": "0", "logicalId": null, "label": "Back", "current": true, "hasOis": true, "isoMin": 50, "isoMax": 3200,
        "shutterMinNs": 100_000, "shutterMaxNs": 1_000_000_000i64, "supportsManualSensor": true, "supportsManualWB": true,
        "supportsManualFocus": false, "minFocusDistance": 0.0, "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167,
        "supportsFlash": true, "hwLevel": "FULL",
    });
    lens.as_object_mut().unwrap().extend(camera.as_object().unwrap().clone());
    let mut state = json!({"cameras": [lens], "codecs": ["mjpeg", "h264"], "camera_toggle": true});
    state.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
    state_with(state)
}

fn with_camera(name: &str) -> Rig {
    let r = Rig::with(name, |link, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(rig::Picker(link.clone())), Box::new(CameraControl::new())]
    });
    r.emit(telescope_core::event::Event::PhonesChanged { count: 2 });
    r.pump();
    r
}

fn sent(r: &Rig, id: &str) -> Vec<Value> {
    r.link.control(id).sent.lock().unwrap().clone()
}

fn forget_sent(r: &Rig, id: &str) {
    r.link.control(id).sent.lock().unwrap().clear();
}

fn exposed(name: &str) -> Rig {
    let r = with_camera(name);
    r.start();
    r.phone_says("pixel", camera_state(json!({"isoMax": 6400}), json!({"auto": false, "iso": 6400, "shutter_ns": 10_000_000})));
    r.pump();
    second(&r);
    r.phone_says("oneplus", camera_state(json!({"isoMax": 800}), json!({}))); // a lens that stops at ISO 800
    r.pump();
    r
}

#[test]
fn coming_back_to_a_stream_keeps_its_exposure_and_sends_nothing() {
    let r = exposed("back-exposure");
    forget_sent(&r, "pixel");
    on(&r, "pixel");
    assert!(config(&r, "camera_control")["iso"].as_f64().unwrap() > 6390.0);
    assert_eq!(sent(&r, "pixel"), [] as [Value; 0], "the phone still has them");
    ui(&r, "camera_control", "card", "iso-picked", vec![json!(1000)]);
    assert!(config(&r, "camera_control")["iso"].as_f64().unwrap() > 6390.0, "the slider's top is this lens's 6400 again");
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn coming_back_to_a_stream_that_changed_route_meanwhile_keeps_its_exposure() {
    let r = exposed("back-route");
    r.source("pixel").kind.as_phone().unwrap().move_to(r.session_id("pixel"), telescope_core::source::RouteSpec::Network { address: "192.168.1.20".into() });
    r.worker(0).says("reconnecting", "Connection lost");
    r.until("it moved", |r| r.source("pixel").session().unwrap().url.contains("192.168.1.20"));
    forget_sent(&r, "pixel");
    on(&r, "pixel");
    assert!(config(&r, "camera_control")["iso"].as_f64().unwrap() > 6390.0);
    assert_eq!(sent(&r, "pixel"), [] as [Value; 0]);
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn coming_back_to_a_stream_keeps_its_focus_point() {
    let r = with_camera("back-focus");
    r.start();
    r.phone_says("pixel", camera_state(json!({"supportsFocusPoint": true}), json!({})));
    r.pump();
    r.emit(telescope_core::event::Event::FocusPoint { x: 0.25, y: 0.75 });
    r.pump();
    second(&r);
    let focus = |r: &Rig| r.heard().into_iter().rfind(|l| l.starts_with("panel focus mode "));
    assert_eq!(focus(&r).as_deref(), Some("panel focus mode auto"), "oneplus's panel");
    forget_sent(&r, "pixel");
    on(&r, "pixel");
    assert_eq!(focus(&r).as_deref(), Some("panel focus mode point"));
    let picked = r.w.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-focus-mode")).expect("the camera card");
    assert_eq!(r.w.ui.get_property(&picked), Ok(slint_interpreter::Value::Number(1.0)), "the Point choice is selected");
    assert_eq!(sent(&r, "pixel"), [] as [Value; 0]);
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn coming_back_to_a_zoomed_phone_doesnt_crop_its_crop_again() {
    let r = with("back-zoom", vec![Box::new(telescope_effects::transforms::TransformsPlugin::new())]);
    r.start();
    r.phone_says("pixel", camera_state(json!({"zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}), json!({})));
    r.pump();
    ui(&r, "transforms", "card", "zoom-moved", vec![json!(2.0)]);
    let crop = |r: &Rig| {
        let st = r.w.state.borrow();
        let shown = st.vc.frame_view(&st.view);
        (shown.zoom, shown.pan_x, shown.pan_y)
    };
    assert_eq!(crop(&r), (1.0, 0.0, 0.0), "the phone crops it all");
    second(&r);
    forget_sent(&r, "pixel");
    on(&r, "pixel");
    assert_eq!(crop(&r), (1.0, 0.0, 0.0));
    assert_eq!(sent(&r, "pixel"), [] as [Value; 0]);
    r.w.stop_all_streams();
    r.pump();
}

// test_widgets.py's LogSliderRow and LensPanel on the real Camera card in the real window, with the real mouse.

fn shown(name: &str) -> Rig {
    let r = with_camera(name);
    r.w.ui.window().set_size(slint::LogicalSize::new(1400.0, 1400.0));
    r.w.ui.show().unwrap();
    r
}

fn found(r: &Rig, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(&r.w.ui, label).next()
}

fn click_at(r: &Rig, position: LogicalPosition) {
    let window = r.w.ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    r.w.process();
    r.pump();
}

fn click_on(r: &Rig, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    click_at(r, LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0));
}

/// The named slider, and a click at its right-hand end.
fn slider(r: &Rig, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(&r.w.ui, label).find(|e| e.accessible_value_maximum().is_some()).unwrap_or_else(|| panic!("no {label} slider"))
}

fn to_the_top(r: &Rig, label: &str) {
    let e = slider(r, label);
    let (p, s) = (e.absolute_position(), e.size());
    click_at(r, LogicalPosition::new(p.x + s.width - 1.0, p.y + s.height / 2.0));
}

fn iso_sent(r: &Rig) -> Vec<i64> {
    sent(r, "pixel").iter().filter(|m| m["action"] == "iso").filter_map(|m| m["value"].as_i64()).collect()
}

fn manual(name: &str, min: i64, max: i64) -> Rig {
    let r = shown(name);
    r.start();
    r.phone_says("pixel", camera_state(json!({"isoMin": min, "isoMax": max}), json!({"auto": false, "iso": 100})));
    r.pump();
    r
}

#[test]
fn integer_log_slider_syncs_slider_spin_and_signal() {
    let r = manual("log-sync", 10, 1000);
    assert_eq!(slider(&r, "ISO").accessible_value().as_deref(), Some("ISO 100"), "a screen reader hears the readout, not the place on the slider");
    assert!(found(&r, "ISO 100").is_some(), "the readout says it");
    forget_sent(&r, "pixel");
    to_the_top(&r, "ISO");
    assert_eq!(slider(&r, "ISO").accessible_value().as_deref(), Some("ISO 1000"));
    assert_eq!(iso_sent(&r), [999], "the value and not the place on the slider, cut to a whole number as Python's int() does");
    assert!(found(&r, "ISO 1000").is_some(), "and the readout follows the hand");
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn log_slider_range_and_enabled_state() {
    let r = manual("log-range", 10, 1000);
    r.phone_says("pixel", camera_state(json!({"isoMin": 20, "isoMax": 2000}), json!({"auto": false, "iso": 100})));
    r.pump();
    forget_sent(&r, "pixel");
    to_the_top(&r, "ISO");
    assert_eq!(iso_sent(&r), [1999], "the slider's top is the lens's top");
    // The lens can't do manual exposure: the Manual segment goes, and the row with it
    r.phone_says("pixel", camera_state(json!({"supportsManualSensor": false}), json!({})));
    r.pump();
    assert!(ElementHandle::find_by_accessible_label(&r.w.ui, "ISO").next().is_none());
    r.phone_says("pixel", camera_state(json!({"isoMin": 20, "isoMax": 2000}), json!({"auto": false, "iso": 100})));
    r.pump();
    // With no phone the card's controls are off, slider and all
    r.w.stop_all_streams();
    r.pump();
    r.emit(telescope_core::event::Event::PhonesChanged { count: 0 });
    r.pump();
    forget_sent(&r, "pixel");
    let before = slider(&r, "ISO").accessible_value();
    to_the_top(&r, "ISO");
    assert_eq!(slider(&r, "ISO").accessible_value(), before, "a disabled slider doesn't move");
    assert_eq!(iso_sent(&r), [] as [i64; 0]);
}

fn tiles(lenses: Value) -> Value {
    let mut state = camera_state(json!({}), json!({}));
    state["cameras"] = lenses;
    state
}

fn lens(id: &str, label: &str, current: bool) -> Value {
    camera_state(json!({"id": id, "label": label, "current": current}), json!({}))["cameras"][0].clone()
}

fn card_prop(r: &Rig, name: &str) -> slint_interpreter::Value {
    let id = r.w.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-lens-placeholder")).expect("the camera card");
    r.w.ui.get_property(&format!("{}{name}", id.trim_end_matches("lens-placeholder"))).unwrap()
}

#[test]
fn empty_lenses_show_a_placeholder() {
    let r = shown("lens-empty");
    r.start();
    r.phone_says("pixel", tiles(json!([])));
    r.pump();
    assert_eq!(card_prop(&r, "lens-placeholder"), slint_interpreter::Value::String("No cameras available".into()), "it's what the dim tiles say");
    assert!(ElementHandle::find_by_accessible_label(&r.w.ui, "Back").next().is_none(), "with no tiles");
    r.w.stop_all_streams();
    r.pump();
}

#[test]
fn lens_panel_load_select_placeholder_and_clear() {
    let r = shown("lens-panel");
    assert_eq!(card_prop(&r, "lens-placeholder"), slint_interpreter::Value::String("Start streaming to load lenses".into()));
    r.start();
    r.phone_says("pixel", tiles(json!([lens("wide", "Wide", false), lens("tele", "Tele", true)])));
    r.pump();
    assert!(found(&r, "Loading lenses...").is_none(), "tiles in its place");
    let tile = |name: &str| ElementHandle::find_by_accessible_label(&r.w.ui, name).find(|e| e.accessible_checked().is_some()).unwrap_or_else(|| panic!("no {name} tile"));
    assert_eq!((tile("Wide").accessible_checked(), tile("Tele").accessible_checked()), (Some(false), Some(true)));
    forget_sent(&r, "pixel");
    click_on(&r, &tile("Wide"));
    assert_eq!(sent(&r, "pixel").iter().filter(|m| m["action"] == "camera").map(|m| m["id"].clone()).collect::<Vec<_>>(), [json!("wide")], "the lens the tile was for");
    assert_eq!((tile("Wide").accessible_checked(), tile("Tele").accessible_checked()), (Some(true), Some(false)));
    assert_eq!(card_prop(&r, "current-lens"), slint_interpreter::Value::Number(0.0));
    r.w.stop_all_streams();
    r.pump();
    assert_eq!(card_prop(&r, "lens-placeholder"), slint_interpreter::Value::String("Start streaming to load lenses".into()), "cleared");
    assert!(ElementHandle::find_by_accessible_label(&r.w.ui, "Wide").next().is_none());
}
