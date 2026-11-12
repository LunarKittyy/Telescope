//! The Privacy blur plugin (plugins/privacy_blur.py): when more than one person is in the picture, blurs all of it or puts the wait screen up instead. Kept as small as it can be, so anything it gets wrong is the people detection's doing.

use crate::cpu::Rgb;
use crate::shared::Paired;
use crate::vision_row;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, Touch};
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr};
use telescope_core::image::{planes_up_to_420, rgb_planes, small_rgb, spare_bytes, with_spares};
use telescope_core::frame::PixelFormat;
use telescope_core::people;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::pose::Person;
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "privacy_blur";
/// What it asks the host and the Wait screen for things under.
const TITLE: &str = n_("Privacy blur");
/// Under the name on the wait screen while it covers the picture, so it doesn't read as a broken camera.
const COVER_NOTE: &str = n_("Paused for privacy");
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/privacy_blur.slint"));
/// What happens while more than one person is in the picture: all of it gets blurred, or the wait screen goes up in place of it.
pub const MODES: [(&str, &str); 2] = [("blur", n_("Blur the whole picture")), ("wait", n_("Show the wait screen"))];
/// The modes a key binding may still name from before they went: they mean the whole picture, as a saved mode does.
const GONE: [&str; 2] = ["Blur the others", "Blur all but me"];
/// Blurring stops once there's been one person (or nobody) in every update for this long, in seconds. People are counted raw, so a second person the model loses for a moment shouldn't end it, and four seconds still feels quick once they've gone.
pub const OFF_S: f64 = 4.0;
/// The blur is worked out at a sixteenth of the size.
const SMALL: usize = 16;

/// When to blur, from how many people the latest update found: on at the first update that says two or more, off once it has said one or none for OFF_S.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trigger {
    pub on: bool,
    calm_since: Option<f64>,
}

impl Trigger {
    pub fn update(&mut self, count: u32, now: f64) -> bool {
        if count >= 2 {
            self.on = true;
            self.calm_since = None;
        } else if self.on {
            let since = *self.calm_since.get_or_insert(now);
            if now - since >= OFF_S {
                self.on = false;
                self.calm_since = None;
            }
        }
        self.on
    }

    pub fn reset(&mut self) {
        *self = Trigger::default();
    }
}

/// How many of these people have a usable box: what the model found, not what tracking has made steady.
pub fn found_in(people: &[Value]) -> u32 {
    people.iter().filter_map(Person::from_value).filter(|p| people::box_of(p).is_some()).count() as u32
}

/// frame blurred past recognising anyone, at a sixteenth of the size and scaled back up.
pub fn blurred(frame: &Rgb) -> Rgb {
    frame.shrink((frame.w / SMALL).max(1), (frame.h / SMALL).max(1)).blur(2.0).linear_exact(frame.w, frame.h)
}

/// `blurred` for a frame in I420, done on its planes: the frame is shrunk and blurred small as a picture and its planes scaled back up into the new frame, so no RGB picture is made at the frame's size. False for a frame in another format, which the caller blurs as RGB.
fn blur_planes(frame: &mut Frame) -> bool {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if frame.format != PixelFormat::I420 || w < 2 || h < 2 || frame.i420_planes().is_none() {
        return false;
    }
    let (sw, sh) = ((w / SMALL).max(1), (h / SMALL).max(1));
    let Some(small) = small_rgb(frame, sw, sh, false) else { return false };
    let small = Rgb { w: sw, h: sh, data: small }.blur(2.0);
    let mut out = spare_bytes(frame.data.len());
    planes_up_to_420(&rgb_planes(&small.data), (sw, sh), &mut out, (w, h));
    frame.replace_data(out);
    true
}

/// The step every stream gets: what it was handed out with stays when the panels move away, and a stream left behind blurs whenever the shown one does.
struct Step {
    active: bool,
    covered: bool,
    shown_blurs: Arc<AtomicBool>,
    spares: Vec<Vec<u8>>,
}

impl CpuStep for Step {
    fn hides(&self) -> bool {
        true
    }

    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        if self.active || self.shown_blurs.load(Ordering::Relaxed) { Touch::Changes } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        // the wait screen goes over the picture further on, so there's nothing to blur while it's up
        let blurs = if self.active { !self.covered } else { self.shown_blurs.load(Ordering::Relaxed) };
        if !blurs {
            return;
        }
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || {
            if blur_planes(frame) {
                return;
            }
            if let Some(rgb) = Rgb::from_frame(frame) {
                blurred(&rgb).write_to(frame);
            }
        });
        self.spares = spares;
    }
}

