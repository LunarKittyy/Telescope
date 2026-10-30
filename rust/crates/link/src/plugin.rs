//! The Connection plugin (plugins/connection.py's window half): the card, the phone picker, the Add phone and Your
//! phones dialogs, the Start banners and the UDP setting. Its decisions are connection.rs's; its state is shared with
//! LinkService (shared.rs), which the window's stream flow calls, so the window never reaches into this plugin.

use crate::adb_download;
use crate::connection::{self, fix_actions, outdated_action, problem_text, route_text, status_line,
                        Check, Connection, Fix, ProbeOutcome, RouteChange, UsbWatch, UseRoute, PAIR_USB_POLL,
                        STATUS_POLL, USB_WATCH};
use crate::phones::is_usb;
use crate::shared::{Shared, StartProblem, POST_CHANGED, POST_START_PROBLEM};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use telescope_core::event::{Event, RememberedSource, SourceName};
use telescope_core::host::{FileDialog, FileDialogKind, Host, Poster, RequestId};
use telescope_core::i18n::{tr, tr_ctx};
use telescope_core::link::{Link, PairingOffer, PhoneStatus, Resolution, RoutePreference, UsbDevice};
use telescope_core::output::{Outputs, SetupAction, SetupResult, SETUP_CANCELLED, V4L2_PHONE_DEV};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::source::{PairedPhone, RouteSpec};
use telescope_core::ui::{ActionSpec, CardSpec, Issue, IssueAction, IssueDoes, SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "connection";
const UI: &str = include_str!("../ui/connection.slint");
/// The pairing server's answer is looked for this often while Add phone is open.
const PAIR_POLL_MS: u64 = 250;
/// The USB offer goes out again this often: the broadcast only lands while the app is on screen.
const OFFER_AGAIN_S: f64 = 4.0;

// Timer tokens
const T_POLL: &str = "poll";
const T_USB_WATCH: &str = "usb_watch";
const T_PAIR_POLL: &str = "pair_poll";
const T_PAIR_USB: &str = "pair_usb";

// Post tokens from this plugin's threads (shared.rs posts "changed" and "start_problem")
const P_RESOLVED: &str = "resolved";
const P_PROBED: &str = "probed";
const P_USB: &str = "usb";
const P_UPDATED: &str = "updated";
const P_USB_STATE: &str = "usb_state";
const P_USB_FORWARD: &str = "usb_forward";
const P_EARLIER: &str = "earlier";
const P_ADB: &str = "adb";
const P_ADB_PROGRESS: &str = "adb_progress";
const P_VCAM: &str = "vcam";

/// The Add phone dialog while it's open.
struct AddPhone {
    offer: Option<Arc<dyn PairingOffer>>,
    qr: (Vec<usize>, usize),
    wifi_text: String,
    usb_text: String,
    usb_kind: &'static str,
    usb_action: String,
    usb_pick_action: String,
    usb_states: Vec<UsbDevice>,
    chosen: Option<String>,
    last_offer: HashMap<String, Instant>,
    /// Paired: the phone's name.
    paired: Option<String>,
    /// Get adb was pressed: the licence question shows until they agree.
    confirming: bool,
}

/// A phone that paired with a new id but may be an earlier one again, waiting for the person to say.
struct SamePhone {
    phone: PairedPhone,
    old: PairedPhone,
    issue: Issue,
}

pub struct ConnectionPlugin {
    link: Arc<dyn Link>,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    /// The StreamSources other plugins offer (Browser camera), after the phones in the picker.
    sources: Vec<SourceName>,
    /// Browsers with settings of their own, for Your phones.
    remembered: Vec<RememberedSource>,
    /// Whose per-phone settings the panels hold.
    active_key: Option<String>,
    /// The "No phone yet" banner, while it may still be up.
    no_phone_issue: Option<Issue>,
    updating_phone: bool,
    /// (kind, text) from the last phone update.
    update_note: Option<(String, String)>,
    add: Option<AddPhone>,
    phones_open: bool,
    same_phone: Option<SamePhone>,
    /// Ids in the picker's order, phones then sources.
    picker_ids: Vec<String>,
    /// Its list is open, and what changed under it waits for it to close.
    picker_open: bool,
    picker_stale: bool,
    /// set_config has no host: the card catches up on the next hook.
    stale: bool,
    adb_available: bool,
    apk_bundled: bool,
    /// Kept across closing Add phone, so a download under way isn't started twice.
    downloader: adb_download::AdbDownloader,
    /// The virtual camera, for the check at Start. None: no check.
    outputs: Option<Arc<dyn Outputs>>,
    /// What the Start waiting for the virtual camera's setup starts again.
    vcam_source: Option<String>,
    /// The file dialog asking for an adb the person has already.
    adb_asked: Option<RequestId>,
}

/// rust/parity's pair script: in a dev profile with TELESCOPE_PARITY_OFFER set, Add phone opens from
/// `--action connection.add_phone` and writes its QR code's content to that file for the fake phone to "scan"
/// (parity/python_desktop.py adds the same two to the Python app).
fn parity_offer_file() -> Option<std::path::PathBuf> {
    telescope_core::config::dev_profile()?;
    std::env::var_os("TELESCOPE_PARITY_OFFER").filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

impl ConnectionPlugin {
    pub fn new(link: Arc<dyn Link>, shared: Arc<Shared>) -> ConnectionPlugin {
        ConnectionPlugin {
            link, shared, poster: None, sources: Vec::new(), remembered: Vec::new(), active_key: None, no_phone_issue: None,
            updating_phone: false, update_note: None, add: None, phones_open: false, same_phone: None, picker_ids: Vec::new(),
            picker_open: false, picker_stale: false, stale: true, adb_available: false, apk_bundled: false, downloader: adb_download::AdbDownloader::system(),
            outputs: None, vcam_source: None, adb_asked: None,
        }
    }

    /// With the virtual camera to check at Start.
    pub fn with_outputs(mut self, outputs: Arc<dyn Outputs>) -> ConnectionPlugin {
        self.outputs = Some(outputs);
        self
    }

    /// With another way to get adb (tests).
    pub fn with_downloader(mut self, downloader: adb_download::AdbDownloader) -> ConnectionPlugin {
        self.downloader = downloader;
        self
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.shared.conn()
    }

    fn now(&self) -> f64 {
        self.shared.now()
    }

    fn source_name(&self, sid: Option<&str>) -> Option<String> {
        self.sources.iter().find(|s| Some(s.id.as_str()) == sid).map(|s| s.name.clone())
    }

    /// Runs work on a thread of its own and posts what it returns under token.
    fn spawn(&self, token: &'static str, work: impl FnOnce() -> Value + Send + 'static) {
        let poster = self.poster.clone();
        std::thread::spawn(move || {
            let data = work();
            if let Some(poster) = poster {
                poster.post(token, data);
            }
        });
    }

    // ── Drawing ───────────────────────────────────────────────────────────

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.stale {
            self.stale = false;
            self.refresh_picker(host);
            self.render(host);
            self.announce_source(host);
            self.check_status(host);
        }
    }

    fn render(&mut self, host: &mut dyn Host) {
        let props = {
            let c = self.conn();
            card_props(&c, &CardCtx {
                source: self.source_name(c.selected_id.as_deref()),
                update_note: self.update_note.clone(),
                updating_phone: self.updating_phone,
                apk_bundled: self.apk_bundled,
                adb_available: self.adb_available,
            })
        };
        host.set_ui_props("card", &props);
    }

    fn refresh_picker(&mut self, host: &mut dyn Host) {
        let (names, ids, current, count) = {
            let c = self.conn();
            let mut names: Vec<String> = c.phones.iter().map(|p| p.name.clone()).collect();
            let mut ids: Vec<String> = c.phones.iter().map(|p| p.id.clone()).collect();
            for s in &self.sources {
                names.push(s.name.clone());
                ids.push(s.id.clone());
            }
            let current = ids.iter().position(|i| Some(i) == c.selected_id.as_ref()).map_or(-1, |i| i as i64);
            (names, ids, current, c.phones.len() as u32)
        };
        if self.picker_open {
            // The open list isn't rebuilt under the pointer, but what went away can't be picked
            let gone: Vec<bool> = self.picker_ids.iter().map(|id| !ids.contains(id)).collect();
            self.picker_stale = true;
            host.set_ui_props("header", &json!({"gone": gone}));
        } else {
            self.picker_ids = ids;
            host.set_ui_props("header", &json!({"names": names, "current": current, "gone": []}));
        }
        host.emit(Event::PhonesChanged { count });
        if self.phones_open {
            self.refresh_phones_dialog(host);
        }
    }

    fn refresh_phones_dialog(&mut self, host: &mut dyn Host) {
        let (rows, computer) = {
            let c = self.conn();
            (phone_rows(&c.phones, &self.remembered), c.computer_name.clone())
        };
        host.set_ui_props("header", &json!({"rows": rows, "computer-name": computer, "phones-open": self.phones_open}));
    }

    fn announce_source(&mut self, host: &mut dyn Host) {
        let selected = self.conn().selected_id.clone();
        let source = selected.filter(|s| self.source_name(Some(s)).is_some()).unwrap_or_default();
        host.emit(Event::SourceSelected { source_id: source });
    }

    // ── Checks ────────────────────────────────────────────────────────────

    fn run_check(&self, check: Option<Check>) {
        let Some(check) = check else { return };
        let link = self.link.clone();
        self.spawn(P_RESOLVED, move || {
            let res = link.check(&check.phone, check.preference);
            json!({"check_id": check.check_id, "phone_id": check.phone.id, "resolution": res})
        });
    }

    fn check_status(&mut self, host: &mut dyn Host) {
        let now = self.now();
        let check = self.conn().check_status(now);
        if check.is_none() && self.conn().selected_phone().is_none() {
            self.render(host);
        }
        self.run_check(check);
    }

    fn on_resolved(&mut self, host: &mut dyn Host, data: &Value) {
        let (Some(check_id), Some(phone_id), Ok(res)) = (data["check_id"].as_u64(), data["phone_id"].as_str(),
                                                         serde_json::from_value::<Resolution>(data["resolution"].clone())) else { return };
        self.drop_stale_update_note(&res);
        let (save, ready) = self.conn().on_resolved(check_id, phone_id, res);
        if save {
            host.schedule_save();
        }
        self.render(host);
        if let Some((phone_id, ready)) = ready {
            host.emit(Event::PhoneReady { phone_id, ready });
        }
    }

    /// The phone moved on from the update: its note is stale.
    fn drop_stale_update_note(&mut self, res: &Resolution) {
        if !matches!(res.status, PhoneStatus::PhoneOutdated | PhoneStatus::Unreachable) {
            self.update_note = None;
        }
    }

    // ── Connect via ───────────────────────────────────────────────────────

    fn set_route_preference(&mut self, host: &mut dyn Host, preference: RoutePreference) {
        let now = self.now();
        let selected = self.conn().selected_id.clone();
        let streaming_from = selected.is_some() && host.is_streaming_from(selected.as_deref());
        let change = self.conn().set_route_preference(preference, streaming_from, now);
        match change {
            RouteChange::Nothing => {}
            RouteChange::Cancelled => self.render(host),
            RouteChange::Probe { probe_id, phone, preference } => {
                self.render(host);
                let link = self.link.clone();
                self.spawn(P_PROBED, move || {
                    let res = link.check(&phone, preference);
                    json!({"probe_id": probe_id, "pid": phone.id, "preference": preference.as_str(), "resolution": res})
                });
            }
            RouteChange::Use(use_route) => self.use_route(host, use_route),
        }
    }

    fn use_route(&mut self, host: &mut dyn Host, use_route: UseRoute) {
        host.clear_issue(Some("route"), None);
        host.schedule_save();
        self.render(host);
        match use_route {
            UseRoute::Reconnect => host.reconnect_stream(),
            UseRoute::Check(check) => self.run_check(check),
        }
    }

    fn on_probed(&mut self, host: &mut dyn Host, data: &Value) {
        let (Some(probe_id), Some(pid), Some(preference), Ok(res)) = (data["probe_id"].as_u64(), data["pid"].as_str(),
            data["preference"].as_str().and_then(RoutePreference::parse), serde_json::from_value::<Resolution>(data["resolution"].clone())) else { return };
        let now = self.now();
        let streaming_from = host.is_streaming_from(Some(pid));
        let outcome = self.conn().on_route_probed(probe_id, pid, preference, &res, streaming_from, now);
        match outcome {
            ProbeOutcome::Stale => {}
            ProbeOutcome::Use(use_route) => self.use_route(host, use_route),
            ProbeOutcome::Refuse { title, text, preference } => {
                self.render(host);
                let again = IssueAction { label: tr("Try again"), does: IssueDoes::Plugin { id: format!("route:{}", preference.as_str()) } };
                host.show_issue("route", Issue { title, text, actions: vec![again], kind: "warn".into(), details: String::new(), tell_dismiss: false });
            }
        }
    }

    // ── Start problems ────────────────────────────────────────────────────

    fn show_start_problem(&mut self, host: &mut dyn Host, problem: StartProblem) {
        match problem {
            StartProblem::NoPhone => {
                let issue = Issue {
                    title: tr("No phone yet"), text: tr("Add a phone to stream from."),
                    actions: vec![IssueAction { label: tr("Add phone"), does: IssueDoes::Plugin { id: "add_phone".into() } }],
                    kind: "warn".into(), details: String::new(), tell_dismiss: false,
                };
                self.no_phone_issue = Some(issue.clone());
                host.show_issue("start", issue);
            }
            StartProblem::Unreachable { source_id, resolution } => {
                let (name, preference) = {
                    let c = self.conn();
                    (c.phone(Some(&source_id)).map(|p| p.name.clone()), c.route_pref)
                };
                let Some(name) = name else { return };
                let actions = fix_actions(&resolution, preference, self.apk_bundled, self.adb_available).into_iter()
                    .map(|fix| IssueAction { label: fix.label(), does: fix_does(fix, &source_id) }).collect();
                host.show_issue("start", Issue {
                    title: tr("Can't connect to the phone"), text: problem_text(&resolution, &name, self.adb_available),
                    actions, kind: "err".into(), details: String::new(), tell_dismiss: false,
                });
            }
            StartProblem::Forward { source_id } => {
                host.show_issue("start", Issue {
                    title: tr("Can't connect to the phone"),
                    text: tr("adb couldn't open a connection to the phone. Unplug it, plug it back in and try again."),
                    actions: vec![IssueAction { label: tr("Try again"), does: IssueDoes::StartAgain { source_id: Some(source_id), before: None } }],
                    kind: "err".into(), details: String::new(), tell_dismiss: false,
                });
            }
        }
    }

    // ── The virtual camera at Start ───────────────────────────────────────

    /// The person answered the question about the password prompt; persist: at every startup too.
    fn answer_vcam(&mut self, host: &mut dyn Host, ok: bool, persist: bool) {
        host.set_ui_props("card", &json!({"vcam-open": false}));
        let Some(outputs) = self.outputs.clone().filter(|_| ok) else { return };
        self.spawn(P_VCAM, move || json!(outputs.set_up(SetupAction::Load { persist }, &|_| {})));
    }

    fn on_vcam(&mut self, host: &mut dyn Host, data: &Value) {
        let Ok(result) = serde_json::from_value::<SetupResult>(data.clone()) else { return };
        if result.ok {
            host.start_again(self.vcam_source.as_deref());
        } else if let Some(command) = result.command {
            host.show_issue("start", Issue {
                title: tr("Set up the virtual camera"), text: result.message, actions: vec![copy_action(&command)],
                kind: "warn".into(), details: command, tell_dismiss: false,
            });
        } else if result.message != SETUP_CANCELLED {
            let again = IssueAction { label: tr("Try again"), does: IssueDoes::StartAgain { source_id: self.vcam_source.clone(), before: None } };
            host.show_issue("start", Issue {
                title: tr("Couldn't set up the virtual camera"), text: result.message, actions: vec![again],
                kind: "err".into(), details: String::new(), tell_dismiss: false,
            });
        }
    }

    // ── App versions ──────────────────────────────────────────────────────

    fn update_action(&mut self, host: &mut dyn Host) {
        let Some(res) = self.conn().resolution.clone() else { return };
        if res.status == PhoneStatus::DesktopOutdated {
            host.emit(Event::UpdateRequested);
            return;
        }
        if self.updating_phone {
            return; // the Start banner offers this too, while the card's own install is still running
        }
        let (Some(RouteSpec::Usb { serial }), Some(apk)) = (res.route.clone(), self.link.bundled_apk()) else { return };
        if res.status != PhoneStatus::PhoneOutdated {
            return;
        }
        self.updating_phone = true;
        self.update_note = None;
        self.render(host);
        let link = self.link.clone();
        self.spawn(P_UPDATED, move || {
            // Always report back, or the card stays on "Updating…" for good
            let installed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| link.install_app(&serial, &apk)));
            match installed {
                Ok(Ok(())) => json!({"ok": true}),
                Ok(Err(detail)) => json!({"ok": false, "detail": detail}),
                Err(panic) => {
                    let error = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned());
                    log::error!("Phone app install failed: {error:?}");
                    json!({"ok": false, "detail": tr("Couldn't update the phone app: {error}").replace("{error}", &error.unwrap_or_default())})
                }
            }
        });
    }

    // ── Plugged in while streaming over Wi-Fi ─────────────────────────────

    fn watch_usb(&mut self, host: &mut dyn Host) {
        let (active, watch) = {
            let mut c = self.conn();
            let active = c.usb_watch_active && c.streaming;
            (active, if active { c.watch_usb() } else { None })
        };
        if !active {
            return;
        }
        host.schedule(USB_WATCH.as_millis() as u64, T_USB_WATCH);
        let Some((watch_id, phone)) = watch else { return };
        let link = self.link.clone();
        self.spawn(P_USB, move || {
            let res = link.check(&phone, RoutePreference::Usb);
            let route = (res.status == PhoneStatus::Ready).then_some(res.route).flatten();
            json!({"watch_id": watch_id, "route": route})
        });
    }

    fn on_usb_available(&mut self, host: &mut dyn Host, data: &Value) {
        let Some(watch_id) = data["watch_id"].as_u64() else { return };
        let route: Option<RouteSpec> = serde_json::from_value(data["route"].clone()).unwrap_or(None);
        let now = self.now();
        let outcome = self.conn().on_usb_available(watch_id, route, now);
        match outcome {
            UsbWatch::Ignore => {}
            UsbWatch::HideButton => self.render(host),
            UsbWatch::HandOver(route) => {
                // adb opens the forward here, not on the UI thread, which would freeze the window while it answers
                let RouteSpec::Usb { serial } = &route else { return self.hand_over(host, &route, false) };
                let (shared, serial) = (self.shared.clone(), serial.clone());
                self.spawn(P_USB_FORWARD, move || json!({"route": route, "held": shared.hold_stream_forward(&serial)}));
            }
        }
    }

    fn on_usb_forward(&mut self, host: &mut dyn Host, data: &Value) {
        let Ok(route) = serde_json::from_value::<RouteSpec>(data["route"].clone()) else { return };
        let held = data["held"].as_bool() == Some(true);
        if held {
            self.hand_over(host, &route, true);
        } else {
            self.conn().switch_usb_visible = true;
            self.render(host);
        }
    }

    /// Hands the stream over to route; held: hold_stream_forward holds its forward, let go once prepare_route counted it.
    fn hand_over(&mut self, host: &mut dyn Host, route: &RouteSpec, held: bool) {
        let selected = self.conn().selected_id.clone();
        let moving = host.hand_over_stream(route, selected.as_deref());
        if let (true, RouteSpec::Usb { serial }) = (held, route) {
            // Removing the forward is adb too when the hand-over didn't take it
            let (shared, serial) = (self.shared.clone(), serial.clone());
            std::thread::spawn(move || shared.release_stream_forward(&serial));
        }
        if !moving {
            self.conn().switch_usb_visible = true;
            self.render(host);
        }
    }

    // ── Phones ────────────────────────────────────────────────────────────

    pub fn open_add_phone(&mut self, host: &mut dyn Host) {
        if self.add.is_some() {
            host.set_ui_props("card", &json!({"add-open": true}));
            return;
        }
        let (cid, cname) = {
            let c = self.conn();
            (c.computer_id.clone(), c.computer_name.clone())
        };
        let mut add = AddPhone {
            offer: None, qr: (Vec::new(), 0), wifi_text: String::new(), usb_text: String::new(), usb_kind: "status_dim",
            usb_action: String::new(), usb_pick_action: String::new(), usb_states: Vec::new(), chosen: None, last_offer: HashMap::new(), paired: None,
            confirming: false,
        };
        match self.link.pair(&cid, &cname) {
            Ok(offer) => {
                if let Some(path) = parity_offer_file() {
                    if let Err(e) = std::fs::write(&path, offer.payload()) {
                        log::warn!("Couldn't write the pairing offer to {}: {e}", path.display());
                    }
                }
                if offer.addresses().is_empty() {
                    add.wifi_text = tr("This computer isn't on a network right now, so pair over USB.");
                } else {
                    add.qr = qr(&offer.payload());
                    // "Scan pairing code" is a button in the phone app
                    add.wifi_text = tr("On the phone, tap Scan pairing code and point it at this code.");
                }
                add.offer = Some(offer);
                host.schedule(PAIR_POLL_MS, T_PAIR_POLL);
            }
            Err(why) => {
                log::warn!("Couldn't start pairing: {why}");
                add.wifi_text = tr("This computer isn't on a network right now, so pair over USB.");
            }
        }
        self.add = Some(add);
        self.start_usb(host);
        self.render_add(host, true);
    }

    fn start_usb(&mut self, host: &mut dyn Host) {
        let adb = self.link.adb_available();
        self.adb_available = adb;
        let Some(add) = self.add.as_mut() else { return };
        add.usb_action.clear();
        add.usb_pick_action.clear();
        if self.downloader.running() {
            // Reopened mid-download: its progress shows on the next step
            add.usb_text = tr("Getting adb from Google…");
        } else if adb {
            add.usb_text = tr("Or plug the phone in with a USB cable to pair it over USB.");
            add.usb_kind = "status_dim";
            host.schedule(PAIR_USB_POLL.as_millis() as u64, T_PAIR_USB);
            self.poll_usb();
        } else if crate::platform::windows() {
            // A button stands in for Python's link, which Slint's Text can't show
            add.usb_text = without_links(&tr("Needs adb, which Telescope can <a href=\"{link}\">get from Google</a>. Wi-Fi works without it."));
            add.usb_action = tr("Get adb from Google");
        } else {
            add.usb_text = tr("Needs adb, which isn't installed. Wi-Fi works without it.");
        }
    }

    fn poll_usb(&self) {
        let link = self.link.clone();
        self.spawn(P_USB_STATE, move || json!(link.usb_devices()));
    }

    fn render_add(&mut self, host: &mut dyn Host, open: bool) {
        let Some(add) = &self.add else {
            host.set_ui_props("card", &json!({"add-open": false}));
            return;
        };
        let picks: Vec<Value> = usable(&add.usb_states).into_iter()
            .map(|(serial, name)| json!({"serial": serial, "label": tr("Pair {phone}").replace("{phone}", &name), "enabled": add.chosen.as_ref() != Some(&serial)}))
            .collect();
        let paired = add.paired.is_some();
        host.set_ui_props("card", &json!({
            "add-open": open,
            "add-subtitle": if paired { tr("You can start streaming now.") } else { tr("Open Telescope on the phone first.") },
            "qr-cells": add.qr.0, "qr-size": add.qr.1,
            "wifi-text": add.wifi_text, "usb-text": add.usb_text, "usb-kind": add.usb_kind, "usb-action": add.usb_action, "usb-pick-action": add.usb_pick_action,
            "picks": picks, "paired": paired,
            "result-text": add.paired.as_ref().map(|n| tr("Paired with {name}").replace("{name}", n)).unwrap_or_default(),
        }));
    }

    fn on_usb_state(&mut self, host: &mut dyn Host, data: &Value) {
        let Ok(states) = serde_json::from_value::<Vec<UsbDevice>>(data.clone()) else { return };
        let Some(add) = self.add.as_mut() else { return };
        if add.paired.is_some() || add.offer.is_none() {
            return;
        }
        let usable = usable(&states);
        add.usb_states = states;
        if add.chosen.as_ref().is_some_and(|c| !usable.iter().any(|(s, _)| s == c)) {
            add.chosen = None;
        }
        if let Some(chosen) = add.chosen.clone() {
            let name = usable.iter().find(|(s, _)| *s == chosen).map(|(_, n)| n.clone()).unwrap_or_default();
            add.usb_text = tr("Pairing {phone}. Keep Telescope open on it until it's done.").replace("{phone}", &name);
            add.usb_kind = "status_dim";
            self.send_offer(&chosen);
        } else if !usable.is_empty() {
            add.usb_text = tr("Pick a phone below to pair it over USB.");
            add.usb_kind = "status_dim";
        } else if add.usb_states.iter().any(|d| d.state == "unauthorized") {
            add.usb_text = tr("Phone plugged in. Allow USB debugging in the prompt on the phone.");
            add.usb_kind = "status_warn";
        } else {
            add.usb_text = tr("Or plug the phone in with a USB cable to pair it over USB.");
            add.usb_kind = "status_dim";
        }
        self.render_add(host, true);
    }

    fn send_offer(&mut self, serial: &str) {
        let Some(add) = self.add.as_mut() else { return };
        let Some(offer) = add.offer.clone() else { return };
        if add.last_offer.get(serial).is_some_and(|t| t.elapsed().as_secs_f64() < OFFER_AGAIN_S) {
            return;
        }
        add.last_offer.insert(serial.to_string(), Instant::now());
        let serial = serial.to_string();
        std::thread::spawn(move || {
            offer.offer_to(&serial, crate::adb::pair_broadcast_package());
        });
    }

    fn pick_usb(&mut self, host: &mut dyn Host, serial: &str) {
        let Some(add) = self.add.as_mut() else { return };
        if add.paired.is_some() {
            return;
        }
        add.chosen = Some(serial.to_string());
        add.last_offer.remove(serial);
        let states = json!(add.usb_states);
        self.on_usb_state(host, &states);
    }

    fn get_adb(&mut self, host: &mut dyn Host) {
        let Some(add) = self.add.as_mut() else { return };
        if self.downloader.running() {
            return;
        }
        if !add.confirming {
            // confirm(): Google's software, so their licence applies
            add.confirming = true;
            add.usb_text = format!("{} {}", tr("USB needs adb, Google's tool for talking to Android phones."),
                                   tr("Telescope can download it from Google (about {size}) and keep it for your user. It's Google's software, so their Android SDK License applies to it: {url}")
                                       .replace("{size}", adb_download::APPROX_SIZE).replace("{url}", adb_download::LICENSE_URL));
            add.usb_kind = "status_dim";
            add.usb_action = tr("Agree and download");
            self.render_add(host, true);
            return;
        }
        add.confirming = false;
        add.usb_action.clear();
        add.usb_pick_action.clear();
        add.usb_text = tr("Getting adb from Google…");
        self.run_adb_job(None);
        self.render_add(host, true);
    }

    /// Pick an adb.exe or platform-tools zip instead, after a download that failed (pick_and_start).
    fn pick_adb(&mut self, host: &mut dyn Host) {
        if self.downloader.running() {
            return;
        }
        self.adb_asked = host.pick_file(&FileDialog {
            kind: FileDialogKind::Open, title: tr("Pick adb.exe or the platform-tools zip"), start: String::new(),
            filters: vec![tr("adb.exe or platform-tools zip (adb.exe *.zip)")],
        });
    }

    fn adb_picked(&mut self, host: &mut dyn Host, path: &str) {
        let Some(add) = self.add.as_mut() else { return };
        add.usb_text = tr("Copying adb…");
        add.usb_kind = "status_dim";
        add.usb_action.clear();
        add.usb_pick_action.clear();
        let path = std::path::PathBuf::from(path);
        self.run_adb_job(Some(Arc::new(move |_| adb_download::use_own_adb(&path, None))));
        self.render_add(host, true);
    }

    fn run_adb_job(&self, job: Option<adb_download::DownloadJob>) {
        let (progress_poster, done_poster) = (self.poster.clone(), self.poster.clone());
        self.downloader.start(job, Box::new(move |msg| {
            if let Some(p) = &progress_poster {
                p.post(P_ADB_PROGRESS, json!(msg));
            }
        }), Box::new(move |result| {
            if let Some(p) = &done_poster {
                p.post(P_ADB, match result {
                    Ok(_) => json!({"ok": true}),
                    Err(detail) => json!({"ok": false, "detail": detail}),
                });
            }
        }));
    }

    fn on_adb(&mut self, host: &mut dyn Host, data: &Value) {
        // Closed meanwhile: opening it again starts USB if adb is there
        let Some(add) = self.add.as_mut() else { return };
        if add.paired.is_some() || add.offer.is_none() {
            return;
        }
        if data["ok"].as_bool() == Some(true) {
            self.start_usb(host);
        } else if let Some(add) = self.add.as_mut() {
            // Python's two links, as buttons
            let links = tr("<a href=\"{get}\">Try again</a> or <a href=\"{pick}\">pick one you already have</a>.");
            add.usb_text = data["detail"].as_str().unwrap_or_default().to_string();
            add.usb_kind = "status_err";
            add.usb_action = link_label(&links, "{get}").unwrap_or_else(|| tr("Try again"));
            add.usb_pick_action = link_label(&links, "{pick}").map(|l| capitalised(&l)).unwrap_or_default();
            add.confirming = true; // they agreed already
        }
        self.render_add(host, true);
    }

    fn close_add(&mut self, host: &mut dyn Host) {
        log::debug!("Add phone closed");
        // Dropping the offer stops the pairing server and takes down its adb reverses
        self.add = None;
        host.cancel_schedule(T_PAIR_POLL);
        host.cancel_schedule(T_PAIR_USB);
        host.set_ui_props("card", &json!({"add-open": false}));
    }

    fn poll_pairing(&mut self, host: &mut dyn Host) {
        let Some(add) = self.add.as_mut() else { return };
        let Some(offer) = add.offer.clone() else { return };
        let Some(phone) = offer.poll() else {
            host.schedule(PAIR_POLL_MS, T_PAIR_POLL);
            return;
        };
        log::info!("Paired with {} ({})", phone.name, phone.id);
        add.paired = Some(phone.name.clone());
        add.offer = None;
        host.cancel_schedule(T_PAIR_USB);
        self.render_add(host, true);
        self.phone_paired(host, phone);
    }

    /// A phone paired: a new one with an earlier one's name may be that phone again, which takes a check first.
    pub fn phone_paired(&mut self, host: &mut dyn Host, phone: PairedPhone) {
        if self.conn().phone(Some(&phone.id)).is_some() {
            self.finish_pairing(host, phone, None);
            return;
        }
        let (candidates, selected_status) = {
            let c = self.conn();
            let selected = c.selected_id.clone();
            let candidates: Vec<PairedPhone> = c.phones.iter()
                .filter(|old| old.id != phone.id && old.name == phone.name && !host.is_streaming_from(Some(&old.id)))
                .filter(|old| Some(&old.id) != selected.as_ref() || c.resolution.is_none())
                .cloned().collect();
            let selected_status = c.resolution.as_ref().map(|r| r.status);
            (candidates, selected_status)
        };
        let named_like_selected = self.conn().selected_phone().is_some_and(|p| p.name == phone.name && p.id != phone.id);
        if candidates.is_empty() && !(named_like_selected && selected_status.is_some()) {
            self.finish_pairing(host, phone, None);
            return;
        }
        let link = self.link.clone();
        self.spawn(P_EARLIER, move || {
            let statuses: HashMap<String, PhoneStatus> = candidates.iter()
                .map(|old| (old.id.clone(), link.check(old, RoutePreference::Auto).status)).collect();
            json!({"phone": phone, "statuses": statuses})
        });
    }

    fn on_earlier(&mut self, host: &mut dyn Host, data: &Value) {
        let Ok(phone) = serde_json::from_value::<PairedPhone>(data["phone"].clone()) else { return };
        let statuses: HashMap<String, PhoneStatus> = serde_json::from_value(data["statuses"].clone()).unwrap_or_default();
        let old = self.conn().earlier_install(&phone, |pid| host.is_streaming_from(Some(pid)), |old| statuses.get(&old.id).copied());
        let Some(old) = old else {
            self.finish_pairing(host, phone, None);
            return;
        };
        let name = old.name.clone();
        let issue = Issue {
            title: tr("Same phone?"),
            text: tr("Is this {name} again?").replace("{name}", &name) + " " + &tr("A phone called {name} paired as a new phone. That happens after Telescope is reinstalled on it or its data is cleared. Yes moves {name}'s settings and presets over to it and removes the old entry.").replace("{name}", &name),
            actions: vec![IssueAction { label: tr("Yes"), does: IssueDoes::Plugin { id: "same_phone:yes".into() } },
                          IssueAction { label: tr("No"), does: IssueDoes::Plugin { id: "same_phone:no".into() } }],
            kind: "info".into(), details: String::new(), tell_dismiss: true,
        };
        host.show_issue("same_phone", issue.clone());
        self.same_phone = Some(SamePhone { phone, old, issue });
    }

    fn answer_same_phone(&mut self, host: &mut dyn Host, same: bool) {
        let Some(pending) = self.same_phone.take() else { return };
        host.clear_issue(Some("same_phone"), Some(&pending.issue));
        let take_over = same.then(|| pending.old.id.clone());
        self.finish_pairing(host, pending.phone, take_over);
    }

    fn finish_pairing(&mut self, host: &mut dyn Host, phone: PairedPhone, take_over: Option<String>) {
        let pid = phone.id.clone();
        let paired = match take_over {
            Some(old_id) => {
                log::info!("{} paired with a new id; moving its settings over", phone.name);
                host.move_device_settings(&old_id, &pid);
                self.conn().take_over(&old_id, phone);
                if self.active_key.as_deref() == Some(old_id.as_str()) {
                    self.active_key = Some(pid.clone()); // the panels hold its settings already, now the new id's
                }
                connection::Paired { existing: false, reconnect: false }
            }
            None => self.conn().on_phone_paired(phone, |id| host.is_streaming_from(Some(id))),
        };
        let shown = host.focused_source_id().or_else(|| self.conn().selected_id.clone());
        if shown.is_none() || shown.as_deref() == Some(pid.as_str()) || !host.is_streaming_from(shown.as_deref()) {
            self.select(host, Some(&pid), true, false); // one streaming next to others gets the panels
        } else {
            self.refresh_picker(host); // the shown card streams: the new phone waits in the list
        }
        if let Some(issue) = self.no_phone_issue.take() {
            host.clear_issue(Some("start"), Some(&issue)); // a Start pressed before there was a phone left it up
        }
        if paired.reconnect {
            host.reconnect_source(&pid); // its running stream still holds the old token
        }
        host.save_now();
    }

    pub fn rename_phone(&mut self, host: &mut dyn Host, pid: &str, name: &str) {
        if self.conn().rename_phone(pid, name) {
            self.refresh_picker(host);
            host.save_now();
            self.render(host);
        }
    }

    pub fn set_computer_name(&mut self, host: &mut dyn Host, name: &str) {
        self.conn().set_computer_name(name);
        host.save_now();
        if self.phones_open {
            self.refresh_phones_dialog(host);
        }
    }

    pub fn forget_phone(&mut self, host: &mut dyn Host, pid: &str) {
        let (selected, streaming) = {
            let c = self.conn();
            if c.phone(Some(pid)).is_none() {
                return;
            }
            (c.selected_id.as_deref() == Some(pid), c.streaming)
        };
        if selected && streaming {
            host.stop_stream(None); // while it's still paired, so the stop can reach the phone's camera
        } else if host.is_streaming_from(Some(pid)) {
            host.stop_stream(Some(pid));
        }
        let Some((phone, reselect)) = self.conn().forget_phone(pid) else { return };
        let link = self.link.clone();
        // Unpair on the phone too when it's reachable, so it stops accepting a forgotten computer
        std::thread::spawn(move || link.unpair(&phone));
        match reselect {
            Some(next) => {
                self.active_key = None; // nothing to save back: its settings are being deleted
                self.select(host, next.as_deref(), false, false);
                if next.is_none() {
                    // select saw no change of profile, but the shell still holds the forgotten phone as picked
                    host.switch_device(None, None);
                }
            }
            None => self.refresh_picker(host),
        }
        host.forget_device_settings(pid);
        host.save_now();
    }

    /// switch: picked by hand mid-stream, so the stream moves to pid instead of just stopping.
    pub fn select(&mut self, host: &mut dyn Host, pid: Option<&str>, force: bool, switch: bool) {
        let (same, streaming) = {
            let c = self.conn();
            (c.selected_id.as_deref() == pid, c.streaming)
        };
        if same && !force {
            return;
        }
        if !same && host.pick_source(pid) {
            self.refresh_picker(host); // the host took it from here (it streams already, or replaces one of several)
            return;
        }
        let mut restart = false;
        if streaming && !same {
            host.stop_stream(None);
            restart = switch;
        }
        self.mark_selected(host, pid);
        self.activate_profile(host, pid);
        self.show_selection(host);
        if restart {
            host.start_stream(true);
        }
    }

    fn mark_selected(&mut self, host: &mut dyn Host, pid: Option<&str>) {
        self.conn().mark_selected(pid);
        self.update_note = None;
        self.refresh_picker(host);
    }

    fn activate_profile(&mut self, host: &mut dyn Host, key: Option<&str>) {
        if key == self.active_key.as_deref() {
            return;
        }
        let prev = std::mem::replace(&mut self.active_key, key.map(str::to_string));
        host.switch_device(prev.as_deref(), key);
    }

    fn show_selection(&mut self, host: &mut dyn Host) {
        self.render(host);
        self.announce_source(host);
        self.check_status(host);
    }

    /// The host moved the panels to pid's stream and swapped the settings itself: show it as picked.
    pub fn show_selected(&mut self, host: &mut dyn Host, pid: Option<&str>) {
        if self.conn().selected_id.as_deref() == pid {
            return;
        }
        self.mark_selected(host, pid);
        self.active_key = pid.map(str::to_string);
        self.show_selection(host);
    }

    fn on_udp(&mut self, host: &mut dyn Host, on: bool) {
        if self.conn().udp == on {
            return;
        }
        self.conn().udp = on;
        host.schedule_save();
        self.refresh_transport(host);
    }

    fn refresh_transport(&mut self, host: &mut dyn Host) {
        let format = self.shared.format();
        if self.conn().transport_needs_reconnect(format) {
            host.reconnect_stream();
        }
    }

    fn on_posted(&mut self, host: &mut dyn Host, token: &str, data: &Value) {
        match token {
            P_RESOLVED => self.on_resolved(host, data),
            P_PROBED => self.on_probed(host, data),
            P_USB => self.on_usb_available(host, data),
            P_USB_FORWARD => self.on_usb_forward(host, data),
            P_USB_STATE => self.on_usb_state(host, data),
            P_EARLIER => self.on_earlier(host, data),
            P_ADB => self.on_adb(host, data),
            P_ADB_PROGRESS => {
                if let Some(add) = self.add.as_mut() {
                    add.usb_text = data.as_str().unwrap_or_default().to_string();
                    add.usb_kind = "status_dim";
                }
                self.render_add(host, true);
            }
            P_VCAM => self.on_vcam(host, data),
            P_UPDATED => {
                self.updating_phone = false;
                self.update_note = Some(if data["ok"].as_bool() == Some(true) {
                    ("status_ok".into(), tr("Phone app updated. Open Telescope on the phone."))
                } else {
                    ("status_err".into(), data["detail"].as_str().unwrap_or_default().to_string())
                });
                self.render(host);
            }
            POST_CHANGED => {
                if data["save"].as_bool() == Some(true) {
                    host.schedule_save();
                }
                self.render(host);
            }
            POST_START_PROBLEM => {
                if let Ok(problem) = serde_json::from_value(data.clone()) {
                    self.render(host);
                    self.show_start_problem(host, problem);
                }
            }
            _ => {}
        }
    }

    fn on_timer(&mut self, host: &mut dyn Host, token: &str) {
        match token {
            T_POLL => {
                host.schedule(STATUS_POLL.as_millis() as u64, T_POLL);
                let now = self.now();
                let check = self.conn().poll_status(now);
                self.run_check(check);
            }
            T_USB_WATCH => self.watch_usb(host),
            T_PAIR_POLL => self.poll_pairing(host),
            T_PAIR_USB if self.add.as_ref().is_some_and(|a| a.paired.is_none() && a.offer.is_some()) => {
                host.schedule(PAIR_USB_POLL.as_millis() as u64, T_PAIR_USB);
                self.poll_usb();
            }
            _ => {}
        }
    }

    fn shows(&self, host: &dyn Host, sid: &str) -> bool {
        sid == host.focused_source_id().unwrap_or_default()
    }

    fn pick_options(&self) -> Vec<(String, String)> {
        let c = self.conn();
        c.phones.iter().map(|p| (p.id.clone(), p.name.clone()))
            .chain(self.sources.iter().map(|s| (s.id.clone(), s.name.clone()))).collect()
    }
}

