//! Setup (plugins/setup.py, its AdvancedSettings cards): no card in the window, only sections in Settings. The virtual
//! camera and the phone app on Virtual camera & phone, the Video card (hardware decoding, power-saving effects, the Zoom
//! slider's reach and the canvas size), the Microphone card (the Gain slider's reach and the limiter), the Look card (card
//! colours and the language) and About. Its settings are what other parts read through the host: canvas_size(), the
//! max_zoom_changed, max_gain_changed and limiter_changed events, the card colours and the decoder for the next stream.
//!
//! Everything that asks the system (lsmod, the registry, adb, a root prompt) runs on a thread of its own and comes back as
//! a post, so the settings window never waits on it.

use telescope_core::i18n::Fields;
use crate::py;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use telescope_core::canvas;
use telescope_core::event::Event;
use telescope_core::host::{FileDialog, FileDialogKind, Host, Poster, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::link::Link;
use telescope_core::media::Media;
use telescope_core::output::{Outputs, SetupAction, SetupResult};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{Issue, IssueAction, IssueDoes, SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "setup";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/setup.slint"));

pub const DEFAULT_MAX_ZOOM: i64 = 10;
pub const MAX_ZOOM_RANGE: (i64, i64) = (2, 30);
pub const MAX_GAIN_CHOICES: [i64; 4] = [12, 24, 36, 48];
pub const DEFAULT_MAX_GAIN: i64 = 12;

const SUDO_HINT: &str = n_("Asks for your password (pkexec or sudo): this changes system settings.");

pub const AUTHOR_URL: &str = "https://github.com/LunarKittyy";
pub const BUG_REPORT_URL: &str = "https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml";
pub const HELP_URL: &str = "https://github.com/LunarKittyy/Telescope/blob/master/docs/troubleshooting.md";
pub const TRANSLATE_URL: &str = "https://github.com/LunarKittyy/Telescope/blob/master/docs/translating.md";

/// platform/linux.py's devices and the message a refused password prompt comes back with (the outputs crate has the same).
pub const V4L2_PHONE_DEV: &str = "/dev/video11";
pub const V4L2_OBS_DEV: &str = "/dev/video10";
pub const CANCELLED: &str = "Cancelled.";
/// platform/windows.py's UC_NAME: what Telescope registers UnityCapture as.
pub const UC_NAME: &str = "Telescope";

/// The decoder names the Hardware acceleration row says (_HW_NAMES).
fn hw_name(device: &str) -> String {
    match device {
        "d3d11va" => "Direct3D 11".into(),
        "cuda" => "NVIDIA CUDA".into(),
        "vaapi" => "VA-API".into(),
        other => other.into(),
    }
}

/// A language Settings lists: its code, its name in itself and whether its translation is still a first draft.
#[derive(Clone, Debug, PartialEq)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub draft: bool,
}

impl Language {
    /// English, then every language with a catalog (i18n.available), sorted: the ones the program was given when folders is empty,
    /// else those of the first folder of .po files that has any.
    pub fn found(folders: &[&Path]) -> Vec<Language> {
        let list = |source: telescope_core::i18n::Source| -> Vec<Language> {
            source.available().into_iter().map(|code| Language { name: native_name(&code), draft: source.is_draft(&code), code }).collect()
        };
        let mut out = vec![Language { code: "en".into(), name: native_name("en"), draft: false }];
        if folders.is_empty() {
            out.extend(telescope_core::i18n::available().into_iter().map(|code| Language { name: native_name(&code), draft: telescope_core::i18n::is_draft(&code), code }));
            return out;
        }
        for folder in folders {
            let found = list(telescope_core::i18n::Source::Folder(folder.to_path_buf()));
            if !found.is_empty() {
                out.extend(found);
                break;
            }
        }
        out
    }
}

/// i18n.native_name: a language's name in itself.
fn native_name(code: &str) -> String {
    telescope_core::i18n::native_name(code)
}

/// What a job on its own thread gets: posts reach the plugin on the host thread as Event::Posted.
pub type Runner = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

fn thread_runner() -> Runner {
    Arc::new(|job| {
        std::thread::spawn(job);
    })
}

/// The status line under a row: text and kind ("status_ok", "status_warn", "status_err" or "status_dim").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub text: String,
    pub kind: String,
}

impl Line {
    fn new(text: impl Into<String>, kind: &str) -> Line {
        Line { text: text.into(), kind: kind.into() }
    }
}

/// What the cards show (AdvancedSettings' widgets), kept here so a section built later starts the same.
#[derive(Clone, Debug)]
pub struct Panel {
    pub v4l: Line,
    pub v4l_command: String,
    pub persist_checked: bool,
    pub persist_enabled: bool,
    /// The line under Load when the computer starts, shown once it's been used.
    pub persist: Option<Line>,
    pub persist_command: String,
    pub extras_shown: bool,
    pub extras: Line,
    pub extras_button: bool,
    pub extras_enabled: bool,
    pub extras_tip: String,
    pub removing: bool,
    pub uc: Line,
    pub uc_button: String,
    pub uc_tip: String,
    pub uc_primary: bool,
    pub uc_enabled: bool,
    pub uc_installing: bool,
    pub adb: Line,
    pub adb_row: bool,
    pub adb_button: String,
    pub adb_enabled: bool,
    pub adb_from_google: bool,
    pub adb_running: bool,
    pub hw_looked: bool,
    pub hw_enabled: bool,
    pub hw_line: String,
    /// The card's name, once looked for.
    pub hw_found: Option<String>,
    pub canvas_current: usize,
    pub custom_w: u32,
    pub custom_h: u32,
    pub canvas_status: Option<Line>,
    pub apply_enabled: bool,
    pub canvas_command: String,
    pub apk: Line,
    pub apk_enabled: bool,
    /// More than one phone is plugged in: the serials to pick from.
    pub apk_choices: Vec<String>,
    pub apk_path: String,
    pub lang_current: usize,
    pub copied: bool,
    /// The Video card is on screen, where a canvas change reports back.
    pub video_visible: bool,
}

