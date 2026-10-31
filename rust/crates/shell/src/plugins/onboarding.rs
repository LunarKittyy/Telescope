//! First-run checklist on the video stage (plugins/onboarding.py): virtual camera, phone app, add a phone, start
//! streaming. It shows until the first stream has delivered frames, then gets out of the way until
//! setup_guide_requested opens it again; the next one closes it.

use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::i18n::{n_, tr, Fields};
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

/// What the checklist's buttons do, each off the UI thread but usb_ready.
#[derive(Clone)]
pub struct Installers {
    /// Installs the virtual camera: (whether it's there now, what went wrong).
    pub camera: Arc<dyn Fn() -> (bool, String) + Send + Sync>,
    /// The phone app is bundled and adb can put it on a phone.
    pub usb_ready: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Installs the phone app over USB; the error is what to tell the user.
    pub phone_app: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

/// The answer to a virtual camera check comes back on the bus as a RequestDone with this id.
const VCAM_CHECKED: RequestId = RequestId(u64::MAX - 1);
const VCAM_INSTALLED: RequestId = RequestId(u64::MAX - 2);
const APP_INSTALLED: RequestId = RequestId(u64::MAX - 3);

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
    bus: Option<BusSender>,
    linux: bool,
    installers: Option<Installers>,
    /// The driver is installing, and why it last didn't.
    vcam_busy: bool,
    vcam_failed: String,
    /// The phone app is installing over USB, and what the last try came to: (text, kind), until something else changes.
    app_busy: bool,
    app_note: Option<(String, &'static str)>,
}

impl Onboarding {
    pub fn new(check: Option<VcamCheck>, bus: Option<BusSender>) -> Onboarding {
        Onboarding {
            phones: 0, vcam_ready: false, vcam_checked: false, streaming: false, streamed: false, source: String::new(),
            asked: false, check, bus, linux: cfg!(target_os = "linux"), installers: None, vcam_busy: false, vcam_failed: String::new(),
            app_busy: false, app_note: None,
        }
    }

    pub fn with_installers(mut self, installers: Option<Installers>) -> Onboarding {
        self.installers = installers;
        self
    }

    /// _vcam_action on Windows.
    fn install_camera(&mut self, host: &mut dyn Host) {
        let (Some(installers), Some(bus)) = (self.installers.clone(), self.bus.clone()) else { return };
        self.vcam_busy = true;
        self.vcam_failed.clear();
        self.render(host);
        std::thread::spawn(move || {
            let (ready, detail) = (installers.camera)();
            bus.emit(Event::RequestDone { request: VCAM_INSTALLED, ok: ready, message: detail, data: Value::Null });
        });
    }

    /// _install_apk.
    fn install_app(&mut self, host: &mut dyn Host) {
        let (Some(installers), Some(bus)) = (self.installers.clone(), self.bus.clone()) else { return };
        self.app_busy = true;
        self.app_note = None;
        self.render(host);
        std::thread::spawn(move || {
            let result = (installers.phone_app)();
            let (ok, message) = (result.is_ok(), result.err().unwrap_or_default());
            bus.emit(Event::RequestDone { request: APP_INSTALLED, ok, message, data: Value::Null });
        });
    }

