//! The Privacy blur plugin (plugins/privacy_blur.py): when someone else shows up, blurs everyone but you, the whole background, or puts the wait screen up instead. A CPU step for the same reason as Background: the person mask only reaches FrameContext.

use crate::cpu::Rgb;
use crate::shared::mask_of;
use crate::imgops::{blur_f32, compose, Gray};
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
use telescope_core::image::with_spares;
use telescope_core::people::{self, Box4, Track};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{upright_to_shown, Geometry};

pub const NAME: &str = "privacy_blur";
/// What it asks the host and the Wait screen for things under.
const TITLE: &str = n_("Privacy blur");
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/privacy_blur.slint"));
/// What happens while someone else is in the picture: the other people get blurred, everything but you gets blurred, or the wait screen goes up in place of the whole picture.
pub const MODES: [(&str, &str); 3] = [("others", n_("Blur the others")), ("background", n_("Blur all but me")), ("wait", n_("Show the wait screen"))];
/// Blurring stops once there's been one person (or nobody) for this long.
pub const OFF_S: f64 = 5.0;
/// A person's box from their landmarks grows by this much of its size each way (arms, a chair).
const GROW: f64 = 0.4;
/// And upwards by this much of its width more, since the landmarks stop at the eyes.
const HEAD: f64 = 1.0;
/// Someone else's box grown this little (each way, and upwards) is kept out of yours.
const NEAR: (f64, f64) = (0.15, 0.5);
/// The blur is worked out at a sixteenth of the size.
const SMALL: usize = 16;
/// The edge of a blurred box fades over this much of the picture.
const EDGE: f64 = 0.04;

/// When to blur, from the steady people count: on as soon as it says two or more (core::people only counts someone new after a couple of updates in a row), off once it has said one or none for OFF_S.
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

/// What a frame step needs, frozen: handed over whole with each new step.
#[derive(Clone, Debug, PartialEq)]
struct Blur {
    mode: &'static str,
    active: bool,
    /// Your box, in the picture as shown at zoom 1.
    you: Option<Box4>,
    /// Everyone else's.
    others: Vec<Box4>,
    /// The wait screen is up in place of the picture.
    covered: bool,
    /// You were found when blurring started but aren't now: everyone in the picture is someone else.
    away: bool,
}

impl Default for Blur {
    fn default() -> Self {
        Blur { mode: MODES[0].0, active: false, you: None, others: Vec::new(), covered: false, away: false }
    }
}

pub fn grown(b: Box4, by: f64, head: f64) -> Box4 {
    let (w, h) = (b.2 - b.0, b.3 - b.1);
    (b.0 - w * by, b.1 - h * by - w * head, b.2 + w * by, b.3 + h * by)
}

/// box from the picture as shown at zoom 1 into the frame as shown now (0..1, not clipped).
pub fn shown_box(b: Box4, view: &Geometry) -> Box4 {
    let corners = [(b.0, b.1), (b.0, b.3), (b.2, b.1), (b.2, b.3)].map(|(x, y)| upright_to_shown(view, x, y));
    let fold = |f: fn(f64, f64) -> f64, pick: fn(&(f64, f64)) -> f64| corners.iter().map(pick).reduce(f).unwrap();
    (fold(f64::min, |p| p.0), fold(f64::min, |p| p.1), fold(f64::max, |p| p.0), fold(f64::max, |p| p.1))
}

/// frame blurred past recognising anyone, at a sixteenth of the size and scaled back up.
pub fn blurred(frame: &Rgb) -> Rgb {
    frame.shrink((frame.w / SMALL).max(1), (frame.h / SMALL).max(1)).blur(2.0).linear_exact(frame.w, frame.h)
}

/// 0..1 at a sixteenth of the frame: where its blurred copy goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Weight {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl Weight {
    fn from_mask(mask: &Gray, w: usize, h: usize) -> Weight {
        Weight { w, h, data: mask.shrink(w, h).data.iter().map(|m| *m as f32 / 255.0).collect() }
    }

    fn with(mut self, other: &Weight, f: impl Fn(f32, f32) -> f32) -> Weight {
        self.data.iter_mut().zip(&other.data).for_each(|(a, b)| *a = f(*a, *b));
        self
    }

    fn map(mut self, f: impl Fn(f32) -> f32) -> Weight {
        self.data.iter_mut().for_each(|a| *a = f(*a));
        self
    }
}

