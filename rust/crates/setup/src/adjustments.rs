//! Adjustments: what Telescope changes by itself to cope, said out loud and undoable. It keeps the list of adjustments in effect now (effects
//! stepped down, Auto framing following faces, effects moved to the processor, Dynamic lowering the bitrate or frame rate, the phone's heat cap,
//! decoding on the processor)
//! for the chip in the top bar, shows a banner when the computer can't keep up even with the effects as light as they go, and has the switches
//! in Settings. It reads stats and the phone's state and acts through the host and events, never through another plugin. What it learns of the
//! computer (busiest programs, load) is used live: never logged, saved or sent.

use crate::titles::plugin_title;
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::effort::{EffortSnapshot, MOST};
use telescope_core::event::Event;
use telescope_core::host::{Host, PluginLook};
use telescope_core::i18n::{n_, number, tr, Fields};
use telescope_core::media::Media;
use telescope_core::monitor::{likely_cause, Cause, SystemReading};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::settings::off_default;
use telescope_core::stats::{BalanceSnapshot, NamedTiming, Snapshot, SourceSnapshot, SHED_BEHIND};
use telescope_core::ui::{Issue, SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "adjustments";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/adjustments.slint"));
const SECTION: &str = "adjustments";
const BANNER: &str = telescope_core::ui::LIVE_ONLY_BANNERS[0];
/// Dynamic has lowered the bitrate when it is under this share of the highest it has been this stream.
const LOWERED_BELOW: f64 = 0.8;
/// A cost under this many ms a frame isn't worth a tag.
const WORTH_NAMING_MS: f32 = 0.5;
/// How many costs the list shows.
const COSTS_SHOWN: usize = 2;
/// How many seconds of frames the banner clears on, once they keep up.
const KEPT_UP_SECONDS: usize = 3;
const KEPT_UP_SHARE: f32 = 0.02;
/// A dismissed banner comes back when this much more of the frames are shed than when it was closed.
const WORSE_BY: f32 = 0.15;
/// A dismissed banner is forgotten after this long without falling behind, so the next episode shows it again.
const QUIET_NS: u64 = 120_000_000_000;

/// What a closed banner remembers: the cause it was about and how bad it was, so only something clearly worse brings it back.
#[derive(Clone, Debug, PartialEq)]
pub struct Dismissal {
    pub cause: String,
    pub shed: f32,
}

/// Names a cause for comparing, so another program or another kind of cause counts as a different one.
pub fn cause_key(cause: Option<&Cause>) -> String {
    match cause {
        Some(Cause::OtherApp { name, .. }) => format!("app:{name}"),
        Some(Cause::GpuBusy { name }) => format!("gpu:{}", name.as_deref().unwrap_or("")),
        Some(Cause::BatterySaver) => "battery-saver".into(),
        Some(Cause::OnBattery) => "on-battery".into(),
        Some(Cause::LowMemory) => "memory".into(),
        Some(Cause::TelescopeItself) => "self".into(),
        None => "unknown".into(),
    }
}

/// Whether a closed banner stays closed: the cause is the same and it isn't clearly worse.
pub fn stays_dismissed(was: &Dismissal, cause: &str, shed: f32) -> bool {
    was.cause == cause && shed < was.shed + WORSE_BY
}

/// Which automatic system an adjustment belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Effects,
    Heads,
    Balance,
    Bitrate,
    FrameRate,
    Heat,
    Decoding,
}

/// What a person can do about one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Override {
    /// For this run of the app.
    KeepFull,
    KeepBodies,
    KeepOnCard,
    FixedBitrate,
    /// The saved switch.
    TurnOff,
    OpenVideoSettings,
}

/// Why it is in effect, with what the text needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    FramesLate,
    PowerSaving,
    CardBusy(Option<String>),
    FollowsConnection { mbps: f64 },
    SlowConnection { fps: i64 },
    PhoneHot { fps: i64 },
    CardTooSlow,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: Kind,
    pub reason: Reason,
    pub overrides: Vec<Override>,
}

/// The switches in Settings: whether each automatic system works. All on is how it comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switches {
    pub effects: bool,
    pub heads: bool,
    pub balance: bool,
    pub dynamic: bool,
}

