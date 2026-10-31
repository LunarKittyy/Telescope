//! The On Air plugin (plugins/on_air.py): lets the people you live with know you're on a call. What counts as on air is picked in
//! Settings: an app reading the virtual camera (camera_watched, the default), that or Telescope's mic being live, the mic alone, or
//! just streaming. The answer is debounced and then goes to a sign page another device keeps open and to a webhook, either or both.
//! camera_watched comes from Wait screen; without it this plugin watches the camera itself.

use telescope_core::i18n::Fields;
use crate::on_air::{self, Debounce, Sign, WebhookRequest, WebhookSender, DEFAULT_TRIGGER, METHODS, ON_AIR_PORT, RETRY_S, TRIGGERS};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::output::{Held, Outputs};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{CardSpec, Issue, IssueAction, IssueDoes, SettingsSection, SlintSource, UiEvent, UiSpec};
use telescope_core::web::{Serve, Web, WebServer};

pub const NAME: &str = "on_air";
/// The Settings tab, which is also the name of its one section.
pub const TAB: &str = "on_air";
pub const ISSUE: &str = "on_air_webhook";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/on_air.slint"));
const HEADER_UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/on_air_header.slint"));

const T_SETTLE: &str = "settle";
const T_RETRY: &str = "retry";
const POST_WATCHED: &str = "watched";
const POST_WEBHOOK: &str = "webhook";
/// Quitting waits this long for the "off" to reach the light.
const QUIT_WAIT: Duration = Duration::from_secs(3);

const TRIGGER_TEXTS: [(&str, &str); 4] = [
    ("camera", n_("An app is using the camera")),
    ("camera_mic", n_("An app is using the camera or the mic is live")),
    ("mic", n_("Telescope's mic is live")),
    ("streaming", n_("Telescope is streaming")),
];
/// Translators: Show Details, visit this website, Advanced and Proceed are what Safari and Chrome call their buttons:
/// use the names those browsers show in your language
const CERT_NOTE: &str = n_("The browser warns that the connection isn't private: it's this computer's own certificate. \
                            On iPhone tap Show Details, then visit this website. In Chrome tap Advanced, then Proceed.");
const TRIGGER_TIP: &str = n_("The mic counts while it streams and isn't muted");
const ADDRESS_TIP: &str = n_("Which of this computer's addresses the link points to");
const COPY_TIP: &str = n_("Copy the link, to open it on a device that can't scan the code");
const NEW_LINK_TIP: &str = n_("Make a new link; the old one stops working");
const PAYLOAD_TIP: &str = n_("POST sends it as the body, GET adds it to the address");
const HEADER_TIP: &str = n_("Open the On Air settings");
const PAGE_NOTE: &str = n_("Open the link in a browser on any device on the same network and tap the page to make it full screen. \
                            The link stays the same after Telescope restarts.");

/// secrets.token_urlsafe(18): 24 characters of the URL-safe alphabet.
fn new_token() -> String {
    use base64::Engine;
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 18];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // No randomness from the system: the clock and the process make a link nobody could guess from outside the house
        let seed = monotonic_ns() ^ ((std::process::id() as u64) << 32);
        bytes.iter_mut().enumerate().for_each(|(i, b)| *b = (seed.rotate_left(i as u32 * 7) >> 3) as u8);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A link's token as the plugin takes it back: 16 to 64 characters of the URL-safe alphabet.