/// np.maximum over float32: a NaN can't come up here.
fn most(a: f32, b: f32) -> f32 {
    if a >= b { a } else { b }
}

/// 1 inside any of boxes (0..1 of the frame of w x h), fading out at the edges.
pub fn boxes_mask(boxes: &[Box4], w: usize, h: usize) -> Weight {
    let (sw, sh) = ((w / SMALL).max(1), (h / SMALL).max(1));
    let mut mask = vec![0f32; sw * sh];
    for b in boxes {
        let (c0, c1) = (((b.0 * sw as f64) as i64).max(0), ((b.2 * sw as f64).ceil() as i64).min(sw as i64));
        let (r0, r1) = (((b.1 * sh as f64) as i64).max(0), ((b.3 * sh as f64).ceil() as i64).min(sh as i64));
        if c1 > c0 && r1 > r0 {
            for r in r0 as usize..r1 as usize {
                mask[r * sw + c0 as usize..r * sw + c1 as usize].fill(1.0);
            }
        }
    }
    Weight { w: sw, h: sh, data: blur_f32(&mask, sw, sh, 1, (EDGE * sw as f64).max(0.5)) }
}

/// frame with its blurred copy laid over it by weight; None leaves the frame as it is.
pub fn laid_over(frame: &Rgb, weight: &Weight) -> Option<Rgb> {
    if weight.data.iter().copied().fold(f32::NEG_INFINITY, f32::max) < 0.01 {
        return None;
    }
    let bytes = weight.data.iter().map(|v| (v * 255.0).clamp(0.0, 255.0) as u8).collect();
    let mask = Gray { w: weight.w, h: weight.h, data: bytes }.linear_exact(frame.w, frame.h);
    Some(compose(&blurred(frame), &mask, frame))
}

fn replaced(frame: &mut Frame, out: Option<Rgb>) {
    if let Some(out) = out {
        out.write_to(frame);
    }
}

fn apply(frame: &mut Frame, blur: &Blur, view: &Geometry, ctx: &dyn FrameContext) {
    if !blur.active || blur.covered {
        return;
    }
    let Some(rgb) = Rgb::from_frame(frame) else { return };
    let (w, h) = (rgb.w, rgb.h);
    let (sw, sh) = ((w / SMALL).max(1), (h / SMALL).max(1));
    let boxes = |list: &[Box4], by: f64, head: f64| {
        let shown: Vec<Box4> = list.iter().map(|b| shown_box(grown(*b, by, head), view)).collect();
        boxes_mask(&shown, w, h)
    };
    if blur.away && blur.mode == "others" {
        let boxed = (!blur.others.is_empty()).then(|| boxes(&blur.others, GROW, HEAD));
        let found = mask_of(ctx, frame).map(|m| Weight::from_mask(&m, sw, sh));
        let weight = match (boxed, found) {
            (Some(b), Some(p)) => Some(b.with(&p, most)),
            (b, p) => b.or(p),
        };
        if let Some(weight) = weight {
            // you're away: whoever is there gets blurred, all of them
            replaced(frame, laid_over(&rgb, &weight));
            return;
        }
    }
    let Some(you_box) = blur.you.filter(|_| blur.mode != "wait") else {
        // nobody to go by (by hand without the vision download), or no wait screen to show
        blurred(&rgb).write_to(frame);
        return;
    };
    let mut you = boxes(&[you_box], GROW, HEAD);
    let found = mask_of(ctx, frame).map(|m| Weight::from_mask(&m, sw, sh));
    if let Some(p) = &found {
        // you: the person mask inside your box
        you = p.clone().with(&you.map(|v| (v * 2.0).min(1.0)), |a, b| a * b);
    }
    if !blur.others.is_empty() {
        // but not someone sitting right next to you
        you = you.with(&boxes(&blur.others, NEAR.0, NEAR.1), |a, b| a * (1.0 - b));
    }
    let weight = if blur.mode == "others" && !blur.others.is_empty() {
        let mut weight = boxes(&blur.others, GROW, HEAD);
        if let Some(p) = &found {
            // anyone the mask finds who isn't you, the whole of them
            weight = weight.with(p, most);
        }
        weight.with(&you, |a, b| a * (1.0 - b))
    } else {
        // all but you (also others with nobody else found yet, from Blur now)
        you.map(|v| 1.0 - v)
    };
    replaced(frame, laid_over(&rgb, &weight));
}