pub struct PrivacyBlurPlugin {
    trigger: Trigger,
    active: bool,
    auto: bool,
    now: bool,
    mode: usize,
    /// Asked for people (the switch is on, or Blur now is down).
    wanting: bool,
    behind: HashMap<String, Trigger>,
    covered: bool,
    asked_cover: bool,
    shown_blurs: Arc<AtomicBool>,
    held: HashMap<String, Option<bool>>,
    paired: Paired,
    last_props: Option<Value>,
    /// set_config has no host: the rest of it happens with the next hook that has one.
    pending: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for PrivacyBlurPlugin {
    fn default() -> Self {
        PrivacyBlurPlugin::new()
    }
}

impl PrivacyBlurPlugin {
    pub fn new() -> PrivacyBlurPlugin {
        PrivacyBlurPlugin::with_clock(Box::new(|| monotonic_ns() as f64 / 1e9))
    }

    /// With the clock that says how long people have been gone, in seconds.
    pub fn with_clock(clock: Box<dyn Fn() -> f64 + Send>) -> PrivacyBlurPlugin {
        PrivacyBlurPlugin {
            trigger: Trigger::default(),
            active: false,
            auto: false,
            now: false,
            mode: 0,
            wanting: false,
            behind: HashMap::new(),
            covered: false,
            asked_cover: false,
            shown_blurs: Arc::new(AtomicBool::new(false)),
            held: HashMap::new(),
            paired: Paired::default(),
            last_props: None,
            pending: true,
            clock,
        }
    }

    /// Whether the wait screen is wanted now.
    fn wants_cover(&self) -> bool {
        self.active && MODES[self.mode].0 == "wait"
    }

    /// The part of update that needs no host: whether frames get blurred now, from the switches and the people count.
    fn work_out(&mut self) {
        self.active = self.now || (self.auto && self.trigger.on);
    }

    /// Frames, what's asked of others, and the card.
    fn update(&mut self, host: &mut dyn Host) {
        let want = self.auto || self.now;
        if want != self.wanting {
            self.wanting = want;
            host.want_people(TITLE, want);
        }
        let before = (self.active, self.covered && self.wants_cover());
        self.work_out();
        self.shown_blurs.store(self.active, Ordering::Relaxed);
        if before != (self.active, self.covered && self.wants_cover()) {
            host.refresh_frame_step();
        }
        let want_cover = self.wants_cover();
        if want_cover != self.asked_cover {
            self.asked_cover = want_cover;
            // Wait screen answers through stream_covered
            host.emit(Event::CoverRequested { owner: TITLE.into(), on: want_cover, note: want_cover.then(|| tr(COVER_NOTE)) });
        }
        self.show(host);
    }

    fn show(&mut self, host: &mut dyn Host) {
        let problem = if self.wants_cover() && !self.covered { tr("With Wait screen switched off in Plugins the whole picture gets blurred instead.") } else { String::new() };
        let mut props = self.all_props();
        if let (Value::Object(p), Value::Object(row)) = (&mut props, vision_row::props(&*host, self.auto || !problem.is_empty(), &problem)) {
            p.extend(row);
        }
        // people come in a dozen times a second and rarely change any of this
        if self.last_props.as_ref() != Some(&props) {
            host.set_ui_props("card", &props);
            self.last_props = Some(props);
        }
    }

    fn all_props(&self) -> Value {
        json!({"active": self.paired.on(), "on": self.auto, "mode": self.mode, "now": self.now})
    }

    fn fixed_props() -> Value {
        json!({
            "switch-help": tr("Blur automatically when more than one person is in the picture."),
            "mode-tip": tr("What happens while more than one person is in the picture."),
            "now-tip": tr("Blur the picture until you click again."),
        })
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.update(host);
        }
    }

    fn set_trigger(&mut self, host: &mut dyn Host, trigger: Trigger) {
        self.trigger = trigger;
        self.update(host);
    }

    fn set_auto(&mut self, host: &mut dyn Host, on: bool) {
        if on != self.auto {
            self.auto = on;
            self.trigger.reset();
        }
        self.update(host);
    }

    fn set_mode(&mut self, host: &mut dyn Host, mode: usize) {
        self.mode = mode;
        host.schedule_save();
        self.update(host);
    }

    fn set_now(&mut self, host: &mut dyn Host, on: bool) {
        self.now = on;
        self.update(host);
    }

    /// A key on one of the two checkable controls (button_action): on, off, toggled or held.
    fn toggled(&mut self, host: &mut dyn Host, id: &str, label: &str, params: &Value, on: bool, set: fn(&mut Self, &mut dyn Host, bool)) -> Option<String> {
        let want = actions::toggle_wanted(params, on, self.held.entry(id.to_string()).or_default());
        if want != on {
            set(self, host, want);
        }
        Some(actions::toggle_text(label, want))
    }

    fn set_switch(&mut self, host: &mut dyn Host, on: bool) {
        host.schedule_save();
        self.set_auto(host, on);
    }
}

