//! The Cinema tools plugin (plugins/cinema.py): focus peaking, zebras, false colour, scopes, framing guides and a level on the
//! preview, for judging focus, exposure, framing and a straight horizon. They are drawn on the preview only, so apps never see
//! them. The pixel work is in `cinema_tools`, the scopes' maths in `scopes` and what is drawn over the picture in `cinema_paint`.

use crate::cinema_paint::{self, CinemaLayer};
use crate::cinema_tools::*;
use crate::shared::Paired;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use telescope_core::actions::{self, Choice};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "cinema";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/cinema.slint"));

/// The timer that checks the preview still draws the level guide.
const T_SEEN: &str = "seen";
const SEEN_EVERY_MS: u64 = 1000;

const PEAKING_TIP: &str = n_("Outline the parts of the picture that are in focus.");
const ZEBRAS_TIP: &str = n_("Show stripes on areas that are about to clip to white.");
const FALSE_TIP: &str = n_("Colour each area of the picture by its brightness.");
const LEVEL_TIP: &str = n_("Show a horizon line from the phone's motion sensors.");
const GUIDES_TIP: &str = n_("Lines to help frame the shot.");
const SENSITIVITY_TIP: &str = n_("Higher values also outline softer edges.");
const ZEBRA_FROM_TIP: &str = n_("How bright something has to be to get stripes.");

/// The four switches, in the order of their actions: (action id, the name on the card, which setting).
const SWITCHES: [(&str, &str); 4] = [
    ("cinema.peaking", n_("Focus peaking")),
    ("cinema.zebras", n_("Zebras")),
    ("cinema.false_colour", n_("False colour")),
    ("cinema.level", n_("Level")),
];