/// What the card shows besides the model.
pub struct CardCtx {
    /// The picked StreamSource's name, when a source rather than a phone is picked.
    pub source: Option<String>,
    pub update_note: Option<(String, String)>,
    pub updating_phone: bool,
    pub apk_bundled: bool,
    pub adb_available: bool,
}

/// The card's properties from the model (_render).
pub fn card_props(c: &Connection, ctx: &CardCtx) -> Value {
    let mut p = Map::new();
    let Some(phone) = c.selected_phone() else {
        let live = c.streaming && c.connected && !c.lost;
        let text = match &ctx.source {
            None => tr("No phone yet"),
            Some(_) if c.streaming && live => tr_ctx("status", "Streaming"),
            Some(source) if c.streaming => tr("Waiting for {source}…").replace("{source}", source),
            Some(source) => tr("{source}, no phone needed").replace("{source}", source),
        };
        p.insert("status-kind".into(), json!(if live { "status_ok" } else { "status_dim" }));
        p.insert("status-text".into(), json!(text));
        for (k, v) in [("using-shown", false), ("route-shown", false), ("switch-usb-shown", false), ("update-shown", false)] {
            p.insert(k.into(), json!(v));
        }
        p.insert("route-icon".into(), json!("x"));
        p.insert("note-text".into(), json!(""));
        return Value::Object(p);
    };
    let res = c.resolution.as_ref();
    let (mut kind, mut text) = status_line(res);
    if c.streaming {
        (kind, text) = if c.lost {
            ("status_warn", tr("Reconnecting…"))
        } else if c.connected {
            ("", match c.stream_route() {
                Some(r) if is_usb(&r) => tr("Streaming over USB"),
                Some(_) => tr("Streaming over Wi-Fi"),
                None => tr_ctx("status", "Streaming"),
            })
        } else {
            ("status_dim", tr("Connecting…"))
        };
    }
    let route = if c.streaming { c.stream_route() } else { res.and_then(|r| r.route.clone()) };
    let mut note = String::new();
    if let (Some(res), false) = (res, c.streaming) {
        note = problem_text(res, &phone.name, ctx.adb_available);
        if note.is_empty() {
            note = connection::not_using_usb(res).unwrap_or_default();
        }
    }
    let mut note_kind = "status_dim".to_string();
    if let Some((k, t)) = &ctx.update_note {
        note_kind = k.clone();
        note = t.clone();
    } else if ctx.updating_phone {
        note = tr("Updating the phone app over USB…");
    }
    let action = if c.streaming { None } else { outdated_action(res, ctx.apk_bundled, ctx.adb_available) };
    if action == Some(Fix::UpdateOverUsb) && ctx.update_note.is_none() && !ctx.updating_phone {
        note = tr("The Telescope app on {phone} is older than this one.").replace("{phone}", &phone.name); // the button is the how
    }
    let preference = c.pending_route.unwrap_or(c.route_pref);
    json!({
        "status-kind": kind, "status-text": text,
        "using-text": route_text(route.as_ref()), "using-shown": route.is_some(),
        "route-icon": match &route { Some(r) if is_usb(r) => "usb", Some(_) => "wifi", None => "x" },
        "route-shown": true, "route-current": route_index(preference),
        "note-text": note, "note-kind": note_kind,
        "switch-usb-shown": c.switch_usb_visible && c.streaming,
        "update-shown": action.is_some(),
        "update-text": action.map(Fix::label).unwrap_or_default(),
        "update-enabled": !ctx.updating_phone,
    })
}

