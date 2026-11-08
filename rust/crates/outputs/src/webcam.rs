//! The Webcam plugin (plugins/webcam.py): stream from a webcam or capture card plugged into this computer.
//!
//! Each webcam is a StreamSource of its own, listed in the phone picker by its name while it's plugged in, and streams
//! like a phone does. One that's unplugged while it streams keeps its place and carries on when it's plugged back in;
//! one that isn't streaming just leaves the picker. The card shows while a webcam is picked and holds its resolution and
//! frame rate, plus whichever picture settings the camera itself offers, all kept per camera and put back each time it's
//! opened. A camera that's only plugged in is never written to.

use telescope_core::i18n::Fields;
use crate::webcam_capture::{self, ReaderDeps};
use crate::webcam_controls::{jog_value, FAR, NEAR};
use crate::webcam_controls::{apply_order, auto_of, choice_text, default_of, local_flicker, shown_value, stops_apart, Control, Controls, AUTOS};
use crate::webcam_hold::{jog_level, JogHold, JogTick};
use telescope_core::hold::{self, Hold, Tick, TICK_MS};
use crate::webcams::{self, Choice, Device};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::media::{Codec, Decoder, VideoFeed};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::source::{ControlClient, SourceStills, StreamSource};
use telescope_core::ui::{CardSpec, Issue, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "webcam";
pub const SOURCE_PREFIX: &str = "webcam:";
const UI: &str = include_str!("../ui/webcam.slint");
/// Linux: how often to look for webcams plugged in or out.
pub const SCAN_MS: u64 = 2000;
pub const DEFAULT_FPS: u32 = 30;
/// Offered when the camera doesn't say which rates it has.
pub const FPS_CHOICES: [u32; 6] = [15, 24, 25, 30, 50, 60];
/// Exposure time slider positions, spread out evenly in stops.
const LOG_STEPS: f64 = 1000.0;
const T_SCAN: &str = "scan";
const T_HOLD: &str = "hold";
const T_JOG: &str = "jog";
const P_REPORTED: &str = "reported";
const P_UNPLUGGED: &str = "unplugged";

/// The card's picture settings, by section, in the order they're shown: (section, icon, [(name, label)]).
type Section = (&'static str, &'static str, &'static [(&'static str, &'static str)]);
const SECTIONS: [Section; 5] = [
    ("light", "sun", &[("exposure_auto", n_("Exposure")), ("exposure_time", n_("Exposure time")), ("gain", n_("Gain")),
                       ("brightness", n_("Brightness")), ("backlight", n_("Backlight compensation")),
                       ("anti_flicker", n_("Anti-flicker")), ("frame_rate_drop", n_("Slow down in the dark"))]),
    ("image", "image", &[("white_balance_auto", n_("White balance")), ("white_balance", n_("Temperature")),
                         ("saturation", n_("Saturation")), ("hue", n_("Hue")), ("contrast", n_("Contrast")),
                         ("gamma", n_("Gamma")), ("sharpness", n_("Sharpness"))]),
    ("focus", "focus", &[("focus_auto", n_("Focus")), ("focus", n_("Focus position"))]),
    // A DSLR's own settings, only the ones its mode allows now (webcam_capture/gphoto_lib.rs)
    ("dslr", "focus", &[("dslr_focus", n_("Focus")), ("dslr_aperture", n_("Aperture")), ("dslr_exposure_comp", n_("Exposure")),
                        ("dslr_white_balance", n_("White balance"))]),
    ("framing", "crop", &[("zoom", n_("Zoom")), ("pan", n_("Pan")), ("tilt", n_("Tilt"))]),
];

/// The rows without what only follows a value (where a slider is, what a box reads, which choice is picked): what stays when a
/// control is only moved.
fn structure(rows: &[Value]) -> Vec<Value> {
    rows.iter().map(|r| {
        let mut r = r.clone();
        if let Some(m) = r.as_object_mut() {
            for key in ["position", "index", "checked", "readout"] {
                m.remove(key);
            }
        }
        r
    }).collect()
}

fn section_title(section: &str) -> String {
    match section {
        // "Light" has the context "camera section" in Python; contexts stay English for now
        "light" => tr("Light"),
        "image" => tr("Image"),
        "focus" => tr("Focus"),
        "dslr" => tr("Camera"),
        _ => tr("Framing"),
    }
}

fn tip(name: &str) -> String {
    match name {
        "exposure_time" => tr("Longer exposure times make the picture brighter but blur movement."),
        "gain" => tr("Higher gain makes the picture brighter but grainier."),
        "backlight" => tr("Brighten a face in front of a bright window."),
        "anti_flicker" => tr("Set it to your mains frequency to stop lights flickering or banding."),
        "dslr_exposure_comp" => tr("Make the picture brighter or darker than the webcam's automatic exposure."),
        "frame_rate_drop" => tr("Let the webcam lower its frame rate in dim light for a brighter picture."),
        "white_balance" => tr("Make colours warmer or cooler."),
        "saturation" => tr("Make the picture more or less colourful."),
        "hue" => tr("Shift every colour around the colour wheel."),
        "contrast" => tr("Spread light and dark parts further apart or closer together."),
        "gamma" => tr("Brighten or darken the midtones."),
        "sharpness" => tr("Make edges crisper or softer."),
        "focus" => tr("How far away the camera focuses."),
        "dslr_aperture" => tr("Lower numbers open the lens wider and let in more light."),
        "pan" => tr("Move the view left or right."),
        "tilt" => tr("Move the view up or down."),
        _ => String::new(),
    }
}

fn flicker_label(v: i64) -> Option<String> {
    Some(match v {
        0 => tr("Off"),
        1 => tr("50 Hz"),
        2 => tr("60 Hz"),
        3 => tr("Auto"),
        _ => return None,
    })
}

pub fn is_webcam(source_id: &str) -> bool {
    source_id.starts_with(SOURCE_PREFIX)
}

pub fn log_pos_to_val(pos: f64, steps: f64, v_min: f64, v_max: f64) -> f64 {
    let v_min = if v_min <= 0.0 { 1.0 } else { v_min };
    if v_max <= v_min {
        return v_min;
    }
    let t = pos / steps.max(1.0);
    (v_min.ln() + t * (v_max.ln() - v_min.ln())).exp().clamp(v_min, v_max)
}

pub fn val_to_log_pos(val: f64, steps: f64, v_min: f64, v_max: f64) -> f64 {
    if val <= 0.0 || v_min <= 0.0 || v_max <= v_min {
        return 0.0;
    }
    let val = val.clamp(v_min, v_max);
    ((val.ln() - v_min.ln()) / (v_max.ln() - v_min.ln()) * steps).round_ties_even()
}

/// One camera's saved settings: its size (None for Auto), fps and picture settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CamCfg {
    pub size: Option<(u32, u32)>,
    pub fps: Option<u32>,
    pub controls: Option<Vec<(String, i64)>>,
}

impl CamCfg {
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        if let Some((w, h)) = self.size {
            m.insert("size".into(), json!([w, h]));
        }
        if let Some(f) = self.fps {
            m.insert("fps".into(), json!(f));
        }
        if let Some(c) = &self.controls {
            m.insert("controls".into(), Value::Object(c.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
        }
        Value::Object(m)
    }
}

/// What the streams' threads read: the webcams as plugged in now and what's picked for each.
#[derive(Default)]
pub struct Shared {
    devices: Mutex<HashMap<String, Device>>,
    settings: Mutex<HashMap<String, CamCfg>>,
}

impl Shared {
    pub fn device(&self, key: &str) -> Option<Device> {
        self.devices.lock().unwrap().get(key).cloned()
    }

    /// (size or None for Auto, fps) picked for this camera.
    pub fn wanted(&self, key: &str) -> (Option<(u32, u32)>, u32) {
        let s = self.settings.lock().unwrap();
        let cfg = s.get(key).cloned().unwrap_or_default();
        (cfg.size, cfg.fps.unwrap_or(DEFAULT_FPS))
    }

    /// What to ask device for, from what's picked for it.
    pub fn choice(&self, device: &Device) -> Option<Choice> {
        let (size, fps) = self.wanted(&device.key);
        if device.modes.is_empty() {
            // Nothing listed: ask for what's picked and take what comes
            let (w, h) = size.unwrap_or((1280, 720));
            return Some(Choice { fourcc: "RAW".into(), width: w, height: h, fps: fps as f64 });
        }
        webcams::choose(&device.modes, size, fps as f64)
    }
}

/// The parts a source needs from the system, so tests can stand in for cameras.
#[derive(Clone)]
pub struct System {
    pub opener: webcam_capture::Opener,
    pub users: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub exists: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl Default for System {
    fn default() -> System {
        #[cfg(target_os = "linux")]
        return System {
            opener: Arc::new(|d: &Device, c: &Choice| {
                if webcams::is_gphoto(&d.location) { webcam_capture::gphoto::open("gphoto2", d) } else { webcam_capture::v4l2::open(&d.location, c) }
            }),
            users: Arc::new(|p: &str| if webcams::is_gphoto(p) { String::new() } else { webcams::who_uses(p, "/proc") }),
            exists: Arc::new(|p: &str| if webcams::is_gphoto(p) { webcams::gphoto_present(p, "/dev/bus/usb") } else { std::path::Path::new(p).exists() }),
        };
        #[cfg(windows)]
        return System {
            opener: Arc::new(|d: &Device, c: &Choice| crate::webcams_win::open(&d.location, c)),
            // Windows doesn't say which app holds a camera
            users: Arc::new(|_: &str| String::new()),
            exists: Arc::new(crate::webcams_win::present),
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        System {
            opener: Arc::new(|_: &Device, _: &Choice| Err((false, "Webcams on this system aren't in this build yet".into()))),
            users: Arc::new(|_: &str| String::new()),
            exists: Arc::new(|_: &str| true),
        }
    }
}

/// One webcam in the picker.
pub struct WebcamSource {
    key: String,
    name: Mutex<String>,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    system: System,
    waiting: Arc<Mutex<String>>,
    last: Mutex<Option<Choice>>,
    /// A DSLR's photo service, made once and following the camera's device, so a camera back on another USB port is still its camera.
    #[cfg(target_os = "linux")]
    stills: std::sync::OnceLock<Arc<webcam_capture::gphoto_stills::GphotoStills>>,
}

impl WebcamSource {
    fn deps(&self) -> ReaderDeps {
        let (s1, s2, k1, k2) = (self.shared.clone(), self.shared.clone(), self.key.clone(), self.key.clone());
        let poster = self.poster.clone();
        let key = self.key.clone();
        ReaderDeps {
            locate: Arc::new(move || s1.device(&k1)),
            want: Arc::new(move |d| {
                let _ = &k2;
                s2.choice(d)
            }),
            report: Arc::new(move |state, detail| {
                if let Some(p) = &poster {
                    p.post(P_REPORTED, json!([key, state, detail]));
                }
            }),
            opener: self.system.opener.clone(),
            users: self.system.users.clone(),
            exists: self.system.exists.clone(),
        }
    }
}

struct NoControl;

impl ControlClient for NoControl {
    fn send(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        Err("a webcam has no phone to ask".into())
    }
    fn has_audio(&self) -> bool {
        false
    }
}

impl StreamSource for WebcamSource {
    fn id(&self) -> String {
        format!("{SOURCE_PREFIX}{}", self.key)
    }
    fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }
    fn url(&self) -> String {
        SOURCE_PREFIX.into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        // At Start: a webcam that was unplugged since it was picked can't stream
        if self.shared.device(&self.key).is_some() {
            return true;
        }
        if let Some(p) = &self.poster {
            p.post(P_UNPLUGGED, json!(self.name()));
        }
        false
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        self.open_feed_stoppable(&telescope_core::media::OpenStop::default())
    }
    fn open_feed_stoppable(&self, open: &telescope_core::media::OpenStop) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        let (choice, feed) = webcam_capture::open_stoppable(&self.deps(), &self.waiting, open)?;
        *self.last.lock().unwrap() = Some(choice);
        Ok((Codec::Mjpeg, Box::new(feed)))
    }
    fn decoder(&self) -> Option<Box<dyn Decoder>> {
        let choice = self.last.lock().unwrap().clone()?;
        (!choice.compressed()).then(|| Box::new(webcam_capture::RawDecoder { fourcc: choice.fourcc, width: choice.width, height: choice.height }) as Box<dyn Decoder>)
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(NoControl))
    }
    fn fps(&self) -> u32 {
        let choice = self.shared.device(&self.key).and_then(|d| self.shared.choice(&d));
        match choice {
            Some(c) if c.fps != 0.0 => c.fps.round() as u32,
            _ => self.shared.wanted(&self.key).1,
        }
    }
    fn lost_hint(&self) -> String {
        tr("Check that it's plugged in.")
    }
    fn waiting_text(&self) -> String {
        self.waiting.lock().unwrap().clone()
    }
    fn stills(&self) -> Option<Arc<dyn SourceStills>> {
        // A DSLR takes full photos through the camera thread that streams its live view
        #[cfg(target_os = "linux")]
        {
            let device = self.shared.device(&self.key)?;
            if webcams::is_gphoto(&device.location) {
                let (shared, key) = (self.shared.clone(), self.key.clone());
                let stills = self.stills.get_or_init(|| Arc::new(webcam_capture::gphoto_stills::GphotoStills::following(Arc::new(move || shared.device(&key).map(|d| d.location)))));
                return Some(stills.clone());
            }
        }
        None
    }
}

/// Lists the webcams on this system.
pub type Scanner = Box<dyn FnMut() -> Result<Vec<Device>, String> + Send>;
/// What reads and sets a webcam's picture settings.
pub type ControlsFor = Box<dyn Fn(&Device) -> Option<Box<dyn Controls>> + Send>;

pub struct WebcamPlugin {
    scan: Scanner,
    controls_for: ControlsFor,
    system: System,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    controls: HashMap<String, HashMap<String, Control>>,
    backends: HashMap<String, (Device, Box<dyn Controls>)>,
    sources: HashMap<String, Arc<WebcamSource>>,
    states: HashMap<String, (String, String)>,
    modes: HashMap<String, String>,
    selected_key: String,
    stopped: bool,
    /// The rows the card holds now, so an unchanged list is never sent again (a new list rebuilds every slider and breaks off a drag)
    shown_rows: Option<Vec<Value>>,
    /// The same for the strip of a DSLR's values
    shown_tiles: Option<Vec<Value>>,
    /// The arrow held down on the card, if any
    held: Option<Hold>,
    /// The focus jog's handle while it's held out, if it is
    jogged: Option<JogHold>,
}

/// What came of a step a button asked for.
#[derive(Debug, PartialEq, Eq)]
enum Nudge {
    Sent,
    /// The camera had no room for it
    Dropped,
    /// There is no further to go
    End,
}

impl WebcamPlugin {
    pub fn new(scan: Scanner, controls_for: ControlsFor, system: System) -> WebcamPlugin {
        WebcamPlugin {
            scan, controls_for, system, shared: Arc::default(), poster: None, controls: HashMap::new(), backends: HashMap::new(),
            sources: HashMap::new(), states: HashMap::new(), modes: HashMap::new(), selected_key: String::new(), stopped: false, shown_rows: None, shown_tiles: None, held: None, jogged: None,
        }
    }

    /// The plugin as the app runs it: Linux's scanner and V4L2 controls.
    pub fn system() -> WebcamPlugin {
        #[cfg(target_os = "linux")]
        let scan: Scanner = {
            let mut scanner = webcams::linux_scanner();
            let mut dslrs = webcams::GphotoScanner::system();
            Box::new(move || {
                let mut found = scanner.scan();
                found.extend(dslrs.scan());
                Ok(found)
            })
        };
        #[cfg(windows)]
        let scan: Scanner = {
            let mut scanner = crate::webcams_win::WindowsScanner::default();
            Box::new(move || Ok(scanner.scan()))
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        let scan: Scanner = Box::new(|| Ok(Vec::new()));
        WebcamPlugin::new(scan, Box::new(crate::webcam_controls::controls_for), System::default())
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn rescan(&mut self, host: &mut dyn Host) {
        let found = match (self.scan)() {
            Ok(f) => f,
            Err(e) => {
                log::error!("Couldn't look for webcams: {e}");
                return;
            }
        };
        let gone: Vec<String> = self.shared.devices.lock().unwrap().keys().filter(|k| !found.iter().any(|d| &d.key == *k)).cloned().collect();
        for key in gone {
            self.backends.remove(&key); // its handle on the camera is dead; a replugged one gets a fresh one
        }
        *self.shared.devices.lock().unwrap() = found.iter().map(|d| (d.key.clone(), d.clone())).collect();
        telescope_core::redact::set_hidden("webcam", &found.iter().map(|d| d.name.clone()).collect::<Vec<_>>());
        for dev in &found {
            match self.sources.get(&dev.key) {
                None => {
                    let src = Arc::new(WebcamSource {
                        key: dev.key.clone(), name: Mutex::new(dev.name.clone()), shared: self.shared.clone(), poster: self.poster.clone(),
                        system: self.system.clone(), waiting: Arc::new(Mutex::new(tr(webcam_capture::BUSY_TEXT))), last: Mutex::new(None),
                        #[cfg(target_os = "linux")]
                        stills: std::sync::OnceLock::new(),
                    });
                    self.sources.insert(dev.key.clone(), src.clone());
                    host.add_stream_source(src);
                }
                Some(src) if src.name() != dev.name => {
                    *src.name.lock().unwrap() = dev.name.clone();
                    host.add_stream_source(src.clone());
                }
                _ => {}
            }
            if self.states.get(&dev.key).is_some_and(|s| s.0 == "gone") {
                self.states.remove(&dev.key); // back: its stream opens it again on its next try
            }
        }
        self.drop_unplugged(host);
        self.refresh_dslr_controls();
        self.render(host);
    }

    /// A DSLR's settings change under us (the mode dial, live view starting or stopping): read again, and the card follows.
    fn refresh_dslr_controls(&mut self) {
        let key = self.selected_key.clone();
        // Not while the camera is still working through a request: its list is behind what the hand has just done
        if self.shared.device(&key).is_some_and(|d| webcams::is_gphoto(&d.location)) && !self.busy(&key) {
            self.read_controls(&key);
        }
    }

    /// Webcams that are gone leave the picker, unless they stream: those wait to be plugged back in.
    fn drop_unplugged(&mut self, host: &mut dyn Host) {
        let keys: Vec<String> = self.sources.keys().cloned().collect();
        for key in keys {
            if self.shared.device(&key).is_some() {
                continue;
            }
            let id = self.sources[&key].id();
            if host.is_streaming_from(Some(&id)) {
                self.states.insert(key, ("gone".into(), String::new()));
                continue;
            }
            self.sources.remove(&key);
            self.controls.remove(&key);
            self.backends.remove(&key);
            self.states.remove(&key);
            self.modes.remove(&key);
            host.clear_issue(Some(&format!("webcam:{key}")), None);
            host.remove_stream_source(&id);
        }
    }

    fn on_reported(&mut self, host: &mut dyn Host, key: &str, state: &str, detail: &str) {
        let Some(src) = self.sources.get(key).cloned() else { return };
        if state == "ok" {
            // Every opening, even one that reads like the last: a replug resets most webcams
            self.modes.insert(key.into(), detail.into());
            self.apply_controls(key);
        }
        if self.states.get(key).is_some_and(|s| s.0 == state && s.1 == detail) {
            return;
        }
        self.states.insert(key.into(), (state.into(), detail.into()));
        let issue_key = format!("webcam:{key}");
        if state == "busy" && host.is_streaming_from(Some(&src.id())) {
            let text = if detail.is_empty() {
                tr("Another app may be using it. Close it there and it starts here.")
            } else {
                tr("{app} is using it. Close it there and it starts here.").field("app", detail)
            };
            host.show_issue(&issue_key, Issue { title: tr("Can't open {name}").field("name", src.name()), text, actions: Vec::new(),
                                                kind: "warn".into(), details: String::new(), tell_dismiss: false });
        } else if state == "claimed" && host.is_streaming_from(Some(&src.id())) {
            host.show_issue(&issue_key, Issue { title: tr("Can't open {name}").field("name", src.name()), text: tr(webcam_capture::CLAIMED_TEXT),
                                                actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false });
        } else if state == "denied" && host.is_streaming_from(Some(&src.id())) {
            host.show_issue(&issue_key, Issue { title: tr("Can't open {name}").field("name", src.name()),
                                                text: tr(webcam_capture::DENIED_TEXT),
                                                actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false });
        } else {
            host.clear_issue(Some(&issue_key), None);
        }
        self.render(host);
    }

    fn on_streams_changed(&mut self, host: &mut dyn Host) {
        let keys: Vec<String> = self.sources.keys().cloned().collect();
        for key in keys {
            let id = self.sources[&key].id();
            if !host.is_streaming_from(Some(&id)) && self.states.remove(&key).is_some() {
                self.modes.remove(&key);
                host.clear_issue(Some(&format!("webcam:{key}")), None);
            }
        }
        self.drop_unplugged(host);
        self.render(host);
    }

    // ── Picture settings ──────────────────────────────────────────────────

    pub fn controls(&self, key: &str) -> HashMap<String, Control> {
        self.controls.get(key).cloned().unwrap_or_default()
    }

    fn saved_controls(&self, key: &str) -> Vec<(String, i64)> {
        self.shared.settings.lock().unwrap().get(key).and_then(|c| c.controls.clone()).unwrap_or_default()
    }

    /// What reads and sets this webcam's controls, kept while it's plugged in as it is.
    fn backend(&mut self, key: &str) -> Option<&mut Box<dyn Controls>> {
        let device = self.shared.device(key)?;
        if self.backends.get(key).is_none_or(|(d, _)| *d != device) {
            self.backends.remove(key);
            let b = (self.controls_for)(&device)?;
            self.backends.insert(key.into(), (device, b));
        }
        self.backends.get_mut(key).map(|(_, b)| b)
    }

    fn read_controls(&mut self, key: &str) -> HashMap<String, Control> {
        let found: HashMap<String, Control> = match self.backend(key) {
            Some(b) => b.list().into_iter().map(|c| (c.name.clone(), c)).collect(),
            None => HashMap::new(),
        };
        self.controls.insert(key.into(), found.clone());
        found
    }

    /// Put back what was picked for this webcam, and anti-flicker to the local mains unless something was picked.
    fn apply_controls(&mut self, key: &str) {
        if self.backend(key).is_none() {
            return;
        }
        let have = self.read_controls(key);
        let mut wanted: Vec<(String, i64)> = self.saved_controls(key);
        if let Some(flicker) = have.get("anti_flicker") {
            if !wanted.iter().any(|(n, _)| n == "anti_flicker") {
                if let Some(local) = local_flicker(flicker) {
                    wanted.push(("anti_flicker".into(), local));
                }
            }
        }
        let value_of = |name: &str| wanted.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
        let names: Vec<String> = wanted.iter().map(|(n, _)| n.clone()).collect();
        let mut changed = false;
        for name in apply_order(&names) {
            // What the camera holds (a DSLR's focus, aperture) isn't put back, whatever an older file saved for it
            let Some(control) = have.get(&name).filter(|c| !c.transient) else { continue };
            if let Some(auto) = auto_of(&name) {
                // Its auto is on, so the manual value would be refused
                if let Some(a) = have.get(auto) {
                    if value_of(auto).unwrap_or(a.value) != 0 {
                        continue;
                    }
                }
            }
            let value = control.clamp(value_of(&name).unwrap());
            if value != control.value {
                changed = self.backend(key).is_some_and(|b| b.set(&name, value)) || changed;
            }
        }
        if changed {
            self.read_controls(key);
        }
    }

    /// Whether the camera took the request.
    fn set_control(&mut self, host: &mut dyn Host, name: &str, value: i64) -> bool {
        let key = self.selected_key.clone();
        // One the camera doesn't offer now (the mode dial turned) has nothing to set, and is never saved as a setting
        let Some(control) = self.controls.get(&key).and_then(|m| m.get(name)).cloned() else { return false };
        let Some(b) = self.backend(&key) else { return false };
        // A menu goes by the choice the card showed, not by its place in a list the camera may have changed since
        let shown = if control.kind == "menu" { usize::try_from(value).ok().and_then(|i| control.labels.get(i)) } else { None };
        let accepted = match shown {
            Some(label) => b.set_labelled(name, value, label),
            None => b.set(name, value),
        };
        let transient = control.transient;
        if transient {
            // Held by the camera, which may refuse it, and different in another mode: read back, never saved. A request it had no room for
            // leaves the card where the camera is
            if let Some(c) = self.controls.get_mut(&key).and_then(|m| m.get_mut(name)).filter(|c| c.kind == "menu" && accepted) {
                c.value = value;
            }
            return accepted;
        }
        {
            let mut s = self.shared.settings.lock().unwrap();
            let cfg = s.entry(key.clone()).or_default();
            let mut list = cfg.controls.take().unwrap_or_default();
            match list.iter_mut().find(|(n, _)| n == name) {
                Some(e) => e.1 = value,
                None => list.push((name.into(), value)),
            }
            cfg.controls = Some(list);
        }
        host.schedule_save();
        if AUTOS.contains(&name) {
            self.read_controls(&key); // which manual controls do anything now
        } else if let Some(c) = self.controls.get_mut(&key).and_then(|m| m.get_mut(name)) {
            c.value = value;
        }
        accepted
    }

    /// The camera is still working through what it was last asked.
    fn busy(&mut self, key: &str) -> bool {
        self.backend(key).is_some_and(|b| b.busy())
    }

    /// One step of a tap or an arrow: the focus a small step nearer or further, a dial a place along its list (`big`: a longer step,
    /// after the button has been held a while).
    fn nudge(&mut self, host: &mut dyn Host, name: &str, dir: i32, big: bool) -> Nudge {
        let key = self.selected_key.clone();
        let Some(mut c) = self.controls.get(&key).and_then(|m| m.get(name)).cloned() else { return Nudge::End };
        // From where the camera is once it has finished the last request; until then, from where the last one put it
        if c.transient && !self.busy(&key) {
            // Gone from the camera's list now (its mode changed): nothing left to step
            let Some(now) = self.read_controls(&key).remove(name) else { return Nudge::End };
            c = now;
        }
        let value = match c.kind.as_str() {
            "buttons" => if dir < 0 { NEAR } else { FAR },
            "menu" => {
                let at = c.menu.iter().position(|v| *v == c.value).unwrap_or(0);
                let by = if big && c.unit == "ev" { stops_apart(&c) } else { 1 };
                match hold::stepped(at, c.menu.len(), dir, by) {
                    Some(to) => c.menu[to],
                    None => return Nudge::End,
                }
            }
            _ => return Nudge::End,
        };
        if self.set_control(host, name, value) { Nudge::Sent } else { Nudge::Dropped }
    }

    /// A tick of the held button: another step if it's time and the camera is ready for it.
    fn hold_tick(&mut self, host: &mut dyn Host) {
        let Some(mut hold) = self.held.take() else { return };
        let key = self.selected_key.clone();
        let settled = !self.busy(&key);
        match hold.tick(settled) {
            Tick::Release => return,
            Tick::Wait => {}
            Tick::Step { big } => {
                let result = self.nudge(host, &hold.name, hold.dir, big);
                // Rows or tiles that went out new rebuilt the button under the hand, which never hears the release
                if self.show_dynamic(host) || result == Nudge::End {
                    return;
                }
            }
        }
        self.held = Some(hold);
        host.schedule(TICK_MS, T_HOLD);
    }

    fn release_hold(&mut self, host: &mut dyn Host) {
        self.held = None;
        self.jogged = None;
        host.cancel_schedule(T_HOLD);
        host.cancel_schedule(T_JOG);
    }

    /// The jog's handle is at `level` (0: let go, and sprung back to the middle): a new one takes its first step at once and the timer
    /// keeps going while it's out, an old one only changes how big the next step is.
    fn set_jog(&mut self, host: &mut dyn Host, name: &str, level: i64) {
        if level == 0 {
            self.release_hold(host);
            return;
        }
        match &mut self.jogged {
            Some(jog) if jog.name == name => jog.set_level(level),
            _ => {
                self.release_hold(host);
                self.jogged = Some(JogHold::new(name, level));
                self.jog_tick(host);
            }
        }
    }

    /// A tick of the held-out jog: a step at the handle's level if the camera is ready for one.
    fn jog_tick(&mut self, host: &mut dyn Host) {
        let Some(mut jog) = self.jogged.take() else { return };
        let key = self.selected_key.clone();
        let settled = !self.busy(&key);
        match jog.tick(settled) {
            JogTick::Release => return,
            JogTick::Wait => {}
            JogTick::Step { level } => {
                if let Some(value) = jog_value(level) {
                    // The camera no longer has the focus drive (its mode changed): the jog ends
                    if !self.set_control(host, &jog.name, value) && !self.has_control(&jog.name) {
                        return;
                    }
                }
            }
        }
        self.jogged = Some(jog);
        host.schedule(TICK_MS, T_JOG);
    }

    /// Whether the selected camera offers a control now.
    fn has_control(&self, name: &str) -> bool {
        self.controls.get(&self.selected_key).is_some_and(|m| m.contains_key(name))
    }

    /// What changes while the hand is on a control, sent without new rows: those would rebuild the control being held. A change in
    /// the rows or tiles themselves (a control that came or went, a new list of choices) is sent too, since the card would never
    /// get it otherwise, and then the hold ends first. Whether it did go out.
    fn show_dynamic(&mut self, host: &mut dyn Host) -> bool {
        let (rows, tiles) = (self.rows(), self.tiles());
        let new_rows = self.shown_rows.as_ref().map(|r| structure(r)) != Some(structure(&rows));
        let new_tiles = self.shown_tiles.as_ref() != Some(&tiles);
        let mut props = json!({"readouts": self.readouts(), "tile-indexes": self.tile_indexes()});
        if new_rows || new_tiles {
            self.release_hold(host);
        }
        if new_rows {
            props["rows"] = json!(rows);
        }
        if new_tiles {
            props["tiles"] = json!(tiles);
        }
        // What a slider or an index reads now is on the card already, so only the lists above are ever behind
        self.shown_rows = Some(rows);
        self.shown_tiles = Some(tiles);
        host.set_ui_props("card", &props);
        new_rows || new_tiles
    }

    /// Every picture setting back to the camera's default (anti-flicker to the local mains).
    pub fn reset_controls(&mut self, host: &mut dyn Host) {
        let key = self.selected_key.clone();
        if self.backend(&key).is_none() {
            return;
        }
        let have = self.read_controls(&key);
        let names: Vec<String> = have.keys().cloned().collect();
        for name in apply_order(&names) {
            let control = &have[&name];
            if control.transient {
                continue;
            }
            if auto_of(&name).and_then(|a| have.get(a)).is_some_and(|a| a.default != 0) {
                continue; // auto by default: the manual value can't be set and doesn't matter
            }
            if control.value != default_of(control) {
                let d = default_of(control);
                if let Some(b) = self.backend(&key) {
                    b.set(&name, d);
                }
            }
        }
        let removed = self.shared.settings.lock().unwrap().get_mut(&key).and_then(|c| c.controls.take()).is_some();
        if removed {
            host.schedule_save();
        }
        self.read_controls(&key);
    }

    // ── The card ──────────────────────────────────────────────────────────

    fn set_wanted(&mut self, host: &mut dyn Host, size: Option<Option<(u32, u32)>>, fps: Option<u32>) {
        let key = self.selected_key.clone();
        if key.is_empty() {
            return;
        }
        {
            let mut s = self.shared.settings.lock().unwrap();
            let cfg = s.entry(key.clone()).or_default();
            let before = cfg.clone();
            if let Some(size) = size {
                cfg.size = size;
            }
            if let Some(f) = fps {
                cfg.fps = Some(f);
            }
            if *cfg == before {
                return;
            }
        }
        host.schedule_save();
        if let Some(src) = self.sources.get(&key) {
            if host.is_streaming_from(Some(&src.id())) {
                host.reconnect_stream(); // a new size means opening the camera again
            }
        }
    }

    fn size_items(&self) -> (Vec<(u32, u32)>, usize) {
        let key = &self.selected_key;
        let modes = self.shared.device(key).map(|d| d.modes).unwrap_or_default();
        let (size, _) = self.shared.wanted(key);
        let mut list = webcams::sizes(&modes);
        let index = match size {
            None => 0,
            Some(s) => match list.iter().position(|x| *x == s) {
                Some(i) => i + 1,
                None => {
                    // Saved for a camera that doesn't offer it now: still shown, so it isn't lost
                    list.push(s);
                    list.len()
                }
            },
        };
        (list, index)
    }

    fn fps_items(&self) -> (Vec<u32>, usize) {
        let key = &self.selected_key;
        let modes = self.shared.device(key).map(|d| d.modes).unwrap_or_default();
        let (size, fps) = self.shared.wanted(key);
        let mut offered = webcams::rates(&modes, size);
        if offered.is_empty() {
            offered = FPS_CHOICES.iter().rev().copied().collect();
        }
        if !offered.contains(&fps) {
            offered.push(fps);
        }
        offered.sort_unstable_by(|a, b| b.cmp(a));
        offered.dedup();
        let i = offered.iter().position(|f| *f == fps).unwrap_or(0);
        (offered, i)
    }

    fn rows(&self) -> Vec<Value> {
        let have = self.controls(&self.selected_key);
        let mut rows = Vec::new();
        // A camera with one kind of settings needs no heading to say which
        let headings = SECTIONS.iter().filter(|(_, _, entries)| entries.iter().any(|(n, _)| have.contains_key(*n))).count() > 1;
        for (section, icon, entries) in SECTIONS {
            let shown: Vec<&(&str, &str)> = entries.iter().filter(|(n, _)| have.contains_key(*n)).collect();
            if shown.is_empty() {
                continue;
            }
            if headings {
                rows.push(json!({"name": section, "label": section_title(section), "kind": "heading", "icon": icon, "enabled": true}));
            }
            for (name, label) in shown {
                let c = &have[*name];
                if section == "dslr" && c.kind == "menu" {
                    continue; // a tile of the strip under the rows
                }
                let enabled = !match auto_of(name).and_then(|a| have.get(a)) {
                    Some(a) => a.value != 0,
                    None => c.inactive,
                };
                let mut row = json!({"name": name, "label": tr(label), "tip": tip(name), "enabled": enabled, "icon": "sun"});
                if AUTOS.contains(name) {
                    row["kind"] = json!("auto");
                    row["checked"] = json!(c.value != 0);
                } else if c.kind == "buttons" {
                    row["kind"] = json!("jog");
                    row["options"] = json!([tr("Near"), tr("Focus"), tr("Far")]);
                } else if c.kind == "bool" || (c.kind == "int" && (c.minimum, c.maximum) == (0, 1)) {
                    row["kind"] = json!("switch");
                    row["checked"] = json!(c.value != 0);
                    row["icon"] = json!(if *name == "frame_rate_drop" { "moon" } else { "sun" });
                } else if c.kind == "menu" {
                    row["kind"] = json!("menu");
                    let options: Vec<String> = c.menu.iter().map(|v| {
                        if let Some(label) = c.labels.get(*v as usize) {
                            label.clone()
                        } else if *name == "anti_flicker" {
                            flicker_label(*v).unwrap_or(v.to_string())
                        } else {
                            v.to_string()
                        }
                    }).collect();
                    row["options"] = json!(options);
                    row["index"] = json!(c.menu.iter().position(|v| *v == c.value).unwrap_or(0));
                } else {
                    row["kind"] = json!("slider");
                    let log = c.unit == "100us";
                    let (lo, hi) = (c.minimum as f64, c.maximum as f64);
                    row["minimum"] = json!(if log { 0.0 } else { lo });
                    row["maximum"] = json!(if log { LOG_STEPS } else { hi });
                    row["step"] = json!(if log { 1 } else { c.step });
                    row["position"] = json!(if log { val_to_log_pos(c.value as f64, LOG_STEPS, lo, hi) } else { c.value as f64 });
                    row["reset-to"] = json!(if log { val_to_log_pos(default_of(c) as f64, LOG_STEPS, lo, hi) } else { default_of(c) as f64 });
                    row["readout"] = json!(shown_value(c, c.value));
                }
                rows.push(row);
            }
        }
        rows
    }

    /// A DSLR's values as a strip of tiles under its buttons: the ones that run in order step like a dial, the rest are a drop-down.
    fn tile_controls(&self) -> Vec<(&'static str, Control)> {
        let have = self.controls(&self.selected_key);
        SECTIONS.iter().filter(|(section, _, _)| *section == "dslr").flat_map(|(_, _, entries)| entries.iter())
            .filter_map(|(name, label)| have.get(*name).filter(|c| c.kind == "menu").map(|c| (*label, c.clone()))).collect()
    }

    fn tiles(&self) -> Vec<Value> {
        self.tile_controls().iter().map(|(label, c)| {
            let options: Vec<String> = c.menu.iter().map(|v| choice_text(c, *v as usize)).collect();
            json!({"name": c.name, "label": tr(label), "kind": if c.ordered { "stepper" } else { "menu" }, "options": options, "tip": tip(&c.name),
                   "enabled": true})
        }).collect()
    }

    /// Where each tile is, apart from the tiles so that stepping never replaces them.
    fn tile_indexes(&self) -> Vec<usize> {
        self.tile_controls().iter().map(|(_, c)| c.menu.iter().position(|v| *v == c.value).unwrap_or(0)).collect()
    }

    /// What each row reads out, in the order of `rows` ("" for the rows that read nothing).
    fn readouts(&self) -> Vec<String> {
        self.rows().iter().map(|r| r["readout"].as_str().unwrap_or("").to_string()).collect()
    }

    pub fn props(&self, streaming: bool) -> Value {
        let key = &self.selected_key;
        let src = self.sources.get(key);
        let (state, detail) = self.states.get(key).cloned().unwrap_or_default();
        let (kind, text) = if key.is_empty() {
            ("status_dim", String::new())
        } else if state == "gone" || self.shared.device(key).is_none() {
            ("status_warn", tr("Not plugged in"))
        } else if state == "denied" {
            ("status_err", tr("Blocked by Windows or security software"))
        } else if state == "busy" {
            ("status_err", if detail.is_empty() { tr("In use by another app") } else { tr("In use by {app}").field("app", &detail) })
        } else if state == "claimed" {
            ("status_err", tr("Mounted as a drive"))
        } else if state == "silent" {
            ("status_warn", tr("Not sending anything"))
        } else if streaming {
            ("status_ok", self.modes.get(key).cloned().unwrap_or_else(|| tr("Starting…")))
        } else {
            ("status_dim", tr("Not streaming"))
        };
        let (sizes, size_i) = self.size_items();
        let mut size_items = vec![tr("Auto")];
        size_items.extend(sizes.iter().map(|(w, h)| format!("{w}×{h}")));
        let (fps, fps_i) = self.fps_items();
        let rows = self.rows();
        json!({
            "visible": !key.is_empty(),
            "status-text": text, "status-kind": kind, "status-tip": src.map(|s| s.name()).unwrap_or_default(),
            "size-items": size_items, "size-current": size_i, "size-enabled": !key.is_empty(),
            "size-tip": tr("The size the webcam sends. Auto takes the largest up to 1080p that keeps the frame rate."),
            "fps-items": fps.iter().map(|f| format!("{f} fps")).collect::<Vec<_>>(), "fps-current": fps_i, "fps-enabled": !key.is_empty(),
            // Reset has nothing to put back on a DSLR, whose settings the camera holds
            "has-controls": self.controls(key).values().any(|c| !c.transient),
            "rows": rows, "readouts": self.readouts(), "tiles": self.tiles(), "tile-indexes": self.tile_indexes(),
        })
    }

    fn render(&mut self, host: &mut dyn Host) {
        let streaming = self.sources.get(&self.selected_key).is_some_and(|s| host.is_streaming_from(Some(&s.id())));
        let mut props = self.props(streaming);
        let rows = props["rows"].as_array().cloned().unwrap_or_default();
        let tiles = props["tiles"].as_array().cloned().unwrap_or_default();
        // New lists rebuild every button on the card, and the one under a hand would never hear the release
        if (self.held.is_some() || self.jogged.is_some()) && (self.shown_rows.as_ref() != Some(&rows) || self.shown_tiles.as_ref() != Some(&tiles)) {
            self.release_hold(host);
        }
        if self.shown_rows.as_ref() == Some(&rows) {
            props.as_object_mut().unwrap().remove("rows");
        } else {
            self.shown_rows = Some(rows);
        }
        if self.shown_tiles.as_ref() == Some(&tiles) {
            props.as_object_mut().unwrap().remove("tiles");
        } else {
            self.shown_tiles = Some(tiles);
        }
        host.set_ui_props("card", &props);
    }
}

impl Plugin for WebcamPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Webcams").into(),
            about: n_("Streaming from webcams and capture cards plugged into this computer.").into(),
            when_off: n_("Without it webcams aren't in the device picker and the Webcam card is gone too.").into(),
            category: "streaming".into(),
            icon: "video".into(),
            pinned: true,
            ..PluginInfo::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        host.schedule(0, T_SCAN); // after the window is up
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.stopped = true;
        self.release_hold(host);
        host.cancel_schedule(T_SCAN);
        self.backends.clear();
    }

    fn card(&mut self) -> Option<CardSpec> {
        // Where the Camera card is, which hides while a webcam is picked
        let props = self.props(false);
        self.shown_rows = props["rows"].as_array().cloned();
        self.shown_tiles = props["tiles"].as_array().cloned();
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "WebcamCard".into(), props }, region: "right".into() })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::TimerFired { owner, token } if owner == NAME && token == T_HOLD => self.hold_tick(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_JOG => self.jog_tick(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_SCAN => {
                if !self.stopped {
                    self.rescan(host);
                    host.schedule(SCAN_MS, T_SCAN);
                }
            }
            Event::Posted { owner, token, data } if owner == NAME => {
                if token == P_REPORTED {
                    let s = |i: usize| data.get(i).and_then(Value::as_str).unwrap_or("").to_string();
                    self.on_reported(host, &s(0), &s(1), &s(2));
                } else if token == P_UNPLUGGED {
                    let name = data.as_str().unwrap_or("");
                    host.show_issue("start", Issue { title: tr("{name} isn't plugged in").field("name", name),
                                                     text: tr("Plug it in, or pick another camera."), actions: Vec::new(),
                                                     kind: "warn".into(), details: String::new(), tell_dismiss: false });
                }
            }
            Event::SourceSelected { source_id } => {
                let key = source_id.strip_prefix(SOURCE_PREFIX).unwrap_or("").to_string();
                if key != self.selected_key {
                    self.release_hold(host);
                    self.selected_key = key.clone();
                    if self.shared.device(&key).is_some() {
                        self.read_controls(&key);
                    }
                    self.render(host);
                }
            }
            Event::StreamsChanged { .. } => self.on_streams_changed(host),
            _ => {}
        }
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        let arg = |i: usize| event.args.get(i).cloned().unwrap_or(Value::Null);
        match event.callback.as_str() {
            "size-picked" => {
                let Some(i) = arg(0).as_u64().map(|i| i as usize) else { return };
                let (sizes, _) = self.size_items();
                let size = if i == 0 { None } else { sizes.get(i - 1).copied() };
                self.set_wanted(host, Some(size), None);
            }
            "fps-picked" => {
                let Some(i) = arg(0).as_u64().map(|i| i as usize) else { return };
                let (fps, _) = self.fps_items();
                if let Some(f) = fps.get(i) {
                    self.set_wanted(host, None, Some(*f));
                }
            }
            "control-set" => {
                let (Some(name), Some(v)) = (arg(0).as_str().map(String::from), arg(1).as_f64()) else { return };
                let Some(c) = self.controls(&self.selected_key).get(&name).cloned() else { return };
                let value = if c.kind == "menu" && !AUTOS.contains(&name.as_str()) {
                    *c.menu.get(v as usize).unwrap_or(&c.value)
                } else if c.unit == "100us" {
                    c.clamp(log_pos_to_val(v, LOG_STEPS, c.minimum as f64, c.maximum as f64).round_ties_even() as i64)
                } else {
                    c.clamp(v.round() as i64)
                };
                self.set_control(host, &name, value);
                if !AUTOS.contains(&name.as_str()) {
                    // Only the readouts: new rows would rebuild the slider being dragged and break the drag off
                    self.show_dynamic(host); // the slider already is where the hand put it
                    return;
                }
            }
            "control-step" => {
                let (Some(name), Some(dir)) = (arg(0).as_str().map(String::from), arg(1).as_f64()) else { return };
                self.nudge(host, &name, dir as i32, false);
                self.show_dynamic(host);
                return;
            }
            "hold-down" => {
                let (Some(name), Some(dir)) = (arg(0).as_str().map(String::from), arg(1).as_f64()) else { return };
                // One step as it goes down; the timer takes over if it stays down
                self.release_hold(host);
                let first = self.nudge(host, &name, dir as i32, false);
                if !self.show_dynamic(host) && first != Nudge::End {
                    self.held = Some(Hold::new(&name, dir as i32));
                    host.schedule(TICK_MS, T_HOLD);
                }
                return;
            }
            "hold-up" => {
                self.release_hold(host);
                return;
            }
            "jog" => {
                let (Some(name), Some(offset)) = (arg(0).as_str().map(String::from), arg(1).as_f64()) else { return };
                self.set_jog(host, &name, jog_level(offset));
                return;
            }
            "action" => self.reset_controls(host),
            _ => return,
        }
        self.render(host);
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let devices = self.shared.devices.lock().unwrap();
        let found: Vec<String> = devices.values().map(|d| format!("{} ({} modes)", d.name, d.modes.len())).collect();
        let mut out = vec![("Webcams".to_string(), if found.is_empty() { "none found".to_string() } else { found.join("; ") })];
        let streams: Vec<String> = self.states.iter().filter_map(|(k, (s, d))| {
            let src = self.sources.get(k)?;
            Some(format!("{}: {s}{}", src.name(), if d.is_empty() { String::new() } else { format!(" {d}") }))
        }).collect();
        if !streams.is_empty() {
            out.push(("Webcam streams".into(), streams.join("; ")));
        }
        out
    }

    fn get_config(&self) -> Value {
        let s = self.shared.settings.lock().unwrap();
        let mut keys: Vec<&String> = s.keys().collect();
        keys.sort();
        json!({"cameras": keys.into_iter().map(|k| (k.clone(), s[k].to_json())).collect::<Map<String, Value>>()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let mut settings = HashMap::new();
        if let Some(cams) = cfg.get("cameras").and_then(Value::as_object) {
            for (key, entry) in cams {
                let Some(entry) = entry.as_object() else { continue };
                let mut clean = CamCfg::default();
                if let Some(size) = entry.get("size").and_then(Value::as_array) {
                    let dims: Vec<u64> = size.iter().filter(|v| v.is_u64()).filter_map(Value::as_u64).filter(|v| *v > 0).collect();
                    if size.len() == 2 && dims.len() == 2 {
                        clean.size = Some((dims[0] as u32, dims[1] as u32));
                    }
                }
                if let Some(f) = entry.get("fps").filter(|v| v.is_u64()).and_then(Value::as_u64).filter(|f| (1..=240).contains(f)) {
                    clean.fps = Some(f as u32);
                }
                if let Some(controls) = entry.get("controls").and_then(Value::as_object) {
                    let kept: Vec<(String, i64)> = controls.iter().filter(|(_, v)| v.is_i64()).map(|(n, v)| (n.clone(), v.as_i64().unwrap())).collect();
                    if !kept.is_empty() {
                        clean.controls = Some(kept);
                    }
                }
                settings.insert(key.clone(), clean);
            }
        }
        *self.shared.settings.lock().unwrap() = settings;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
