//! The real Microphone plugin in the window with two cameras (test_microphone.py's two_cams): the mic stays with the main stream and
//! moves with the window's own flow.
mod rig;

use rig::{Listener, Rig};
use serde_json::{json, Value};
use slint_interpreter::Value as SValue;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use telescope_audio::backend::{MicBackend, Problem};
use telescope_audio::plugin::{AudioRoutes, MakeWorker, MicWorker, MicrophonePlugin};
use telescope_audio::worker::{OpenSink, Opener};
use telescope_core::plugin::Plugin;
use telescope_core::ui::UiEvent;
use telescope_core::Event;

#[derive(Default)]
struct Backend {
    torn_down: AtomicU32,
}

impl MicBackend for Backend {
    fn pick_name(&self) -> String {
        "Telescope Microphone".into()
    }
    fn problem(&self) -> Option<Problem> {
        None
    }
    fn prepare(&self) -> String {
        String::new()
    }
    fn open_sink(&self) -> OpenSink {
        Arc::new(|| Err("none".into()))
    }
    fn teardown(&self) {
        self.torn_down.fetch_add(1, Ordering::SeqCst);
    }
}

struct Routes;

impl AudioRoutes for Routes {
    fn audio(&self, source_id: &str) -> Option<(String, Opener)> {
        Some((format!("http://{source_id}/v1/audio"), Arc::new(|| Err("not in tests".into()))))
    }
}

#[derive(Default)]
struct Running(Mutex<Vec<(String, bool)>>);

struct Worker(Arc<Running>, usize);

impl MicWorker for Worker {
    fn start(&mut self) {}
    fn stop(&mut self) {
        self.0.0.lock().unwrap()[self.1].1 = true;
    }
    fn hand_over(&self, _url: &str, _open: Opener) {}
    fn set_gain(&self, _gain: f32) {}
    fn set_muted(&self, _on: bool) {}
    fn set_limit(&self, _on: bool) {}
    fn set_auto_level(&self, _on: bool) {}
    fn set_noise_reduction(&self, _on: bool) {}
    fn take_level(&self) -> (f32, f32, bool) {
        (0.0, 0.0, false)
    }
}

struct Mic {
    r: Rig,
    workers: Arc<Running>,
    backend: Arc<Backend>,
}

impl Mic {
    /// A property of the mic's card, as the window has it.
    fn prop(&self, name: &str) -> SValue {
        let ui = &self.r.w.ui;
        let piece = (0..20).find(|i| ui.get_property(&format!("p{i}-toggle-enabled")).is_ok()).expect("the card");
        ui.get_property(&format!("p{piece}-{name}")).unwrap()
    }

    fn flag(&self, name: &str) -> bool {
        self.prop(name) == SValue::Bool(true)
    }

    fn status(&self) -> String {
        match self.prop("status-text") {
            SValue::String(s) => s.to_string(),
            other => panic!("{other:?}"),
        }
    }

    /// The switch, clicked.
    fn toggle(&self, on: bool) {
        self.r.w.state.borrow_mut().ui_event("microphone", UiEvent { ui: "card".into(), callback: "toggled".into(), args: vec![json!(on)] });
        self.settle();
    }

    fn settle(&self) {
        self.r.w.process();
        self.r.pump();
    }

    fn running(&self) -> Vec<String> {
        self.workers.0.lock().unwrap().iter().filter(|(_, stopped)| !stopped).map(|(url, _)| url.clone()).collect()
    }

    fn main(&self) -> Option<String> {
        self.r.w.state.borrow().streams.main.clone().map(|m| m.0)
    }

    fn enabled(&self) -> bool {
        self.r.w.state.borrow().plugin_config("microphone").unwrap()["enabled"] == true
    }

    fn focus(&self, id: &str) {
        self.r.w.with_flow(|flow, host| flow.focus_stream(host, id));
        self.settle();
    }