/// The step every stream gets: the blur and view it was handed out with stay when the panels move away, and a stream left behind blurs whenever the shown one does.
struct Step {
    blur: Blur,
    view: Geometry,
    shown_blurs: Arc<AtomicBool>,
    spares: Vec<Vec<u8>>,
}

impl CpuStep for Step {
    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        if self.blur.active || self.shown_blurs.load(Ordering::Relaxed) { Touch::Changes } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame, ctx));
        self.spares = spares;
    }
}

impl Step {
    fn run(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        if self.blur.active {
            // a stream left blurred stays blurred behind the panels
            apply(frame, &self.blur, &self.view, ctx);
        } else if self.shown_blurs.load(Ordering::Relaxed) {
            // the shown camera blurs, so every camera does, all of it (nobody to tell apart here)
            if let Some(rgb) = Rgb::from_frame(frame) {
                blurred(&rgb).write_to(frame);
            }
        }
    }
}

/// What's kept of a stream the panels moved away from, to pick up where it was (_state in Python).
#[derive(Clone, Debug, Default)]
struct Kept {
    trigger: Trigger,
    tracks: Vec<Track>,
    you_id: Option<u64>,
    you_box: Option<Box4>,
    others: Vec<Box4>,
}

pub struct PrivacyBlurPlugin {
    state: Kept,
    blur: Blur,
    auto: bool,
    now: bool,
    mode: usize,
    /// Asked for people (the switch is on, or Blur now is down).
    wanting: bool,
    behind: HashMap<String, Kept>,
    covered: bool,
    asked_cover: bool,
    view: Geometry,
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
            state: Kept::default(),
            blur: Blur::default(),
            auto: false,
            now: false,
            mode: 0,
            wanting: false,
            behind: HashMap::new(),
            covered: false,
            asked_cover: false,
            view: Geometry::default(),
            shown_blurs: Arc::new(AtomicBool::new(false)),
            held: HashMap::new(),
            paired: Paired::default(),
            last_props: None,
            pending: true,
            clock,
        }
    }

    /// You: whoever was there longest when blurring started, while they're still followed. Once their track is gone, whoever is where you were last seen (the model lost you for a while). Anyone else is never you, so None while you're away.
    fn you(&self) -> Option<Track> {
        let tracks = &self.state.tracks;
        let Some(you_box) = self.state.you_box else { return people::main(tracks).cloned() };
        if let Some(held) = tracks.iter().find(|t| Some(t.id) == self.state.you_id) {
            return Some(held.clone());
        }
        // max() keeps the first of equals
        let mut best: Option<&Track> = None;
        for t in tracks {
            if best.is_none_or(|b| people::overlap(t.bbox, you_box) > people::overlap(b.bbox, you_box)) {
                best = Some(t);
            }
        }
        best.filter(|b| people::overlap(b.bbox, you_box) > 0.1).cloned()
    }

    /// Whether the wait screen is wanted now, for what `update` works out.
    fn wants_cover(&self, active: bool) -> bool {
        active && MODES[self.mode].0 == "wait"
    }

    /// The part of _update that needs no host: what frames get now, from the switches and the latest people.
    fn work_out(&mut self) {
        let active = self.now || (self.auto && self.state.trigger.on);
        if !active {
            self.state.you_id = None;
            self.state.you_box = None;
            self.state.others = Vec::new();
        }
        let you = if active { self.you() } else { None };
        let gone = you.is_none();
        if let Some(you) = &you {
            self.state.you_id = Some(you.id);
            self.state.you_box = Some(you.bbox);
        }
        let others: Vec<Box4> = self.state.tracks.iter().filter(|t| you.as_ref().is_none_or(|y| t.id != y.id)).map(|t| t.bbox).collect();
        if active && !others.is_empty() {
            self.state.others = others;
        }
        self.blur = Blur {
            mode: MODES[self.mode].0,
            active,
            you: you.map(|y| y.bbox),
            others: self.state.others.clone(),
            covered: self.covered && self.wants_cover(active),
            away: active && gone && self.state.you_box.is_some(),
        };
    }

    /// _update: frames, what's asked of others, and the card.
    fn update(&mut self, host: &mut dyn Host) {
        let want = self.auto || self.now;
        if want != self.wanting {
            self.wanting = want;
            host.want_people(TITLE, want);
        }
        let before = self.blur.clone();
        let view = Geometry::from(&host.view());
        self.work_out();
        self.shown_blurs.store(self.blur.active, Ordering::Relaxed);
        // the view moves all the time under Auto framing, and only a blur has any use for it
        let moved = view != self.view;
        self.view = view;
        if before != self.blur || (moved && self.blur.active) {
            host.refresh_frame_step();
        }
        let want_cover = self.wants_cover(self.blur.active);
        if want_cover != self.asked_cover {
            self.asked_cover = want_cover;
            // Wait screen answers through stream_covered
            host.emit(Event::CoverRequested { owner: TITLE.into(), on: want_cover });
        }
        self.show(host);
    }

    fn show(&mut self, host: &mut dyn Host) {
        let problem = if self.wants_cover(self.blur.active) && !self.covered { tr("With Wait screen switched off in Plugins the whole picture gets blurred instead.") } else { String::new() };
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
            "switch-help": tr("Blur automatically when a second person shows up"),
            "mode-tip": tr("What happens while someone else is in the picture"),
            "now-tip": tr("Blur by hand until you click again"),
        })
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.update(host);
        }
    }

    fn set_state(&mut self, host: &mut dyn Host, state: Kept) {
        self.state = state;
        self.update(host);
    }

    fn set_auto(&mut self, host: &mut dyn Host, on: bool) {
        if on != self.auto {
            self.auto = on;
            self.state.trigger.reset();
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
            about: n_("Blurs the other people, everything but you, or puts the wait screen up when someone else walks in. A proof of concept that can't guarantee privacy.").into(),
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
            host.emit(Event::CoverRequested { owner: TITLE.into(), on: false });
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
            Event::PeopleTracked { source_id, tracks, count } if *source_id == host.focused_source_id().unwrap_or_default() => {
                self.state.tracks = Track::all_from(tracks);
                if self.auto {
                    let now = (self.clock)();
                    self.state.trigger.update(*count, now);
                }
                self.update(host);
            }
            Event::StreamCovered { covered } => {
                self.covered = *covered;
                self.update(host);
            }
            Event::ViewChanged { .. } => self.update(host),
            _ => {}
        }
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        // The stream carries on blurred (frame_step), and coming back to it picks up where it was
        self.catch_up(host);
        self.behind.insert(host.focused_source_id().unwrap_or_default(), self.state.clone());
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        let sid = host.focused_source_id().unwrap_or_default();
        if let Some(kept) = self.behind.remove(&sid) {
            self.set_state(host, kept);
            return;
        }
        let (tracks, count) = host.tracked_people(Some(&sid));
        let mut trigger = Trigger::default();
        if self.auto {
            trigger.update(count, (self.clock)());
        }
        self.set_state(host, Kept { trigger, tracks: Track::all_from(&tracks), ..Kept::default() });
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.behind.remove(&host.focused_source_id().unwrap_or_default());
        if !host.is_streaming() {
            self.behind.clear();
        }
        self.set_state(host, Kept::default());
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
                let shown: Vec<String> = MODES.iter().map(|(_, t)| tr(t)).collect();
                let options: Vec<Choice> = MODES.iter().zip(&shown).map(|((_, t), s)| Choice { value: t, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(self.mode), true)?;
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
        Some(FrameStep::Cpu(Box::new(Step { spares: Vec::new(), blur: self.blur.clone(), view: self.view, shown_blurs: self.shown_blurs.clone() })))
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("on".into(), json!(self.auto));
        cfg.insert("mode".into(), json!(MODES[self.mode].0));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(|m| MODES.iter().position(|(k, _)| *k == m)).unwrap_or(0);
        let on = cfg.get("on") == Some(&Value::Bool(true));
        if on != self.auto {
            self.auto = on;
            self.state.trigger.reset();
        }
        self.work_out();
        self.shown_blurs.store(self.blur.active, Ordering::Relaxed);
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
        let state = if self.blur.active { "blurring" } else if self.auto { "on" } else { "off" };
        vec![("Privacy blur".into(), format!("{state}, {}", MODES[self.mode].0))]
    }
}

#[cfg(test)]
#[path = "privacy_blur_tests.rs"]
mod tests;