impl Default for Switches {
    fn default() -> Self {
        Switches { effects: true, heads: true, balance: true, dynamic: true }
    }
}

/// What the phone's state says about its own adjustments.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Phone {
    pub dynamic_fps: Option<i64>,
    pub heat_fps: Option<i64>,
    pub bitrate_bps: Option<f64>,
    /// The highest bitrate seen this stream, which Dynamic starts from.
    pub peak_bps: f64,
}

impl Phone {
    pub fn from_state(state: &Value, before: Phone) -> Phone {
        let whole = |key: &str| state.get(key).and_then(Value::as_f64).filter(|n| *n > 0.0).map(|n| n as i64);
        let bitrate_bps = state.get("bitrate").and_then(Value::as_f64).filter(|n| *n > 0.0);
        Phone { dynamic_fps: whole("dynamic_fps"), heat_fps: whole("heat_fps"), bitrate_bps, peak_bps: before.peak_bps.max(bitrate_bps.unwrap_or(0.0)) }
    }
}

/// Everything the list is made from.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub effort: EffortSnapshot,
    pub balance: BalanceSnapshot,
    /// The busiest other program on the graphics card, when the monitor was asked and could tell.
    pub busiest_gpu: Option<String>,
    pub phone: Phone,
    /// Stream output is set to Dynamic, and nothing has asked for a fixed bitrate.
    pub dynamic_in_use: bool,
    pub decoding_slow: bool,
}

/// The adjustments in effect now, in the order they are listed.
pub fn items(seen: &Seen) -> Vec<Item> {
    let mut out = Vec::new();
    let e = &seen.effort;
    // The level comes from stepping down only while that is above what Power-saving effects asks for
    let stepping = e.auto && !e.held && e.auto_level > e.floor;
    let why = if stepping { Reason::FramesLate } else { Reason::PowerSaving };
    if e.level > 0 {
        out.push(Item {
            kind: Kind::Effects,
            reason: why.clone(),
            overrides: if stepping { vec![Override::KeepFull, Override::TurnOff] } else { Vec::new() },
        });
    }
    if e.heads {
        out.push(Item { kind: Kind::Heads, reason: why, overrides: vec![Override::KeepBodies, Override::TurnOff] });
    }
    if !seen.balance.on_card && !seen.balance.pinned {
        out.push(Item { kind: Kind::Balance, reason: Reason::CardBusy(seen.busiest_gpu.clone()), overrides: vec![Override::KeepOnCard, Override::TurnOff] });
    }
    let p = &seen.phone;
    if seen.dynamic_in_use {
        if let Some(now) = p.bitrate_bps.filter(|b| p.peak_bps > 0.0 && *b < p.peak_bps * LOWERED_BELOW) {
            out.push(Item { kind: Kind::Bitrate, reason: Reason::FollowsConnection { mbps: now / 1e6 }, overrides: vec![Override::FixedBitrate, Override::TurnOff] });
        }
    }
    // The phone takes the lower of the two, which is the one it reports
    match (p.heat_fps, p.dynamic_fps) {
        (Some(fps), _) => out.push(Item { kind: Kind::Heat, reason: Reason::PhoneHot { fps }, overrides: Vec::new() }),
        (None, Some(fps)) if seen.dynamic_in_use => {
            out.push(Item { kind: Kind::FrameRate, reason: Reason::SlowConnection { fps }, overrides: vec![Override::FixedBitrate, Override::TurnOff] });
        }
        _ => {}
    }
    if seen.decoding_slow {
        out.push(Item { kind: Kind::Decoding, reason: Reason::CardTooSlow, overrides: vec![Override::OpenVideoSettings] });
    }
    out
}

/// Whether the banner for major trouble shows: the stream sheds a tenth of its frames for a few seconds (and stops once frames keep up) while the
/// effects are already as light as they go, or can't get lighter because stepping down is off or held.
pub fn falling_behind(showing: bool, source: Option<&SourceSnapshot>, effort: &EffortSnapshot) -> bool {
    let Some(source) = source else { return false };
    let at_the_limit = !effort.auto || effort.held || effort.level >= MOST;
    if !at_the_limit {
        return false;
    }
    if showing {
        source.shed_share(KEPT_UP_SECONDS).is_some_and(|share| share > KEPT_UP_SHARE)
    } else {
        source.computer_is_slow()
    }
}