fn route_index(preference: RoutePreference) -> i64 {
    match preference {
        RoutePreference::Auto => 0,
        RoutePreference::Usb => 1,
        RoutePreference::Wifi => 2,
    }
}

fn route_of_index(i: i64) -> RoutePreference {
    match i {
        1 => RoutePreference::Usb,
        2 => RoutePreference::Wifi,
        _ => RoutePreference::Auto,
    }
}

fn fix_does(fix: Fix, source_id: &str) -> IssueDoes {
    match fix {
        Fix::AddPhone => IssueDoes::Plugin { id: "add_phone".into() },
        Fix::UpdateDesktop | Fix::UpdateOverUsb => IssueDoes::Plugin { id: "update".into() },
        Fix::SwitchToAutomatic => IssueDoes::StartAgain { source_id: Some(source_id.into()), before: Some("switch_auto".into()) },
        Fix::TryAgain => IssueDoes::StartAgain { source_id: Some(source_id.into()), before: None },
    }
}

/// Text with its <a href="..."> tags taken out.
fn without_links(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else { return out + &rest[start..] };
        rest = &rest[start + end + 1..];
    }
    out + rest
}

/// What the link to target says, in a text with <a href="target"> in it.
fn link_label(text: &str, target: &str) -> Option<String> {
    let open = format!("<a href=\"{target}\">");
    let start = text.find(&open)? + open.len();
    Some(text[start..].split("</a>").next()?.to_string())
}

