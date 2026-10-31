//! First-run checklist on the video stage (plugins/onboarding.py): virtual camera, phone app, add a phone, start
//! streaming. It shows until the first stream has delivered frames, then gets out of the way until
//! setup_guide_requested opens it again; the next one closes it.

use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::i18n::{n_, tr, Fields};
use telescope_core::output::SetupResult;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::host::RequestId;
use telescope_core::text::{app_font, Face, Weight, BODY_AVERAGE_CHAR};
use telescope_core::{BusSender, Host};

/// The phone app's download link (version.release_asset_url): dev builds point at nightly.
pub const APK_URL: &str = "https://github.com/LunarKittyy/Telescope/releases/download/nightly/Telescope.apk";

const LINUX_INSTALL_HINT: &str = n_("Install the v4l2loopback package. Fedora and Nobara: v4l2loopback (from RPM Fusion). Debian, Ubuntu and Arch: v4l2loopback-dkms.");

/// Whether this computer has its virtual camera set up; run off the UI thread. None: nobody can tell (scenes).
pub type VcamCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Windows: installs the virtual camera, asking for admin access; run off the UI thread.
pub type VcamInstall = Arc<dyn Fn() -> SetupResult + Send + Sync>;

/// The answer to a virtual camera check comes back on the bus as a RequestDone with this id.
const VCAM_CHECKED: RequestId = RequestId(u64::MAX - 1);

/// The lines a label's sizeHint gives this text at the body size, which the card's height follows on the stage.
fn hint_lines(text: &str, weight: Weight) -> u32 {
    app_font(weight).map_or(0, |font| Face::em(font, 13.0).qt_hint_lines(text, BODY_AVERAGE_CHAR))
}

pub struct Onboarding {
    phones: u32,
    vcam_ready: bool,
    vcam_checked: bool,
    streaming: bool,
    streamed: bool,
    source: String,
    asked: bool,
    check: Option<VcamCheck>,
    install: Option<VcamInstall>,
    bus: Option<BusSender>,
    linux: bool,
    /// The driver's install is running, so its button is off.
    installing: bool,
    /// What the virtual camera step says over its usual text, until the next redraw of the steps.
    vcam_note: Option<VcamNote>,
}

enum VcamNote {
    Installing,
    Failed(String),
}

impl Onboarding {
    pub fn new(check: Option<VcamCheck>, install: Option<VcamInstall>, bus: Option<BusSender>) -> Onboarding {
        Onboarding {
            phones: 0, vcam_ready: false, vcam_checked: false, streaming: false, streamed: false, source: String::new(),
            asked: false, check, install, bus, linux: cfg!(target_os = "linux"), installing: false, vcam_note: None,
        }
    }

    fn check_virtual_camera(&mut self) {
        let (Some(check), Some(bus)) = (self.check.clone(), self.bus.clone()) else { return };
        std::thread::spawn(move || {
            let ready = check();
            bus.emit(Event::RequestDone { request: VCAM_CHECKED, ok: ready, message: String::new(), data: Value::Null });
        });
    }