/// One effect's cost per frame, in ms.
#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    pub name: String,
    pub ms: f32,
}

/// What each running effect costs a frame, dearest first: each plugin's frame step and each vision model's runs.
pub fn costs(source: &SourceSnapshot, models: &[NamedTiming], looks: &[PluginLook]) -> Vec<Cost> {
    let steps = source.plugins.iter().map(|t| Cost { name: plugin_title(&t.name, looks), ms: t.timing.mean_ms });
    let models = models.iter().map(|t| Cost { name: model_name(&t.name), ms: t.timing.mean_ms });
    let mut all: Vec<Cost> = steps.chain(models).filter(|c| c.ms >= WORTH_NAMING_MS).collect();
    all.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    all
}

fn model_name(model: &str) -> String {
    match model {
        "mask" => tr("Person mask"),
        "cutout" => tr("Cutout"),
        "people" => tr("People tracking"),
        other => other.to_string(),
    }
}

/// "Smooth at 30 fps" or "Behind at 24 fps": whether the shown stream gets every frame through, and the status kind to draw it in.
pub fn status_line(source: &SourceSnapshot) -> (String, &'static str) {
    let behind = source.shed_share(3).is_some_and(|share| share > SHED_BEHIND) || (source.target_fps > 0 && source.now.delivered_fps > 0.0 && source.now.delivered_fps < source.target_fps as f32 * 0.85);
    if behind {
        (tr("Behind at {n} fps").field("n", number(source.now.delivered_fps as f64, 0, false)), "warn")
    } else {
        (tr("Smooth at {n} fps").field("n", source.target_fps.max(source.now.delivered_fps.round() as u32)), "ok")
    }
}

/// The banner: the likely cause in one sentence and what helps in another.
pub fn behind_issue(cause: Option<&Cause>, dearest_effect: Option<&str>) -> Issue {
    let (cause_text, help) = match cause {
        Some(Cause::OtherApp { name, .. }) => (tr("{name} is using most of the processor.").field("name", name.clone()), tr("Closing it will help the most.")),
        Some(Cause::GpuBusy { name: Some(name) }) => (tr("{name} is keeping the graphics card busy.").field("name", name.clone()), tr("Closing it will help the most.")),
        Some(Cause::GpuBusy { name: None }) => (tr("Another program is keeping the graphics card busy."), tr("Closing it will help the most.")),
        Some(Cause::BatterySaver) => (tr("Battery saver is slowing the computer down."), tr("Turning it off will help.")),
        Some(Cause::OnBattery) => (tr("The computer is running on battery."), tr("Plugging it in will help.")),
        Some(Cause::LowMemory) => (tr("The computer is low on memory."), tr("Closing other programs will help.")),
        Some(Cause::TelescopeItself) | None => {
            let cause_text = if cause.is_some() { tr("Telescope's own effects are using most of the computer.") } else { tr("The computer is too slow for the effects in use.") };
            let help = match dearest_effect {
                Some(name) => tr("Turning off {name} will help the most.").field("name", name.to_string()),
                None => tr("Turning off an effect or lowering the frame rate will help."),
            };
            (cause_text, help)
        }
    };
    Issue { title: tr("Telescope is falling behind"), text: format!("{cause_text} {help}"), actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: true }
}

// ── What the texts say ──────────────────────────────────────────────────────

fn name_of(kind: Kind) -> String {
    match kind {
        Kind::Effects => tr("Lighter effects"),
        Kind::Heads => tr("Framing by face"),
        Kind::Balance => tr("Effects on the processor"),
        Kind::Bitrate => tr("Lower bitrate"),
        Kind::FrameRate | Kind::Heat => tr("Lower frame rate"),
        Kind::Decoding => tr("Decoding on the processor"),
    }
}