impl Panel {
    fn new(apk_bundled: bool) -> Panel {
        Panel {
            v4l: Line::new(tr("Checking..."), "status_dim"),
            v4l_command: String::new(),
            persist_checked: false,
            persist_enabled: true,
            persist: None,
            persist_command: String::new(),
            extras_shown: false,
            extras: Line::new("", "status_dim"),
            extras_button: false,
            extras_enabled: false,
            extras_tip: String::new(),
            removing: false,
            uc: Line::new(tr("Checking..."), "status_dim"),
            uc_button: tr("Install driver"),
            uc_tip: String::new(),
            uc_primary: true,
            uc_enabled: true,
            uc_installing: false,
            adb: Line::new(tr("Checking..."), "status_dim"),
            adb_row: false,
            adb_button: tr("Get adb"),
            adb_enabled: true,
            adb_from_google: true,
            adb_running: false,
            hw_looked: false,
            hw_enabled: false,
            hw_line: String::new(),
            hw_found: None,
            canvas_current: 0,
            custom_w: canvas::CUSTOM_DEFAULT.0,
            custom_h: canvas::CUSTOM_DEFAULT.1,
            canvas_status: None,
            apply_enabled: true,
            canvas_command: String::new(),
            apk: Line::new(if apk_bundled { tr("Comes with this app") } else { tr("None came with this app") }, "status_dim"),
            apk_enabled: true,
            apk_choices: Vec::new(),
            apk_path: String::new(),
            lang_current: 0,
            copied: false,
            video_visible: false,
        }
    }
}

pub struct SetupPlugin {
    outputs: Arc<dyn Outputs>,
    link: Arc<dyn Link>,
    media: Arc<dyn Media>,
    pub linux: bool,
    version: String,
    languages: Vec<Language>,
    /// Where the full log is, for Open log (None hides it).
    pub log_path: Option<String>,
    run: Runner,
    poster: Option<Arc<dyn Poster>>,

    // What's saved
    pub canvas_preset: Value,
    pub custom_w: u32,
    pub custom_h: u32,
    pub max_zoom: i64,
    pub max_gain: Value,
    pub limiter: bool,
    pub hw_decode: bool,
    pub power_saving: bool,
    pub plugin_hues: bool,
    pub language: String,
    /// set_config has no host: the others hear the loaded values with the next hook that has one.
    pending: bool,

    pub panel: Panel,
    /// The canvas restart asked for and what to go back to if the host refuses it.
    canvas_request: Option<(RequestId, (Value, u32, u32))>,
    extras_request: Option<RequestId>,
    apk_pick: Option<RequestId>,
    adb_pick: Option<RequestId>,
}

impl SetupPlugin {
    pub fn new(outputs: Arc<dyn Outputs>, link: Arc<dyn Link>, media: Arc<dyn Media>, version: &str, languages: Vec<Language>) -> SetupPlugin {
        let apk_bundled = link.bundled_apk().is_some();
        SetupPlugin {
            outputs,
            link,
            media,
            linux: cfg!(target_os = "linux"),
            version: version.into(),
            languages: if languages.is_empty() { Language::found(&[]) } else { languages },
            log_path: None,
            run: thread_runner(),
            poster: None,
            canvas_preset: json!(canvas::AUTO),
            custom_w: canvas::CUSTOM_DEFAULT.0,
            custom_h: canvas::CUSTOM_DEFAULT.1,
            max_zoom: DEFAULT_MAX_ZOOM,
            max_gain: json!(DEFAULT_MAX_GAIN),
            limiter: true,
            hw_decode: true,
            power_saving: false,
            plugin_hues: true,
            language: String::new(),
            pending: true,
            panel: Panel::new(apk_bundled),
            canvas_request: None,
            extras_request: None,
            apk_pick: None,
            adb_pick: None,
        }
    }

    /// Jobs run with this instead of on threads of their own (tests run them where they're started).
    pub fn with_runner(mut self, run: Runner) -> SetupPlugin {
        self.run = run;
        self
    }

    /// The Linux or Windows cards, whichever this computer is.
    pub fn for_platform(mut self, linux: bool) -> SetupPlugin {
        self.linux = linux;
        self
    }

    /// (canvas_w, canvas_h) for the stream, or (None, None) for Auto.
    pub fn canvas_dims(&self) -> (Option<u32>, Option<u32>) {
        canvas::canvas_dims(&self.get_config())
    }

    fn preset_label(&self) -> &str {
        self.canvas_preset.as_str().unwrap_or("")
    }

    fn max_gain_i32(&self) -> i32 {
        self.max_gain.as_f64().unwrap_or(DEFAULT_MAX_GAIN as f64) as i32
    }