fn token_ok(token: &str) -> bool {
    (16..=64).contains(&token.chars().count()) && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub struct OnAirPlugin {
    web: Arc<dyn Web>,
    outputs: Option<Arc<dyn Outputs>>,
    /// Where the certificate is kept; the settings folder when None (tests give a scratch one).
    cert_folder: Option<PathBuf>,
    clock: Box<dyn Fn() -> f64 + Send>,
    send: Arc<dyn Fn(&WebhookRequest) -> String + Send + Sync>,

    trigger: String,
    page: bool,
    token: String,
    address: String,
    webhook_url: String,
    webhook_method: String,
    webhook_on: String,
    webhook_off: String,

    watched: bool,
    mic: (bool, bool),
    debounce: Debounce,
    sign: Option<Arc<Sign>>,
    server: Option<Box<dyn WebServer>>,
    /// Why the sign page isn't up.
    problem: String,
    /// Our own camera watch, only without Wait screen.
    watch: Option<Box<dyn Held>>,
    webhook_failing: bool,
    /// Moves on with each change, so a late answer about an older one is ignored.
    state_gen: u64,
    retries: usize,
    /// A retry is waiting for its time.
    retrying: bool,
    /// The text and status kind of the last Send test.
    test_result: (String, &'static str),
    sender: Option<WebhookSender>,
    poster: Option<Arc<dyn Poster>>,
    /// The link the QR code in the section was drawn for.
    qr_url: String,
    /// set_config has no host: the section hears the settings, and a token made fresh is saved, with the next hook that has one.
    pending: bool,
    save_token: bool,
}

impl OnAirPlugin {
    /// `outputs` is what the camera is watched through when Wait screen isn't there to say (None: not at all).
    pub fn new(web: Arc<dyn Web>, outputs: Option<Arc<dyn Outputs>>) -> OnAirPlugin {
        OnAirPlugin {
            web,
            outputs,
            cert_folder: None,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
            send: Arc::new(on_air::send_webhook),
            trigger: DEFAULT_TRIGGER.into(),
            page: true,
            token: new_token(),
            address: String::new(),
            webhook_url: String::new(),
            webhook_method: "POST".into(),
            webhook_on: String::new(),
            webhook_off: String::new(),
            watched: false,
            mic: (false, false),
            debounce: Debounce::default(),
            sign: None,
            server: None,
            problem: String::new(),
            watch: None,
            webhook_failing: false,
            state_gen: 0,
            retries: 0,
            retrying: false,
            test_result: (String::new(), ""),
            sender: None,
            poster: None,
            qr_url: String::new(),
            pending: false,
            save_token: false,
        }
    }

    /// For tests: a scratch folder for the certificate, a clock of their own and a way to send that never leaves the machine.
    pub fn for_test(web: Arc<dyn Web>, outputs: Option<Arc<dyn Outputs>>, folder: PathBuf, clock: impl Fn() -> f64 + Send + 'static,
                    send: impl Fn(&WebhookRequest) -> String + Send + Sync + 'static) -> OnAirPlugin {
        OnAirPlugin { cert_folder: Some(folder), clock: Box::new(clock), send: Arc::new(send), ..OnAirPlugin::new(web, outputs) }
    }

    // ── Whether we're on air ──────────────────────────────────────────

    pub fn on(&self) -> bool {
        self.debounce.state
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn on_watched(&mut self, host: &mut dyn Host, watched: bool) {
        self.watched = watched;
        self.evaluate(host);
    }

    fn evaluate(&mut self, host: &mut dyn Host) {
        let raw = on_air::is_on_air(&self.trigger, self.watched, host.is_streaming(), self.mic.0, self.mic.1);
        let now = self.now();
        self.debounce.feed(raw, now);
        self.settle(host);
    }

    fn settle(&mut self, host: &mut dyn Host) {
        let now = self.now();
        if self.debounce.settle(now) {
            self.changed(host);
        }
        match self.debounce.due_in(now) {
            None => host.cancel_schedule(T_SETTLE),
            Some(left) => host.schedule((left * 1000.0) as u64 + 20, T_SETTLE),
        }
    }

    fn changed(&mut self, host: &mut dyn Host) {
        log::info!("On Air: {}", if self.on() { "on" } else { "off" });
        self.publish();
        self.send_state(host, true);
        self.refresh(host);
    }

    fn publish(&self) {
        if let Some(sign) = &self.sign {
            sign.publish(json!({"on": self.on(), "text": if self.on() { tr("On air") } else { tr("Off air") }, "title": tr("On Air")}));
        }
    }

    // ── Webhook ───────────────────────────────────────────────────────

    pub fn webhook_ready(&self) -> bool {
        !self.webhook_url.is_empty() && on_air::webhook_problem(&self.webhook_url).is_empty()
    }

    fn request(&self, on: bool) -> WebhookRequest {
        let payload = if on { &self.webhook_on } else { &self.webhook_off };
        on_air::webhook_request(&self.webhook_url, &self.webhook_method, payload, on)
    }

    /// Sends what's true now; `fresh`: a change, which starts the retries over.
    fn send_state(&mut self, host: &mut dyn Host, fresh: bool) {
        if fresh {
            self.state_gen += 1;
            self.retries = 0;
            self.retrying = false;
            host.cancel_schedule(T_RETRY);
        }
        if self.webhook_ready() {
            if let Some(sender) = &self.sender {
                sender.send(self.request(self.on()), &self.state_tag(), Some("state"));
            }
        }
    }

    fn state_tag(&self) -> String {
        format!("state:{}", self.state_gen)
    }

    fn send_test(&mut self, host: &mut dyn Host, on: bool) {
        if !self.webhook_ready() {
            self.test_result = (tr("Enter an address starting with http:// or https:// first."), "status_err");
            self.refresh(host);
            return;
        }
        self.test_result = (tr("Sending…"), "status_dim");
        self.refresh(host);
        if let Some(sender) = &self.sender {
            sender.send(self.request(on), "test", None);
        }
    }

    fn on_webhook_done(&mut self, host: &mut dyn Host, tag: &str, error: &str) {
        let (kind, gen) = tag.split_once(':').unwrap_or((tag, ""));
        if kind == "state" && gen == self.state_gen.to_string() {
            self.state_done(host, error);
        }
        if tag == "test" {
            self.test_result = if error.is_empty() {
                (tr("It went through."), "status_ok")
            } else {
                (tr("It didn't go through: {error}").field("error", error), "status_err")
            };
        }
        self.refresh(host);
    }

    fn state_done(&mut self, host: &mut dyn Host, error: &str) {
        if error.is_empty() {
            self.retries = 0;
            self.retrying = false;
            host.cancel_schedule(T_RETRY);
            self.webhook_failing = false;
            host.clear_issue(Some(ISSUE), None);
            return;
        }
        if !self.webhook_failing {
            // One quiet banner per run of failures, however many changes follow
            self.webhook_failing = true;
            log::warn!("On Air webhook didn't go through: {error}");
            host.show_issue(ISSUE, Issue {
                title: tr("The On Air webhook didn't go through"),
                text: tr("It answered: {error}").field("error", error),
                actions: vec![IssueAction { label: tr("Settings"), does: IssueDoes::OpenSettings { tab: TAB.into() } }],
                kind: "warn".into(),
                details: String::new(),
                tell_dismiss: false,
            });
        }
        let delay = RETRY_S[self.retries.min(RETRY_S.len() - 1)];
        self.retries += 1;
        // Until it works or a change replaces it
        self.retrying = true;
        host.schedule(delay * 1000, T_RETRY);
    }

    // ── Sign page ─────────────────────────────────────────────────────

    fn sync_server(&mut self, host: &mut dyn Host) {
        if self.page && self.server.is_none() {
            self.start_server();
        } else if !self.page && self.server.is_some() {
            self.stop_server();
        }
        self.refresh(host);
    }

    fn start_server(&mut self) {
        self.problem.clear();
        self.fill_address();
        let folder = self.cert_folder.clone().or_else(|| telescope_core::config::config_path().parent().map(PathBuf::from)).unwrap_or_default();
        let started = self.web.certificate(&folder, &self.addresses()).and_then(|certificate| {
            let sign = Arc::new(Sign::new(&self.token));
            let serve = Serve { port: ON_AIR_PORT, host: String::new(), label: "On Air".into() };
            let server = self.web.serve(&certificate, &serve, sign.clone())?;
            Ok((sign, server))
        });
        match started {
            Ok((sign, server)) => {
                self.sign = Some(sign);
                self.server = Some(server);
                self.publish();
            }
            Err(error) => {
                log::error!("On Air sign server didn't start: {error}");
                self.problem = tr("Couldn't start: {error}").field("error", &error);
            }
        }
    }

    fn stop_server(&mut self) {
        if let Some(sign) = self.sign.take() {
            sign.stop();
        }
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }

    fn fill_address(&mut self) {
        let candidates = self.addresses();
        if !candidates.contains(&self.address) {
            self.address = candidates.first().cloned().unwrap_or_default();
        }
    }

    /// The network may have changed since the server started: the link points at an address this computer has.
    fn refresh_address(&mut self, host: &mut dyn Host) {
        let before = self.address.clone();
        self.fill_address();
        if self.address != before {
            host.schedule_save();
        }
    }

    pub fn addresses(&self) -> Vec<String> {
        self.web.addresses()
    }

    pub fn url(&self) -> String {
        match &self.server {
            Some(server) if !self.address.is_empty() => format!("https://{}:{}/#{}", self.address, server.port(), self.token),
            _ => String::new(),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The old link stops working: a sign still open on it says so.
    fn new_link(&mut self, host: &mut dyn Host) {
        self.token = new_token();
        if let Some(sign) = &self.sign {
            sign.set_token(&self.token);
        }
        host.schedule_save();
        self.refresh(host);
    }

    // ── Settings from the section ─────────────────────────────────────

    fn set_trigger(&mut self, host: &mut dyn Host, trigger: &str) {
        if TRIGGERS.contains(&trigger) && trigger != self.trigger {
            self.trigger = trigger.into();
            host.schedule_save();
            self.evaluate(host);
        }
    }

    fn set_page(&mut self, host: &mut dyn Host, on: bool) {
        if on != self.page {
            self.page = on;
            host.schedule_save();
            self.sync_server(host);
        }
    }

    fn set_address(&mut self, host: &mut dyn Host, ip: &str) {
        if !ip.is_empty() && ip != self.address {
            self.address = ip.into();
            host.schedule_save();
            self.refresh(host);
        }
    }

    fn set_webhook(&mut self, host: &mut dyn Host, url: Option<&str>, method: Option<&str>, on: Option<&str>, off: Option<&str>) {
        if let Some(url) = url {
            self.webhook_url = url.trim().into();
        }
        if let Some(method) = method.filter(|m| METHODS.contains(m)) {
            self.webhook_method = method.into();
        }
        if let Some(on) = on {
            self.webhook_on = on.into();
        }
        if let Some(off) = off {
            self.webhook_off = off.into();
        }
        if !self.webhook_ready() && (self.webhook_failing || self.retrying) {
            // Nothing left to send to: a failure from before can't keep its banner up
            self.retrying = false;
            host.cancel_schedule(T_RETRY);
            (self.retries, self.webhook_failing) = (0, false);
            host.clear_issue(Some(ISSUE), None);
        }
        host.schedule_save();
    }

    // ── UI ────────────────────────────────────────────────────────────

    fn url_problem(&self) -> String {
        let bad = !self.webhook_url.is_empty() && !on_air::webhook_problem(&self.webhook_url).is_empty();
        if bad { tr("The address has to start with http:// or https://") } else { String::new() }
    }

    fn placeholders(&self) -> (String, String) {
        (on_air::default_payload(&self.webhook_method, true), on_air::default_payload(&self.webhook_method, false))
    }

    fn trigger_texts() -> Vec<String> {
        TRIGGER_TEXTS.iter().map(|(_, t)| tr(t)).collect()
    }

    /// What doesn't change while it runs.
    fn fixed_props() -> Value {
        json!({
            "trigger-items": Self::trigger_texts(), "trigger-tip": tr(TRIGGER_TIP), "new-link-tip": tr(NEW_LINK_TIP),
            "page-name": tr("Show the sign on a web page"), "qr-label": tr("Scan"), "address-tip": tr(ADDRESS_TIP),
            "copy-text": tr("Copy"), "copy-tip": tr(COPY_TIP), "page-note": format!("{} {}", tr(PAGE_NOTE), tr(CERT_NOTE)),
            "payload-tip": tr(PAYLOAD_TIP),
        })
    }

    /// What follows the state: the section's refresh().
    fn state_props(&mut self, with_qr: bool) -> Value {
        let url = self.url();
        let problem = if self.page { self.problem.clone() } else { String::new() };
        let addresses = if url.is_empty() { Vec::new() } else { self.addresses() };
        let (on_placeholder, off_placeholder) = self.placeholders();
        let mut props = json!({
            "state-text": if self.on() { tr("On air") } else { tr("Off air") },
            "state-kind": if self.on() { "status_err" } else { "status_dim" },
            "trigger-current": TRIGGERS.iter().position(|t| *t == self.trigger).unwrap_or(0),
            "page": self.page, "problem": problem,
            "url-shown": !url.is_empty(), "url-text": url,
            "address-shown": addresses.len() > 1, "address-current": addresses.iter().position(|a| *a == self.address).unwrap_or(0),
            "addresses": addresses,
            "url-problem": self.url_problem(),
            "method-current": METHODS.iter().position(|m| *m == self.webhook_method).unwrap_or(0),
            "on-placeholder": on_placeholder, "off-placeholder": off_placeholder,
            "test-text": self.test_result.0, "test-kind": if self.test_result.1.is_empty() { "status_dim" } else { self.test_result.1 },
        });
        if with_qr || url != self.qr_url {
            let (cells, size) = if url.is_empty() { (Vec::new(), 0) } else { telescope_core::qr::encode(&url) };
            props["qr-cells"] = json!(cells);
            props["qr-size"] = json!(size);
            self.qr_url = url;
        }
        props
    }

    /// The settings a person types, which the boxes own once they're on screen: only sent when they were loaded.
    fn typed_props(&self) -> Value {
        json!({"webhook-url": self.webhook_url, "webhook-on": self.webhook_on, "webhook-off": self.webhook_off})
    }

    fn section_props(&mut self) -> Value {
        let mut all = Self::fixed_props();
        for part in [self.state_props(true), self.typed_props()] {
            if let (Value::Object(a), Value::Object(p)) = (&mut all, part) {
                a.extend(p);
            }
        }
        all
    }

    fn refresh(&mut self, host: &mut dyn Host) {
        host.set_ui_props("header", &json!({"shown": self.on()}));
        let props = self.state_props(false);
        host.set_ui_props(TAB, &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if std::mem::take(&mut self.save_token) {
            // The link made in setup has to survive a restart
            host.schedule_save();
        }
        if std::mem::take(&mut self.pending) {
            host.set_ui_props(TAB, &self.typed_props());
            self.refresh(host);
        }
    }
}

impl Plugin for OnAirPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("On Air sign").into(),
            about: n_("Shows the people around you that you're on a call, on another device's screen or through a smart home light.").into(),
            when_off: n_("Without it there's no On Air tab in Settings and no sign or webhook.").into(),
            category: "on_screen".into(),
            icon: "bell".into(),
            optional: true,
            header_side: "right".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        let poster = self.poster.clone();
        let send = self.send.clone();
        self.sender = Some(WebhookSender::with_send(
            move |tag, error| {
                if let Some(poster) = &poster {
                    poster.post(POST_WEBHOOK, json!({"tag": tag, "error": error}));
                }
            },
            move |request| send(request),
        ));
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        if host.plugin_config("wait_screen").is_none() && self.watch.is_none() {
            // Nobody else tells us about the camera
            if let (Some(outputs), Some(poster)) = (self.outputs.clone(), self.poster.clone()) {
                self.watch = outputs.watch(Box::new(move |watched| poster.post(POST_WATCHED, json!({"watched": watched}))));
                if let Some(watch) = &self.watch {
                    watch.start();
                }
            }
        }
        self.sync_server(host);
        self.evaluate(host);
        // The light may still show how the last run left it
        self.send_state(host, true);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        host.cancel_schedule(T_SETTLE);
        let retrying = std::mem::take(&mut self.retrying);
        host.cancel_schedule(T_RETRY);
        if (self.on() || retrying) && self.webhook_ready() {
            if let Some(sender) = &self.sender {
                // Behind anything still going out, so a late "on" can't land after it; quitting waits only so long
                sender.send(self.request(false), "quit", Some("state"));
                sender.wait_idle(QUIT_WAIT);
            }
        }
        self.stop_server();
        if let Some(watch) = self.watch.take() {
            watch.stop();
        }
        self.poster = None;
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::CameraWatched { watched } => self.on_watched(host, *watched),
            Event::MicChanged { enabled, muted } => {
                self.mic = (*enabled, *muted);
                self.evaluate(host);
            }
            Event::StreamStarted { .. } | Event::StreamStopped | Event::StreamsChanged { .. } => self.evaluate(host),
            Event::Posted { owner, token, data } if owner == NAME => match token.as_str() {
                POST_WATCHED => {
                    let watched = data.get("watched").and_then(Value::as_bool).unwrap_or(false);
                    self.on_watched(host, watched);
                }
                POST_WEBHOOK => {
                    let text = |key: &str| data.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                    self.on_webhook_done(host, &text("tag"), &text("error"));
                }
                _ => {}
            },
            Event::TimerFired { owner, token } if owner == NAME => match token.as_str() {
                T_SETTLE => self.settle(host),
                T_RETRY => {
                    self.retrying = false;
                    self.send_state(host, false);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        None
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec {
            source: SlintSource::Text(HEADER_UI.into()),
            component: "OnAirButton".into(),
            props: json!({"shown": self.on(), "tip": tr(HEADER_TIP)}),
        })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        let props = self.section_props();
        vec![SettingsSection {
            id: TAB.into(),
            tab: TAB.into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "OnAirSettings".into(), props },
            order: 0,
            tab_title: tr("On Air"),
            keywords: [
                tr_ctx("search", "on air sign busy light call meeting do not disturb webhook home assistant smart home"),
                tr_ctx("search", "tablet phone screen page link qr code"),
                tr_ctx("search", "smart home light home assistant url http request automation"),
            ].join(" "),
            footer: None,
        }]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let text = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        let index = |i: usize| event.args.get(i).and_then(Value::as_u64).unwrap_or(0) as usize;
        if event.ui == "header" {
            if event.callback == "clicked" {
                host.open_settings(TAB);
            }
            return;
        }
        if event.ui != TAB {
            return;
        }
        match event.callback.as_str() {
            // The window shows it: the network may have changed since the server started
            "shown" => {
                self.refresh_address(host);
                self.refresh(host);
            }
            "trigger-picked" => {
                if let Some(trigger) = TRIGGERS.get(index(0)) {
                    self.set_trigger(host, trigger);
                }
            }
            "page-toggled" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.set_page(host, on);
            }
            "new-link-clicked" => self.new_link(host),
            "address-picked" => {
                let ip = self.addresses().get(index(0)).cloned().unwrap_or_default();
                self.set_address(host, &ip);
            }
            "copy-clicked" => {
                let url = self.url();
                host.copy_text(&url);
            }
            "url-edited" => {
                self.set_webhook(host, Some(&text(0)), None, None, None);
                // The problem line follows what's typed
                self.refresh(host);
            }
            "method-picked" => {
                if let Some(method) = METHODS.get(index(0)) {
                    self.set_webhook(host, None, Some(method), None, None);
                    self.refresh(host);
                }
            }
            "on-edited" => self.set_webhook(host, None, None, Some(&text(0)), None),
            "off-edited" => self.set_webhook(host, None, None, None, Some(&text(0))),
            "test-clicked" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.send_test(host, on);
            }
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        json!({
            "trigger": self.trigger, "page": self.page, "token": self.token, "address": self.address,
            "webhook_url": self.webhook_url, "webhook_method": self.webhook_method,
            "webhook_on": self.webhook_on, "webhook_off": self.webhook_off,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let text = |key: &str| cfg.get(key).and_then(Value::as_str).map(String::from);
        self.trigger = text("trigger").filter(|t| TRIGGERS.contains(&t.as_str())).unwrap_or_else(|| DEFAULT_TRIGGER.into());
        self.page = cfg.get("page") != Some(&Value::Bool(false));
        match text("token").filter(|t| token_ok(t)) {
            Some(token) => self.token = token,
            // The link made in setup has to survive a restart
            None => self.save_token = true,
        }
        if let Some(sign) = &self.sign {
            sign.set_token(&self.token);
        }
        self.address = text("address").unwrap_or_default();
        self.webhook_url = text("webhook_url").unwrap_or_default();
        self.webhook_method = text("webhook_method").filter(|m| METHODS.contains(&m.as_str())).unwrap_or_else(|| "POST".into());
        self.webhook_on = text("webhook_on").unwrap_or_default();
        self.webhook_off = text("webhook_off").unwrap_or_default();
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let page = match &self.server {
            Some(server) => format!("on, port {}", server.port()),
            None if !self.page => "off".into(),
            None => "not running".into(),
        };
        let webhook = if self.webhook_url.is_empty() {
            "not set".to_string()
        } else {
            format!("{} ({})", if self.webhook_failing { "failing" } else { "set" }, self.webhook_method)
        };
        vec![
            ("On Air".into(), if self.on() { "on" } else { "off" }.into()),
            ("On Air when".into(), self.trigger.clone()),
            ("On Air sign page".into(), page),
            ("On Air webhook".into(), webhook),
        ]
    }
}

#[cfg(test)]
#[path = "on_air_plugin_tests.rs"]
mod tests;