pub struct CinemaPlugin {
    layer: Arc<CinemaLayer>,
    look: Look,
    /// The layer is on the preview.
    shown: bool,
    filter: LevelFilter,
    wants_sensors: bool,
    level_note: String,
    paired: Paired,
    held: [Option<bool>; 4],
    /// set_config has no host: the card and the preview hear the settings with the next hook that has one.
    pending: bool,
    /// The check that the guide is drawn is waiting on the host's timer.
    timer: bool,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for CinemaPlugin {
    fn default() -> Self {
        CinemaPlugin::new()
    }
}

/// The key a saved name is, or None when it isn't one of them.
fn key_of(keys: impl Iterator<Item = &'static str>, name: Option<&Value>) -> Result<Option<&'static str>, ConfigRejected> {
    match name {
        // Python looks the value up in a dict, which a list or dict can't be
        Some(Value::Array(_)) | Some(Value::Object(_)) => Err(ConfigRejected("not a name".into())),
        Some(Value::String(s)) => Ok({
            let mut keys = keys;
            keys.find(|k| k == s)
        }),
        _ => Ok(None),
    }
}

impl CinemaPlugin {
    pub fn new() -> CinemaPlugin {
        CinemaPlugin::with_clock(Arc::new(cinema_paint::now))
    }

    pub fn with_clock(clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> CinemaPlugin {
        CinemaPlugin {
            layer: Arc::new(CinemaLayer::with_clock(clock.clone())),
            look: Look::default(),
            shown: false,
            filter: LevelFilter::default(),
            wants_sensors: false,
            level_note: String::new(),
            paired: Paired::default(),
            held: [None; 4],
            pending: false,
            timer: false,
            clock,
        }
    }

    /// The layer the preview draws, for tests that look at it.
    pub fn layer(&self) -> Arc<CinemaLayer> {
        self.layer.clone()
    }

    pub fn look(&self) -> &Look {
        &self.look
    }

    fn names(table: &[(&str, &str)]) -> Vec<String> {
        table.iter().map(|(_, name)| tr(name)).collect()
    }

    fn fixed_props() -> Value {
        json!({
            "switch-names": SWITCHES.iter().map(|(_, n)| tr(n)).collect::<Vec<_>>(),
            "switch-tips": [tr(PEAKING_TIP), tr(ZEBRAS_TIP), tr(FALSE_TIP), tr(LEVEL_TIP)],
            "guides-tip": tr(GUIDES_TIP),
            "guide-items": Self::names(&GUIDES),
            "scope-items": Self::names(&SCOPES),
        })
    }

    fn card_state(&self) -> Value {
        let look = &self.look;
        json!({
            "active": self.paired.on(),
            "peaking": look.peaking,
            "zebras": look.zebras,
            "false-colour": look.false_colour,
            "level": look.level,
            "guide-current": GUIDES.iter().position(|(k, _)| *k == look.guides).unwrap_or(0),
            "scope-current": SCOPES.iter().position(|(k, _)| *k == look.scope).unwrap_or(0),
            "level-note": self.level_note,
            "level-note-shown": !self.level_note.is_empty(),
        })
    }

    fn card_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.card_state()) {
            a.extend(s);
        }
        all
    }

    fn section_props(&self) -> Value {
        let look = &self.look;
        json!({
            "colour-current": PEAKING_COLOURS.iter().position(|(k, ..)| *k == look.peaking_colour).unwrap_or(0),
            "size-current": SIZES.iter().position(|(k, ..)| *k == look.scope_size).unwrap_or(1),
            "peaking-level": look.peaking_level,
            "peaking-readout": look.peaking_level.to_string(),
            "zebra-level": look.zebra_level,
            "zebra-readout": format!("{}%", look.zebra_level),
            "peaking-tip": tr(SENSITIVITY_TIP),
            "zebra-tip": tr(ZEBRA_FROM_TIP),
            "reset-hint": tr("Double-click to reset"),
        })
    }

    fn show(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.card_state());
        host.set_ui_props(NAME, &self.section_props());
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            let look = self.look.clone();
            self.set_look(host, look);
            host.set_ui_props("card", &self.card_props());
            host.set_ui_props(NAME, &self.section_props());
        }
    }

    /// _changed: what the controls say is what the layer draws, and it is kept.
    fn changed(&mut self, host: &mut dyn Host, look: Look) {
        self.set_look(host, look);
        self.show(host);
        host.schedule_save();
    }

    /// _set_look: the layer draws this, and is on the preview while anything is on.
    fn set_look(&mut self, host: &mut dyn Host, look: Look) {
        self.layer.set_look(look.clone());
        let any = look.any();
        if any && !self.shown {
            host.add_preview_pass(NAME, self.layer.clone());
        } else if !any && self.shown {
            host.remove_preview_layer(NAME);
        }
        self.shown = any;
        if look.level && !self.timer {
            // just added: the preview gets its chance to draw it
            self.layer.set_painted_at((self.clock)());
            self.timer = true;
            host.schedule(SEEN_EVERY_MS, T_SEEN);
        } else if !look.level && self.timer {
            self.timer = false;
            host.cancel_schedule(T_SEEN);
        }
        self.look = look.clone();
        self.want_sensors(host, look.level && self.seen());
        host.refresh_preview();
    }

    // ── Level guide ───────────────────────────────────────────────────────

    fn want_sensors(&mut self, host: &mut dyn Host, on: bool) {
        if on == self.wants_sensors {
            return;
        }
        self.wants_sensors = on;
        host.want_sensors(NAME, &if on { vec!["orientation".to_string()] } else { Vec::new() });
        if !on {
            self.filter.reset();
            self.layer.set_level(None);
        }
        self.show_level_status(host);
    }

    /// Whether the preview drew the guide lately.
    fn seen(&self) -> bool {
        (self.clock)() - self.layer.painted_at() < LEVEL_UNSEEN_S
    }

    fn check_seen(&mut self, host: &mut dyn Host) {
        if self.timer {
            host.schedule(SEEN_EVERY_MS, T_SEEN);
        }
        let on = self.look.level && self.seen();
        self.want_sensors(host, on);
    }

    fn on_sensors(&mut self, host: &mut dyn Host, source_id: &str) {
        if !self.wants_sensors || host.focused_source_id().as_deref() != Some(source_id) {
            return;
        }
        self.show_level_status(host);
        let reading = host.sensors().reading(Some(source_id));
        let number = |name: &str| reading.as_ref().and_then(|r| r.get(name)).and_then(Value::as_f64);
        let (Some(_), Some(pitch)) = (reading.as_ref(), number("pitch")) else {
            if self.layer.level().is_some() {
                self.filter.reset();
                self.layer.set_level(None);
                host.refresh_preview();
            }
            return;
        };
        let at = number("orientation_at").unwrap_or(0.0);
        let old = self.layer.level();
        if old.is_some_and(|(_, old_at)| at <= old_at) {
            // a heartbeat or another kind: nothing new about the horizon, so it can still go stale
            return;
        }
        let roll = number("roll").map(|r| shown_roll(r, &host.view()));
        let level = self.filter.update(roll, Some(pitch), at);
        self.layer.set_level(level.map(|l| (l, at)));
        // Painted again only when what's drawn changes, at the tenth of a degree it shows
        if old.is_none() || level.is_none() || drawn(&old.unwrap().0) != drawn(&level.unwrap()) {
            host.refresh_preview();
        }
    }

    /// Why the level can't show, if it can't, on the card.
    fn show_level_status(&mut self, host: &mut dyn Host) {
        let status = if self.wants_sensors { Some(host.sensor_status(None)) } else { None };
        let problem = status.as_ref().filter(|s| s.get("state").and_then(Value::as_str) == Some("unavailable")).and_then(|s| {
            let reason = s.get("reason").and_then(Value::as_str).unwrap_or("");
            LEVEL_PROBLEMS.iter().find(|(r, _)| *r == reason).map(|(_, text)| tr(text))
        });
        self.level_note = problem.unwrap_or_default();
        host.set_ui_props("card", &json!({"level-note": self.level_note, "level-note-shown": !self.level_note.is_empty()}));
    }

    // ── Handlers ──────────────────────────────────────────────────────────

    fn switched(&mut self, host: &mut dyn Host, which: usize, on: bool) {
        let mut look = self.look.clone();
        match which {
            0 => look.peaking = on,
            1 => look.zebras = on,
            2 => look.false_colour = on,
            _ => look.level = on,
        }
        self.changed(host, look);
    }

    fn switch_on(&self, which: usize) -> bool {
        [self.look.peaking, self.look.zebras, self.look.false_colour, self.look.level][which]
    }

    fn guide_picked(&mut self, host: &mut dyn Host, index: usize) {
        if let Some((key, _)) = GUIDES.get(index) {
            let mut look = self.look.clone();
            look.guides = key;
            self.changed(host, look);
        }
    }

    fn scope_picked(&mut self, host: &mut dyn Host, index: usize) {
        if let Some((key, _)) = SCOPES.get(index) {
            let mut look = self.look.clone();
            look.scope = key;
            self.changed(host, look);
        }
    }

    fn choice_options(table: &[(&'static str, &'static str)]) -> Vec<(&'static str, String)> {
        // The English name is what a saved shortcut keeps, so it still hits in another language
        table.iter().map(|(_, name)| (*name, tr(name))).collect()
    }
}