/// The first letter in capitals: a link from the middle of a sentence, shown as a button.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

/// A banner button that copies command (copy_action).
fn copy_action(command: &str) -> IssueAction {
    IssueAction { label: tr("Copy command"), does: IssueDoes::OpenUrl { url: format!("copy:{command}") } }
}

/// Your phones' rows: each phone with the address it was last seen at, then the browsers with settings of their own.
pub fn phone_rows(phones: &[PairedPhone], remembered: &[RememberedSource]) -> Vec<Value> {
    let mut rows = Vec::new();
    for phone in phones {
        let ip = phone.active_ip.clone().or_else(|| phone.ips.first().cloned()).unwrap_or_default();
        let sub = if ip.is_empty() { tr("Phone") } else { tr("Phone · {ip}").replace("{ip}", &ip) };
        let confirm = tr("Remove \"{name}\"? This deletes its camera settings on this computer, and unpairs it on the phone too if Telescope can reach it.")
            .replace("{name}", &phone.name);
        rows.push(json!({"id": phone.id, "name": phone.name, "sub": sub, "renamable": true,
                         "rename-label": tr("Rename {name}").replace("{name}", &phone.name),
                         "remove-label": tr("Remove {name}").replace("{name}", &phone.name),
                         "confirm-title": tr("Remove phone"), "confirm": confirm}));
    }
    for source in remembered {
        let confirm = tr("Remove \"{name}\"? This deletes the settings it had on this computer. It can still connect with the code, and starts from the defaults.")
            .replace("{name}", &source.name);
        rows.push(json!({"id": source.id, "name": source.name, "sub": source.detail, "renamable": false,
                         "remove-label": tr("Remove {name}").replace("{name}", &source.name),
                         "confirm-title": tr("Remove browser"), "confirm": confirm}));
    }
    rows
}

/// Plugged-in phones that allow USB debugging, as (serial, name).
fn usable(states: &[UsbDevice]) -> Vec<(String, String)> {
    states.iter().filter(|d| d.state == "device")
        .map(|d| (d.serial.clone(), if d.model.is_empty() { d.serial.clone() } else { d.model.clone() })).collect()
}

/// The QR code's dark cells and size, shared with the other cards that show one.
pub use telescope_core::qr::encode as qr;

