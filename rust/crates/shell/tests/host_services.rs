// Host services through the shell's real State with the real plugins, where a fake host would hide a gap.
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::event::Event;
use telescope_core::frame::{FrameContext, FrameStep};
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::ui::UiEvent;
use telescope_core::{Frame, PixelFormat};
use telescope_shell::shell::State;
use telescope_shell::stream_flow::Hook;

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn state(name: &str, plugins: Vec<Box<dyn Plugin>>) -> State {
    let mut st = State::new(ConfigFile::at(telescope_core::testing::scratch_dir(name).join(CONFIG_FILENAME)));
    st.register(Box::new(telescope_shell::plugins::preview::Preview::default())).unwrap();
    for p in plugins {
        st.register(p).unwrap();
    }
    st.deliver_events();
    st
}

fn configure(st: &mut State, name: &str, cfg: Value) {
    let i = st.index(name).unwrap();
    st.call(i, |p, _| p.set_config(&cfg).unwrap());
    st.deliver_events();
}

fn start(st: &mut State) {
    st.streams.streaming = true;
    st.streams.source_id = Some("phone".into());
    let names: Vec<String> = st.slots.iter().map(|s| s.info.name.clone()).collect();
    let info = StreamInfo { source_id: "phone".into(), url: "http://phone".into() };
    st.call_hook(&names, &Hook::StreamStart(info));
    st.bus.emit(Event::StreamStarted { url: "http://phone".into() });
    st.deliver_events();
}

fn stop(st: &mut State) {
    st.streams.streaming = false;
    let names: Vec<String> = st.slots.iter().map(|s| s.info.name.clone()).collect();
    st.call_hook(&names, &Hook::StreamStop);
    st.bus.emit(Event::StreamStopped);
    st.deliver_events();
}

/// Runs owner's frame step as the stream would, then lets its posts reach it.
fn run_step(st: &mut State, owner: &str, frame: &mut Frame) {
    let mut step = st.pipeline_steps().into_iter().find(|s| s.owner == owner).expect("the plugin's frame step").step;
    let FrameStep::Cpu(step) = &mut step else { panic!("a CPU step") };
    step.process(frame, &Ctx);
    st.deliver_events();
}

fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [rgb[0], rgb[1], rgb[2], 255].repeat((w * h) as usize))
}

fn preview(st: &mut State, callback: &str, args: Value) {
    let args = args.as_array().cloned().unwrap_or_default();
    st.ui_event("preview", UiEvent { ui: "card".into(), callback: callback.into(), args });
}

fn click(st: &mut State, u: f64, v: f64) {
    preview(st, "pick-pointer", json!(["down", u, v, 640.0, 360.0]));
    preview(st, "pick-pointer", json!(["up", u, v, 640.0, 360.0]));
}

#[test]
fn desk_mode_tells_the_preview_its_warp_again_after_a_restart() {
    let mut st = state("host-desk-warp", vec![Box::new(telescope_effects::desk::DeskPlugin::new())]);
    configure(&mut st, "desk", json!({"on": true, "corners": [[0.3, 0.1], [0.7, 0.15], [0.75, 0.9], [0.25, 0.85]]}));
    start(&mut st);
    run_step(&mut st, "desk", &mut solid(320, 180, [0, 0, 0]));
    assert!(st.overlays.warping());
    stop(&mut st);
    start(&mut st);
    run_step(&mut st, "desk", &mut solid(320, 180, [0, 0, 0]));
    assert!(st.overlays.warping(), "or taps, zoom and picks land beside what's shown");
    configure(&mut st, "desk", json!({"on": false, "corners": [[0.3, 0.1], [0.7, 0.15], [0.75, 0.9], [0.25, 0.85]]}));
    // set_config has no host: Desk catches up on its next hook
    st.bus.emit(Event::StreamsChanged { count: 1 });
    st.deliver_events();
    assert!(!st.overlays.warping(), "switching Desk mode off takes its warp away");
}

#[test]
fn desk_mode_marks_the_sheet_on_the_preview() {
    let mut st = state("host-desk-mark", vec![Box::new(telescope_effects::desk::DeskPlugin::new())]);
    st.ui_event("desk", UiEvent { ui: "card".into(), callback: "mark-clicked".into(), args: vec![] });
    assert_eq!(st.overlays.picking(), "", "nothing to pick on without a stream");
    start(&mut st);
    st.ui_event("desk", UiEvent { ui: "card".into(), callback: "mark-clicked".into(), args: vec![] });
    assert_eq!(st.overlays.picking(), "desk");
    // Drag the first corner, then Enter
    preview(&mut st, "pick-pointer", json!(["down", 0.2, 0.2, 640.0, 360.0]));
    preview(&mut st, "pick-pointer", json!(["move", 0.1, 0.1, 640.0, 360.0]));
    preview(&mut st, "pick-pointer", json!(["up", 0.1, 0.1, 640.0, 360.0]));
    preview(&mut st, "pick-key", json!(["\n"]));
    assert_eq!(st.overlays.picking(), "");
    let corners = st.plugin_config("desk").unwrap()["corners"].clone();
    let first: Vec<f64> = corners[0].as_array().unwrap().iter().map(|c| c.as_f64().unwrap()).collect();
    assert!((first[0] - 0.1).abs() < 1e-4 && (first[1] - 0.1).abs() < 1e-4, "{corners}");
    assert_eq!(st.plugin_config("desk").unwrap()["on"], json!(true));
}

#[test]
fn a_stream_stopping_cancels_the_pick_and_escape_does_too() {
    let mut st = state("host-pick-cancel", vec![Box::new(telescope_effects::desk::DeskPlugin::new())]);
    start(&mut st);
    st.ui_event("desk", UiEvent { ui: "card".into(), callback: "mark-clicked".into(), args: vec![] });
    stop(&mut st);
    assert_eq!(st.overlays.picking(), "");
    start(&mut st);
    st.ui_event("desk", UiEvent { ui: "card".into(), callback: "mark-clicked".into(), args: vec![] });
    preview(&mut st, "pick-key", json!(["\u{1b}"]));
    assert_eq!(st.overlays.picking(), "");
    assert_eq!(st.plugin_config("desk").unwrap()["corners"], Value::Null);
}

#[test]
fn chroma_keys_picked_colour_comes_from_a_click_on_the_preview() {
    let mut st = state("host-chroma-pick", vec![Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::new())]);
    start(&mut st);
    configure(&mut st, "chroma_key", json!({"on": true}));
    st.ui_event("chroma_key", UiEvent { ui: "card".into(), callback: "screen-picked".into(), args: vec![json!(2)] });
    assert_eq!(st.overlays.picking(), "chroma_key");
    run_step(&mut st, "chroma_key", &mut solid(64, 36, [200, 40, 120]));
    click(&mut st, 0.5, 0.5);
    let cfg = st.plugin_config("chroma_key").unwrap();
    assert_eq!(cfg["screen"], json!("picked"));
    assert_eq!(cfg["picked"], json!([120, 40, 200]), "B, G, R of the spot clicked");
}
