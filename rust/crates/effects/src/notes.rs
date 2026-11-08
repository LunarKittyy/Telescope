//! The Notes plugin (plugins/notes.py): notes for the phone's screen right under its front camera, to read while looking at the
//! people on the call. The card holds up to nine saved sets and sends the open one with its style (size, scroll speed, playing) as
//! /v1/control bodies through Host::phone_control; what the phone says back (its state) says whether they are up.

use telescope_core::i18n::Fields;
use crate::shared::Paired;
use serde_json::{json, Value};
use std::collections::HashMap;
use telescope_core::actions::{self, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::json::{dumps, Style};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::text::{py_lines, py_space, py_strip};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "notes";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/notes.slint"));

/// sp on the phone; 44 is about 4 words a line in portrait.
pub const SIZE_MIN: i64 = 16;
pub const SIZE_MAX: i64 = 120;
pub const SIZE_DEFAULT: i64 = 44;
pub const SPEED_MAX: i64 = 10;
pub const SPEED_DEFAULT: i64 = 3;
pub const MAX_SETS: usize = 9;
/// How much of the open set the text size bubble draws.
pub const SAMPLE_CHARS: usize = 160;
/// The phone reads control requests up to 64 KiB.
pub const NOTES_MAX_BYTES: usize = 60_000;
pub const NAME_CHARS: usize = 32;
/// How often to ask the phone whether the notes are on its screen, and the first ask soon after sending.
const POLL_MS: u64 = 3000;
const FIRST_POLL_MS: u64 = 1200;
const T_POLL: &str = "poll";
const T_FIRST_POLL: &str = "first-poll";
/// The phone lost notes sent longer ago than this (its stream started again): send them again.
const RESEND_AFTER_S: f64 = 8.0;

/// The scroll modes by the names saved settings use, with their texts.
const SCROLLS: [(&str, &str); 2] = [("auto", n_("Auto")), ("hand", n_("By hand"))];

const PLACEHOLDER: &str = n_("Type or paste your notes.");
const SET_TIP: &str = n_("Saved notes");
const NEW_TIP: &str = n_("New notes");
const EDITOR_TIP: &str = n_("Type or paste what you want to read on the phone.");
const SCROLL_TIP: &str = n_("Scroll automatically or move one line at a time by hand.");
const DELETE_TIP: &str = n_("Delete notes");
const SIZE_TIP: &str = n_("How big the notes are on the phone.");
const SPEED_TIP: &str = n_("How fast the notes scroll on the phone.");
const BY_HAND_TIP: &str = n_("Only used with Auto scrolling.");
const BACK_TIP: &str = n_("Go back one line.");
const PLAY_TIP: &str = n_("Start or pause scrolling on the phone.");
const NEXT_TIP: &str = n_("Go to the next line.");
const RESET_HINT: &str = n_("Double-click to reset");

/// How big the notes control is on the wire, as the phone client sends it.
pub fn body_size(text: &str) -> usize {
    let body = json!({"action": "notes", "text": text});
    dumps(&body, Style { indent: None, sort_keys: false, ensure_ascii: false }).len()
}

/// What the set picker calls a set of notes: its first line with something on it, or "" for an empty one.
pub fn set_name(text: &str) -> String {
    let line = py_lines(text).into_iter().map(py_strip).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= NAME_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(NAME_CHARS - 1).collect();
    format!("{}\u{2026}", cut.trim_end_matches(py_space))
}

/// Whether the phone streams from its front camera; None when the state doesn't say (an older phone).
pub fn front_facing(state: &Value) -> Option<bool> {
    let current = state.get("cameras").and_then(Value::as_array)?.iter().find(|c| c.is_object() && truthy(c.get("current")))?;
    let facing = current.get("facing").and_then(Value::as_str).filter(|f| !f.is_empty())?;
    Some(facing == "front")
}

/// Python's bool() of what a phone's state holds.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// What the card knows when it says how the notes are doing.
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusFacts {
    pub streaming: bool,
    pub phone: bool,
    pub takes_notes: Option<bool>,
    pub too_long: bool,
    pub sent: bool,
    pub shown: bool,
    pub front: Option<bool>,
    pub edited: bool,
    pub closed: bool,
    pub preview: bool,
}

/// (what the card says about the notes, its status kind).
pub fn notes_status(f: &StatusFacts) -> (String, &'static str) {
    if !f.phone {
        return (tr("Notes only work with a phone as the camera."), "dim");
    }
    if f.takes_notes == Some(false) {
        return (tr("Update Telescope on the phone to show notes there."), "status_warn");
    }
    if f.too_long {
        return (tr("These notes are too long to send to the phone."), "status_err");
    }
    // Not streaming: the Send button is off and says nothing more
    if !f.streaming {
        return (String::new(), "dim");
    }
    if !f.sent {
        if f.front == Some(false) {
            return (tr("Switch to the front camera to have the phone's screen facing you."), "status_warn");
        }
        return (String::new(), "dim");
    }
    if !f.shown {
        if f.closed {
            return (tr("The notes were closed on the phone. Send again to show them."), "status_warn");
        }
        if f.preview {
            return (tr("The notes show up on the phone once you close its preview."), "status_warn");
        }
        return (tr("Open Telescope on the phone to see the notes."), "status_warn");
    }
    if f.front == Some(false) {
        return (tr("The notes are up but the phone streams from its back camera."), "status_warn");
    }
    if f.edited {
        return (tr("Send again to show your changes on the phone."), "status_ok");
    }
    (tr("The notes are on the phone's screen."), "status_ok")
}

pub struct NotesPlugin {
    /// The shown stream's id ("" for the one with no phone picked); None while nothing streams.
    ctrl: Option<String>,
    /// For each stream that has notes up: (the text sent, when it was sent).
    sent: HashMap<String, (String, f64)>,
    takes_notes: Option<bool>,
    shown: bool,
    /// Closed on the phone (Back or its close button) since they were sent.
    closed: bool,
    /// The phone holds them back while its Preview is open.
    preview: bool,
    front: Option<bool>,
    /// "" for a phone, else the picked StreamSource.
    source: String,
    sets: Vec<String>,
    current: usize,
    size: i64,
    speed: i64,
    scroll_auto: bool,
    playing: bool,
    held: Option<bool>,
    paired: Paired,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for NotesPlugin {
    fn default() -> Self {
        NotesPlugin::new()
    }
}

impl NotesPlugin {
    pub fn new() -> NotesPlugin {
        NotesPlugin {
            ctrl: None,
            sent: HashMap::new(),
            takes_notes: None,
            shown: false,
            closed: false,
            preview: false,
            front: None,
            source: String::new(),
            sets: vec![String::new()],
            current: 0,
            size: SIZE_DEFAULT,
            speed: SPEED_DEFAULT,
            scroll_auto: true,
            playing: false,
            held: None,
            paired: Paired::default(),
            pending: false,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
        }
    }

    #[cfg(test)]
    fn with_clock(clock: impl Fn() -> f64 + Send + 'static) -> NotesPlugin {
        NotesPlugin { clock: Box::new(clock), ..NotesPlugin::new() }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn text(&self) -> &str {
        &self.sets[self.current]
    }

    fn set_label(index: usize, text: &str) -> String {
        let name = set_name(text);
        if name.is_empty() { tr("Notes {n}").field("n", (index + 1).to_string()) } else { name }
    }

    // ── What the card shows ───────────────────────────────────────────

    fn is_sent(&self) -> bool {
        self.ctrl.as_ref().is_some_and(|c| self.sent.contains_key(c))
    }

    fn sent_text(&self) -> &str {
        self.ctrl.as_ref().and_then(|c| self.sent.get(c)).map_or("", |(t, _)| t.as_str())
    }

    fn sendable(&self) -> bool {
        self.ctrl.is_some() && self.source.is_empty() && self.takes_notes != Some(false)
    }

    fn too_long(&self) -> bool {
        body_size(self.text()) > NOTES_MAX_BYTES
    }

    fn status(&self) -> (String, &'static str) {
        let sent = self.is_sent();
        notes_status(&StatusFacts {
            streaming: self.ctrl.is_some(),
            phone: self.source.is_empty(),
            takes_notes: self.takes_notes,
            too_long: self.too_long(),
            sent,
            shown: self.shown,
            front: self.front,
            edited: sent && self.text() != self.sent_text(),
            closed: self.closed,
            preview: self.preview,
        })
    }

    fn send_enabled(&self) -> bool {
        self.sendable() && !self.too_long() && !py_strip(self.text()).is_empty()
    }

    fn fixed_props() -> Value {
        json!({
            "set-tip": tr(SET_TIP), "new-tip": tr(NEW_TIP), "delete-tip": tr(DELETE_TIP),
            "placeholder": tr(PLACEHOLDER), "editor-tip": tr(EDITOR_TIP), "scroll-tip": tr(SCROLL_TIP), "editor-label": tr("Notes"), "size-tip": tr(SIZE_TIP), "speed-tip": tr(SPEED_TIP), "by-hand-tip": tr(BY_HAND_TIP),
            "scroll-items": SCROLLS.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>(), "reset-hint": tr(RESET_HINT),
            "back-text": tr_ctx("notes", "Previous"), "back-tip": tr(BACK_TIP), "play-tip": tr(PLAY_TIP),
            "next-text": tr_ctx("notes", "Next"), "next-tip": tr(NEXT_TIP), "send-text": tr("Send to phone"),
            "clear-text": tr("Clear from phone"),
        })
    }

    /// The sets: what the picker lists, which is open, and the text in the editor.
    fn sets_props(&self) -> Value {
        json!({
            "set-items": self.sets.iter().enumerate().map(|(i, t)| Self::set_label(i, t)).collect::<Vec<_>>(),
            "set-current": self.current,
            "text": self.text(),
            // The size bubble's sample: just the start, so a long set never costs a long layout while it is up
            "sample": self.text().chars().take(SAMPLE_CHARS).collect::<String>(),
            "new-enabled": self.sets.len() < MAX_SETS,
            "delete-enabled": self.sets.len() > 1 || !self.sets[0].is_empty(),
        })
    }

    fn state_props(&self) -> Value {
        let (status, kind) = self.status();
        let sent = self.is_sent();
        json!({
            "active": self.paired.on(),
            "size": self.size, "size-text": self.size.to_string(),
            "scroll": if self.scroll_auto { 0 } else { 1 },
            "speed": self.speed, "speed-text": self.speed.to_string(),
            "show-auto": self.scroll_auto,
            "playing": self.playing, "play-text": if self.playing { tr("Pause") } else { tr("Play") },
            "send-enabled": self.send_enabled(), "clear-enabled": sent, "nav-enabled": sent,
            "status": status, "status-kind": if kind == "dim" { "status_dim" } else { kind },
        })
    }

    /// Every property the card has.
    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        for part in [self.sets_props(), self.state_props()] {
            if let (Value::Object(a), Value::Object(p)) = (&mut all, part) {
                a.extend(p);
            }
        }
        all
    }

    fn show(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.state_props());
    }

    /// The set picker and the editor, after the sets or the open one changed.
    fn fill_sets(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.sets_props());
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.all_props());
        }
    }

    // ── Talking to the phone ──────────────────────────────────────────

    fn control(&self, host: &mut dyn Host, body: Value) {
        host.phone_control(self.ctrl.as_deref().filter(|c| !c.is_empty()), &body);
    }

    /// The scroll controls as the phone gets them.
    fn style(&self) -> Value {
        let auto = self.scroll_auto;
        json!({"action": "notes_style", "size": self.size, "speed": if auto { self.speed } else { 0 }, "playing": i32::from(auto && self.playing)})
    }

    fn send_style(&self, host: &mut dyn Host) {
        if self.is_sent() {
            self.control(host, self.style());
        }
    }

    /// The Play button changing: its text, and the phone hears it.
    fn set_playing(&mut self, host: &mut dyn Host, playing: bool) {
        if self.playing == playing {
            return;
        }
        self.playing = playing;
        self.send_style(host);
    }

    fn send(&mut self, host: &mut dyn Host) {
        let text = self.text().to_string();
        if !self.sendable() || body_size(&text) > NOTES_MAX_BYTES {
            return;
        }
        if py_strip(&text).is_empty() {
            self.clear(host);
            return;
        }
        self.set_playing(host, false); // starts at the top, waiting for Play
        self.control(host, self.style());
        self.control(host, json!({"action": "notes", "text": text}));
        let now = self.now();
        if let Some(ctrl) = self.ctrl.clone() {
            self.sent.insert(ctrl, (text, now));
        }
        self.shown = true; // until the phone says otherwise; it opens them right away when Telescope is on screen
        self.closed = false;
        self.preview = false;
        host.schedule(POLL_MS, T_POLL);
        host.schedule(FIRST_POLL_MS, T_FIRST_POLL);
        self.show(host);
    }

    fn clear(&mut self, host: &mut dyn Host) {
        if self.sendable() {
            self.control(host, json!({"action": "notes", "text": ""}));
        }
        self.set_playing(host, false);
        if let Some(ctrl) = &self.ctrl {
            self.sent.remove(ctrl);
        }
        self.shown = false;
        self.closed = false;
        self.preview = false;
        self.show(host);
    }

    fn move_by(&mut self, host: &mut dyn Host, by: i64) {
        if self.is_sent() {
            self.control(host, json!({"action": "notes_scroll", "by": by}));
        }
    }

    // ── Saved sets ────────────────────────────────────────────────────

    fn pick_set(&mut self, host: &mut dyn Host, index: usize) {
        if index < self.sets.len() && index != self.current {
            self.current = index;
            self.fill_sets(host);
            self.show(host);
            host.schedule_save();
        }
    }

    fn new_set(&mut self, host: &mut dyn Host) {
        if self.sets.len() >= MAX_SETS {
            return;
        }
        self.sets.push(String::new());
        self.current = self.sets.len() - 1;
        self.fill_sets(host);
        self.show(host);
        host.schedule_save();
    }

    fn delete_set(&mut self, host: &mut dyn Host) {
        self.sets.remove(self.current);
        if self.sets.is_empty() {
            self.sets.push(String::new());
        }
        self.current = self.current.min(self.sets.len() - 1);
        self.fill_sets(host);
        self.show(host);
        host.schedule_save();
    }

    fn edited(&mut self, host: &mut dyn Host, text: &str) {
        self.sets[self.current] = text.to_string();
        // The editor already shows it: only the picker's names and the buttons change
        host.set_ui_props("card", &{
            let mut p = self.sets_props();
            if let Value::Object(o) = &mut p {
                o.remove("text");
            }
            p
        });
        self.show(host);
        host.schedule_save();
    }

    // ── Handlers ──────────────────────────────────────────────────────

    fn set_scroll(&mut self, host: &mut dyn Host, auto: bool) {
        self.scroll_auto = auto;
        if !auto {
            self.set_playing(host, false);
        }
        self.send_style(host);
        self.show(host);
        host.schedule_save();
    }

    fn set_size(&mut self, host: &mut dyn Host, value: i64) {
        self.size = value.clamp(SIZE_MIN, SIZE_MAX);
        self.send_style(host);
        self.show(host);
        host.schedule_save();
    }

    fn set_speed(&mut self, host: &mut dyn Host, value: i64) {
        self.speed = value.clamp(1, SPEED_MAX);
        self.send_style(host);
        self.show(host);
        host.schedule_save();
    }

    fn forget_stopped(&mut self, host: &mut dyn Host) {
        let mut running: Vec<String> = host.background_streams().into_iter().map(|b| b.source_id).collect();
        if host.is_streaming() {
            running.push(host.focused_source_id().unwrap_or_default());
        }
        self.sent.retain(|c, _| running.contains(c));
    }

    /// Asks the phone for its state, which comes back through on_phone_state; nothing to follow until notes are sent.
    fn poll(&mut self, host: &mut dyn Host) {
        let Some(ctrl) = self.ctrl.clone() else { return };
        if self.takes_notes.is_some() && !self.sent.contains_key(&ctrl) {
            return;
        }
        host.request_phone_state(Some(&ctrl));
    }

    fn leave_stream(&mut self, host: &mut dyn Host) {
        host.cancel_schedule(T_POLL);
        host.cancel_schedule(T_FIRST_POLL);
        self.ctrl = None;
        self.takes_notes = None;
        self.front = None;
        self.show(host);
    }

    fn read_state(&mut self, host: &mut dyn Host, state: &Value) {
        if state.get("cameras").is_none() {
            return; // not a whole state
        }
        self.takes_notes = Some(truthy(state.get("takes_notes")));
        self.front = front_facing(state);
        if self.is_sent() {
            self.shown = truthy(state.get("notes_shown"));
            self.closed = truthy(state.get("notes_closed")); // older phones leave these out
            self.preview = truthy(state.get("notes_preview"));
            let lost = self.takes_notes == Some(true) && !truthy(state.get("notes"));
            let ctrl = self.ctrl.clone().unwrap_or_default();
            let (text, sent_at) = self.sent[&ctrl].clone();
            if lost && self.now() - sent_at > RESEND_AFTER_S {
                self.control(host, self.style());
                self.control(host, json!({"action": "notes", "text": text}));
                let now = self.now();
                self.sent.insert(ctrl, (text, now));
            }
        }
        self.show(host);
    }
}