impl Plugin for PrivacyBlurPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: TITLE.into(),
            about: n_("Blurs the whole picture, or puts the wait screen up, when more than one person is in it. It only knows about the people the vision models find, so it can miss someone and can't guarantee privacy.").into(),
            when_off: n_("Without it there's no Privacy blur card and nothing gets hidden when someone walks in.").into(),
            category: "vision".into(),
            icon: "eye_off".into(),
            optional: true,
            needs_vision: true,
            in_presets: true,
            frame_stage: FrameStage::Privacy,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &PrivacyBlurPlugin::fixed_props());
        self.update(host);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.asked_cover {
            host.emit(Event::CoverRequested { owner: TITLE.into(), on: false, note: None });
        }
        if self.wanting {
            host.want_people(TITLE, false);
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if self.paired.hear(event).is_some() || vision_row::changes_it(event) {
            self.show(host);
        }
        match event {
            // the model's own count of this update, not the steadied one in PeopleTracked
            Event::PeopleSeen { source_id, people } if *source_id == host.focused_source_id().unwrap_or_default() => {
                if self.auto {
                    let now = (self.clock)();
                    self.trigger.update(found_in(people), now);
                }
                self.update(host);
            }
            Event::StreamCovered { covered } => {
                self.covered = *covered;
                self.update(host);
            }
            _ => {}
        }
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        // The stream carries on blurred (frame_step), and coming back to it picks up where it was
        self.catch_up(host);
        self.behind.insert(host.focused_source_id().unwrap_or_default(), self.trigger.clone());
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        let sid = host.focused_source_id().unwrap_or_default();
        if let Some(kept) = self.behind.remove(&sid) {
            self.set_trigger(host, kept);
            return;
        }
        let (_, count) = host.tracked_people(Some(&sid));
        let mut trigger = Trigger::default();
        if self.auto {
            trigger.update(count, (self.clock)());
        }
        self.set_trigger(host, trigger);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.behind.remove(&host.focused_source_id().unwrap_or_default());
        if !host.is_streaming() {
            self.behind.clear();
        }
        self.set_trigger(host, Trigger::default());
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = PrivacyBlurPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props()) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "PrivacyBlurCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_switch(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "mode-picked" => {
                let picked = event.args.first().and_then(Value::as_f64).unwrap_or(0.0) as usize;
                if picked < MODES.len() {
                    self.set_mode(host, picked);
                }
            }
            "now-clicked" => self.set_now(host, !self.now),
            "download-clicked" => {
                host.download_vision();
                self.show(host);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Privacy blur");
        // saved by their English text, so a binding made in one language works in another
        let modes: Vec<(&str, String)> = MODES.iter().map(|(_, t)| (*t, tr(t))).collect();
        vec![
            actions::toggle_action("privacy_blur.now", &tr("Blur now"), &group, "", &[]),
            actions::toggle_action("privacy_blur.auto", &tr("Privacy blur when someone walks in"), &group, "", &[]),
            actions::choice_action("privacy_blur.mode", &tr("Privacy blur: what happens"), &group, "", &modes),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        match id {
            "privacy_blur.now" => self.toggled(host, id, &tr("Blur now"), params, self.now, Self::set_now),
            "privacy_blur.auto" => self.toggled(host, id, &tr("Privacy blur when someone walks in"), params, self.auto, Self::set_switch),
            "privacy_blur.mode" => {
                let mut params = params.clone();
                if params.get("option").and_then(Value::as_str).is_some_and(|o| GONE.contains(&o)) {
                    params["option"] = json!(MODES[0].1);
                }
                let shown: Vec<String> = MODES.iter().map(|(_, t)| tr(t)).collect();
                let options: Vec<Choice> = MODES.iter().zip(&shown).map(|((_, t), s)| Choice { value: t, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(&params, &options, Some(self.mode), true)?;
                if pick {
                    self.set_mode(host, said);
                }
                Some(actions::slider_text(&tr("Privacy blur: what happens"), &shown[said]))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        let Some(held) = self.held.get_mut(id) else { return };
        let Some(before) = actions::hold_released(params, held) else { return };
        if !self.paired.on() {
            return;
        }
        match id {
            "privacy_blur.now" if before != self.now => self.set_now(host, before),
            "privacy_blur.auto" if before != self.auto => self.set_switch(host, before),
            _ => {}
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        Some(FrameStep::Cpu(Box::new(Step { spares: Vec::new(), active: self.active, covered: self.covered && self.wants_cover(), shown_blurs: self.shown_blurs.clone() })))
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("on".into(), json!(self.auto));
        cfg.insert("mode".into(), json!(MODES[self.mode].0));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        // a saved mode that's gone (Blur the others, Blur all but me) means the whole picture
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(|m| MODES.iter().position(|(k, _)| *k == m)).unwrap_or(0);
        let on = cfg.get("on") == Some(&Value::Bool(true));
        if on != self.auto {
            self.auto = on;
            self.trigger.reset();
        }
        self.work_out();
        self.shown_blurs.store(self.active, Ordering::Relaxed);
        // what it asks of the others and the card: the next hook with a host
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.catch_up(host);
        host.refresh_frame_step();
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let state = if self.active { "blurring" } else if self.auto { "on" } else { "off" };
        vec![("Privacy blur".into(), format!("{state}, {}", MODES[self.mode].0))]
    }
}

#[cfg(test)]
#[path = "privacy_blur_tests.rs"]
mod tests;