fn why_of(reason: &Reason) -> String {
    match reason {
        Reason::FramesLate => tr("Frames were running late"),
        Reason::PowerSaving => tr("Power-saving effects is on"),
        Reason::CardBusy(Some(name)) => tr("{name} is using the graphics card").field("name", name.clone()),
        Reason::CardBusy(None) => tr("The graphics card is busy"),
        Reason::FollowsConnection { mbps } => tr("Dynamic is following the connection: {n} Mbps").field("n", number(*mbps, 1, false)),
        Reason::SlowConnection { fps } => tr("The connection is slow: {n} fps").field("n", *fps),
        Reason::PhoneHot { fps } => tr("The phone is hot: {n} fps").field("n", *fps),
        Reason::CardTooSlow => tr("The graphics card was too slow"),
    }
}

/// The tooltip's sentence for the row itself.
fn row_tip_of(kind: Kind) -> String {
    match kind {
        Kind::Effects => tr("Heavy effects work less often while frames run late. They return to full quality once there is room."),
        Kind::Heads => tr("Finding your face costs a fraction of finding your body. Auto framing uses your body again when it finds no face or sees several people."),
        Kind::Balance => tr("Effects that can run on either the graphics card or the processor moved to the processor because the card is busy. They move back when it has room."),
        Kind::Bitrate => tr("Dynamic lowers how much video the phone sends when the connection can't carry more."),
        Kind::FrameRate => tr("Dynamic lowers the frame rate when the connection is too slow for the one asked for."),
        Kind::Heat => tr("The phone lowers its frame rate when it gets hot to protect itself. There is no way to override that."),
        Kind::Decoding => tr("The processor is decoding for a while because the graphics card was too slow. The card gets another turn afterwards."),
    }
}

/// A button's (icon, name, tooltip sentence).
fn override_text(kind: Kind, o: Override) -> (&'static str, String, String) {
    match o {
        Override::KeepFull => ("sparkle", tr("Use full quality"), tr("Runs effects at full quality until you close Telescope, even while frames run late.")),
        Override::KeepBodies => ("framing", tr("Follow your body"), tr("Auto framing follows your body until you close Telescope, even while effects are lighter.")),
        Override::KeepOnCard => ("chip", tr("Use graphics card"), tr("Runs effects on the graphics card until you close Telescope, even while it is busy.")),
        Override::FixedBitrate => ("lock", tr("Use a fixed bitrate"), tr("Makes the phone send at a steady bitrate until you close Telescope, in place of Dynamic.")),
        Override::OpenVideoSettings => ("sliders", tr("Open Video settings"), tr("Hardware acceleration is set there.")),
        Override::TurnOff => ("power", tr("Turn this off"), match kind {
            Kind::Effects => tr("Stops Telescope lightening effects when frames run late, until you switch it back on under Video in Settings."),
            Kind::Heads => tr("Stops Auto framing following your face to save effort, until you switch it back on under Video in Settings."),
            Kind::Balance => tr("Stops Telescope moving effects to the processor when the graphics card is busy, until you switch it back on under Video in Settings."),
            _ => tr("Stops Dynamic lowering the bitrate or frame rate, until you switch it back on under Video in Settings."),
        }),
    }
}