impl Plugin for NotesPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Notes").into(),
            about: n_("Shows your notes on the phone's screen right under the camera, to read while looking people in the eye.").into(),
            when_off: n_("Without it there's no Notes card and the phone never shows notes.").into(),
            category: "on_screen".into(),
            icon: "notes".into(),
            optional: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.all_props());
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::SourceSelected { source_id } => {
                self.source = source_id.clone();
                self.show(host);
            }
            Event::CameraSwitched { camera } => {
                if let Some(facing) = camera.get("facing").and_then(Value::as_str).filter(|f| !f.is_empty()) {
                    self.front = Some(facing == "front");
                    self.show(host);
                }
            }
            Event::StreamsChanged { .. } => self.forget_stopped(host),
            Event::TimerFired { owner, token } if owner == NAME && self.ctrl.is_some() => {
                if token == T_POLL {
                    host.schedule(POLL_MS, T_POLL);
                }
                if token == T_POLL || token == T_FIRST_POLL {
                    self.poll(host);
                }
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "NotesCard".into(), props: self.all_props() }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let b = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        match event.callback.as_str() {
            "set-picked" => self.pick_set(host, n(0).max(0.0) as usize),
            "new-clicked" => self.new_set(host),
            "delete-clicked" => self.delete_set(host),
            "text-edited" => {
                let text = event.args.first().and_then(Value::as_str).unwrap_or("").to_string();
                self.edited(host, &text);
            }
            "size-moved" => self.set_size(host, n(0).round() as i64),
            "scroll-picked" => self.set_scroll(host, n(0) as usize == 0),
            "speed-moved" => self.set_speed(host, n(0).round() as i64),
            "back-clicked" => self.move_by(host, -1),
            "next-clicked" => self.move_by(host, 1),
            "play-toggled" => {
                self.playing = b(0);
                self.show(host);
                self.send_style(host);
            }
            "send-clicked" => self.send(host),
            "clear-clicked" => self.clear(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Notes");
        let click = |id: &str, label: String| ActionSpec { id: id.into(), label, group: group.clone(), params: Vec::new(), repeats: false, on_release: false, current: None, keywords: String::new() };
        let speed_label = tr("Notes scroll speed");
        let speed = SliderSpec { id: "notes.speed", label: &speed_label, group: &group, keywords: "", minimum: 1.0, maximum: SPEED_MAX as f64, step: 1.0, decimals: 0, suffix: "", has_default: true };
        vec![
            click("notes.send", tr("Send notes")),
            click("notes.clear", tr("Clear notes")),
            click("notes.next", tr("Next line")),
            click("notes.previous", tr("Previous line")),
            actions::toggle_action("notes.play", &tr("Scroll notes"), &group, "", &[]),
            actions::slider_action(&speed, self.speed as f64),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        let sent = self.is_sent();
        match id {
            "notes.send" if self.send_enabled() => {
                self.send(host);
                Some(tr("Send notes"))
            }
            "notes.clear" if sent => {
                self.clear(host);
                Some(tr("Clear notes"))
            }
            "notes.next" if sent => {
                self.move_by(host, 1);
                Some(tr("Next line"))
            }
            "notes.previous" if sent => {
                self.move_by(host, -1);
                Some(tr("Previous line"))
            }
            "notes.play" if sent && self.scroll_auto => {
                let want = actions::toggle_wanted(params, self.playing, &mut self.held);
                if want != self.playing {
                    self.playing = want;
                    self.show(host);
                    self.send_style(host);
                }
                let state = if self.playing { tr("playing") } else { tr("paused") };
                Some(tr("{label}: {value}").field("label", tr("Scroll notes")).field("value", &state))
            }
            "notes.speed" if self.scroll_auto => {
                let target = actions::slider_target(params, self.speed, 1.0, 1, SPEED_MAX, Some(SPEED_DEFAULT))?;
                self.set_speed(host, target);
                Some(actions::slider_text(&tr("Notes scroll speed"), &self.speed.to_string()))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "notes.play" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.playing && self.is_sent() && self.scroll_auto {
                self.playing = before;
                self.show(host);
                self.send_style(host);
            }
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.ctrl = Some(stream.source_id.clone());
        host.schedule(POLL_MS, T_POLL);
        self.show(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        // The phone takes the notes down when its stream ends
        let had = self.ctrl.as_ref().is_some_and(|c| self.sent.remove(c).is_some());
        if had {
            self.playing = false;
        }
        self.shown = false;
        self.leave_stream(host);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        // The phone still shows whatever it had: nothing to send again
        self.ctrl = Some(stream.source_id.clone());
        self.takes_notes = None;
        self.front = None;
        host.schedule(POLL_MS, T_POLL);
        self.show(host);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        self.leave_stream(host);
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        self.catch_up(host);
        self.read_state(host, state);
    }

    fn get_config(&self) -> Value {
        json!({"sets": self.sets, "current": self.current, "size": self.size, "speed": self.speed, "scroll": if self.scroll_auto { "auto" } else { "hand" }})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let sets: Vec<String> = match cfg.get("sets") {
            Some(Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(String::from)).take(MAX_SETS).collect(),
            _ => Vec::new(),
        };
        self.sets = if sets.is_empty() { vec![String::new()] } else { sets };
        self.current = match cfg.get("current") {
            Some(Value::Number(n)) => n.as_u64().map(|c| c as usize).filter(|c| *c < self.sets.len()).unwrap_or(0),
            // Python's bool is an int
            Some(Value::Bool(true)) if self.sets.len() > 1 => 1,
            _ => 0,
        };
        let get = |k: &str| cfg.get(k).cloned().unwrap_or(Value::Null);
        self.size = clamped_int(&get("size"), SIZE_DEFAULT, SIZE_MIN, SIZE_MAX);
        self.speed = clamped_int(&get("speed"), SPEED_DEFAULT, 1, SPEED_MAX);
        self.scroll_auto = cfg.get("scroll").and_then(Value::as_str) != Some("hand");
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let on = match self.takes_notes {
            None => "not known",
            Some(true) => "yes",
            Some(false) => "phone too old",
        };
        vec![
            ("Notes".into(), if self.is_sent() { "on the phone" } else { "not sent" }.into()),
            ("Notes on phone".into(), on.into()),
        ]
    }
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