impl Plugin for CinemaPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Cinema tools").into(),
            about: n_("Focus peaking, zebras, false colour, scopes, framing guides and a level on the preview, for judging focus, exposure, framing and a straight horizon.").into(),
            when_off: n_("Without it there's no Cinema tools card and the preview shows just the picture.").into(),
            category: "preview".into(),
            icon: "focus".into(),
            optional: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.card_props());
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.timer {
            self.timer = false;
            host.cancel_schedule(T_SEEN);
        }
        self.want_sensors(host, false);
        if self.shown {
            host.remove_preview_layer(NAME);
            self.shown = false;
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::SensorsChanged { source_id } => self.on_sensors(host, source_id),
            // the old tilt as shown means nothing now
            Event::ViewChanged { .. } | Event::DeviceChanged { .. } => self.filter.reset(),
            Event::TimerFired { owner, token } if owner == NAME && token == T_SEEN => self.check_seen(host),
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "CinemaCard".into(), props: self.card_props() }, region: "right".into() })
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: NAME.into(),
            tab: "video".into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "CinemaSettings".into(), props: self.section_props() },
            order: 40,
            tab_title: String::new(),
            keywords: tr_ctx("search", "focus peaking zebras false colour color scopes waveform parade vectorscope histogram exposure monitor grid guides rule of thirds golden ratio composition framing"),
            footer: None,
        }]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let number = event.args.first().and_then(Value::as_f64).unwrap_or(0.0);
        match (event.ui.as_str(), event.callback.as_str()) {
            ("card", "tool-toggled") => {
                let on = event.args.get(1).and_then(Value::as_bool).unwrap_or(false);
                self.switched(host, number as usize, on);
            }
            ("card", "guide-picked") => self.guide_picked(host, number as usize),
            ("card", "scope-picked") => self.scope_picked(host, number as usize),
            (NAME, "shown") => host.set_ui_props(NAME, &self.section_props()),
            (NAME, callback) => {
                let mut look = self.look.clone();
                match callback {
                    "colour-picked" => {
                        if let Some((key, ..)) = PEAKING_COLOURS.get(number as usize) {
                            look.peaking_colour = key;
                        }
                    }
                    "peaking-moved" => look.peaking_level = (number.round() as i64).clamp(1, 10),
                    "zebra-moved" => look.zebra_level = (number.round() as i64).clamp(70, 100),
                    "size-picked" => {
                        if let Some((key, ..)) = SIZES.get(number as usize) {
                            look.scope_size = key;
                        }
                    }
                    _ => return,
                }
                self.changed(host, look);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Cinema tools");
        let mut out: Vec<ActionSpec> = SWITCHES.iter().map(|(id, name)| actions::toggle_action(id, &tr(name), &group, "", &[])).collect();
        out.push(actions::choice_action("cinema.guides", &tr("Guides"), &group, "", &Self::choice_options(&GUIDES)));
        out.push(actions::choice_action("cinema.scope", &tr("Scope"), &group, "", &Self::choice_options(&SCOPES)));
        out
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        if let Some(which) = SWITCHES.iter().position(|(action, _)| *action == id) {
            let want = actions::toggle_wanted(params, self.switch_on(which), &mut self.held[which]);
            if want != self.switch_on(which) {
                self.switched(host, which, want);
            }
            return Some(actions::toggle_text(&tr(SWITCHES[which].1), want));
        }
        let (table, current, label): (&[(&'static str, &'static str)], usize, String) = match id {
            "cinema.guides" => (&GUIDES, GUIDES.iter().position(|(k, _)| *k == self.look.guides).unwrap_or(0), tr("Guides")),
            "cinema.scope" => (&SCOPES, SCOPES.iter().position(|(k, _)| *k == self.look.scope).unwrap_or(0), tr("Scope")),
            _ => return None,
        };
        let shown: Vec<String> = table.iter().map(|(_, n)| tr(n)).collect();
        let options: Vec<Choice> = table.iter().zip(&shown).map(|((_, english), s)| Choice { value: english, shown: s, enabled: true }).collect();
        let (said, pick) = actions::choice_target(params, &options, Some(current), true)?;
        if pick {
            if id == "cinema.guides" {
                self.guide_picked(host, said);
            } else {
                self.scope_picked(host, said);
            }
        }
        Some(actions::slider_text(&label, &shown[said]))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        let Some(which) = SWITCHES.iter().position(|(action, _)| *action == id) else { return };
        if let Some(before) = actions::hold_released(params, &mut self.held[which]) {
            if before != self.switch_on(which) && self.paired.on() {
                self.switched(host, which, before);
            }
        }
    }

    fn get_config(&self) -> Value {
        let look = &self.look;
        let mut cfg = Map::new();
        cfg.insert("peaking".into(), json!(look.peaking));
        cfg.insert("zebras".into(), json!(look.zebras));
        cfg.insert("false_colour".into(), json!(look.false_colour));
        cfg.insert("level".into(), json!(look.level));
        cfg.insert("scope".into(), json!(look.scope));
        cfg.insert("guides".into(), json!(look.guides));
        cfg.insert("peaking_colour".into(), json!(look.peaking_colour));
        cfg.insert("peaking_level".into(), json!(look.peaking_level));
        cfg.insert("zebra_level".into(), json!(look.zebra_level));
        cfg.insert("scope_size".into(), json!(look.scope_size));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        let flag = |name: &str| cfg.get(name) == Some(&Value::Bool(true));
        let null = Value::Null;
        let look = Look {
            peaking: flag("peaking"),
            zebras: flag("zebras"),
            false_colour: flag("false_colour"),
            level: flag("level"),
            scope: key_of(SCOPES.iter().map(|t| t.0), cfg.get("scope"))?.unwrap_or("off"),
            guides: key_of(GUIDES.iter().map(|t| t.0), cfg.get("guides"))?.unwrap_or("off"),
            peaking_colour: key_of(PEAKING_COLOURS.iter().map(|t| t.0), cfg.get("peaking_colour"))?.unwrap_or("red"),
            peaking_level: clamped_int(cfg.get("peaking_level").unwrap_or(&null), 5, 1, 10),
            zebra_level: clamped_int(cfg.get("zebra_level").unwrap_or(&null), 95, 70, 100),
            scope_size: key_of(SIZES.iter().map(|t| t.0), cfg.get("scope_size"))?.unwrap_or("medium"),
        };
        self.look = look;
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let look = &self.look;
        let guides = format!("{} guides", look.guides);
        let on: Vec<&str> = [
            ("peaking", look.peaking),
            ("zebras", look.zebras),
            ("false colour", look.false_colour),
            (look.scope, look.scope != "off"),
            ("level", look.level),
            (guides.as_str(), look.guides != "off"),
        ]
        .into_iter()
        .filter(|(_, used)| *used)
        .map(|(name, _)| name)
        .collect();
        vec![("Cinema tools".into(), if on.is_empty() { "nothing on".into() } else { on.join(", ") })]
    }
}

#[cfg(test)]
#[path = "cinema_tests.rs"]
mod tests;