/// A row of the list as the chip's component takes it.
fn row_json(item: &Item) -> Value {
    let button = |i: usize| item.overrides.get(i).map(|o| override_text(item.kind, *o));
    let (a, b) = (button(0), button(1));
    let field = |t: &Option<(&'static str, String, String)>, n: usize| t.as_ref().map(|t| match n { 0 => t.0.to_string(), 1 => t.1.clone(), _ => t.2.clone() }).unwrap_or_default();
    json!({
        "name": name_of(item.kind), "why": why_of(&item.reason), "tip": row_tip_of(item.kind),
        "a-icon": field(&a, 0), "a-label": field(&a, 1), "a-tip": field(&a, 2),
        "b-icon": field(&b, 0), "b-label": field(&b, 1), "b-tip": field(&b, 2),
    })
}

/// A cost's tag as the chip's list shows it: the effect and what a frame spends on it.
fn cost_json(c: &Cost) -> Value {
    let ms = if c.ms < 10.0 { number(c.ms as f64, 1, false) } else { number(c.ms as f64, 0, false) };
    json!({"name": tr("{name} {ms} ms").field("name", c.name.clone()).field("ms", ms), "tip": tr("What {name} adds to every frame").field("name", c.name.clone())})
}

pub struct AdjustmentsPlugin {
    media: Arc<dyn Media>,
    /// Saved: whether each automatic system works.
    switches: Switches,
    /// For this run of the app.
    held_full: bool,
    held_bodies: bool,
    held_card: bool,
    held_fixed: bool,
    /// What was last handed to the host, so a repeat says nothing.
    told_fixed: bool,
    phone: Phone,
    rows: Vec<Item>,
    /// The computer is falling behind now, shown or not.
    behind: bool,
    showing_banner: bool,
    /// The banner the person closed, kept while the same trouble goes on.
    dismissed: Option<Dismissal>,
    /// When frames last stopped falling behind, while a dismissal is remembered.
    quiet_since: Option<u64>,
    /// The share of frames shed at the last reading while behind.
    last_shed: f32,
    list_open: bool,
    asking: bool,
    /// The banner's cause and dearest effect, as of the last reading.
    last_cause: Option<Cause>,
}

impl AdjustmentsPlugin {
    pub fn new(media: Arc<dyn Media>) -> AdjustmentsPlugin {
        AdjustmentsPlugin {
            media, switches: Switches::default(), held_full: false, held_bodies: false, held_card: false, held_fixed: false, told_fixed: false, phone: Phone::default(), rows: Vec::new(),
            behind: false, showing_banner: false, dismissed: None, quiet_since: None, last_shed: 0.0, list_open: false, asking: false, last_cause: None,
        }
    }

    // ── What the host is told ──────────────────────────────────────────

    fn apply_all(&mut self, host: &mut dyn Host) {
        host.set_effects_step_down(self.switches.effects);
        host.hold_effects_full(self.held_full);
        self.apply_heads(host);
        self.apply_card(host);
        self.apply_fixed(host);
    }

    fn apply_heads(&self, host: &mut dyn Host) {
        host.allow_heads(self.switches.heads && !self.held_bodies);
    }

    fn apply_card(&self, host: &mut dyn Host) {
        host.keep_steps_on_card(self.held_card || !self.switches.balance);
    }

    fn apply_fixed(&mut self, host: &mut dyn Host) {
        let on = self.held_fixed || !self.switches.dynamic;
        if on != self.told_fixed {
            self.told_fixed = on;
            host.emit(Event::FixedBitrateRequested { on });
        }
    }

    fn do_override(&mut self, host: &mut dyn Host, kind: Kind, o: Override) {
        match o {
            Override::KeepFull => {
                self.held_full = true;
                host.hold_effects_full(true);
            }
            Override::KeepBodies => {
                self.held_bodies = true;
                self.apply_heads(host);
            }
            Override::KeepOnCard => {
                self.held_card = true;
                self.apply_card(host);
            }
            Override::FixedBitrate => {
                self.held_fixed = true;
                self.apply_fixed(host);
            }
            Override::OpenVideoSettings => host.open_settings("video"),
            Override::TurnOff => {
                match kind {
                    Kind::Effects => self.switches.effects = false,
                    Kind::Heads => self.switches.heads = false,
                    Kind::Balance => self.switches.balance = false,
                    _ => self.switches.dynamic = false,
                }
                self.apply_all(host);
                host.schedule_save();
                host.set_ui_props(SECTION, &self.section_props());
            }
        }
    }

    // ── What the readings make of it ───────────────────────────────────

    fn shown_source<'a>(&self, host: &dyn Host, snapshot: &'a Snapshot) -> Option<&'a SourceSnapshot> {
        let shown = host.focused_source_id().unwrap_or_default();
        snapshot.sources.iter().find(|s| s.source_id == shown).or_else(|| snapshot.sources.first())
    }

    fn dynamic_in_use(&self, host: &dyn Host) -> bool {
        let chosen = host.plugin_config("stream_output").and_then(|c| c.get("bitrate_mbps").and_then(Value::as_f64)) == Some(-1.0);
        chosen && !self.told_fixed
    }

    fn on_stats(&mut self, host: &mut dyn Host, snapshot: &Snapshot) {
        self.on_stats_at(host, snapshot, telescope_core::time::monotonic_ns());
    }

    fn on_stats_at(&mut self, host: &mut dyn Host, snapshot: &Snapshot, now: u64) {
        let source = self.shown_source(host, snapshot);
        let system: Option<&SystemReading> = snapshot.app.system.as_ref();
        let seen = Seen {
            effort: snapshot.app.effort.clone(),
            balance: snapshot.app.balance.clone(),
            busiest_gpu: system.and_then(|s| s.gpu_busiest.first().map(|p| p.name.clone())),
            phone: self.phone,
            dynamic_in_use: self.dynamic_in_use(host),
            decoding_slow: self.media.decoding_status().hardware_slow,
        };
        self.rows = if source.is_some() { items(&seen) } else { Vec::new() };
        let behind = falling_behind(self.behind, source, &seen.effort);
        let looks = host.plugin_looks();
        let costs = source.map(|s| costs(s, &snapshot.app.models, &looks)).unwrap_or_default();
        if behind {
            self.quiet_since = None;
            let cause = system.and_then(likely_cause);
            if cause.is_some() || system.is_some() || !self.behind {
                self.last_cause = cause;
            }
            let shed = source.and_then(|s| s.shed_share(KEPT_UP_SECONDS)).unwrap_or(0.0);
            self.last_shed = shed;
            let key = cause_key(self.last_cause.as_ref());
            if self.dismissed.as_ref().is_some_and(|was| !stays_dismissed(was, &key, shed)) {
                self.dismissed = None;
            }
            if self.dismissed.is_none() {
                host.show_issue(BANNER, behind_issue(self.last_cause.as_ref(), costs.first().map(|c| c.name.as_str())));
                self.showing_banner = true;
            }
        } else {
            if self.showing_banner {
                host.clear_issue(Some(BANNER), None);
            }
            self.showing_banner = false;
            self.last_cause = None;
            if self.dismissed.is_some() {
                let since = *self.quiet_since.get_or_insert(now);
                if now.saturating_sub(since) >= QUIET_NS {
                    self.dismissed = None;
                    self.quiet_since = None;
                }
            }
        }
        self.behind = behind;
        self.sync_wants(host);
        let status = source.map(status_line);
        host.set_ui_props("header", &self.header_props(status, &costs));
    }

    /// The monitor and the per-step times are only read while the banner shows or the list is open.
    fn sync_wants(&mut self, host: &mut dyn Host) {
        let want = self.behind || self.list_open;
        if want != self.asking {
            self.asking = want;
            host.want_system(NAME, want);
            host.want_stats(NAME, want);
        }
    }

    fn header_props(&self, status: Option<(String, &'static str)>, costs: &[Cost]) -> Value {
        let n = self.rows.len();
        let (status_text, status_kind) = status.unwrap_or_default();
        json!({
            "shown": n > 0,
            "text": tr("Adjusting · {n}").field("n", n),
            "short-text": n.to_string(),
            "reserve": [tr("Adjusting · {n}").field("n", 88)],
            "tip": tr("Automatic adjustments"),
            "tip-detail": tr("Open the list to see what Telescope has adjusted for a smoother stream and to undo any of it."),
            "rows": self.rows.iter().map(row_json).collect::<Vec<_>>(),
            "status": status_text, "status-kind": status_kind,
            "costs": costs.iter().take(COSTS_SHOWN).map(cost_json).collect::<Vec<_>>(),
        })
    }

    fn section_props(&self) -> Value {
        json!({
            "reset-shown": off_default(&self.get_config(), &AdjustmentsPlugin::defaults(), &[]),
            "effects": self.switches.effects, "heads": self.switches.heads, "balance": self.switches.balance, "dynamic": self.switches.dynamic,
        })
    }

    fn defaults() -> Value {
        json!({"effects": true, "heads": true, "balance": true, "dynamic": true})
    }

    /// Nothing is running any more: the list empties, the banner goes and nothing is asked for.
    fn stopped(&mut self, host: &mut dyn Host) {
        self.phone = Phone::default();
        self.rows.clear();
        if self.showing_banner {
            host.clear_issue(Some(BANNER), None);
        }
        self.behind = false;
        self.showing_banner = false;
        self.dismissed = None;
        self.quiet_since = None;
        self.last_cause = None;
        self.sync_wants(host);
        host.set_ui_props("header", &self.header_props(None, &[]));
    }
}