    /// The Windows button: the install runs off the UI thread and always reports back, or the step stays on Installing….
    fn install_driver(&mut self, host: &mut dyn Host) {
        let (Some(install), Some(bus)) = (self.install.clone(), self.bus.clone()) else { return };
        self.installing = true;
        self.vcam_note = Some(VcamNote::Installing);
        self.render(host);
        let check = self.check.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let result = install();
                let ready = result.ok && check.as_ref().is_some_and(|check| check());
                (ready, if result.ok { String::new() } else { result.message })
            }));
            let (ok, message) = result.unwrap_or_else(|panic| {
                let text = panic.downcast_ref::<String>().map(String::as_str).or(panic.downcast_ref::<&str>().copied());
                (false, text.unwrap_or("the install stopped").to_string())
            });
            bus.emit(Event::RequestDone { request: VCAM_CHECKED, ok, message, data: Value::Null });
        });
    }

    /// Whether the checklist shows now.
    pub fn needed(&self) -> bool {
        let has_device = self.phones > 0 || !self.source.is_empty();
        self.asked || !self.streaming && !(has_device && self.vcam_ready && self.streamed)
    }

    /// The four steps as the card shows them.
    pub fn steps(&self) -> Vec<Value> {
        let step = |title: &str, text: String, kind: &str, done: bool, button: String, primary: bool| {
            let title = tr(title);
            json!({"title-lines": hint_lines(&title, Weight::SemiBold), "text-lines": hint_lines(&text, Weight::Regular), "title": title,
                   "text": text, "kind": if done && kind == "status_dim" { "status_ok" } else { kind },
                   "done": done, "button": button, "primary": primary, "busy": false})
        };
        let vcam = if let Some(VcamNote::Failed(detail)) = &self.vcam_note {
            step("Virtual camera", tr("Didn't install: {detail}").field("detail", detail), "status_err", false, tr("Try again"), true)
        } else if !self.vcam_checked {
            step("Virtual camera", tr("Checking…"), "status_dim", false, String::new(), false)
        } else if self.vcam_ready {
            let text = if self.linux { tr("Installed. Starting a stream may ask for your password to switch it on.") } else {
                tr("Installed. If Discord, Zoom or a similar app was already open, quit it fully (closing the window isn't enough if it's still in the tray) and open it again to see the camera.")
            };
            step("Virtual camera", text, "status_dim", true, String::new(), false)
        } else if self.linux {
            step("Virtual camera", tr(LINUX_INSTALL_HINT), "status_warn", false, tr("Check again"), false)
        } else {
            let text = if matches!(self.vcam_note, Some(VcamNote::Installing)) { tr("Installing… Windows asks for admin access.") } else {
                tr("Other apps see Telescope as a webcam through this driver. Installing it needs admin access.")
            };
            step("Virtual camera", text, "status_dim", false, tr("Install driver"), true)
        };
        let mut vcam = vcam;
        vcam["busy"] = json!(self.installing);
        let no_phone = !self.source.is_empty() && !self.asked;
        let paired = self.phones > 0 || no_phone;
        let not_needed = || tr("Not needed: you picked a camera that isn't a phone.");
        let app = if no_phone {
            step("Phone app", not_needed(), "status_dim", true, String::new(), false)
        } else if paired {
            step("Phone app", tr("Installed."), "status_dim", true, String::new(), false)
        } else {
            step("Phone app", tr("Scan this with the phone's camera app to download Telescope.apk, then open it to install. It's a download link, not the pairing code."),
                 "status_dim", false, String::new(), false)
        };
        let pair = if no_phone {
            step("Add your phone", not_needed(), "status_dim", true, String::new(), false)
        } else if paired {
            step("Add your phone", tr("Paired."), "status_dim", true, String::new(), false)
        } else {
            step("Add your phone", tr("Open Telescope on the phone first."), "status_dim", false, tr("Add phone"), true)
        };
        // Points at the real button rather than duplicating it, so people learn where it lives
        let start = if self.streamed {
            step("Start streaming", tr("Done."), "status_dim", true, String::new(), false)
        } else {
            step("Start streaming", tr("Click Start streaming in the top right corner of this window."), "status_dim", false, String::new(), false)
        };
        vec![vcam, app, pair, start]
    }

    fn render(&self, host: &mut dyn Host) {
        let paired = self.phones > 0 || (!self.source.is_empty() && !self.asked);
        host.set_ui_props("card", &json!({"steps": self.steps(), "asked": self.asked, "qr-shown": !paired, "visible": self.needed()}));
        host.emit(Event::SetupNeeded { showing: self.needed() });
    }
}

/// The QR code's dark cells and size, shared with the other cards that show one.
pub use telescope_core::qr::encode as qr;