    /// What the saved settings say about this phone's mic.
    fn saved(&self, id: &str) -> Value {
        let mut state = self.r.w.state.borrow_mut();
        state.save_now();
        state.file.load()["devices"][id]["plugin_configs"]["microphone"]["enabled"].clone()
    }
}

/// pixel streams and has the mic; oneplus started next to it and has the panels.
fn two_cams(name: &str) -> Mic {
    let (backend, workers) = (Arc::new(Backend::default()), Arc::new(Running::default()));
    let (b, made) = (backend.clone(), workers.clone());
    let r = Rig::with(name, move |_, heard| {
        let make_worker: MakeWorker = Box::new(move |url, _open, _sink, _status| {
            let mut all = made.0.lock().unwrap();
            all.push((url.to_string(), false));
            Box::new(Worker(made.clone(), all.len() - 1))
        });
        let mic = MicrophonePlugin::new(b, Some(Arc::new(Routes) as Arc<dyn AudioRoutes>))
            .with_parts(make_worker, Box::new(|job| job()), Box::new(String::new));
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(mic)]
    });
    r.emit(Event::PhonesChanged { count: 2 }); // the card is greyed out until then
    r.pump();
    r.start();
    let mic = Mic { r, workers, backend };
    mic.toggle(true); // on for the first
    mic.r.worker(0).says("ok", "Streaming");
    mic.r.add("oneplus"); // the panels move to it
    mic.r.worker(1).says("ok", "Streaming");
    mic.settle();
    mic
}

#[test]
fn another_streams_card_shows_its_mic_off_and_turning_it_on_moves_the_mic() {
    let m = two_cams("mic-moves");
    assert_eq!(m.r.shown().as_deref(), Some("oneplus"));
    assert_eq!(m.main().as_deref(), Some("pixel"));
    assert!(!m.flag("on") && m.flag("toggle-enabled"));
    assert!(!m.flag("here") && m.status().contains("On for Pixel"), "{}", m.status());
    assert_eq!(m.running(), ["http://pixel/v1/audio"]);

    m.toggle(true);
    assert_eq!(m.main().as_deref(), Some("oneplus"));
    assert!(m.flag("on") && m.flag("here") && m.status() == "Connecting…", "{}", m.status());
    assert_eq!(m.running(), ["http://oneplus/v1/audio"]);
    assert_eq!(m.saved("pixel"), false, "the one it left keeps it off");
    assert_eq!(m.backend.torn_down.load(Ordering::SeqCst), 0, "apps keep the same microphone throughout");

    m.focus("pixel");
    assert!(!m.flag("on") && m.status().contains("On for OnePlus"), "{}", m.status());

    m.focus("oneplus");
    m.toggle(false); // off on the one that has it: off, and it stays there
    assert!(m.main().as_deref() == Some("oneplus") && !m.enabled());
    m.focus("pixel");
    assert!(!m.flag("on") && m.status().is_empty(), "{}", m.status());
    m.toggle(true); // on again, here
    assert!(m.main().as_deref() == Some("pixel") && m.enabled());
    assert_eq!(m.running(), ["http://pixel/v1/audio"]);
}

#[test]
fn the_mic_stays_on_when_the_stream_with_it_stops() {
    let m = two_cams("mic-stays");
    m.toggle(true); // moved to oneplus; pixel keeps it off
    m.r.stop(Some("oneplus"));
    m.settle();
    assert_eq!(m.main().as_deref(), Some("pixel"));
    assert!(m.enabled() && m.flag("on"));
    assert_eq!(m.running(), ["http://pixel/v1/audio"]);
}

#[test]
fn a_card_whose_camera_is_still_starting_cant_take_the_mic() {
    let m = two_cams("mic-starting");
    m.r.stop(Some("oneplus"));
    m.settle();
    let source = m.r.source("oneplus");
    m.r.w.with_flow(|flow, host| flow.move_focus(host, &source)); // as + does, before its stream is up
    m.settle();
    assert!(!m.flag("on") && !m.flag("toggle-enabled"));
}