impl Plugin for ConnectionPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: "Phones and connection".into(),
            about: "Pairing phones, picking one and connecting to it over Wi-Fi or USB.".into(),
            category: "streaming".into(),
            icon: "connection".into(),
            pinned: true,
            required: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        self.shared.set_poster(self.poster.clone());
        self.adb_available = self.link.adb_available();
        self.apk_bundled = self.link.bundled_apk().is_some();
        host.schedule(STATUS_POLL.as_millis() as u64, T_POLL);
        self.catch_up(host);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        let count = self.conn().phones.len() as u32;
        host.emit(Event::PhonesChanged { count });
        self.announce_source(host);
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        self.add = None;
        self.shared.set_poster(None);
        self.link.shutdown();
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        let now = self.now();
        match event {
            Event::Posted { owner, token, data } if owner == NAME => self.on_posted(host, token, data),
            Event::TimerFired { owner, token } if owner == NAME => self.on_timer(host, token),
            Event::RequestDone { request, ok, data, .. } if self.adb_asked == Some(*request) => {
                self.adb_asked = None;
                if let (true, Some(path)) = (*ok, data["path"].as_str().filter(|p| !p.is_empty())) {
                    self.adb_picked(host, path);
                }
            }
            Event::StreamConnected { source_id } => {
                let shows = self.shows(host, source_id);
                self.conn().on_stream_connected(shows);
                if shows {
                    self.render(host);
                }
            }
            Event::StreamLost { source_id } => {
                let shows = self.shows(host, source_id);
                self.conn().on_stream_lost(shows, now);
                if shows {
                    host.cancel_schedule(T_USB_WATCH);
                    self.render(host);
                }
            }
            Event::StreamTransportChanged => self.refresh_transport(host),
            Event::AddPhoneRequested => self.open_add_phone(host),
            Event::SelectSource { source_id } => self.select(host, Some(source_id), false, false),
            Event::StreamSourcesChanged { sources } => {
                self.sources = sources.clone();
                self.refresh_picker(host);
                self.render(host);
                self.announce_source(host);
            }
            Event::RememberedSources { sources } => {
                self.remembered = sources.clone();
                if self.phones_open {
                    self.refresh_phones_dialog(host);
                }
            }
            Event::WindowShown { shown } => {
                let check = self.conn().set_poll_reason("shown", *shown, now);
                self.run_check(check);
            }
            Event::CameraWatched { watched } => {
                let check = self.conn().set_poll_reason("watched", *watched, now);
                self.run_check(check);
            }
            Event::PhoneWanted { wanted, held } => {
                let check = self.conn().phone_wanted(*wanted, *held, now);
                self.run_check(check);
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "ConnectionCard".into(), props: json!({"status-text": tr("No phone yet")}) },
            region: "left".into(),
        })
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec { source: SlintSource::Text(UI.into()), component: "PhonePicker".into(), props: json!({"names": [], "current": -1}) })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: "udp".into(),
            tab: "streaming".into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "ConnectionSettings".into(), props: json!({"udp": self.conn().udp}) },
            order: 0,
            tab_title: String::new(),
            keywords: tr_ctx("search", "udp tcp transport network lag freezes stutter wifi"),
            footer: None,
        }]
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let options: Vec<Value> = self.pick_options().into_iter().map(|(id, name)| json!([id, name])).collect();
        let mut actions = vec![ActionSpec {
            id: "phone.pick".into(),
            label: tr("Phone"),
            group: "Telescope".into(),
            params: vec![
                json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "next",
                       "choices": [["next", tr("Next")], ["previous", tr("Previous")], ["set", tr("Pick")]]}),
                json!({"name": "option", "label": tr("Option"), "kind": "choice", "default": "", "choices": options,
                       "shown_if": ["op", ["set"]]}),
            ],
            repeats: false,
            on_release: false,
            current: None,
            keywords: String::new(),
        }];
        if parity_offer_file().is_some() {
            actions.push(ActionSpec {
                id: "connection.add_phone".into(), label: "Add phone".into(), group: "Telescope".into(), params: Vec::new(),
                repeats: false, on_release: false, current: None, keywords: String::new(),
            });
        }
        actions
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        if id == "connection.add_phone" && parity_offer_file().is_some() {
            self.open_add_phone(host);
            return Some("Add phone".into());
        }
        if id != "phone.pick" {
            return None;
        }
        let options = self.pick_options();
        if options.is_empty() {
            return None;
        }
        let current = {
            let c = self.conn();
            options.iter().position(|(id, _)| Some(id) == c.selected_id.as_ref())
        };
        let target = match params["op"].as_str().unwrap_or("next") {
            "set" => {
                let want = params["option"].as_str()?;
                options.iter().position(|(id, name)| id == want || name == want)?
            }
            op => {
                let n = options.len();
                let step = if op == "next" { 1 } else { n - 1 };
                let from = current.unwrap_or(if op == "next" { n - 1 } else { 0 });
                let next = (from + step) % n;
                if Some(next) == current {
                    return Some(tr("{label}: {value}").replace("{label}", &tr("Phone")).replace("{value}", &options[next].1));
                }
                next
            }
        };
        let (pid, name) = options[target].clone();
        self.select(host, Some(&pid), false, true);
        Some(tr("{label}: {value}").replace("{label}", &tr("Phone")).replace("{value}", &name))
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let s = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or_default().to_string();
        match (event.ui.as_str(), event.callback.as_str()) {
            ("card", "add-phone") | ("header", "add-phone") => self.open_add_phone(host),
            ("card", "guide") => host.emit(Event::SetupGuideRequested),
            ("card", "switch-usb") => {
                self.conn().switch_usb_visible = false;
                self.render(host);
                host.reconnect_stream(); // automatic routing picks USB now that it answers
            }
            ("card", "update") => self.update_action(host),
            ("card", "route-picked") => {
                let i = event.args.first().and_then(Value::as_i64).unwrap_or(0);
                self.set_route_preference(host, route_of_index(i));
            }
            ("card", "add-closed") => self.close_add(host),
            ("card", "pick") => self.pick_usb(host, &s(0)),
            ("card", "usb-action-clicked") => self.get_adb(host),
            ("card", "usb-pick-clicked") => self.pick_adb(host),
            ("card", "vcam-answered") => {
                let answer = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
                self.answer_vcam(host, answer(0), answer(1));
            }
            ("header", "picked") => {
                let i = event.args.first().and_then(Value::as_i64).unwrap_or(-1);
                if let Some(pid) = usize::try_from(i).ok().and_then(|i| self.picker_ids.get(i).cloned()) {
                    self.select(host, Some(&pid), false, true);
                }
            }
            ("header", "picker-open") => {
                self.picker_open = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                if !self.picker_open && std::mem::take(&mut self.picker_stale) {
                    self.refresh_picker(host);
                }
            }
            ("header", "manage") => {
                self.phones_open = true;
                self.refresh_phones_dialog(host);
            }
            ("header", "phones-closed") => {
                self.phones_open = false;
                host.set_ui_props("header", &json!({"phones-open": false}));
            }
            ("header", "rename") if !s(1).trim().is_empty() => self.rename_phone(host, &s(0), s(1).trim()),
            ("header", "rename-computer") if !s(0).trim().is_empty() => self.set_computer_name(host, s(0).trim()),
            ("header", "remove") => {
                let pid = s(0);
                if self.remembered.iter().any(|r| r.id == pid) {
                    host.emit(Event::ForgetSourceRequested { source_id: pid });
                } else {
                    self.forget_phone(host, &pid);
                }
            }
            ("udp", "udp-toggled") => self.on_udp(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            _ => {}
        }
    }

    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        match (key, id) {
            (_, "add_phone") => self.open_add_phone(host),
            (_, "update") => self.update_action(host),
            (_, "switch_auto") => self.set_route_preference(host, RoutePreference::Auto),
            ("same_phone", "same_phone:yes") => self.answer_same_phone(host, true),
            ("same_phone", "same_phone:no") | ("same_phone", "dismiss") => self.answer_same_phone(host, false),
            (_, id) if id.starts_with("route:") => {
                if let Some(preference) = RoutePreference::parse(&id["route:".len()..]) {
                    self.set_route_preference(host, preference);
                }
            }
            _ => {}
        }
    }

    fn ensure_virtual_camera(&mut self, host: &mut dyn Host, interactive: bool, source_id: Option<&str>) -> bool {
        let Some(outputs) = self.outputs.clone().filter(|_| !crate::platform::windows()) else { return true };
        if outputs.slots().iter().any(|s| s.slot == 0) {
            return true;
        }
        let status = outputs.setup_status();
        if status.ready {
            return true;
        }
        if status.loaded {
            let command = "sudo modprobe -r v4l2loopback";
            host.show_issue("start", Issue {
                title: tr("Virtual camera is set up differently"),
                text: tr("v4l2loopback is loaded, but without {device}: another app set it up with other settings. Close that app and run this to hand it over to Telescope:")
                    .replace("{device}", V4L2_PHONE_DEV),
                actions: vec![copy_action(command)], kind: "err".into(), details: command.into(), tell_dismiss: false,
            });
            return false;
        }
        if !interactive {
            // Started by itself (nobody at the keyboard asked): no password prompt out of nowhere
            let again = IssueDoes::StartAgain { source_id: source_id.map(str::to_string), before: None };
            host.show_issue("start", Issue {
                title: tr("The virtual camera is off"), text: tr("Switching it on asks for your password."),
                actions: vec![IssueAction { label: tr("Switch on"), does: again }],
                kind: "warn".into(), details: String::new(), tell_dismiss: false,
            });
            return false;
        }
        // Python's question held the Start up; here it ends this one and an OK starts again once the camera is on
        self.vcam_source = source_id.map(str::to_string);
        host.set_ui_props("card", &json!({"vcam-open": true}));
        false
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.catch_up(host);
        self.conn().on_stream_start(&stream.url);
        host.cancel_schedule(T_USB_WATCH);
        if self.conn().usb_watch_active {
            host.schedule(USB_WATCH.as_millis() as u64, T_USB_WATCH);
        }
        self.render(host);
    }

    /// The window moved the panels onto a stream that was already running (show_selected), with its settings.
    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        let pid = Some(stream.source_id.as_str()).filter(|s| !s.is_empty());
        self.show_selected(host, pid);
        self.on_stream_start(host, stream);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        let now = self.now();
        host.cancel_schedule(T_USB_WATCH);
        let ids: HashSet<String> = self.conn().phones.iter().map(|p| p.id.clone()).collect();
        let still: HashSet<String> = ids.into_iter().filter(|id| host.is_streaming_from(Some(id))).collect();
        let check = self.conn().on_stream_stop(|pid| still.contains(pid), now);
        self.render(host);
        self.run_check(check);
    }

    fn get_config(&self) -> Value {
        self.conn().get_config()
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let sources: HashSet<String> = self.sources.iter().map(|s| s.id.clone()).collect();
        self.conn().set_config(cfg, |s| sources.contains(s));
        self.stale = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let c = self.conn();
        let source = self.source_name(c.selected_id.as_deref());
        match c.diagnostics(source.as_deref()) {
            Value::Object(map) => map.into_iter().map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect(),
            _ => Vec::new(),
        }
    }
}

/// The plugin over a LinkService, sharing its model.
impl crate::service::LinkService {
    pub fn connection_plugin(self: &Arc<Self>) -> ConnectionPlugin {
        ConnectionPlugin::new(self.clone(), self.shared())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Tunnels;
    use std::sync::Mutex;
    use std::time::Duration;
    use telescope_core::link::{PhoneSession, RoutePrefs, Unreachable};
    use telescope_core::output::{SetupStatus, Slot, VideoOutput};
    use telescope_core::PixelFormat;
    use telescope_core::testing::FakeHost;

    struct NoTunnels;

    impl Tunnels for NoTunnels {
        fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
            Some(41000)
        }
        fn release(&self, _serial: &str, _remote_port: u16) {}
    }

    /// A phone link whose checks answer from `answer` and that writes down unpairs and installs.
    struct FakeLink {
        answer: Mutex<Resolution>,
        unpaired: Mutex<Vec<String>>,
        install: Mutex<Result<(), String>>,
        install_panics: Mutex<bool>,
        offer: Mutex<Option<Arc<FakeOffer>>>,
        adb: Mutex<bool>,
        offline: Mutex<bool>,
    }

    impl FakeLink {
        fn new() -> Arc<FakeLink> {
            Arc::new(FakeLink {
                answer: Mutex::new(Resolution::new(PhoneStatus::Unreachable)), unpaired: Mutex::default(),
                install: Mutex::new(Ok(())), install_panics: Mutex::default(), offer: Mutex::default(), adb: Mutex::new(true), offline: Mutex::new(false),
            })
        }

        fn answer(&self, res: Resolution) {
            *self.answer.lock().unwrap() = res;
        }
    }

    #[derive(Default)]
    struct FakeOffer {
        paired: Mutex<Option<PairedPhone>>,
        offline: bool,
        offered: Mutex<Vec<String>>,
    }

    impl PairingOffer for FakeOffer {
        fn payload(&self) -> String {
            "{\"v\": 4}".into()
        }
        fn port(&self) -> u16 {
            8765
        }
        fn addresses(&self) -> Vec<String> {
            if self.offline { Vec::new() } else { vec!["192.168.1.2".into()] }
        }
        fn offer_over_usb(&self, _package: &str) -> usize {
            0
        }
        fn poll(&self) -> Option<PairedPhone> {
            self.paired.lock().unwrap().take()
        }
        fn offer_to(&self, serial: &str, _package: &str) -> bool {
            self.offered.lock().unwrap().push(serial.to_string());
            true
        }
    }