    /// Tells the others what was loaded, once there's a host to tell.
    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.poster.is_none() {
            self.poster = host.poster();
        }
        if std::mem::take(&mut self.pending) {
            host.emit(Event::MaxZoomChanged { max_zoom: self.max_zoom as i32 });
            host.emit(Event::MaxGainChanged { max_gain: self.max_gain_i32() });
            host.emit(Event::LimiterChanged { on: self.limiter });
            host.set_plugin_hues(self.plugin_hues);
            host.set_power_saving(self.power_saving);
        }
    }

    /// Runs job on its own thread; what it returns comes back as a post with this token.
    fn spawn(&mut self, host: &mut dyn Host, token: &str, job: impl FnOnce(&dyn Fn(&str, Value)) -> Value + Send + 'static) {
        let Some(poster) = self.poster.clone().or_else(|| host.poster()) else { return };
        let token = token.to_string();
        (self.run)(Box::new(move || {
            let say = |t: &str, data: Value| poster.post(t, data);
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&say)));
            // Always report back, or the button stays off until a restart
            let data = caught.unwrap_or_else(|why| {
                let why = why.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| why.downcast_ref::<String>().cloned());
                log::error!("{token} failed: {why:?}");
                json!({"ok": false, "message": why.unwrap_or_default()})
            });
            poster.post(&token, data);
        }));
    }

    // ── What the sections show ────────────────────────────────────────

    fn canvas_items(&self) -> Vec<String> {
        canvas::PRESETS.iter().map(|(label, _)| tr(label)).collect()
    }

    fn language_items(&self) -> Vec<String> {
        let mut items = vec![tr("Same as the system")];
        for lang in &self.languages {
            // Translators: after a language's name in the Language list, for a translation that's only a first draft
            let draft = if lang.draft { format!(" {}", tr("(beta)")) } else { String::new() };
            items.push(format!("{}{draft}", lang.name));
        }
        items
    }

    pub fn camera_props(&self) -> Value {
        let p = &self.panel;
        let persist = p.persist.clone().unwrap_or_default();
        let busy_tip = p.extras_tip.clone();
        json!({
            "linux": self.linux,
            "v4l-text": p.v4l.text, "v4l-kind": p.v4l.kind, "v4l-command": p.v4l_command,
            "load-tip": tr(SUDO_HINT),
            "persist-checked": p.persist_checked, "persist-enabled": p.persist_enabled,
            "persist-text": persist.text, "persist-kind": persist.kind, "persist-command": p.persist_command,
            "persist-tip": format!("{}\n{}", tr("Also writes it to /etc/modprobe.d/ and /etc/modules-load.d/, so it survives a reboot."), tr(SUDO_HINT)),
            "extras-shown": p.extras_shown, "extras-text": p.extras.text, "extras-kind": p.extras.kind,
            "extras-button": p.extras_button, "extras-enabled": p.extras_enabled, "extras-tip": busy_tip,
            "uc-text": p.uc.text, "uc-kind": p.uc.kind, "uc-button": p.uc_button, "uc-tip": p.uc_tip, "uc-primary": p.uc_primary,
            "uc-enabled": p.uc_enabled,
            "adb-text": p.adb.text, "adb-kind": p.adb.kind, "adb-row": p.adb_row, "adb-button": p.adb_button, "adb-enabled": p.adb_enabled,
        })
    }

    pub fn video_props(&self) -> Value {
        let p = &self.panel;
        let status = p.canvas_status.clone().unwrap_or_default();
        let (apply, tip) = if self.linux {
            let note = tr("Applying stops the stream, unloads v4l2loopback and loads it again. Close OBS and anything else using the virtual camera first.");
            (tr("Apply and reload"), format!("{note}\n{}", tr(SUDO_HINT)))
        } else {
            (tr("Apply"), tr("Applying restarts the stream at the new size. If OBS loses the source, remove it and add it again."))
        };
        json!({
            "hw-enabled": p.hw_enabled, "hw-checked": self.hw_decode && (p.hw_enabled || p.hw_line.is_empty()), "hw-line": p.hw_line,
            "saving-checked": self.power_saving,
            "zoom-text": format!("{}×", self.max_zoom),
            "canvas-items": self.canvas_items(), "canvas-current": p.canvas_current,
            "custom-shown": canvas::PRESETS.get(p.canvas_current).is_some_and(|(l, _)| *l == canvas::CUSTOM),
            "custom-w-text": format!("{} px", p.custom_w), "custom-h-text": format!("{} px", p.custom_h),
            "canvas-status": status.text, "canvas-status-kind": status.kind, "canvas-status-shown": p.canvas_status.is_some(),
            "apply-enabled": p.apply_enabled, "apply-text": apply, "apply-tip": tip, "canvas-command": p.canvas_command,
        })
    }

    pub fn mic_props(&self) -> Value {
        let gain = MAX_GAIN_CHOICES.iter().position(|g| *g == self.max_gain_i32() as i64).unwrap_or(0);
        json!({
            "card-group": "sound",
            "gain-items": MAX_GAIN_CHOICES.iter().map(|db| format!("+{db} dB")).collect::<Vec<_>>(),
            "gain-current": gain,
            "limiter-checked": self.limiter,
        })
    }

    pub fn apk_props(&self) -> Value {
        let p = &self.panel;
        json!({
            "apk-button": if self.link.bundled_apk().is_some() { tr("Install over USB") } else { tr("Choose APK...") },
            "apk-text": p.apk.text, "apk-kind": p.apk.kind, "apk-enabled": p.apk_enabled,
            "pick-shown": !p.apk_choices.is_empty(), "pick-items": p.apk_choices, "pick-current": 0,
        })
    }

    pub fn look_props(&self) -> Value {
        let draft = self.languages.iter().any(|l| l.code == current_language() && l.draft);
        json!({
            "hues-current": if self.plugin_hues { 0 } else { 1 },
            "lang-items": self.language_items(), "lang-current": self.panel.lang_current,
            "lang-restart": self.restart_needed(),
            "help-text": if draft { tr("This translation is a first draft and some texts might read a bit oddly.") }
                         else { tr("Missing your language, or spotted a text that reads wrong?") },
        })
    }

    pub fn about_props(&self) -> Value {
        // Translators: keep the <a href="{url}"> link and the name LunarKittyy as they are
        let made = tr("Made by <a href=\"{url}\">LunarKittyy</a>");
        let (before, link, after) = split_link(&made);
        json!({
            "version-text": tr("Version {version}").field("version", &self.version),
            "made-before": before, "made-link": link, "made-after": after,
            "diag-text": if self.panel.copied { tr("Copied") } else { tr("Copy diagnostics") },
            "log-shown": self.log_path.is_some(),
        })
    }

    fn refresh(&self, host: &mut dyn Host, sections: &[&str]) {
        for id in sections {
            let props = match *id {
                "virtual-camera" => self.camera_props(),
                "video" => self.video_props(),
                "microphone" => self.mic_props(),
                "phone-app" => self.apk_props(),
                "look" => self.look_props(),
                "about" => self.about_props(),
                _ => continue,
            };
            host.set_ui_props(id, &props);
        }
    }

    // ── The controls show what's saved ────────────────────────────────

    /// set_canvas_preset, set_max_zoom and the rest: what's saved changes while Settings is closed (a preset, a reload).
    pub fn sync_panel(&mut self) {
        if let Some(i) = canvas::PRESETS.iter().position(|(l, _)| *l == self.preset_label()) {
            self.panel.canvas_current = i;
        }
        self.panel.custom_w = self.custom_w;
        self.panel.custom_h = self.custom_h;
        let lang = self.language.clone();
        self.panel.lang_current = if lang.is_empty() { 0 } else { self.languages.iter().position(|l| l.code == lang).map_or(0, |i| i + 1) };
    }

    fn restart_needed(&self) -> bool {
        let picked = match self.panel.lang_current {
            0 => String::new(),
            i => self.languages.get(i - 1).map(|l| l.code.clone()).unwrap_or_default(),
        };
        resolve(&picked, &self.languages) != current_language()
    }

    /// on_show: their tabs came on screen, so the system is looked at again.
    pub fn on_show(&mut self, host: &mut dyn Host) {
        self.look_for_graphics_card(host);
        self.refresh_extras(host, None, "status_dim");
        if self.linux {
            self.v4l_check(host);
        } else {
            self.check_win_setup(host);
        }
    }

    /// Fills in the Hardware acceleration row, once: finding the card opens a decoder, so off the host thread.
    pub fn look_for_graphics_card(&mut self, host: &mut dyn Host) {
        if self.panel.hw_looked {
            // Finding the card is done by now, and it may have failed since
            if self.panel.hw_found.is_some() {
                self.show_hardware_failed(self.media.decoding_status().hardware_failed);
            }
            return;
        }
        self.panel.hw_looked = true;
        let media = self.media.clone();
        self.spawn(host, "hw-found", move |_| {
            let status = media.decoding_status();
            json!({"device": status.hardware_device, "failed": status.hardware_failed})
        });
    }

    pub fn show_graphics_card(&mut self, device: Option<&str>, failed: bool) {
        self.panel.hw_enabled = device.is_some();
        self.panel.hw_found = device.map(hw_name);
        self.show_hardware_failed(failed);
    }

    /// The line under the switch. It stays on when the card fails, since that's only for this run, and the line says so.
    fn show_hardware_failed(&mut self, failed: bool) {
        self.panel.hw_line = match &self.panel.hw_found {
            Some(_) if failed => tr("The graphics card failed, so the processor decodes until you restart Telescope"),
            Some(name) => tr("Found {device}").field("device", name),
            None => tr("No graphics card Telescope can use"),
        };
    }

    // ── Virtual camera on Linux ───────────────────────────────────────

    pub fn v4l_check(&mut self, host: &mut dyn Host) {
        let outputs = self.outputs.clone();
        self.spawn(host, "v4l-status", move |_| {
            let s = outputs.setup_status();
            json!({"ready": s.ready, "loaded": s.loaded, "persisted": s.persisted})
        });
    }

    /// _v4l_check's answer, and _refresh_persist_status's with it.
    pub fn show_v4l_status(&mut self, ready: bool, loaded: bool, persisted: bool) {
        self.panel.v4l = if ready {
            Line::new(tr("Ready: {phone} + {obs}").field("phone", V4L2_PHONE_DEV).field("obs", V4L2_OBS_DEV), "status_ok")
        } else if loaded {
            Line::new(tr("Module loaded but {device} not found - another config active").field("device", V4L2_PHONE_DEV), "status_warn")
        } else {
            Line::new(tr("Not loaded - click Load"), "status_err")
        };
        self.panel.persist_checked = persisted;
    }

    fn set_up(&mut self, host: &mut dyn Host, token: &str, action: SetupAction) {
        let outputs = self.outputs.clone();
        self.spawn(host, token, move |_| result_value(&outputs.set_up(action, &|_| {})));
    }

    pub fn v4l_load(&mut self, host: &mut dyn Host) {
        self.panel.v4l = Line::new(tr("Loading..."), "status_dim");
        self.set_up(host, "v4l-loaded", SetupAction::Load { persist: false });
    }

    pub fn v4l_unload(&mut self, host: &mut dyn Host) {
        self.panel.v4l = Line::new(tr("Unloading..."), "status_dim");
        self.set_up(host, "v4l-unloaded", SetupAction::Unload);
    }

    pub fn on_v4l_result(&mut self, ok: bool, msg: &str, command: &str, unloaded: bool) {
        let text = match (ok, unloaded) {
            (true, false) => tr("Loaded - {message}"),
            (true, true) => tr("Unloaded - {message}"),
            (false, _) => tr("Failed - {message}"),
        };
        self.panel.v4l = Line::new(text.field("message", msg), if ok { "status_ok" } else { "status_err" });
        self.panel.v4l_command = command.into();
    }

    pub fn on_persist_toggled(&mut self, host: &mut dyn Host, checked: bool) {
        self.panel.persist_checked = checked;
        self.panel.persist_enabled = false;
        self.panel.persist = Some(Line::new(tr("Working..."), "status_dim"));
        self.set_up(host, "persisted", SetupAction::Persist { on: checked });
    }

    pub fn on_persist_result(&mut self, ok: bool, msg: &str, command: &str) {
        self.panel.persist_enabled = true;
        self.panel.persist_command = command.into();
        if !ok {
            self.panel.persist_checked = !self.panel.persist_checked; // back, without writing or removing again
        }
        self.panel.persist = Some(Line::new(msg, if ok { "status_ok" } else { "status_err" }));
    }

    // ── Extra cameras ─────────────────────────────────────────────────

    pub fn refresh_extras(&mut self, host: &mut dyn Host, text: Option<String>, kind: &str) {
        let extras: Vec<u32> = (1..self.outputs.max_slots()).filter(|n| self.outputs.slot_ready(*n)).collect();
        let busy = host.stream_count() > 1;
        let text = text.unwrap_or_else(|| extras.iter().map(|n| self.outputs.slot_label(*n)).collect::<Vec<_>>().join(", "));
        let p = &mut self.panel;
        p.extras = Line::new(text, kind);
        p.extras_shown = !extras.is_empty() || kind != "status_dim";
        p.extras_button = !extras.is_empty();
        p.extras_enabled = !extras.is_empty() && !busy && !p.removing;
        p.extras_tip = if busy {
            tr("Stop the streams going to them first")
        } else {
            format!("{} {}", tr("Back to just the one camera; Add camera sets them up again."),
                    if self.linux { tr("Asks for your password.") } else { tr("Windows asks for permission.") })
        };
    }

    pub fn remove_extras(&mut self, host: &mut dyn Host) {
        self.panel.removing = true;
        self.panel.extras_enabled = false;
        self.panel.extras = Line::new(tr("Removing…"), &self.panel.extras.kind.clone());
        self.extras_request = Some(host.remove_extra_cameras());
    }

    pub fn on_extras_removed(&mut self, host: &mut dyn Host, ok: bool, msg: &str) {
        self.panel.removing = false;
        if ok {
            self.refresh_extras(host, Some(tr("Removed")), "status_ok");
        } else if msg == CANCELLED {
            self.refresh_extras(host, None, "status_dim");
        } else {
            self.refresh_extras(host, Some(tr("Couldn't remove them: {message}").field("message", msg)), "status_err");
        }
    }

    // ── Virtual camera on Windows ─────────────────────────────────────

    pub fn check_win_setup(&mut self, host: &mut dyn Host) {
        let (outputs, link) = (self.outputs.clone(), self.link.clone());
        self.spawn(host, "win-checks", move |_| {
            let s = outputs.setup_status();
            let mf = if s.windows_camera { if s.outdated { "update" } else { "ready" } } else if s.installable { "offer" } else { "" };
            json!({"uc_name": s.registered_name, "adb": link.adb_available(), "in_app_folder": s.in_app_folder, "mf": mf})
        });
    }

    /// _on_win_checks: mf is "ready" or "update" (an older one) for the Windows 11 camera, "offer" where it could be
    /// installed. dlls: UnityCapture's files are downloaded already.
    pub fn on_win_checks(&mut self, uc_name: &str, adb_ok: bool, in_app_folder: bool, mf: &str, dlls: bool) {
        let p = &mut self.panel;
        p.uc_enabled = !p.uc_installing;
        let (line, button, tip, primary) = if mf == "ready" {
            (Line::new(tr("Ready"), "status_ok"), tr("Reinstall"), String::new(), false)
        } else if mf == "update" || (mf == "offer" && !uc_name.is_empty()) {
            let text = if mf == "update" { tr("Update ready") } else { tr("Some apps can't see it") };
            (Line::new(text, "status_warn"), tr("Update"), tr("Installs the camera every Windows 11 app can see (Windows asks for admin access once)."), true)
        } else if !uc_name.is_empty() && in_app_folder {
            // Registered by an older Telescope from the app's own folder, which any program running as this user can write to
            (Line::new(tr("Reinstall for a security fix"), "status_warn"), tr("Reinstall"),
             tr("Moves the driver into Program Files (Windows asks for admin access once)."), true)
        } else if uc_name == UC_NAME {
            (Line::new(tr("Ready"), "status_ok"), tr("Reinstall"), String::new(), false)
        } else if !uc_name.is_empty() {
            // Registered by an older Telescope under UnityCapture's own name: works, just harder to find
            (Line::new(tr("Apps list it as “{name}”").field("name", uc_name), "status_warn"), tr("Rename"),
             tr("Register it again as “{name}”").field("name", UC_NAME), true)
        } else {
            let dlls = mf == "offer" || dlls; // the Windows 11 one comes with Telescope
            let text = if dlls { tr("Not installed") } else { tr("Not installed (Install driver downloads it first)") };
            (Line::new(text, "status_err"), tr("Install driver"), String::new(), true)
        };
        p.uc = line;
        p.uc_button = button;
        p.uc_tip = tip;
        p.uc_primary = primary;
        if p.adb_running {
            return;
        }
        p.adb_row = !adb_ok;
        p.adb = if adb_ok {
            Line::new(tr("Ready"), "status_ok")
        } else {
            Line::new(tr("Not found. Pairing and installing over USB need it; Wi-Fi doesn't."), "status_err")
        };
    }

    pub fn install_uc(&mut self, host: &mut dyn Host) {
        self.panel.uc_installing = true;
        self.panel.uc_enabled = false;
        self.panel.uc.kind = "status_dim".into();
        let outputs = self.outputs.clone();
        // The Windows 11 camera takes away cameras past the count asked for, so extras set up earlier stay
        let extras = (1..outputs.max_slots()).any(|n| outputs.slot_ready(n));
        let devices = if extras { outputs.max_slots() } else { 1 };
        self.spawn(host, "uc-done", move |say| {
            result_value(&outputs.set_up(SetupAction::Install { devices, reset: false }, &|msg| say("uc-msg", json!(msg))))
        });
    }

    pub fn on_uc_done(&mut self, ok: bool, msg: &str) {
        let p = &mut self.panel;
        p.uc_installing = false;
        p.uc_enabled = true;
        if ok {
            p.uc = Line::new(tr("Ready"), "status_ok");
            p.uc_button = tr("Reinstall");
            p.uc_tip = String::new();
        } else {
            p.uc = Line::new(tr("Failed: {message}").field("message", msg), "status_err");
            p.uc_button = tr("Retry");
        }
    }

    fn adb_starting(&mut self) {
        self.panel.adb_enabled = false;
        self.panel.adb_running = true;
        self.panel.adb.kind = "status_dim".into();
    }

    /// Get adb: Google's platform-tools; own: one picked by hand.
    pub fn get_adb(&mut self, host: &mut dyn Host, own: Option<String>) {
        if self.panel.adb_running {
            return;
        }
        self.panel.adb_from_google = own.is_none();
        self.adb_starting();
        let link = self.link.clone();
        self.spawn(host, "adb-done", move |say| match link.get_adb(own.as_deref(), &|msg| say("adb-progress", json!(msg))) {
            Ok(version) => json!({"ok": true, "message": version}),
            Err(why) => json!({"ok": false, "message": why}),
        });
    }

    pub fn on_adb_downloaded(&mut self, ok: bool, detail: &str) {
        let p = &mut self.panel;
        p.adb_enabled = true;
        p.adb_running = false;
        if ok {
            p.adb_row = false;
            let text = match (p.adb_from_google, detail.is_empty()) {
                (true, false) => tr("Ready (adb {version}, from Google)").field("version", detail),
                (true, true) => tr("Ready (adb, from Google)"),
                (false, false) => tr("Ready (adb {version})").field("version", detail),
                (false, true) => tr("Ready (adb)"),
            };
            p.adb = Line::new(text, "status_ok");
        } else {
            p.adb_button = tr("Try again");
            p.adb = Line::new(detail, "status_err");
        }
    }

    // ── The phone app ─────────────────────────────────────────────────

    pub fn install_apk(&mut self, host: &mut dyn Host) {
        if !self.link.adb_available() {
            let text = if self.linux {
                tr("Installing over USB needs adb. Install it from your distro's packages.")
            } else {
                tr("Installing over USB needs adb. Click Get adb under Virtual camera above.")
            };
            self.panel.apk = Line::new(text, "status_err");
            return;
        }
        match self.link.bundled_apk() {
            Some(path) => self.find_phones(host, path),
            None => {
                let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Select APK"), start: String::new(),
                                          filters: vec![tr("Android Package (*.apk)")] };
                self.apk_pick = host.pick_file(&dialog);
            }
        }
    }

    fn find_phones(&mut self, host: &mut dyn Host, path: String) {
        self.panel.apk_path = path;
        let link = self.link.clone();
        self.spawn(host, "apk-phones", move |_| {
            let serials: Vec<String> = link.usb_devices().into_iter().filter(|d| d.state == "device").map(|d| d.serial).collect();
            json!(serials)
        });
    }

    pub fn on_phones_found(&mut self, host: &mut dyn Host, serials: Vec<String>) {
        match serials.len() {
            0 => self.panel.apk = Line::new(tr("No phone found over USB. Plug it in and allow USB debugging when the phone asks."), "status_err"),
            1 => self.install_on(host, &serials[0]),
            _ => self.panel.apk_choices = serials, // which one: the card asks
        }
    }

    /// The phone picked from several (None: cancelled).
    pub fn phone_picked(&mut self, host: &mut dyn Host, index: Option<usize>) {
        let choices = std::mem::take(&mut self.panel.apk_choices);
        if let Some(serial) = index.and_then(|i| choices.get(i)) {
            self.install_on(host, &serial.clone());
        }
    }

    fn install_on(&mut self, host: &mut dyn Host, serial: &str) {
        self.panel.apk_enabled = false;
        self.panel.apk = Line::new(tr("Installing..."), "status_dim");
        let (link, serial, path) = (self.link.clone(), serial.to_string(), self.panel.apk_path.clone());
        self.spawn(host, "apk-done", move |_| match link.install_app(&serial, &path) {
            Ok(()) => json!({"ok": true, "message": ""}),
            Err(detail) => json!({"ok": false, "message": detail}),
        });
    }

    pub fn on_apk_done(&mut self, ok: bool, msg: &str) {
        self.panel.apk_enabled = true;
        self.panel.apk = Line::new(if ok { tr("Installed") } else { msg.to_string() }, if ok { "status_ok" } else { "status_err" });
    }

    // ── The canvas ────────────────────────────────────────────────────

    /// _get_selected_dims: a typed odd size goes up to the next even one, and the boxes show what's used.
    pub fn selected_dims(&mut self) -> (Option<u32>, Option<u32>) {
        match canvas::PRESETS.get(self.panel.canvas_current) {
            Some((label, _)) if *label == canvas::CUSTOM => {
                self.panel.custom_w = canvas::even(self.panel.custom_w, canvas::CUSTOM_MAX.0);
                self.panel.custom_h = canvas::even(self.panel.custom_h, canvas::CUSTOM_MAX.1);
                (Some(self.panel.custom_w), Some(self.panel.custom_h))
            }
            Some((_, Some((w, h)))) => (Some(*w), Some(*h)),
            _ => (None, None),
        }
    }

    /// Apply: the canvas is in effect for the restart, but kept only once the host accepts it.
    pub fn apply_canvas(&mut self, host: &mut dyn Host) {
        let (w, h) = self.selected_dims();
        self.panel.apply_enabled = false;
        let text = if self.linux { tr("Reloading loopback...") } else { tr("Restarting stream...") };
        self.panel.canvas_status = Some(Line::new(text, "status_dim"));
        let before = (self.canvas_preset.clone(), self.custom_w, self.custom_h);
        let label = canvas::PRESETS[self.panel.canvas_current.min(canvas::PRESETS.len() - 1)].0;
        self.canvas_preset = json!(label);
        if label == canvas::CUSTOM {
            (self.custom_w, self.custom_h) = (w.unwrap_or(self.custom_w), h.unwrap_or(self.custom_h));
        }
        let request = host.restart_vcam_canvas(w.unwrap_or(0), h.unwrap_or(0), true);
        self.canvas_request = Some((request, before));
    }

    pub fn on_canvas_done(&mut self, host: &mut dyn Host, ok: bool, msg: &str, command: &str, before: (Value, u32, u32)) {
        if !ok {
            (self.canvas_preset, self.custom_w, self.custom_h) = before;
        }
        host.schedule_save();
        self.set_canvas_apply_result(ok, msg, command);
        if !command.is_empty() && !self.panel.video_visible {
            // Closed meanwhile: the main window says it
            host.show_issue("vcam", Issue {
                title: tr("Resize the virtual camera"),
                text: msg.into(),
                actions: vec![IssueAction { label: tr("Copy command"), does: IssueDoes::OpenUrl { url: format!("copy:{command}") } }],
                kind: "warn".into(),
                details: command.into(),
                tell_dismiss: false,
            });
        }
    }

    pub fn set_canvas_apply_result(&mut self, ok: bool, msg: &str, command: &str) {
        self.panel.apply_enabled = true;
        self.panel.canvas_command = command.into();
        self.panel.canvas_status = Some(if ok {
            Line::new(if self.linux { tr("Done - loopback reloaded.") } else { tr("Done - canvas updated.") }, "status_ok")
        } else if msg.to_lowercase().contains("in use") {
            Line::new(tr("Failed: module is still in use. Close OBS and any other app using the virtual camera, then try again."), "status_err")
        } else {
            Line::new(tr("Failed: {message}").field("message", msg), "status_err")
        });
    }

    // ── Changes from the cards ────────────────────────────────────────

    pub fn on_max_zoom(&mut self, host: &mut dyn Host, value: i64) {
        let value = value.clamp(MAX_ZOOM_RANGE.0, MAX_ZOOM_RANGE.1);
        if value == self.max_zoom {
            return;
        }
        self.max_zoom = value;
        host.emit(Event::MaxZoomChanged { max_zoom: value as i32 });
        host.schedule_save();
    }

    pub fn on_max_gain(&mut self, host: &mut dyn Host, value: i64) {
        self.max_gain = json!(value);
        host.emit(Event::MaxGainChanged { max_gain: value as i32 });
        host.schedule_save();
    }

    pub fn on_limiter(&mut self, host: &mut dyn Host, on: bool) {
        self.limiter = on;
        host.emit(Event::LimiterChanged { on });
        host.schedule_save();
    }

    /// The decoder for the next stream: the window reads it from these settings when it starts one.
    pub fn on_hw_decode(&mut self, host: &mut dyn Host, on: bool) {
        self.hw_decode = on;
        host.schedule_save();
    }

    pub fn on_power_saving(&mut self, host: &mut dyn Host, on: bool) {
        self.power_saving = on;
        host.set_power_saving(on);
        host.schedule_save();
    }

    pub fn on_plugin_hues(&mut self, host: &mut dyn Host, on: bool) {
        self.plugin_hues = on;
        host.set_plugin_hues(on);
        host.schedule_save();
    }

    /// The language is loaded at the next start.
    pub fn on_language(&mut self, host: &mut dyn Host, index: usize) {
        self.panel.lang_current = index;
        self.language = match index {
            0 => String::new(),
            i => self.languages.get(i - 1).map(|l| l.code.clone()).unwrap_or_default(),
        };
        host.schedule_save();
    }

    pub fn copy_diagnostics(&mut self, host: &mut dyn Host) {
        host.copy_text(&host.diagnostics_report(&self.diagnostics()));
        self.panel.copied = true;
        host.schedule(2000, "copied");
    }

    fn decoding(&self) -> String {
        let status = self.media.decoding_status();
        let chosen = if self.hw_decode { "graphics card" } else { "processor" };
        if status.backends.is_empty() {
            return if self.hw_decode && status.hardware_failed {
                "not streaming (graphics card failed; processor until restart)".to_string()
            } else {
                format!("not streaming ({chosen} selected)")
            };
        }
        let mut text = status.backends.iter()
            .map(|b| if b == "processor" { "processor".to_string() } else { format!("graphics card ({b})") })
            .collect::<Vec<_>>().join(", ");
        if self.hw_decode != status.backends.iter().any(|b| b != "processor") {
            text += &if self.hw_decode && status.hardware_failed {
                " (graphics card failed; processor until restart)".to_string()
            } else {
                format!(" ({chosen} selected for the next connection)")
            };
        }
        text
    }

    fn on_posted(&mut self, host: &mut dyn Host, token: &str, data: &Value) -> Vec<&'static str> {
        let ok = data.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let msg = data.get("message").and_then(Value::as_str).unwrap_or("");
        let command = data.get("command").and_then(Value::as_str).unwrap_or("");
        match token {
            "hw-found" => {
                self.show_graphics_card(data.get("device").and_then(Value::as_str), data.get("failed") == Some(&Value::Bool(true)));
                vec!["video"]
            }
            "v4l-status" => {
                let flag = |k: &str| data.get(k).and_then(Value::as_bool).unwrap_or(false);
                self.show_v4l_status(flag("ready"), flag("loaded"), flag("persisted"));
                vec!["virtual-camera"]
            }
            "v4l-loaded" | "v4l-unloaded" => {
                self.on_v4l_result(ok, msg, command, token == "v4l-unloaded");
                vec!["virtual-camera"]
            }
            "persisted" => {
                self.on_persist_result(ok, msg, command);
                vec!["virtual-camera"]
            }
            "win-checks" => {
                let s = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let b = |k: &str| data.get(k).and_then(Value::as_bool).unwrap_or(false);
                self.on_win_checks(&s("uc_name"), b("adb"), b("in_app_folder"), &s("mf"), false);
                vec!["virtual-camera"]
            }
            "uc-msg" => {
                self.panel.uc.text = data.as_str().unwrap_or("").into();
                vec!["virtual-camera"]
            }
            "uc-done" => {
                self.on_uc_done(ok, msg);
                vec!["virtual-camera"]
            }
            "adb-progress" => {
                self.panel.adb.text = data.as_str().unwrap_or("").into();
                vec!["virtual-camera"]
            }
            "adb-done" => {
                self.on_adb_downloaded(ok, msg);
                vec!["virtual-camera"]
            }
            "apk-phones" => {
                let serials = data.as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
                self.on_phones_found(host, serials);
                vec!["phone-app"]
            }
            "apk-done" => {
                self.on_apk_done(ok, msg);
                vec!["phone-app"]
            }
            _ => vec![],
        }
    }
}