impl Plugin for Onboarding {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "onboarding".into(),
            title: n_("Setup guide").into(),
            about: n_("The step by step checklist shown until the first stream works.").into(),
            when_off: n_("Without it the setup checklist never shows, not even when nothing is set up yet.").into(),
            category: "extras".into(),
            icon: "check".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, _host: &mut dyn Host) {
        self.check_virtual_camera();
    }

    fn card(&mut self) -> Option<CardSpec> {
        let (cells, size) = qr(APK_URL);
        let paired = self.phones > 0;
        Some(CardSpec {
            ui: UiSpec {
                source: SlintSource::Text(include_str!("../../ui/cards/onboarding.slint").into()),
                component: "OnboardingCard".into(),
                props: json!({"steps": self.steps(), "qr-cells": cells, "qr-size": size, "qr-shown": !paired, "visible": self.needed()}),
            },
            region: "center".into(),
        })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.render(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::RequestDone { request, ok, message, .. } if *request == VCAM_CHECKED => {
                (self.vcam_checked, self.vcam_ready, self.installing) = (true, *ok, false);
                self.vcam_note = (!message.is_empty() && !ok).then(|| VcamNote::Failed(message.clone()));
            }
            Event::PhonesChanged { count } => {
                self.phones = *count;
                self.vcam_note = None;
            }
            Event::SourceSelected { source_id } => {
                self.source = source_id.clone();
                self.vcam_note = None;
            }
            Event::SetupGuideRequested => {
                self.asked = !self.asked;
                self.vcam_note = None;
            }
            Event::StreamStarted { .. } => {
                self.streaming = true;
                self.asked = false;
            }
            Event::StreamStopped => self.streaming = false,
            Event::StreamConnected { .. } if !self.streamed => {
                self.streamed = true;
                self.vcam_note = None;
                host.schedule_save();
            }
            _ => return,
        }
        self.render(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        match (event.callback.as_str(), event.args.first().and_then(Value::as_i64)) {
            ("close", _) => {
                self.asked = false;
                self.vcam_note = None;
                self.render(host);
            }
            ("step-clicked", Some(0)) if self.linux => {
                self.vcam_checked = false;
                self.render(host);
                self.check_virtual_camera();
            }
            ("step-clicked", Some(0)) => self.install_driver(host),
            ("step-clicked", Some(2)) => host.emit(Event::AddPhoneRequested),
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        json!({"streamed": self.streamed})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.streamed = cfg.get("streamed") == Some(&Value::Bool(true));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use telescope_core::bus::EventBus;
    use telescope_core::testing::FakeHost;

    /// The plugin on its own, its card drawn once, as test_onboarding.py's env.
    struct Env {
        o: Onboarding,
        host: FakeHost,
    }

    impl Env {
        fn new() -> Env {
            let mut env = Env { o: Onboarding::new(None, None, None), host: FakeHost::new() };
            env.o.linux = true;
            env.o.on_plugins_ready(&mut env.host);
            env
        }

        fn windows() -> Env {
            let mut env = Env::new();
            env.o.linux = false;
            env
        }

        fn event(&mut self, event: Event) {
            self.o.on_event(&mut self.host, &event);
        }

        fn vcam(&mut self, ready: bool, detail: &str) {
            self.event(Event::RequestDone { request: VCAM_CHECKED, ok: ready, message: detail.into(), data: Value::Null });
        }

        fn ready_and_streamed(&mut self) {
            self.o.set_config(&json!({"streamed": true})).unwrap();
            self.vcam(true, "");
        }

        fn close(&mut self) {
            self.o.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: "close".into(), args: vec![] });
        }

        /// The properties the card last got.
        fn card(&self) -> Value {
            let last = self.host.calls.iter().rev().find_map(|c| c.strip_prefix("set_ui_props card ")).expect("a redraw");
            serde_json::from_str(last).unwrap()
        }

        fn shown(&self) -> bool {
            self.card()["visible"] == true
        }

        fn qr(&self) -> bool {
            self.card()["qr-shown"] == true
        }

        /// The close button shows when the card has an action, which it does while asked.
        fn close_button(&self) -> bool {
            self.card()["asked"] == true
        }

        fn step(&self, i: usize) -> Value {
            self.card()["steps"][i].clone()
        }

        fn needed(&self) -> Vec<bool> {
            self.host.events.iter().filter_map(|e| match e { Event::SetupNeeded { showing } => Some(*showing), _ => None }).collect()
        }
    }

    // test_onboarding.py::test_starts_as_a_checklist_with_nothing_done
    #[test]
    fn starts_as_a_checklist_with_nothing_done() {
        let env = Env::new();
        assert!(env.shown());
        assert_eq!(env.needed().last(), Some(&true));
        assert_eq!(env.step(0)["text"], "Checking…");
        assert_eq!(env.step(2)["button"], "Add phone");
        assert!(env.qr());
        assert!((0..4).all(|i| env.step(i)["done"] == false));
    }

    // test_onboarding.py::test_missing_v4l2loopback_says_which_package
    #[test]
    fn missing_v4l2loopback_says_which_package() {
        let mut env = Env::new();
        env.vcam(false, "");
        assert!(env.step(0)["text"].as_str().unwrap().contains("RPM Fusion"));
        assert!(env.step(0)["text"].as_str().unwrap().contains("v4l2loopback-dkms"));
        assert_eq!(env.step(0)["button"], "Check again");
    }

    // test_onboarding.py::test_windows_offers_to_install_the_driver
    #[test]
    fn windows_offers_to_install_the_driver() {
        let mut env = Env::windows();
        env.vcam(false, "");
        assert_eq!(env.step(0)["button"], "Install driver");
        env.vcam(false, "access denied");
        assert!(env.step(0)["text"].as_str().unwrap().contains("access denied"));
        assert_eq!(env.step(0)["kind"], "status_err");
        assert_eq!(env.step(0)["button"], "Try again");
        // A redraw for anything else goes back to the plain step, as Python's _render does
        env.event(Event::PhonesChanged { count: 1 });
        assert_eq!(env.step(0)["button"], "Install driver");
    }

    #[test]
    fn the_windows_button_runs_the_install_and_reports_back() {
        for (ok, detail, ready, panics) in [(true, "", true, false), (false, "access denied", false, false), (false, "boom", false, true)] {
            let bus = EventBus::new();
            let runs = Arc::new(AtomicU32::new(0));
            let counted = runs.clone();
            let install: VcamInstall = Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                assert!(!panics, "boom");
                SetupResult { ok, message: detail.into(), command: None }
            });
            let mut env = Env { o: Onboarding::new(Some(Arc::new(move || ready)), Some(install), Some(bus.sender())), host: FakeHost::new() };
            env.o.linux = false;
            env.vcam(false, "");
            env.o.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "step-clicked".into(), args: vec![json!(0)] });
            assert_eq!(env.step(0)["text"], "Installing… Windows asks for admin access.");
            assert_eq!(env.step(0)["busy"], true);
            let done = (0..500).find_map(|_| bus.next().or_else(|| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                None
            })).expect("the install reports back");
            env.event(done);
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert_eq!(env.step(0)["busy"], false);
            assert_eq!(env.step(0)["done"], ready);
            if !ready {
                assert!(env.step(0)["text"].as_str().unwrap().contains(detail), "{}", env.step(0));
            }
        }
    }

    // test_onboarding.py::test_add_phone_goes_through_the_bus
    #[test]
    fn add_phone_goes_through_the_bus() {
        let mut env = Env::new();
        env.o.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "step-clicked".into(), args: vec![json!(2)] });
        assert_eq!(env.host.events.iter().filter(|e| matches!(e, Event::AddPhoneRequested)).count(), 1);
    }

    // test_onboarding.py::test_checklist_steps_aside_after_the_first_stream
    #[test]
    fn checklist_steps_aside_after_the_first_stream() {
        let mut env = Env::new();
        env.vcam(true, "");
        env.event(Event::PhonesChanged { count: 1 });
        assert!(env.shown(), "paired and ready, but step 4 still points at Start streaming");
        assert_eq!(env.step(2)["done"], true);
        assert!(env.step(3)["text"].as_str().unwrap().contains("top right"));
        env.event(Event::StreamStarted { url: "url".into() });
        env.event(Event::StreamConnected { source_id: String::new() });
        env.event(Event::StreamStopped);
        assert!(!env.shown());
        assert_eq!(env.needed().last(), Some(&false));
        assert_eq!(env.o.get_config(), json!({"streamed": true}));
    }

    // test_onboarding.py::test_a_saved_first_stream_keeps_the_checklist_away
    #[test]
    fn a_saved_first_stream_keeps_the_checklist_away() {
        let mut env = Env::new();
        env.ready_and_streamed();
        env.event(Event::PhonesChanged { count: 1 });
        assert!(!env.shown());
        env.event(Event::PhonesChanged { count: 0 });
        assert!(env.shown(), "every phone removed: back to the checklist");
    }

    // test_onboarding.py::test_a_paired_phone_alone_is_not_enough_without_the_camera
    #[test]
    fn a_paired_phone_alone_is_not_enough_without_the_camera() {
        let mut env = Env::new();
        env.event(Event::PhonesChanged { count: 1 });
        env.vcam(false, "");
        assert!(env.shown());
        assert_eq!(env.step(1)["done"], true);
        assert!(!env.qr());
    }

    // test_onboarding.py::test_a_stream_source_stands_in_for_the_phone_steps
    #[test]
    fn a_stream_source_stands_in_for_the_phone_steps() {
        let mut env = Env::new();
        env.ready_and_streamed();
        env.event(Event::SourceSelected { source_id: "browser".into() });
        assert!(!env.shown(), "no phone, but nothing left to set up");
        assert!(env.step(2)["text"].as_str().unwrap().starts_with("Not needed"));
        assert!(!env.qr());
        env.event(Event::SourceSelected { source_id: String::new() });
        assert!(env.shown());
    }

    // test_onboarding.py::test_the_help_button_brings_the_phone_steps_back_until_closed
    #[test]
    fn the_help_button_brings_the_phone_steps_back_until_closed() {
        let mut env = Env::new();
        env.ready_and_streamed();
        env.event(Event::SourceSelected { source_id: "browser".into() });
        assert!(!env.shown() && !env.close_button());
        env.event(Event::SetupGuideRequested);
        assert!(env.shown());
        assert_eq!(env.needed().last(), Some(&true));
        assert_eq!(env.step(2)["button"], "Add phone", "not Not needed: a phone is why they asked");
        assert!(env.qr() && env.close_button());
        env.close();
        assert!(!env.shown());
        assert_eq!(env.needed().last(), Some(&false));
        assert!(env.step(2)["text"].as_str().unwrap().starts_with("Not needed"));
        env.event(Event::SetupGuideRequested);
        env.event(Event::SetupGuideRequested);
        assert!(!env.shown(), "the same button closes it");
    }

    // test_onboarding.py::test_a_stream_starting_closes_the_guide
    #[test]
    fn a_stream_starting_closes_the_guide() {
        let mut env = Env::new();
        env.ready_and_streamed();
        env.event(Event::PhonesChanged { count: 1 });
        env.event(Event::SetupGuideRequested);
        assert!(env.shown());
        env.event(Event::StreamStarted { url: "url".into() });
        assert!(!env.shown());
        env.event(Event::StreamStopped);
        assert!(!env.shown());
    }

    // test_onboarding.py::test_streaming_hides_the_checklist
    #[test]
    fn streaming_hides_the_checklist() {
        let mut env = Env::new();
        env.event(Event::StreamStarted { url: "url".into() });
        assert!(!env.shown());
        assert_eq!(env.needed().last(), Some(&false));
        env.event(Event::StreamStopped);
        assert!(env.shown());
    }

    #[test]
    fn the_qr_code_is_the_download_link() {
        let (cells, size) = qr(APK_URL);
        assert!(size >= 21 && !cells.is_empty() && cells.iter().all(|&c| c < size * size));
    }

    #[test]
    fn the_qr_code_has_the_cells_pythons_qrcode_draws() {
        // (size, dark cells, sum of their indexes) from qrcode.QRCode(ERROR_CORRECT_L).modules in Python
        for (data, want) in [(APK_URL, (37, 704, 483140)), ("telescope://pair?x=1", (25, 324, 102703)), ("hello", (21, 230, 49898))] {
            let (cells, size) = qr(data);
            assert_eq!((size, cells.len(), cells.iter().sum::<usize>()), want, "{data}");
        }
    }
}