    impl Link for FakeLink {
        fn new_identity(&self) -> (String, String) {
            ("cid".into(), "desk".into())
        }
        fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
            Err(Unreachable::NoAnswer)
        }
        fn pair(&self, _cid: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
            let offer = Arc::new(FakeOffer { offline: *self.offline.lock().unwrap(), ..Default::default() });
            *self.offer.lock().unwrap() = Some(offer.clone());
            Ok(offer)
        }
        fn check(&self, _phone: &PairedPhone, _preference: RoutePreference) -> Resolution {
            self.answer.lock().unwrap().clone()
        }
        fn unpair(&self, phone: &PairedPhone) {
            self.unpaired.lock().unwrap().push(phone.id.clone());
        }
        fn bundled_apk(&self) -> Option<String> {
            Some("Telescope.apk".into())
        }
        fn adb_available(&self) -> bool {
            *self.adb.lock().unwrap()
        }
        fn install_app(&self, _serial: &str, _apk: &str) -> Result<(), String> {
            assert!(!*self.install_panics.lock().unwrap(), "no space left");
            self.install.lock().unwrap().clone()
        }
    }

    fn phone(pid: &str, name: &str) -> PairedPhone {
        PairedPhone { id: pid.into(), name: name.into(), token: "tok".into(), ips: vec!["10.0.0.5".into()], active_ip: None,
                      cert_sha256: "ab".repeat(32) }
    }

    fn ready(route: RouteSpec) -> Resolution {
        Resolution { route: Some(route), ..Resolution::new(PhoneStatus::Ready) }
    }

    fn wifi() -> RouteSpec {
        RouteSpec::Network { address: "10.0.0.5".into() }
    }

    fn usb() -> RouteSpec {
        RouteSpec::Usb { serial: "serial-1".into() }
    }

    struct Env {
        plugin: ConnectionPlugin,
        host: FakeHost,
        link: Arc<FakeLink>,
        shared: Arc<Shared>,
    }

    fn env() -> Env {
        let link = FakeLink::new();
        let shared = Shared::new(Arc::new(NoTunnels));
        let mut plugin = ConnectionPlugin::new(link.clone(), shared.clone());
        let mut host = FakeHost::new();
        plugin.setup(&mut host);
        Env { plugin, host, link, shared }
    }

    impl Env {
        fn add(&mut self, pid: &str, name: &str) {
            self.plugin.phone_paired(&mut self.host, phone(pid, name));
            self.settle();
        }

        /// Hands every post (from the plugin's threads too) back to the plugin until nothing more comes.
        fn settle(&mut self) {
            loop {
                std::thread::sleep(Duration::from_millis(30));
                let posted = self.host.take_posted();
                if posted.is_empty() {
                    return;
                }
                for (token, data) in posted {
                    self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
                }
            }
        }

        fn props(&self, ui: &str) -> Map<String, Value> {
            let prefix = format!("set_ui_props {ui} ");
            let mut all = Map::new();
            for call in &self.host.calls {
                if let Some(json) = call.strip_prefix(&prefix) {
                    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(json) {
                        all.extend(m);
                    }
                }
            }
            all
        }

        fn status(&self) -> String {
            self.props("card")["status-text"].as_str().unwrap_or_default().to_string()
        }

        fn called(&self, call: &str) -> bool {
            self.host.calls.iter().any(|c| c.starts_with(call))
        }

        fn ui(&mut self, ui: &str, callback: &str, args: Vec<Value>) {
            self.plugin.on_ui(&mut self.host, &UiEvent { ui: ui.into(), callback: callback.into(), args });
            self.settle();
        }

        fn phones_changed(&self) -> Vec<u32> {
            self.host.events.iter().filter_map(|e| match e { Event::PhonesChanged { count } => Some(*count), _ => None }).collect()
        }
    }

    /// A computer without the virtual camera, whose setup answers from `result`.
    struct FakeOutputs {
        status: Mutex<SetupStatus>,
        result: Mutex<SetupResult>,
        asked: Mutex<Vec<bool>>,
    }

    impl FakeOutputs {
        fn new(loaded: bool) -> Arc<FakeOutputs> {
            Arc::new(FakeOutputs {
                status: Mutex::new(SetupStatus { loaded, ..SetupStatus::default() }),
                result: Mutex::new(SetupResult { ok: true, message: "ok".into(), command: None }),
                asked: Mutex::default(),
            })
        }

        fn answers(&self, ok: bool, message: &str, command: Option<&str>) {
            *self.result.lock().unwrap() = SetupResult { ok, message: message.into(), command: command.map(str::to_string) };
        }
    }

    impl Outputs for FakeOutputs {
        fn slots(&self) -> Vec<Slot> {
            Vec::new()
        }
        fn formats(&self) -> Vec<PixelFormat> {
            Vec::new()
        }
        fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
            Err("no camera".into())
        }
        fn setup_status(&self) -> SetupStatus {
            self.status.lock().unwrap().clone()
        }
        fn set_up(&self, action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
            if let SetupAction::Load { persist } = action {
                self.asked.lock().unwrap().push(persist);
            }
            self.result.lock().unwrap().clone()
        }
    }

    fn linux() -> std::sync::MutexGuard<'static, ()> {
        let lock = crate::platform::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::platform::set_windows(Some(false));
        lock
    }

    fn vcam_env(outputs: &Arc<FakeOutputs>) -> Env {
        let mut e = env();
        e.plugin.outputs = Some(outputs.clone());
        e
    }

    impl Env {
        fn start(&mut self, interactive: bool) -> bool {
            self.plugin.ensure_virtual_camera(&mut self.host, interactive, Some("id-a"))
        }

        fn start_issue(&self) -> Option<Issue> {
            self.host.issues.iter().find(|(k, _)| k == "start").map(|(_, i)| i.clone())
        }

        fn vcam_open(&self) -> bool {
            self.props("card").get("vcam-open").and_then(Value::as_bool) == Some(true)
        }

        fn answer_vcam(&mut self, ok: bool, persist: bool) {
            self.ui("card", "vcam-answered", vec![json!(ok), json!(persist)]);
        }
    }

    #[test]
    fn linux_virtual_camera_conflict_and_cancel() {
        let _lock = linux();
        let outputs = FakeOutputs::new(true);
        let mut e = vcam_env(&outputs);
        assert!(!e.start(true));
        let issue = e.start_issue().unwrap();
        assert_eq!(issue.title, "Virtual camera is set up differently");
        assert_eq!(issue.details, "sudo modprobe -r v4l2loopback");
        assert!(issue.text.contains(V4L2_PHONE_DEV));
        assert_eq!(issue.actions[0].label, "Copy command");
        assert!(matches!(&issue.actions[0].does, IssueDoes::OpenUrl { url } if url == "copy:sudo modprobe -r v4l2loopback"));

        e.host.issues.clear();
        outputs.status.lock().unwrap().loaded = false;
        assert!(!e.start(true));
        assert!(e.vcam_open());
        e.answer_vcam(false, false);
        assert!(!e.vcam_open());
        assert!(outputs.asked.lock().unwrap().is_empty()); // no password prompt after Cancel
        assert!(e.start_issue().is_none());
    }

    #[test]
    fn linux_virtual_camera_one_prompt_with_the_startup_choice() {
        let _lock = linux();
        let outputs = FakeOutputs::new(false);
        let mut e = vcam_env(&outputs);
        outputs.answers(false, "denied", None);
        assert!(!e.start(true));
        e.answer_vcam(true, true);
        assert_eq!(*outputs.asked.lock().unwrap(), vec![true]);
        assert_eq!(e.start_issue().unwrap().title, "Couldn't set up the virtual camera");
        assert!(!e.called("start_again"));

        e.host.issues.clear();
        outputs.answers(false, SETUP_CANCELLED, None);
        e.start(true);
        e.answer_vcam(true, true);
        assert!(e.start_issue().is_none()); // the person said no; nothing to explain

        outputs.answers(true, "ok", None);
        e.start(true);
        e.answer_vcam(true, false);
        assert_eq!(outputs.asked.lock().unwrap().last(), Some(&false));
        assert!(e.called("start_again Some(\"id-a\")")); // goes on with the Start the question ended
    }

    #[test]
    fn linux_virtual_camera_without_a_password_prompt_shows_the_command() {
        let _lock = linux();
        let outputs = FakeOutputs::new(false);
        let mut e = vcam_env(&outputs);
        outputs.answers(false, "no prompt", Some("sudo modprobe v4l2loopback"));
        e.start(true);
        e.answer_vcam(true, true);
        let issue = e.start_issue().unwrap();
        assert_eq!(issue.details, "sudo modprobe v4l2loopback");
        assert_eq!(issue.actions.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Copy command"]);
    }

    #[test]
    fn a_start_nobody_asked_for_never_prompts_for_a_password() {
        let _lock = linux();
        let outputs = FakeOutputs::new(false);
        let mut e = vcam_env(&outputs);
        assert!(!e.start(false));
        assert!(!e.vcam_open());
        let issue = e.start_issue().unwrap();
        assert_eq!(issue.title, "The virtual camera is off");
        assert!(matches!(&issue.actions[0].does, IssueDoes::StartAgain { source_id: Some(s), before: None } if s == "id-a"));
    }

    #[test]
    fn a_ready_camera_or_windows_needs_no_check() {
        let _lock = linux();
        let outputs = FakeOutputs::new(false);
        outputs.status.lock().unwrap().ready = true;
        let mut e = vcam_env(&outputs);
        assert!(e.start(true));
        outputs.status.lock().unwrap().ready = false;
        crate::platform::set_windows(Some(true));
        assert!(e.start(true));
        crate::platform::set_windows(None);
        assert!(e.host.issues.is_empty() && !e.vcam_open());
    }

    #[test]
    fn empty_state_is_just_the_status() {
        let mut e = env();
        e.settle();
        assert_eq!(e.status(), "No phone yet");
        assert_eq!(e.props("card")["route-shown"], false);
        assert_eq!(e.props("card")["using-shown"], false);
    }

    #[test]
    fn header_holds_the_phone_picker_and_manage_button() {
        let mut e = env();
        let header = e.plugin.header_widget().unwrap();
        assert_eq!(header.component, "PhonePicker");
        assert!(UI.contains("callback manage;") && UI.contains("@tr(\"Your phones: add, rename, remove\")"));
        assert_eq!(e.plugin.card().unwrap().ui.component, "ConnectionCard");
    }

    #[test]
    fn pairing_adds_and_selects_the_phone() {
        let mut e = env();
        e.link.answer(ready(wifi()));
        e.add("id-a", "Pixel");
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-a"));
        assert_eq!(e.props("header")["names"], json!(["Pixel"]));
        assert_eq!(e.props("header")["current"], 0);
        assert!(e.called("switch_device None Some(\"id-a\")"));
        assert!(e.called("save_now"));
        // Its first check landed on the card
        assert_eq!(e.status(), "Ready");
    }

    #[test]
    fn phone_count_is_announced_on_the_bus() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        assert_eq!(e.phones_changed().last(), Some(&2));
    }

    #[test]
    fn once_every_plugin_is_in_connection_says_again_how_many_phones_and_whats_picked() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.host.events.clear();
        e.plugin.on_plugins_ready(&mut e.host);
        assert_eq!(e.phones_changed(), vec![1]);
        assert!(e.host.events.iter().any(|ev| matches!(ev, Event::SourceSelected { source_id } if source_id.is_empty())));
    }

    #[test]
    fn rename_updates_the_picker() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.ui("header", "rename", vec![json!("id-a"), json!("  Work phone ")]);
        assert_eq!(e.props("header")["names"], json!(["Work phone"]));
    }

    #[test]
    fn rename_dialog_line_edit_has_a_length_cap() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.ui("header", "rename", vec![json!("id-a"), json!("x".repeat(500))]);
        let name = e.props("header")["names"][0].as_str().unwrap().to_string();
        assert_eq!(name.chars().count(), crate::util::MAX_NAME_CHARS);
    }

    #[test]
    fn computer_name_can_be_renamed_from_the_phones_dialog() {
        let mut e = env();
        e.ui("header", "manage", vec![]);
        e.ui("header", "rename-computer", vec![json!("Studio PC")]);
        assert_eq!(e.plugin.get_config()["computer_name"], "Studio PC");
        assert_eq!(e.props("header")["computer-name"], "Studio PC");
    }

    #[test]
    fn phones_dialog_lists_renames_and_removes() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.ui("header", "manage", vec![]);
        let rows = e.props("header")["rows"].clone();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(rows[0]["sub"], "Phone · 10.0.0.5");
        assert!(rows[0]["confirm"].as_str().unwrap().starts_with("Remove \"Pixel\"?"));
        assert_eq!((&rows[0]["rename-label"], &rows[0]["remove-label"], &rows[0]["confirm-title"]),
                   (&json!("Rename Pixel"), &json!("Remove Pixel"), &json!("Remove phone")));
        e.ui("header", "remove", vec![json!("id-b")]);
        assert_eq!(e.props("header")["rows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn phones_dialog_lists_remembered_browsers_to_remove() {
        let mut e = env();
        let browser = RememberedSource { id: "browser:1".into(), name: "Laptop browser".into(), detail: "Browser".into() };
        e.plugin.on_event(&mut e.host, &Event::RememberedSources { sources: vec![browser] });
        e.ui("header", "manage", vec![]);
        assert_eq!(e.props("header")["rows"][0]["name"], "Laptop browser");
        e.ui("header", "remove", vec![json!("browser:1")]);
        assert!(e.host.events.iter().any(|ev| matches!(ev, Event::ForgetSourceRequested { source_id } if source_id == "browser:1")));
    }

    #[test]
    fn forgetting_a_phone_revokes_it_and_drops_its_settings() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.plugin.forget_phone(&mut e.host, "id-a");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*e.link.unpaired.lock().unwrap(), vec!["id-a".to_string()]);
        assert!(e.called("forget_device_settings id-a"));
        assert!(e.shared.phones().is_empty());
    }

    #[test]
    fn removing_a_phone_deletes_its_stored_settings() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.ui("header", "remove", vec![json!("id-a")]);
        assert!(e.called("forget_device_settings id-a"));
        assert_eq!(e.plugin.get_config()["phones"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn forgetting_the_phone_being_streamed_stops_it_while_it_can_still_be_reached() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.plugin.on_stream_start(&mut e.host, &StreamInfo { source_id: "id-a".into(), url: "https://10.0.0.5:8080/v1/video.h264".into() });
        e.plugin.forget_phone(&mut e.host, "id-a");
        let stop = e.host.calls.iter().position(|c| c == "stop_stream None").unwrap();
        let forget = e.host.calls.iter().position(|c| c == "forget_device_settings id-a").unwrap();
        assert!(stop < forget);
    }

    #[test]
    fn card_says_connecting_until_the_first_frame() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.shared.conn().set_stream_route(Some(wifi()));
        e.host.focused = Some("id-a".into());
        e.plugin.on_stream_start(&mut e.host, &StreamInfo { source_id: "id-a".into(), url: "https://10.0.0.5:8080/v1/video.h264".into() });
        assert_eq!(e.status(), "Connecting…");
        e.plugin.on_event(&mut e.host, &Event::StreamConnected { source_id: "id-a".into() });
        assert_eq!(e.status(), "Streaming over Wi-Fi");
        e.plugin.on_event(&mut e.host, &Event::StreamLost { source_id: "id-a".into() });
        assert_eq!(e.status(), "Reconnecting…");
    }

    #[test]
    fn start_without_a_phone_explains_and_refuses() {
        let mut e = env();
        let link = e.link.clone();
        assert!(e.shared.start_route(None, Default::default(), &|p, pref| link.check(p, pref)).is_err());
        e.settle();
        assert!(e.called("show_issue start No phone yet"));
    }

    #[test]
    fn pairing_a_phone_takes_down_the_no_phone_banner() {
        let mut e = env();
        let link = e.link.clone();
        let _ = e.shared.start_route(None, Default::default(), &|p, pref| link.check(p, pref));
        e.settle();
        e.add("id-a", "Pixel");
        assert!(e.called("clear_issue Some(\"start\")"));
    }

    #[test]
    fn start_on_an_unreachable_phone_shows_the_fix() {
        let mut e = env();
        e.add("id-a", "Pixel");
        let link = e.link.clone();
        assert!(e.shared.start_route(None, Default::default(), &|p, pref| link.check(p, pref)).is_err());
        e.settle();
        assert!(e.called("show_issue start Can't connect to the phone"));
    }

    #[test]
    fn the_help_button_asks_for_the_setup_guide() {
        let mut e = env();
        e.ui("card", "guide", vec![]);
        assert!(e.host.events.contains(&Event::SetupGuideRequested));
    }

    #[test]
    fn add_phone_request_on_the_bus_opens_pairing() {
        let mut e = env();
        e.plugin.on_event(&mut e.host, &Event::AddPhoneRequested);
        assert_eq!(e.props("card")["add-open"], true);
        assert!(e.props("card")["qr-size"].as_u64().unwrap() > 0);
        assert!(e.host.is_scheduled(T_PAIR_POLL));
    }

    #[test]
    fn a_phone_that_scans_the_code_is_added_and_the_dialog_says_so() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        let offer = e.link.offer.lock().unwrap().clone().unwrap();
        *offer.paired.lock().unwrap() = Some(phone("id-a", "Pixel"));
        e.plugin.on_event(&mut e.host, &Event::TimerFired { owner: NAME.into(), token: T_PAIR_POLL.into() });
        e.settle();
        assert_eq!(e.props("card")["paired"], true);
        assert_eq!(e.props("card")["result-text"], "Paired with Pixel");
        assert_eq!(e.shared.phones().len(), 1);
        e.ui("card", "add-closed", vec![]);
        assert_eq!(e.props("card")["add-open"], false);
    }

    #[test]
    fn plugged_in_phones_get_a_pair_button_each() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        let states = json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}, {"serial": "s2", "state": "unauthorized", "model": ""}]);
        e.plugin.on_event(&mut e.host, &Event::Posted { owner: NAME.into(), token: P_USB_STATE.into(), data: states.clone() });
        assert_eq!(e.props("card")["picks"], json!([{"serial": "s1", "label": "Pair Pixel 8", "enabled": true}]));
        assert_eq!(e.props("card")["usb-text"], "Pick a phone below to pair it over USB.");
        e.ui("card", "pick", vec![json!("s1")]);
        assert_eq!(e.props("card")["usb-text"], "Pairing Pixel 8. Keep Telescope open on it until it's done.");
        let only_unauthorized = json!([{"serial": "s2", "state": "unauthorized", "model": ""}]);
        e.plugin.on_event(&mut e.host, &Event::Posted { owner: NAME.into(), token: P_USB_STATE.into(), data: only_unauthorized });
        assert_eq!(e.props("card")["usb-kind"], "status_warn");
    }

    #[test]
    fn a_newer_phone_app_points_at_this_app_s_update() {
        let mut e = env();
        e.link.answer(Resolution::new(PhoneStatus::DesktopOutdated));
        e.add("id-a", "Pixel");
        assert_eq!(e.props("card")["update-shown"], true);
        assert_eq!(e.props("card")["update-text"], "Update this app");
        e.ui("card", "update", vec![]);
        assert!(e.host.events.contains(&Event::UpdateRequested));
    }

    #[test]
    fn a_problem_shows_its_fix_on_the_card() {
        let mut e = env();
        e.link.answer(Resolution { route: Some(usb()), ..Resolution::new(PhoneStatus::PhoneOutdated) });
        e.add("id-a", "Pixel");
        assert_eq!(e.props("card")["update-text"], "Update over USB");
        assert_eq!(e.props("card")["note-text"], "The Telescope app on Pixel is older than this one.");
    }

    #[test]
    fn a_failed_phone_update_says_why() {
        let mut e = env();
        e.link.answer(Resolution { route: Some(usb()), ..Resolution::new(PhoneStatus::PhoneOutdated) });
        *e.link.install.lock().unwrap() = Err("The phone said no".into());
        e.add("id-a", "Pixel");
        e.ui("card", "update", vec![]);
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert_eq!(e.props("card")["note-text"], "The phone said no");
        assert_eq!(e.props("card")["note-kind"], "status_err");
    }

    #[test]
    fn a_phone_update_that_blows_up_still_reports_back() {
        let mut e = env();
        e.link.answer(Resolution { route: Some(usb()), ..Resolution::new(PhoneStatus::PhoneOutdated) });
        *e.link.install_panics.lock().unwrap() = true;
        e.add("id-a", "Pixel");
        e.ui("card", "update", vec![]);
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert_eq!(e.props("card")["note-text"], "Couldn't update the phone app: no space left");
        assert!(!e.plugin.updating_phone);
    }

    #[test]
    fn sources_changing_while_the_picker_is_open_wait_for_it_to_close() {
        let mut e = env();
        let source = |id: &str, name: &str| telescope_core::event::SourceName { id: id.into(), name: name.into() };
        e.add("id-a", "Pixel");
        e.plugin.on_event(&mut e.host, &Event::StreamSourcesChanged { sources: vec![source("webcam:a", "HD Webcam")] });
        e.ui("header", "picker-open", vec![json!(true)]);
        e.plugin.on_event(&mut e.host, &Event::StreamSourcesChanged { sources: vec![source("webcam:b", "Brio")] });
        // The open list isn't rebuilt under the pointer, and the unplugged one can't be picked
        assert_eq!(e.props("header")["names"], json!(["Pixel", "HD Webcam"]));
        assert_eq!(e.props("header")["gone"], json!([false, true]));

        e.ui("header", "picker-open", vec![json!(false)]);
        assert_eq!(e.props("header")["names"], json!(["Pixel", "Brio"]));
        assert_eq!(e.props("header")["gone"], json!([]));
        assert_eq!(e.props("header")["current"], json!(0));
    }

    fn browser_camera_offered(e: &mut Env) {
        let sources = vec![telescope_core::event::SourceName { id: "browser".into(), name: "Browser camera".into() }];
        e.plugin.on_event(&mut e.host, &Event::StreamSourcesChanged { sources });
    }

    fn picked(e: &Env) -> Option<String> {
        e.host.events.iter().rev().find_map(|ev| match ev { Event::SourceSelected { source_id } => Some(source_id.clone()), _ => None })
    }

    // test_browser_camera.py::test_sources_are_listed_after_the_phones_and_can_be_picked
    #[test]
    fn sources_are_listed_after_the_phones_and_can_be_picked() {
        let mut e = env();
        e.add("id-a", "Pixel");
        browser_camera_offered(&mut e);
        assert_eq!(e.props("header")["names"], json!(["Pixel", "Browser camera"]));

        e.ui("header", "picked", vec![json!(1)]);
        assert_eq!(e.shared.picked_source().as_deref(), Some("browser")); // its settings are kept apart from the phone's
        assert!(e.called("switch_device Some(\"id-a\") Some(\"browser\")"));
        assert_eq!(picked(&e).as_deref(), Some("browser"));
        assert!(e.status().contains("no phone needed"), "{}", e.status());

        e.ui("header", "picked", vec![json!(0)]);
        assert_eq!(picked(&e).as_deref(), Some(""));
    }

    // test_browser_camera.py::test_a_saved_source_selection_is_restored
    #[test]
    fn a_saved_source_selection_is_restored() {
        let mut e = env();
        browser_camera_offered(&mut e);
        e.add("id-a", "Pixel");
        let mut cfg = e.plugin.get_config();
        cfg["selected_phone"] = json!("browser");
        e.plugin.set_config(&cfg).unwrap();
        e.settle();
        assert_eq!(e.shared.picked_source().as_deref(), Some("browser"));
    }

    #[test]
    fn picking_another_phone_while_idle_does_not_start_it() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.ui("header", "picked", vec![json!(0)]);
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-a"));
        assert!(!e.called("start_stream"));
    }

    #[test]
    fn picking_another_phone_by_hand_mid_stream_streams_that_one() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.plugin.on_stream_start(&mut e.host, &StreamInfo { source_id: "id-b".into(), url: "u".into() });
        e.ui("header", "picked", vec![json!(0)]);
        assert!(e.called("stop_stream None") && e.called("start_stream true"));
    }

    #[test]
    fn switching_phones_mid_stream_stops_the_stream() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.plugin.on_stream_start(&mut e.host, &StreamInfo { source_id: "id-b".into(), url: "u".into() });
        e.plugin.select(&mut e.host, Some("id-a"), false, false);
        assert!(e.called("stop_stream None") && !e.called("start_stream"));
    }

    #[test]
    fn selecting_another_phone_switches_its_settings_profile() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        assert!(e.called("switch_device Some(\"id-a\") Some(\"id-b\")"));
        e.ui("header", "picked", vec![json!(0)]);
        assert!(e.called("switch_device Some(\"id-b\") Some(\"id-a\")"));
    }

    #[test]
    fn phones_and_selection_survive_a_restart() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        let saved = e.plugin.get_config();
        let mut again = env();
        again.plugin.set_config(&saved).unwrap();
        again.plugin.on_plugins_ready(&mut again.host);
        assert_eq!(again.shared.picked_source().as_deref(), Some("id-b"));
        assert_eq!(again.props("header")["names"], json!(["Pixel", "Galaxy"]));
        assert_eq!(again.plugin.get_config()["computer_id"], saved["computer_id"]);
    }

    #[test]
    fn pairing_while_the_shown_card_streams_leaves_the_stream_alone() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.host.streaming = true;
        e.host.focused = Some("id-a".into());
        e.add("id-b", "Galaxy");
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-a"));
        assert!(!e.called("stop_stream"));
    }

    #[test]
    fn pairing_while_idle_still_picks_the_new_phone() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-b"));
    }

    #[test]
    fn saying_no_to_a_reinstalled_phone_adds_it_next_to_the_old_one() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        e.link.answer(Resolution::new(PhoneStatus::NotPaired));
        e.add("id-c", "Pixel");
        assert!(e.called("show_issue same_phone Same phone?"));
        e.plugin.on_issue_action(&mut e.host, "same_phone", "same_phone:no");
        e.settle();
        assert_eq!(e.shared.phones().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["id-a", "id-b", "id-c"]);
    }

    #[test]
    fn a_reinstalled_phone_that_was_picked_keeps_the_settings_on_screen() {
        let mut e = env();
        e.link.answer(Resolution::new(PhoneStatus::NotPaired));
        e.add("id-a", "Pixel");
        e.add("id-c", "Pixel");
        e.plugin.on_issue_action(&mut e.host, "same_phone", "same_phone:yes");
        e.settle();
        assert!(e.called("move_device_settings id-a id-c"));
        assert_eq!(e.shared.phones().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["id-c"]);
        // The panels held id-a's settings, which are id-c's now: no switch back and forth
        assert!(!e.called("switch_device Some(\"id-c\")"));
    }

    #[test]
    fn a_new_connect_via_is_offered_back_when_the_phone_cant_be_reached_that_way() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.plugin.on_stream_start(&mut e.host, &StreamInfo { source_id: "id-a".into(), url: "u".into() });
        e.host.streaming = true;
        e.host.focused = Some("id-a".into());
        e.link.answer(Resolution::new(PhoneStatus::Unreachable));
        e.ui("card", "route-picked", vec![json!(1)]);
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert!(e.called("show_issue route Can't reach Pixel over USB"));
        assert_eq!(e.plugin.get_config()["route"], "auto");
    }

    #[test]
    fn the_udp_switch_saves_and_lives_in_the_streaming_tab() {
        let mut e = env();
        let sections = e.plugin.settings_sections();
        assert_eq!((sections[0].tab.as_str(), sections[0].ui.component.as_str()), ("streaming", "ConnectionSettings"));
        e.ui("udp", "udp-toggled", vec![json!(true)]);
        assert_eq!(e.plugin.get_config()["udp"], true);
        assert!(e.called("schedule_save"));
    }

    #[test]
    fn the_phone_shortcut_picks_the_next_phone() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        assert_eq!(e.plugin.actions()[0].id, "phone.pick");
        assert_eq!(e.plugin.run_action(&mut e.host, "phone.pick", &json!({"op": "next"})).as_deref(), Some("Phone: Pixel"));
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-a"));
        assert_eq!(e.plugin.run_action(&mut e.host, "phone.pick", &json!({"op": "set", "option": "Galaxy"})).as_deref(), Some("Phone: Galaxy"));
    }

    #[test]
    fn moving_the_panels_to_another_running_stream_shows_it_as_picked() {
        let mut e = env();
        e.add("id-a", "Pixel");
        e.add("id-b", "Galaxy");
        let switches = e.host.calls.iter().filter(|c| c.starts_with("switch_device")).count();
        e.plugin.on_focus_gained(&mut e.host, &StreamInfo { source_id: "id-a".into(), url: "u".into() });
        assert_eq!(e.shared.picked_source().as_deref(), Some("id-a"));
        // The window swapped the settings itself
        assert_eq!(e.host.calls.iter().filter(|c| c.starts_with("switch_device")).count(), switches);
    }

    #[test]
    fn the_status_poll_keeps_itself_going() {
        let mut e = env();
        assert!(e.host.is_scheduled(T_POLL));
        e.host.scheduled.clear();
        e.plugin.on_event(&mut e.host, &Event::TimerFired { owner: NAME.into(), token: T_POLL.into() });
        assert!(e.host.is_scheduled(T_POLL));
    }

    fn usb_state(e: &mut Env, states: Value) {
        e.plugin.on_event(&mut e.host, &Event::Posted { owner: NAME.into(), token: P_USB_STATE.into(), data: states });
    }

    fn offer(e: &Env) -> Arc<FakeOffer> {
        e.link.offer.lock().unwrap().clone().unwrap()
    }

    fn offered(e: &Env) -> Vec<String> {
        std::thread::sleep(Duration::from_millis(50));
        offer(e).offered.lock().unwrap().clone()
    }

    #[test]
    fn closing_stops_the_pairing_server() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        let held = offer(&e);
        e.ui("card", "add-closed", vec![]);
        // Only the test holds the offer now: dropping it is what stops the server
        assert_eq!(Arc::strong_count(&held), 2);
        *e.link.offer.lock().unwrap() = None;
        assert_eq!(Arc::strong_count(&held), 1);
        assert!(!e.host.is_scheduled(T_PAIR_POLL) && !e.host.is_scheduled(T_PAIR_USB));
    }

    #[test]
    fn with_several_phones_plugged_in_only_the_picked_one_pairs() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        let two = json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}, {"serial": "s2", "state": "device", "model": "Galaxy"}]);
        usb_state(&mut e, two.clone());
        assert!(offered(&e).is_empty()); // a phone left plugged in never pairs by itself
        e.ui("card", "pick", vec![json!("s2")]);
        assert_eq!(offered(&e), vec!["s2".to_string()]);
        assert_eq!(e.props("card")["picks"][1]["enabled"], false);
    }

    #[test]
    fn a_picked_phone_gets_the_pairing_offer_over_usb() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        usb_state(&mut e, json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}]));
        e.ui("card", "pick", vec![json!("s1")]);
        // Sent again only after a few seconds: the next poll doesn't repeat it straight away
        usb_state(&mut e, json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}]));
        assert_eq!(offered(&e), vec!["s1".to_string()]);
    }

    #[test]
    fn unplugging_the_picked_phone_forgets_the_pick() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        usb_state(&mut e, json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}]));
        e.ui("card", "pick", vec![json!("s1")]);
        usb_state(&mut e, json!([]));
        assert_eq!(e.props("card")["usb-text"], "Or plug the phone in with a USB cable to pair it over USB.");
        usb_state(&mut e, json!([{"serial": "s1", "state": "device", "model": "Pixel 8"}]));
        assert_eq!(e.props("card")["usb-text"], "Pick a phone below to pair it over USB.");
    }

    #[test]
    fn a_scanned_phone_pairs_while_another_is_plugged_in() {
        let mut e = env();
        e.ui("card", "add-phone", vec![]);
        usb_state(&mut e, json!([{"serial": "s1", "state": "device", "model": "Galaxy"}]));
        *offer(&e).paired.lock().unwrap() = Some(phone("id-a", "Pixel"));
        e.plugin.on_event(&mut e.host, &Event::TimerFired { owner: NAME.into(), token: T_PAIR_POLL.into() });
        e.settle();
        assert_eq!(e.props("card")["result-text"], "Paired with Pixel");
        assert!(offered(&e).is_empty());
    }

    #[test]
    fn without_a_network_it_steers_to_usb() {
        let mut e = env();
        *e.link.offline.lock().unwrap() = true;
        e.ui("card", "add-phone", vec![]);
        assert_eq!(e.props("card")["wifi-text"], "This computer isn't on a network right now, so pair over USB.");
        assert_eq!(e.props("card")["qr-size"], 0);
    }

    #[test]
    fn without_adb_usb_says_so() {
        let mut e = env();
        *e.link.adb.lock().unwrap() = false;
        e.ui("card", "add-phone", vec![]);
        let text = e.props("card")["usb-text"].as_str().unwrap().to_string();
        assert!(text.starts_with("Needs adb"), "{text}");
        assert!(!e.host.is_scheduled(T_PAIR_USB));
    }

    #[test]
    fn a_failed_adb_download_says_why_and_offers_another_go() {
        let mut e = env();
        *e.link.adb.lock().unwrap() = false;
        e.ui("card", "add-phone", vec![]);
        e.plugin.on_event(&mut e.host, &Event::Posted { owner: NAME.into(), token: P_ADB.into(),
                                                         data: json!({"ok": false, "detail": "Couldn't reach Google."}) });
        assert_eq!(e.props("card")["usb-text"], "Couldn't reach Google.");
        assert_eq!(e.props("card")["usb-kind"], "status_err");
        assert_eq!(e.props("card")["usb-action"], "Try again");
        assert_eq!(e.props("card")["usb-pick-action"], "Pick one you already have");

        // An adb.exe the person has already, which doesn't work out here
        e.ui("card", "usb-pick-clicked", vec![]);
        assert!(e.called("pick_file Open Pick adb.exe or the platform-tools zip"));
        let request = e.plugin.adb_asked.unwrap();
        e.plugin.on_event(&mut e.host, &Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": "/nonexistent/adb.exe"}) });
        assert!(e.host.calls.iter().any(|c| c.contains("Copying adb…")));
        assert_eq!(e.props("card")["usb-action"], "");
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert_eq!(e.props("card")["usb-kind"], "status_err");
    }

    #[test]
    fn links_become_plain_text_and_button_labels() {
        let text = "Needs adb, which Telescope can <a href=\"{link}\">get from Google</a>. Wi-Fi works without it.";
        assert_eq!(without_links(text), "Needs adb, which Telescope can get from Google. Wi-Fi works without it.");
        assert_eq!(link_label(text, "{link}").as_deref(), Some("get from Google"));
        assert_eq!(link_label(text, "{pick}"), None);
    }

    fn held_downloader() -> (adb_download::AdbDownloader, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let rx = Mutex::new(rx);
        let downloader = adb_download::AdbDownloader::new(Arc::new(move |_say| {
            rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).ok();
            Ok("37.0.1".into())
        }));
        (downloader, tx)
    }

    #[test]
    fn on_windows_without_adb_it_offers_to_get_it_and_then_pairs_over_usb() {
        let link = FakeLink::new();
        *link.adb.lock().unwrap() = false;
        let shared = Shared::new(Arc::new(NoTunnels));
        let (downloader, release) = held_downloader();
        let mut e = Env { plugin: ConnectionPlugin::new(link.clone(), shared.clone()).with_downloader(downloader), host: FakeHost::new(), link, shared };
        e.plugin.setup(&mut e.host);
        e.ui("card", "add-phone", vec![]);
        e.ui("card", "usb-action-clicked", vec![]);
        assert_eq!(e.props("card")["usb-action"], "Agree and download");
        assert!(e.props("card")["usb-text"].as_str().unwrap().contains("Android SDK License"));
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "usb-action-clicked".into(), args: vec![] });
        assert_eq!(e.props("card")["usb-text"], "Getting adb from Google…");
        *e.link.adb.lock().unwrap() = true;
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert!(e.host.is_scheduled(T_PAIR_USB));
        assert_eq!(e.props("card")["usb-text"], "Or plug the phone in with a USB cable to pair it over USB.");
    }

    #[test]
    fn reopened_mid_download_it_says_so_instead_of_offering_it_again() {
        let link = FakeLink::new();
        *link.adb.lock().unwrap() = false;
        let shared = Shared::new(Arc::new(NoTunnels));
        let (downloader, release) = held_downloader();
        let mut e = Env { plugin: ConnectionPlugin::new(link.clone(), shared.clone()).with_downloader(downloader), host: FakeHost::new(), link, shared };
        e.plugin.setup(&mut e.host);
        e.ui("card", "add-phone", vec![]);
        e.ui("card", "usb-action-clicked", vec![]);
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "usb-action-clicked".into(), args: vec![] });
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "add-closed".into(), args: vec![] });
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "add-phone".into(), args: vec![] });
        assert_eq!(e.props("card")["usb-text"], "Getting adb from Google…");
        assert_eq!(e.props("card")["usb-action"], "");
        release.send(()).unwrap();
    }

    #[test]
    fn an_adb_download_that_ends_after_closing_doesn_t_poll_usb() {
        let link = FakeLink::new();
        *link.adb.lock().unwrap() = false;
        let shared = Shared::new(Arc::new(NoTunnels));
        let (downloader, release) = held_downloader();
        let mut e = Env { plugin: ConnectionPlugin::new(link.clone(), shared.clone()).with_downloader(downloader), host: FakeHost::new(), link, shared };
        e.plugin.setup(&mut e.host);
        e.ui("card", "add-phone", vec![]);
        e.ui("card", "usb-action-clicked", vec![]);
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "usb-action-clicked".into(), args: vec![] });
        e.plugin.on_ui(&mut e.host, &UiEvent { ui: "card".into(), callback: "add-closed".into(), args: vec![] });
        *e.link.adb.lock().unwrap() = true;
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        e.settle();
        assert!(!e.host.is_scheduled(T_PAIR_USB));
    }

    #[test]
    fn qr_widget_builds_matrix_and_renders() {
        let (cells, size) = qr("{\"v\":4,\"nonce\":\"abc\"}");
        assert!(size >= 21 && !cells.is_empty() && cells.iter().all(|&i| i < size * size));
        // The finder pattern's corner is dark
        assert!(cells.contains(&0));
    }

    /// Counts forwards and says which thread opened them.
    #[derive(Default)]
    struct WatchedTunnels {
        seen: Mutex<Vec<String>>,
        test_thread: Option<std::thread::ThreadId>,
    }

    impl Tunnels for WatchedTunnels {
        fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
            let on_ui = Some(std::thread::current().id()) == self.test_thread;
            self.seen.lock().unwrap().push(format!("acquire on_ui={on_ui}"));
            Some(41000)
        }
        fn release(&self, _serial: &str, _remote_port: u16) {
            self.seen.lock().unwrap().push("release".into());
        }
    }

    #[test]
    fn plugging_in_while_streaming_opens_the_usb_forward_off_the_ui_thread() {
        let tunnels = Arc::new(WatchedTunnels { test_thread: Some(std::thread::current().id()), ..Default::default() });
        let link = FakeLink::new();
        let shared = Shared::new(tunnels.clone());
        let mut plugin = ConnectionPlugin::new(link.clone(), shared.clone());
        let mut host = FakeHost::new();
        plugin.setup(&mut host);
        let mut e = Env { plugin, host, link, shared };
        e.add("id-a", "Pixel");
        let watch_id = {
            let mut c = e.shared.conn();
            c.streaming = true;
            c.watch_usb().unwrap().0
        };
        let data = json!({"watch_id": watch_id, "route": usb()});
        e.plugin.on_event(&mut e.host, &Event::Posted { owner: NAME.into(), token: P_USB.into(), data });
        assert!(!e.called("hand_over_stream"), "waits for the forward first");
        e.settle();
        std::thread::sleep(Duration::from_millis(50));
        assert!(e.called("hand_over_stream"));
        assert_eq!(*tunnels.seen.lock().unwrap(), ["acquire on_ui=false", "release"]);
        // FakeHost refuses the hand-over, so the button shows to try by hand
        assert!(e.shared.conn().switch_usb_visible);
    }
}