impl Plugin for AdjustmentsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Adjustments").into(),
            about: n_("Lists the changes Telescope makes for a smoother stream, with a button to undo each one.").into(),
            when_off: n_("Switching it off removes the Adjusting list, the Automatic adjustments settings and the falling behind banner. Telescope still lowers effects, frame rate and bitrate as before.").into(),
            category: "streaming".into(),
            icon: "sliders".into(),
            header_side: "right".into(),
            ..Default::default()
        }
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec { source: SlintSource::Text(UI.into()), component: "AdjustmentsChip".into(), props: self.header_props(None, &[]) })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: SECTION.into(),
            tab: "video".into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "AdjustmentsSettings".into(), props: self.section_props() },
            order: 10,
            tab_title: String::new(),
            keywords: String::new(),
            footer: None,
        }]
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.apply_all(host);
        host.set_ui_props(SECTION, &self.section_props());
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        // Nothing this plugin held stays held without it
        host.hold_effects_full(false);
        host.allow_heads(true);
        host.keep_steps_on_card(false);
        host.set_effects_step_down(true);
        host.want_system(NAME, false);
        host.want_stats(NAME, false);
        if self.told_fixed {
            self.told_fixed = false;
            host.emit(Event::FixedBitrateRequested { on: false });
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::StatsUpdated { snapshot } => self.on_stats(host, snapshot),
            Event::StreamStopped | Event::DeviceChanged { .. } => self.stopped(host),
            _ => {}
        }
    }

    fn on_phone_state(&mut self, _host: &mut dyn Host, state: &Value) {
        self.phone = Phone::from_state(state, self.phone);
    }

    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        if key == BANNER && id == "dismiss" {
            let shed = self.last_shed;
            self.dismissed = Some(Dismissal { cause: cause_key(self.last_cause.as_ref()), shed });
            self.showing_banner = false;
            host.clear_issue(Some(BANNER), None);
        }
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        let flag = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        match (event.ui.as_str(), event.callback.as_str()) {
            ("header", "opened") => {
                self.list_open = true;
                self.sync_wants(host);
            }
            ("header", "closed") => {
                self.list_open = false;
                self.sync_wants(host);
            }
            ("header", "override-clicked") => {
                let at = |i: usize| event.args.get(i).and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
                if let Some(item) = self.rows.get(at(0)).cloned() {
                    if let Some(o) = item.overrides.get(at(1)) {
                        self.do_override(host, item.kind, *o);
                    }
                }
            }
            (SECTION, "effects-toggled") | (SECTION, "heads-toggled") | (SECTION, "balance-toggled") | (SECTION, "dynamic-toggled") => {
                match event.callback.as_str() {
                    "effects-toggled" => self.switches.effects = flag(0),
                    "heads-toggled" => self.switches.heads = flag(0),
                    "balance-toggled" => self.switches.balance = flag(0),
                    _ => self.switches.dynamic = flag(0),
                }
                self.apply_all(host);
                host.schedule_save();
                host.set_ui_props(SECTION, &self.section_props());
            }
            (SECTION, "reset-clicked") => {
                self.switches = Switches::default();
                self.apply_all(host);
                host.schedule_save();
                host.set_ui_props(SECTION, &self.section_props());
            }
            _ => {}
        }
    }

    fn get_config(&self) -> Value {
        json!({"effects": self.switches.effects, "heads": self.switches.heads, "balance": self.switches.balance, "dynamic": self.switches.dynamic})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let on = |key: &str| cfg.get(key).and_then(Value::as_bool).unwrap_or(true);
        self.switches = Switches { effects: on("effects"), heads: on("heads"), balance: on("balance"), dynamic: on("dynamic") };
        Ok(())
    }
}

#[cfg(test)]
#[path = "adjustments_tests.rs"]
mod tests;