/// A root operation's result as a post carries it.
fn result_value(r: &SetupResult) -> Value {
    json!({"ok": r.ok, "message": r.message, "command": r.command.clone().unwrap_or_default()})
}

/// 'Made by <a href="{url}">LunarKittyy</a>' as the text before the link, the link's own and the text after.
pub fn split_link(text: &str) -> (String, String, String) {
    let Some(start) = text.find("<a ") else { return (text.into(), String::new(), String::new()) };
    let Some(open_end) = text[start..].find('>').map(|i| start + i + 1) else { return (text.into(), String::new(), String::new()) };
    let Some(close) = text[open_end..].find("</a>").map(|i| open_end + i) else { return (text.into(), String::new(), String::new()) };
    (text[..start].into(), text[open_end..close].into(), text[close + 4..].into())
}

/// The language loaded now, "en" when nothing is translated.
fn current_language() -> String {
    let lang = telescope_core::i18n::language();
    if lang.is_empty() { "en".into() } else { lang }
}

/// i18n.resolve: the language to load for the one picked ("" is the system's); TELESCOPE_LANG wins over both.
pub fn resolve(chosen: &str, languages: &[Language]) -> String {
    let have: Vec<String> = languages.iter().map(|l| l.code.clone()).collect();
    telescope_core::i18n::lang::resolve(chosen, &have)
}