    fn check_virtual_camera(&mut self) {
        let (Some(check), Some(bus)) = (self.check.clone(), self.bus.clone()) else { return };
        std::thread::spawn(move || {
            let ready = check();
            bus.emit(Event::RequestDone { request: VCAM_CHECKED, ok: ready, message: String::new(), data: Value::Null });
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
        let vcam = if !self.vcam_checked {
            step("Virtual camera", tr("Checking…"), "status_dim", false, String::new(), false)
        } else if self.vcam_ready {
            let text = if self.linux { tr("Installed. Starting a stream may ask for your password to switch it on.") } else {
                tr("Installed. If Discord, Zoom or a similar app was already open, quit it fully (closing the window isn't enough if it's still in the tray) and open it again to see the camera.")
            };
            step("Virtual camera", text, "status_dim", true, String::new(), false)
        } else if self.linux {
            step("Virtual camera", tr(LINUX_INSTALL_HINT), "status_warn", false, tr("Check again"), false)
        } else if self.vcam_busy {
            let mut installing = step("Virtual camera", tr("Installing… Windows asks for admin access."), "status_dim", false, tr("Install driver"), true);
            installing["busy"] = json!(true);
            installing
        } else if !self.vcam_failed.is_empty() {
            step("Virtual camera", tr("Didn't install: {detail}").field("detail", &self.vcam_failed), "status_err", false, tr("Try again"), true)
        } else {
            step("Virtual camera", tr("Other apps see Telescope as a webcam through this driver. Installing it needs admin access."),
                 "status_dim", false, tr("Install driver"), true)
        };
        let no_phone = !self.source.is_empty() && !self.asked;
        let paired = self.phones > 0 || no_phone;
        let not_needed = || tr("Not needed: you picked a camera that isn't a phone.");
        let app = if no_phone {
            step("Phone app", not_needed(), "status_dim", true, String::new(), false)
        } else if paired {
            step("Phone app", tr("Installed."), "status_dim", true, String::new(), false)
        } else if self.app_busy {
            let mut installing = step("Phone app", tr("Installing over USB…"), "status_dim", false, tr("Install over USB"), false);
            installing["busy"] = json!(true);
            installing
        } else if let Some((text, kind)) = &self.app_note {
            step("Phone app", text.clone(), kind, false, tr("Install over USB"), false)
        } else {
            let usb = self.installers.as_ref().is_some_and(|i| (i.usb_ready)());
            step("Phone app", tr("Scan this with the phone's camera app to download Telescope.apk, then open it to install. It's a download link, not the pairing code."),
                 "status_dim", false, if usb { tr("Install over USB") } else { String::new() }, false)
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
        let note = self.app_note.take();
        match event {
            Event::RequestDone { request, ok, .. } if *request == VCAM_CHECKED => (self.vcam_checked, self.vcam_ready) = (true, *ok),
            Event::RequestDone { request, ok, message, .. } if *request == VCAM_INSTALLED => {
                (self.vcam_checked, self.vcam_ready, self.vcam_busy) = (true, *ok, false);
                self.vcam_failed = if *ok { String::new() } else { message.clone() };
            }
            Event::RequestDone { request, ok, message, .. } if *request == APP_INSTALLED => {
                self.app_busy = false;
                self.app_note = Some(if *ok { (tr("Installed. Open Telescope on the phone for the next step."), "status_ok") } else { (message.clone(), "status_err") });
            }
            Event::PhonesChanged { count } => self.phones = *count,
            Event::SourceSelected { source_id } => self.source = source_id.clone(),
            Event::SetupGuideRequested => self.asked = !self.asked,
            Event::StreamStarted { .. } => {
                self.streaming = true;
                self.asked = false;
            }
            Event::StreamStopped => self.streaming = false,
            Event::StreamConnected { .. } if !self.streamed => {
                self.streamed = true;
                host.schedule_save();
            }
            _ => {
                self.app_note = note;
                return;
            }
        }
        self.render(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        match (event.callback.as_str(), event.args.first().and_then(Value::as_i64)) {
            ("close", _) => {
                self.asked = false;
                self.render(host);
            }
            ("step-clicked", Some(0)) if self.linux => {
                self.vcam_checked = false;
                self.render(host);
                self.check_virtual_camera();
            }
            ("step-clicked", Some(0)) => self.install_camera(host),
            ("step-clicked", Some(1)) => self.install_app(host),
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
    use telescope_core::testing::FakeHost;

    fn texts(o: &Onboarding) -> Vec<(String, bool)> {
        o.steps().iter().map(|s| (s["text"].as_str().unwrap().to_string(), s["done"].as_bool().unwrap())).collect()
    }

    #[test]
    fn starts_as_a_checklist_with_nothing_done() {
        let o = Onboarding::new(None, None);
        assert!(texts(&o).iter().all(|(_, done)| !done));
        assert_eq!(texts(&o)[0].0, "Checking…");
        assert!(o.needed());
    }

    #[test]
    fn missing_v4l2loopback_says_which_package() {
        let mut o = Onboarding::new(None, None);
        (o.vcam_checked, o.vcam_ready, o.linux) = (true, false, true);
        let steps = o.steps();
        assert!(steps[0]["text"].as_str().unwrap().contains("v4l2loopback-dkms"));
        assert_eq!(steps[0]["button"], "Check again");
    }

    #[test]
    fn windows_offers_to_install_the_driver() {
        let mut o = Onboarding::new(None, None);
        (o.vcam_checked, o.vcam_ready, o.linux) = (true, false, false);
        assert_eq!(o.steps()[0]["button"], "Install driver");
    }

    #[test]
    fn add_phone_goes_through_the_bus() {
        let mut o = Onboarding::new(None, None);
        let mut host = FakeHost::new();
        o.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "step-clicked".into(), args: vec![json!(2)] });
        assert!(matches!(host.events.last(), Some(Event::AddPhoneRequested)));
    }

    #[test]
    fn checklist_steps_aside_after_the_first_stream() {
        let mut o = Onboarding::new(None, None);
        let mut host = FakeHost::new();
        (o.vcam_checked, o.vcam_ready) = (true, true);
        o.on_event(&mut host, &Event::PhonesChanged { count: 1 });
        assert!(o.needed());
        o.on_event(&mut host, &Event::StreamStarted { url: String::new() });
        assert!(!o.needed());
        o.on_event(&mut host, &Event::StreamConnected { source_id: "p".into() });
        o.on_event(&mut host, &Event::StreamStopped);
        assert!(!o.needed());
        assert_eq!(o.get_config(), json!({"streamed": true}));
    }

    #[test]
    fn a_paired_phone_alone_is_not_enough_without_the_camera() {
        let mut o = Onboarding::new(None, None);
        o.set_config(&json!({"streamed": true})).unwrap();
        o.phones = 1;
        assert!(o.needed());
        (o.vcam_checked, o.vcam_ready) = (true, true);
        assert!(!o.needed());
    }

    #[test]
    fn a_stream_source_stands_in_for_the_phone_steps() {
        let mut o = Onboarding::new(None, None);
        o.source = "browser".into();
        let t = texts(&o);
        assert!(t[1].1 && t[2].1 && t[1].0.starts_with("Not needed"));
    }

    #[test]
    fn the_help_button_brings_the_phone_steps_back_until_closed() {
        let mut o = Onboarding::new(None, None);
        let mut host = FakeHost::new();
        o.source = "browser".into();
        o.on_event(&mut host, &Event::SetupGuideRequested);
        assert!(o.asked && !texts(&o)[1].1);
        o.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "close".into(), args: vec![] });
        assert!(!o.asked);
    }

    #[test]
    fn a_stream_starting_closes_the_guide() {
        let mut o = Onboarding::new(None, None);
        let mut host = FakeHost::new();
        o.on_event(&mut host, &Event::SetupGuideRequested);
        o.on_event(&mut host, &Event::StreamStarted { url: String::new() });
        assert!(!o.asked && !o.needed());
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

    fn event(o: &mut Onboarding, host: &mut FakeHost, e: Event) {
        o.on_event(host, &e);
    }

    fn checked(ready: bool) -> (Onboarding, FakeHost) {
        let (mut o, mut host) = (Onboarding::new(None, None), FakeHost::new());
        event(&mut o, &mut host, Event::RequestDone { request: VCAM_CHECKED, ok: ready, message: String::new(), data: Value::Null });
        (o, host)
    }

    #[test]
    fn a_saved_first_stream_keeps_the_checklist_away() {
        let (mut o, mut host) = checked(true);
        o.set_config(&json!({"streamed": true})).unwrap();
        event(&mut o, &mut host, Event::PhonesChanged { count: 1 });
        assert!(!o.needed());
        event(&mut o, &mut host, Event::PhonesChanged { count: 0 });
        assert!(o.needed(), "every phone removed: back to the checklist");
    }

    #[test]
    fn streaming_hides_the_checklist() {
        let (mut o, mut host) = checked(false);
        event(&mut o, &mut host, Event::StreamStarted { url: "url".into() });
        assert!(!o.needed());
        assert!(matches!(host.events.last(), Some(Event::SetupNeeded { showing: false })));
        event(&mut o, &mut host, Event::StreamStopped);
        assert!(o.needed());
    }

    fn installers(usb: bool, camera: (bool, &'static str), app: Result<(), &'static str>) -> Option<Installers> {
        Some(Installers {
            camera: Arc::new(move || (camera.0, camera.1.to_string())),
            usb_ready: Arc::new(move || usb),
            phone_app: Arc::new(move || app.map_err(String::from)),
        })
    }

    fn step_of(o: &Onboarding, i: usize) -> Value {
        o.steps()[i].clone()
    }

    #[test]
    fn usb_install_is_offered_only_with_a_bundled_apk_and_adb() {
        let (o, _) = checked(false);
        assert_eq!(step_of(&o, 1)["button"], "");
        let (o, _) = checked(false);
        let o = o.with_installers(installers(false, (true, ""), Ok(())));
        assert_eq!(step_of(&o, 1)["button"], "");
        let (o, _) = checked(false);
        let o = o.with_installers(installers(true, (true, ""), Ok(())));
        assert_eq!(step_of(&o, 1)["button"], "Install over USB");
    }

    fn click(o: &mut Onboarding, host: &mut FakeHost, step: i64) {
        o.on_ui(host, &UiEvent { ui: "card".into(), callback: "step-clicked".into(), args: vec![json!(step)] });
    }

    /// A checklist whose installs report back on a bus of its own.
    fn installing(camera: (bool, &'static str), app: Result<(), &'static str>) -> (Onboarding, FakeHost, telescope_core::EventBus) {
        let bus = telescope_core::EventBus::new();
        let (mut o, mut host) = (Onboarding::new(None, Some(bus.sender())).with_installers(installers(true, camera, app)), FakeHost::new());
        o.linux = false;
        event(&mut o, &mut host, Event::RequestDone { request: VCAM_CHECKED, ok: false, message: String::new(), data: Value::Null });
        (o, host, bus)
    }

    fn answer(o: &mut Onboarding, host: &mut FakeHost, bus: &telescope_core::EventBus) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if let Some(e) = bus.next() {
                return event(o, host, e);
            }
            std::thread::yield_now();
        }
        panic!("no answer");
    }

    #[test]
    fn usb_install_reports_a_missing_phone() {
        let (mut o, mut host, bus) = installing((true, ""), Err("No phone found over USB."));
        click(&mut o, &mut host, 1);
        let busy = step_of(&o, 1);
        assert_eq!((busy["text"].as_str(), busy["busy"].as_bool()), (Some("Installing over USB…"), Some(true)));
        answer(&mut o, &mut host, &bus);
        let failed = step_of(&o, 1);
        assert_eq!((failed["text"].as_str(), failed["kind"].as_str(), failed["busy"].as_bool()), (Some("No phone found over USB."), Some("status_err"), Some(false)));
    }

    #[test]
    fn a_phone_app_installed_over_usb_says_what_comes_next_until_something_changes() {
        let (mut o, mut host, bus) = installing((true, ""), Ok(()));
        click(&mut o, &mut host, 1);
        answer(&mut o, &mut host, &bus);
        assert_eq!(step_of(&o, 1)["text"], "Installed. Open Telescope on the phone for the next step.");
        assert_eq!(step_of(&o, 1)["kind"], "status_ok");
        event(&mut o, &mut host, Event::StreamStopped);
        assert!(step_of(&o, 1)["text"].as_str().unwrap().starts_with("Scan this"));
    }

    #[test]
    fn the_driver_installs_from_the_checklist_and_says_why_it_didnt() {
        let (mut o, mut host, bus) = installing((false, "Refused"), Ok(()));
        click(&mut o, &mut host, 0);
        let busy = step_of(&o, 0);
        assert_eq!((busy["text"].as_str(), busy["busy"].as_bool()), (Some("Installing… Windows asks for admin access."), Some(true)));
        answer(&mut o, &mut host, &bus);
        let failed = step_of(&o, 0);
        assert_eq!((failed["text"].as_str(), failed["kind"].as_str(), failed["button"].as_str()), (Some("Didn't install: Refused"), Some("status_err"), Some("Try again")));

        let (mut o, mut host, bus) = installing((true, ""), Ok(()));
        click(&mut o, &mut host, 0);
        answer(&mut o, &mut host, &bus);
        assert_eq!(step_of(&o, 0)["done"], true);
    }
}