impl Plugin for SetupPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Advanced settings").into(),
            about: n_("The virtual camera, its size, extra cameras, video and microphone limits, and the Updates & About tab.").into(),
            category: "streaming".into(),
            icon: "chip".into(),
            required: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        self.sync_panel();
        let section = |id: &str, tab: &str, component: &str, order: i32, props: Value| SettingsSection {
            id: id.into(),
            tab: tab.into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: component.into(), props },
            order,
            tab_title: String::new(),
            keywords: String::new(),
            footer: None,
        };
        vec![
            section("virtual-camera", "devices", "VirtualCameraSettings", 0, self.camera_props()),
            section("video", "video", "VideoSettings", 0, self.video_props()),
            section("microphone", "audio", "MicrophoneLimits", 0, self.mic_props()),
            section("phone-app", "devices", "PhoneAppSettings", 10, self.apk_props()),
            section("look", "general", "LookSettings", 10, self.look_props()),
            section("about", "about", "AboutSettings", 10, self.about_props()),
        ]
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::Posted { owner, token, data } if owner == NAME => {
                let sections = self.on_posted(host, token, data);
                self.refresh(host, &sections);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == "copied" => {
                self.panel.copied = false;
                self.refresh(host, &["about"]);
            }
            Event::RequestDone { request, ok, message, data } => {
                let command = data.get("command").and_then(Value::as_str).unwrap_or("");
                if self.canvas_request.as_ref().is_some_and(|(r, _)| r == request) {
                    let (_, before) = self.canvas_request.take().unwrap();
                    self.on_canvas_done(host, *ok, message, command, before);
                    self.refresh(host, &["video"]);
                } else if self.extras_request == Some(*request) {
                    self.extras_request = None;
                    self.on_extras_removed(host, *ok, message);
                    self.refresh(host, &["virtual-camera"]);
                } else if self.apk_pick == Some(*request) {
                    self.apk_pick = None;
                    if let Some(path) = data.get("path").and_then(Value::as_str).filter(|p| *ok && !p.is_empty()) {
                        self.find_phones(host, path.to_string());
                    }
                } else if self.adb_pick == Some(*request) {
                    self.adb_pick = None;
                    if let Some(path) = data.get("path").and_then(Value::as_str).filter(|p| *ok && !p.is_empty()) {
                        self.get_adb(host, Some(path.to_string()));
                        self.refresh(host, &["virtual-camera"]);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        let index = event.args.first().and_then(Value::as_i64).unwrap_or(0);
        let text = event.args.first().and_then(Value::as_str).unwrap_or("").to_string();
        let digits = |t: &str| t.trim().chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<i64>().ok();
        let id = event.ui.as_str();
        match (id, event.callback.as_str()) {
            (_, "shown") => {
                self.panel.video_visible = id == "video";
                self.sync_panel();
                self.on_show(host);
                self.refresh(host, &["virtual-camera", "video", "microphone", "phone-app", "look", "about"]);
                return;
            }
            ("virtual-camera", "check-clicked") => self.v4l_check(host),
            ("virtual-camera", "load-clicked") => self.v4l_load(host),
            ("virtual-camera", "unload-clicked") => self.v4l_unload(host),
            ("virtual-camera", "persist-toggled") => self.on_persist_toggled(host, on),
            ("virtual-camera", "extras-clicked") => self.remove_extras(host),
            ("virtual-camera", "uc-clicked") => self.install_uc(host),
            ("virtual-camera", "adb-clicked") => self.get_adb(host, None),
            ("virtual-camera", "own-adb-clicked") => {
                let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Use my own adb…"), start: String::new(),
                                          filters: vec!["adb.exe (adb.exe)".into(), "platform-tools (*.zip)".into()] };
                self.adb_pick = host.pick_file(&dialog);
            }
            ("virtual-camera" | "video", "copy") => host.copy_text(&text),
            ("video", "hw-toggled") => self.on_hw_decode(host, on),
            ("video", "saving-toggled") => self.on_power_saving(host, on),
            ("video", "zoom-committed") => {
                if let Some(v) = digits(&text) {
                    self.on_max_zoom(host, v);
                }
            }
            ("video", "zoom-step") => {
                let v = self.max_zoom + index;
                self.on_max_zoom(host, v);
            }
            ("video", "canvas-picked") => self.panel.canvas_current = (index.max(0) as usize).min(canvas::PRESETS.len() - 1),
            ("video", "custom-committed") => {
                let which = event.args.first().and_then(Value::as_str).unwrap_or("");
                let typed = event.args.get(1).and_then(Value::as_str).and_then(digits);
                if let Some(v) = typed {
                    if which == "w" {
                        self.panel.custom_w = (v.clamp(canvas::CUSTOM_MIN as i64, canvas::CUSTOM_MAX.0 as i64)) as u32;
                    } else {
                        self.panel.custom_h = (v.clamp(canvas::CUSTOM_MIN as i64, canvas::CUSTOM_MAX.1 as i64)) as u32;
                    }
                }
            }
            ("video", "custom-step") => {
                let which = event.args.first().and_then(Value::as_str).unwrap_or("");
                let by = event.args.get(1).and_then(Value::as_i64).unwrap_or(0) * 2; // odd sizes break the colours on Linux
                if which == "w" {
                    self.panel.custom_w = (self.panel.custom_w as i64 + by).clamp(canvas::CUSTOM_MIN as i64, canvas::CUSTOM_MAX.0 as i64) as u32;
                } else {
                    self.panel.custom_h = (self.panel.custom_h as i64 + by).clamp(canvas::CUSTOM_MIN as i64, canvas::CUSTOM_MAX.1 as i64) as u32;
                }
            }
            ("video", "apply-clicked") => self.apply_canvas(host),
            ("microphone", "gain-picked") => {
                if let Some(db) = MAX_GAIN_CHOICES.get(index.max(0) as usize) {
                    self.on_max_gain(host, *db);
                }
            }
            ("microphone", "limiter-toggled") => self.on_limiter(host, on),
            ("phone-app", "apk-clicked") => self.install_apk(host),
            ("phone-app", "pick-done") => self.phone_picked(host, Some(index.max(0) as usize)),
            ("phone-app", "pick-cancelled") => self.phone_picked(host, None),
            ("look", "hues-picked") => self.on_plugin_hues(host, index == 0),
            ("look", "lang-picked") => self.on_language(host, index.max(0) as usize),
            ("look", "restart-clicked") => {
                host.restart_app();
            }
            ("look", "translate-clicked") => host.open_url(TRANSLATE_URL),
            ("about", "author-clicked") => host.open_url(AUTHOR_URL),
            ("about", "diag-clicked") => self.copy_diagnostics(host),
            ("about", "bug-clicked") => {
                self.copy_diagnostics(host);
                host.open_url(BUG_REPORT_URL);
            }
            ("about", "log-clicked") => {
                if let Some(path) = self.log_path.clone() {
                    host.open_url(&telescope_core::system::file_url(std::path::Path::new(&path)));
                }
            }
            ("about", "help-clicked") => host.open_url(HELP_URL),
            _ => return,
        }
        self.refresh(host, &[id]);
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Decoding".into(), self.decoding())]
    }

    fn get_config(&self) -> Value {
        json!({
            "canvas_preset": self.canvas_preset,
            "custom_canvas_w": self.custom_w,
            "custom_canvas_h": self.custom_h,
            "max_zoom": self.max_zoom,
            "max_gain": self.max_gain,
            "limiter": self.limiter,
            "hw_decode": self.hw_decode,
            "power_saving": self.power_saving,
            "plugin_hues": self.plugin_hues,
            "language": self.language,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.canvas_preset = cfg.get("canvas_preset").cloned().unwrap_or_else(|| json!(canvas::AUTO));
        (self.custom_w, self.custom_h) = canvas::custom_size(cfg);
        self.max_zoom = cfg.get("max_zoom").and_then(py::whole)
            .filter(|z| (MAX_ZOOM_RANGE.0..=MAX_ZOOM_RANGE.1).contains(z)).unwrap_or(DEFAULT_MAX_ZOOM);
        // `max_gain in MAX_GAIN_CHOICES`: 24.0 counts, as Python compares it equal to 24, and is kept as it was written
        self.max_gain = match cfg.get("max_gain") {
            Some(v @ Value::Number(n)) if n.as_f64().is_some_and(|f| MAX_GAIN_CHOICES.iter().any(|c| *c as f64 == f)) => v.clone(),
            _ => json!(DEFAULT_MAX_GAIN),
        };
        self.limiter = cfg.get("limiter") != Some(&Value::Bool(false)); // only an explicit off turns it off
        self.hw_decode = telescope_core::media::hardware_decode_on(Some(cfg));
        self.power_saving = cfg.get("power_saving") == Some(&Value::Bool(true));
        self.plugin_hues = cfg.get("plugin_hues") != Some(&Value::Bool(false));
        self.language = cfg.get("language").and_then(Value::as_str).unwrap_or("").to_string();
        self.pending = true;
        Ok(())
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
