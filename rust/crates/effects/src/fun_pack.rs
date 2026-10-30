//! The Fun pack plugin (plugins/fun_pack.py): silly effects on the picture. Game Boy is a shader (game_boy.wgsl). The others work
//! the picture over in a CPU step (fun_effects): they keep a trail, a few frames back or a mask between frames, or draw lines, which
//! a shader can't, and Python does them with OpenCV on the stream's thread too. Datamosh changes the phone's H.264 packets (mosh.rs).
//!
//! The empty room and the clones are taken from the next frame the shown stream makes, so the step puts what it took in a mailbox
//! and posts to the plugin, as Python's worker thread signals the card.

use crate::cpu::Rgb;
use crate::fun_effects::{self as fx, Clones, Effect, Inputs, Look, PlateMask};
use crate::imgops::Gray;
use crate::mosh::{Mosh, MosherFactory};
use crate::shared::{mask_of, Paired};
use crate::vision_row;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStep, ShaderStep};
use telescope_core::host::{Host, Poster, View};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::pose::{Person, BONES, SEEN};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{upright_to_shown, Geometry};

pub const NAME: &str = "fun_pack";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/fun_pack.slint"));
const GAME_BOY: &str = include_str!("game_boy.wgsl");
const T_TICK: &str = "tick";
const T_TAKEN: &str = "taken";
const COUNTDOWN_S: i64 = 3;

pub const EFFECTS: [(&str, &str); 12] = [
    ("off", n_("Off")),
    ("clones", n_("Clone party")),
    ("cloak", n_("Invisibility cloak")),
    ("light", n_("Light painting")),
    ("slit", n_("Slit-scan")),
    ("stick", n_("Stick figure")),
    ("game_boy", n_("Game Boy")),
    ("webcam", n_("1998 webcam")),
    ("vhs", n_("VHS")),
    ("crt", n_("CRT")),
    ("ascii", n_("ASCII")),
    ("mosh", n_("Datamosh")),
];
const HINTS: [(&str, &str); 6] = [
    ("clones", n_("Add a clone, move somewhere else and add another.")),
    ("cloak", n_("Take the empty room, then step back in.")),
    ("light", n_("Move a torch or a phone screen around in the dark.")),
    ("slit", n_("Each band of the picture runs a little behind the one before.")),
    ("stick", n_("A stick figure takes your place.")),
    ("mosh", n_("Move around and the picture smears instead of refreshing.")),
];
const FACE: [&str; 3] = ["eye", "ear", "nose"];

/// The sliders: saved name, range, default and the tooltip.
struct SliderDef {
    key: &'static str,
    lo: i64,
    hi: i64,
    default: i64,
    tip: &'static str,
}

const SLIDERS: [SliderDef; 5] = [
    SliderDef { key: "edges", lo: 0, hi: 10, default: 3, tip: n_("How soft the edge around you is") },
    SliderDef { key: "fade", lo: 1, hi: 10, default: 5, tip: n_("How long the light trails last") },
    SliderDef { key: "brightness", lo: 1, hi: 10, default: 6, tip: n_("How bright a light has to be to paint") },
    SliderDef { key: "spread", lo: 5, hi: 45, default: 20, tip: n_("Frames between one end of the picture and the other") },
    SliderDef { key: "smear", lo: 0, hi: 3, default: 0, tip: n_("Plays each frame's motion this many extra times") },
];

fn effect_text(key: &str) -> &'static str {
    EFFECTS.iter().find(|(k, _)| *k == key).map_or("", |(_, t)| *t)
}

/// stick_figure: the lines and heads of a stick figure from the people the pose model found. `map_point` puts a point of the picture
/// as shown at zoom 1 where the view now has it; aspect is the picture's width over height.
pub type Lines = Vec<((f64, f64), (f64, f64))>;
pub type Heads = Vec<((f64, f64), f64)>;

pub fn stick_figure(people: &[Person], map_point: &dyn Fn(f64, f64) -> (f64, f64), aspect: f64) -> (Lines, Heads) {
    let (mut lines, mut heads) = (Vec::new(), Vec::new());
    for person in people {
        let mut pts = std::collections::HashMap::new();
        for name in person.points.keys() {
            if let Some((x, y)) = person.point(name, SEEN) {
                pts.insert(name.as_str(), map_point(x, y));
            }
        }
        for (a, b) in BONES {
            if let (Some(&pa), Some(&pb)) = (pts.get(a), pts.get(b)) {
                if !FACE.iter().any(|f| a.contains(f) || b.contains(f)) {
                    lines.push((pa, pb));
                }
            }
        }
        let ears: Vec<(f64, f64)> = ["left_ear", "right_ear", "left_eye", "right_eye"].iter().filter_map(|n| pts.get(n).copied()).collect();
        let mut head = pts.get("nose").copied();
        if head.is_none() && ears.len() >= 2 {
            head = Some(((ears[0].0 + ears[1].0) / 2.0, (ears[0].1 + ears[1].1) / 2.0));
        }
        if let Some(head) = head {
            let mut r = 0.05;
            if ears.len() >= 2 {
                r = 0.02f64.max(((ears[0].0 - ears[1].0) * aspect).hypot(ears[0].1 - ears[1].1) * 0.8);
            }
            heads.push((head, r));
            if let (Some(&(lx, ly)), Some(&(rx, ry))) = (pts.get("left_shoulder"), pts.get("right_shoulder")) {
                lines.push((((lx + rx) / 2.0, (ly + ry) / 2.0), (head.0, head.1 + r)));
            }
        }
    }
    (lines, heads)
}

/// What the stream's step took for the card.
enum Taken {
    Plate(Rgb),
    Clone(Option<Clones>),
    Reshaped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grab {
    Plate,
    Clone,
}

/// What the card and the stream's steps share.
struct Hub {
    state: Mutex<HubState>,
    /// The shown stream's effect, which keeps its trail or history while the settings change under it
    runner: Mutex<Runner>,
}

struct HubState {
    look: Arc<Look>,
    grab: Option<Grab>,
    mailbox: Vec<Taken>,
    /// The stream the panels show: the room and the pose are its own
    focused: String,
    /// A new stream size has been reported once already
    stale_sent: bool,
    poster: Option<Arc<dyn Poster>>,
}

/// Runs whichever effect look names, keeping its state while it stays the same one.
#[derive(Default)]
struct Runner {
    name: Option<(String, u32)>,
    effect: Option<Effect>,
}

impl Runner {
    fn run(&mut self, frame: &Rgb, look: &Look, mask: Option<Gray>) -> Rgb {
        let id = (look.effect.clone(), look.reset);
        if self.name.as_ref() != Some(&id) {
            self.name = Some(id);
            self.effect = Effect::named(&look.effect);
        }
        match &mut self.effect {
            Some(e) => e.run(frame, look, Inputs { mask }),
            None => frame.clone(),
        }
    }
}

/// The effects as a frame step: the plugin changes the look under it and the next frame has it.
struct FunStep {
    hub: Arc<Hub>,
    /// The settings this step was handed out with: a stream behind the panels keeps them
    frozen: Arc<Look>,
    /// A stream that isn't the shown one starts without anyone else's history
    own: Runner,
}

impl CpuStep for FunStep {
    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let (look, shown) = {
            let s = self.hub.state.lock().unwrap();
            let shown = s.focused == ctx.source_id();
            (if shown { s.look.clone() } else { self.frozen.clone() }, shown)
        };
        // the room and pose are the shown stream's
        if !shown && fx::PLATE.contains(&look.effect.as_str()) {
            return;
        }
        let Some(rgb) = Rgb::from_frame(frame) else { return };
        let edges = fx::EDGES.contains(&look.effect.as_str());
        if shown {
            let taken = look.plate.as_ref().map(|p| (p.w, p.h)).or_else(|| look.clones.as_ref().map(|c| (c.picture.w, c.picture.h)));
            let mut s = self.hub.state.lock().unwrap();
            if taken.is_some_and(|t| t != (rgb.w, rgb.h)) && !s.stale_sent {
                // a new stream size: what was taken no longer lines up
                s.stale_sent = true;
                s.mailbox.push(Taken::Reshaped);
                s.post();
            }
            let grab = s.grab.take();
            drop(s);
            if let Some(grab) = grab {
                let taken = match grab {
                    Grab::Plate => Taken::Plate(rgb.clone()),
                    Grab::Clone => {
                        let mask = if edges || look.effect == "clones" { mask_of(ctx, frame) } else { None };
                        let mask = match mask {
                            Some(m) => Some(m),
                            None => look.plate.as_ref().and_then(|p| PlateMask::default().run(&rgb, p)),
                        };
                        Taken::Clone(mask.map(|m| Clones::add(look.clones.as_deref(), &rgb, &m.soften(look.edges))))
                    }
                };
                let mut s = self.hub.state.lock().unwrap();
                s.mailbox.push(taken);
                s.post();
            }
        }
        let mask = if edges { mask_of(ctx, frame) } else { None };
        let out = if shown { self.hub.runner.lock().unwrap().run(&rgb, &look, mask) } else { self.own.run(&rgb, &look, mask) };
        if (out.w, out.h) == (rgb.w, rgb.h) {
            out.write_to(frame);
        }
    }
}

impl HubState {
    fn post(&self) {
        if let Some(p) = &self.poster {
            p.post(T_TAKEN, Value::Null);
        }
    }
}

pub struct FunPackPlugin {
    effect: &'static str,
    sliders: [i64; 5],
    across: bool,
    hub: Arc<Hub>,
    /// The effect the frame step was last handed out for, to ask for a new one only when it changes kind
    stepped: Option<&'static str>,
    plate: Option<Arc<Rgb>>,
    clones: Option<Arc<Clones>>,
    /// (source, view, picture size) the empty room and clones were taken with
    taken_with: Option<(String, View, (usize, usize))>,
    /// They were dropped because one of those changed
    stale: bool,
    people: Vec<Person>,
    lines: Lines,
    heads: Heads,
    reset: u32,
    countdown: i64,
    counting: Option<Grab>,
    mosh_on: bool,
    wants_people: bool,
    problem: String,
    mosh: Arc<Mutex<Mosh>>,
    paired: Paired,
    /// set_config has no host: the card hears the settings with the next hook that has one
    pending: bool,
}

impl Default for FunPackPlugin {
    fn default() -> Self {
        FunPackPlugin::new()
    }
}

impl FunPackPlugin {
    pub fn new() -> FunPackPlugin {
        let hub = Arc::new(Hub {
            state: Mutex::new(HubState { look: Arc::new(Look::default()), grab: None, mailbox: Vec::new(), focused: String::new(), stale_sent: false, poster: None }),
            runner: Mutex::new(Runner::default()),
        });
        FunPackPlugin {
            effect: "off",
            sliders: [SLIDERS[0].default, SLIDERS[1].default, SLIDERS[2].default, SLIDERS[3].default, SLIDERS[4].default],
            across: false,
            hub,
            stepped: None,
            plate: None,
            clones: None,
            taken_with: None,
            stale: false,
            people: Vec::new(),
            lines: Vec::new(),
            heads: Vec::new(),
            reset: 0,
            countdown: 0,
            counting: None,
            mosh_on: false,
            wants_people: false,
            problem: String::new(),
            mosh: Arc::new(Mutex::new(Mosh::default())),
            paired: Paired::default(),
            pending: true,
        }
    }

    pub fn effect(&self) -> &str {
        self.effect
    }

    fn effect_index(&self) -> usize {
        EFFECTS.iter().position(|(k, _)| *k == self.effect).unwrap_or(0)
    }

    fn slider(&self, key: &str) -> i64 {
        SLIDERS.iter().position(|d| d.key == key).map_or(0, |i| self.sliders[i])
    }

    // ── What the card shows ───────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "effect-items": EFFECTS.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>(),
            "slider-tips": SLIDERS.iter().map(|d| tr(d.tip)).collect::<Vec<_>>(),
            "slider-min": SLIDERS.iter().map(|d| d.lo).collect::<Vec<_>>(),
            "slider-max": SLIDERS.iter().map(|d| d.hi).collect::<Vec<_>>(),
            "slider-default": SLIDERS.iter().map(|d| d.default).collect::<Vec<_>>(),
            "plate-tip": tr("Step out of the picture: it's taken after a short countdown"),
            "clone-tip": tr("Get in place: you're copied after a short countdown"),
            "clean-tip": tr("Let the next keyframe through so the picture comes back"),
            "reset-hint": tr("Double-click to reset"),
        })
    }

    fn all_props(&self, host: Option<&dyn Host>) -> Value {
        let effect = self.effect;
        let packets_problem = host.map(|h| h.packets_problem()).unwrap_or_default();
        let unavailable = effect == "mosh" && !packets_problem.is_empty();
        let hint = if unavailable { "" } else { HINTS.iter().find(|(k, _)| *k == effect).map_or("", |(_, t)| *t) };
        let counting = self.counting;
        let n = self.countdown.to_string();
        let plate_text = if counting == Some(Grab::Plate) {
            tr("Taking it in {n}…").replace("{n}", &n)
        } else if self.plate.is_some() {
            tr("Take the empty room again")
        } else {
            tr("Take the empty room")
        };
        let clone_text = if counting == Some(Grab::Clone) { tr("Copying you in {n}…").replace("{n}", &n) } else { tr("Add a clone") };
        let full = self.clones.as_ref().is_some_and(|c| c.count >= Clones::MAX);
        let shows = |effects: &[&str]| effects.contains(&effect) && !unavailable;
        let problem = if effect == "mosh" {
            packets_problem.clone()
        } else if fx::PLATE.contains(&effect) && self.stale && self.plate.is_none() {
            tr("The picture changed, so take the empty room again.")
        } else if effect == "cloak" && self.plate.is_none() {
            tr("Take the empty room first.")
        } else if effect == "clones" {
            self.problem.clone()
        } else {
            String::new()
        };
        json!({
            "active": self.paired.on(),
            "effect": self.effect_index(),
            "hint": if hint.is_empty() { String::new() } else { tr(hint) },
            "show-plate": fx::PLATE.contains(&effect),
            "plate-text": plate_text,
            "plate-enabled": counting.is_none(),
            "show-clone": effect == "clones",
            "clone-text": clone_text,
            "clone-enabled": counting.is_none() && !full,
            "show-clean": effect == "mosh" && !unavailable,
            "show-clear": effect == "clones" || effect == "light",
            "clear-enabled": effect != "clones" || self.clones.is_some(),
            "show-buttons": effect == "clones" || effect == "light" || (effect == "mosh" && !unavailable),
            "slider-show": [shows(&fx::EDGES), shows(&["light"]), shows(&["light"]), shows(&["slit"]), shows(&["mosh"])],
            "slider-value": self.sliders,
            "slider-text": self.sliders.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            "show-direction": effect == "slit",
            "direction": self.across as i32,
            "problem": problem,
        })
    }

    fn show(&self, host: &mut dyn Host) {
        let mut props = self.all_props(Some(&*host));
        // Stick figure needs vision; the edges of a cloak or clones are only cleaner with it
        let edges = fx::EDGES.contains(&self.effect);
        if let (Value::Object(p), Value::Object(row)) = (&mut props, vision_row::props_for(&*host, edges || self.effect == "stick", "", edges)) {
            p.extend(row);
        }
        host.set_ui_props("card", &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        self.hub.state.lock().unwrap().focused = host.focused_source_id().unwrap_or_default();
        if self.hub.state.lock().unwrap().poster.is_none() {
            self.hub.state.lock().unwrap().poster = host.poster();
        }
        if self.pending {
            self.pending = false;
            self.apply(host, true);
        }
    }

    // ── Handlers ──────────────────────────────────────────────────────

    fn on_effect(&mut self, host: &mut dyn Host, index: usize) {
        let Some((key, _)) = EFFECTS.get(index) else { return };
        self.effect = key;
        host.cancel_schedule(T_TICK);
        self.counting = None;
        self.hub.state.lock().unwrap().grab = None;
        self.apply(host, true);
        host.schedule_save();
    }

    fn start_countdown(&mut self, host: &mut dyn Host, what: Grab) {
        self.counting = Some(what);
        self.countdown = COUNTDOWN_S;
        host.schedule(1000, T_TICK);
        self.show(host);
    }

    fn tick(&mut self, host: &mut dyn Host) {
        if self.counting.is_none() {
            return;
        }
        self.countdown -= 1;
        if self.countdown <= 0 {
            self.hub.state.lock().unwrap().grab = self.counting.take();
        } else {
            host.schedule(1000, T_TICK);
        }
        self.show(host);
    }

    fn taken_key(&self, host: &dyn Host, size: (usize, usize)) -> (String, View, (usize, usize)) {
        (host.focused_source_id().unwrap_or_default(), host.view(), size)
    }

    fn on_taken(&mut self, host: &mut dyn Host, what: Taken) {
        let value = match what {
            Taken::Reshaped => return self.drop_taken(host),
            Taken::Plate(rgb) => {
                let key = self.taken_key(host, (rgb.w, rgb.h));
                self.note_taken(key);
                self.plate = Some(Arc::new(rgb));
                true
            }
            Taken::Clone(Some(clones)) => {
                let key = self.taken_key(host, (clones.picture.w, clones.picture.h));
                self.note_taken(key);
                self.clones = Some(Arc::new(clones));
                true
            }
            Taken::Clone(None) => false,
        };
        self.problem = if value { String::new() } else { tr("Take the empty room first, so you can be cut out.") };
        self.apply(host, true);
    }

    fn note_taken(&mut self, key: (String, View, (usize, usize))) {
        if self.taken_with.as_ref() != Some(&key) {
            self.plate = None;
            self.clones = None;
        }
        self.taken_with = Some(key);
        self.stale = false;
        self.hub.state.lock().unwrap().stale_sent = false;
    }

    /// The empty room and clones no longer line up with the picture: the card asks for the room again.
    fn drop_taken(&mut self, host: &mut dyn Host) {
        if self.plate.is_none() && self.clones.is_none() {
            return;
        }
        self.plate = None;
        self.clones = None;
        self.taken_with = None;
        self.stale = true;
        self.hub.state.lock().unwrap().stale_sent = false;
        self.apply(host, true);
    }

    fn check_taken(&mut self, host: &mut dyn Host) {
        if self.taken_with.as_ref().is_some_and(|t| host.view() != t.1) {
            self.drop_taken(host);
        }
    }

    fn new_source(&mut self, host: &mut dyn Host) {
        // slit-scan history and light trails belong to the stream before
        *self.hub.runner.lock().unwrap() = Runner::default();
        if self.counting.is_some() || self.hub.state.lock().unwrap().grab.is_some() {
            // the room or clone was for the stream before, not this one's next frame
            host.cancel_schedule(T_TICK);
            self.counting = None;
            self.hub.state.lock().unwrap().grab = None;
            self.show(host);
        }
        self.drop_taken(host);
    }

    fn clear(&mut self, host: &mut dyn Host) {
        if self.effect == "clones" {
            self.clones = None;
        }
        self.reset += 1;
        self.apply(host, true);
    }

    fn clean_up(&mut self, host: &mut dyn Host) {
        // off and on again: the phone is asked for a keyframe and a new Mosher lets it through
        if self.mosh_on {
            host.change_packets(NAME, None);
            host.change_packets(NAME, Some(Arc::new(MosherFactory { settings: self.mosh.clone(), clean_up: true })));
        }
    }

    fn on_people(&mut self, host: &mut dyn Host, source_id: &str, people: &[Value]) {
        if self.effect == "stick" && source_id == host.focused_source_id().unwrap_or_default() {
            self.people = people.iter().filter_map(Person::from_value).collect();
            self.map_people(host);
        }
    }

    fn map_people(&mut self, host: &mut dyn Host) {
        if self.effect != "stick" {
            return;
        }
        let geometry = Geometry::from(&host.view());
        let aspect = host.view_aspect() as f64;
        let (lines, heads) = stick_figure(&self.people, &|x, y| upright_to_shown(&geometry, x, y), if aspect > 0.0 { aspect } else { 16.0 / 9.0 });
        self.lines = lines;
        self.heads = heads;
        self.apply(host, false);
    }

    fn apply(&mut self, host: &mut dyn Host, show: bool) {
        let look = Look {
            effect: self.effect.into(),
            plate: self.plate.clone(),
            clones: self.clones.clone(),
            edges: self.slider("edges"),
            fade: self.slider("fade"),
            brightness: self.slider("brightness"),
            spread: self.slider("spread"),
            across: self.across,
            lines: self.lines.clone(),
            heads: self.heads.clone(),
            reset: self.reset,
        };
        self.hub.state.lock().unwrap().look = Arc::new(look);
        if !fx::EFFECTS.contains(&self.effect) {
            // Python runs Off through its runner, which drops the effect before; Datamosh and Game Boy have no picture step to do it
            *self.hub.runner.lock().unwrap() = Runner::default();
        }
        *self.mosh.lock().unwrap() = Mosh { drop_keyframes: true, repeats: self.slider("smear") as u32 };
        // The shown stream reads the look as it changes; the others are handed it again
        if show || self.stepped != Some(self.effect) {
            self.stepped = Some(self.effect);
            host.refresh_frame_step();
        }
        if show {
            self.refresh(host);
        }
    }

    /// Packets, people and the card, after anything that changes what the effect needs.
    fn refresh(&mut self, host: &mut dyn Host) {
        let effect = self.effect;
        let mosh = effect == "mosh" && host.packets_problem().is_empty();
        if mosh != self.mosh_on {
            self.mosh_on = mosh;
            let factory: Option<Arc<dyn telescope_core::host::PacketStepFactory>> = mosh.then(|| Arc::new(MosherFactory { settings: self.mosh.clone(), clean_up: false }) as _);
            host.change_packets(NAME, factory);
        }
        // only on a change: asking loads MediaPipe, which signals back here
        if (effect == "stick") != self.wants_people {
            self.wants_people = effect == "stick";
            host.want_people(NAME, self.wants_people);
        }
        if effect != "stick" {
            self.people.clear();
            self.lines.clear();
            self.heads.clear();
        }
        self.show(host);
    }

    fn set_slider(&mut self, host: &mut dyn Host, i: usize, value: i64) {
        self.sliders[i] = value.clamp(SLIDERS[i].lo, SLIDERS[i].hi);
        self.apply(host, true);
        host.schedule_save();
    }
}

impl Plugin for FunPackPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Fun pack").into(),
            about: n_("Silly effects: clones, an invisibility cloak, light painting, retro looks and datamosh.").into(),
            when_off: n_("Without it there's no Fun pack card and the picture has no effects.").into(),
            category: "look".into(),
            icon: "sparkle".into(),
            optional: true,
            in_presets: true,
            // an effect on one camera stays on that camera
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        host.set_ui_props("card", &FunPackPlugin::fixed_props());
        self.show(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.mosh_on {
            self.mosh_on = false;
            host.change_packets(NAME, None);
        }
        if self.wants_people {
            self.wants_people = false;
            host.want_people(NAME, false);
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::PeopleSeen { source_id, people } => self.on_people(host, source_id, people),
            Event::ViewChanged { .. } => {
                self.check_taken(host);
                self.map_people(host);
            }
            Event::SourceSelected { .. } | Event::DeviceChanged { .. } => {
                self.new_source(host);
                self.refresh(host);
            }
            Event::StreamTransportChanged | Event::StreamStopped | Event::StreamStarted { .. } | Event::VisionStateChanged { .. } => self.refresh(host),
            e if vision_row::changes_it(e) => self.show(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_TICK => self.tick(host),
            Event::Posted { owner, token, .. } if owner == NAME && token == T_TAKEN => {
                let taken: Vec<Taken> = std::mem::take(&mut self.hub.state.lock().unwrap().mailbox);
                for t in taken {
                    self.on_taken(host, t);
                }
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = FunPackPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props(None)) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "FunPackCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "effect-picked" => self.on_effect(host, n(0) as usize),
            "plate-clicked" => self.start_countdown(host, Grab::Plate),
            "clone-clicked" => self.start_countdown(host, Grab::Clone),
            "clean-clicked" => self.clean_up(host),
            "clear-clicked" => self.clear(host),
            "download-clicked" => host.download_vision(),
            "slider-moved" => self.set_slider(host, (n(0) as usize).min(4), n(1).round() as i64),
            "direction-picked" => {
                self.across = n(0) as usize == 1;
                self.apply(host, true);
                host.schedule_save();
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Fun pack");
        let options: Vec<(&str, String)> = EFFECTS.iter().map(|(_, t)| (*t, tr(t))).collect();
        let button = |id: &str, label: String| ActionSpec { id: id.into(), label, group: group.clone(), params: Vec::new(), repeats: false, on_release: false, current: None, keywords: String::new() };
        let smear = &SLIDERS[4];
        let label = tr("Datamosh smear");
        let spec = SliderSpec { id: "fun_pack.smear", label: &label, group: &group, keywords: "", minimum: smear.lo as f64, maximum: smear.hi as f64, step: 1.0, decimals: 0, suffix: "", has_default: true };
        vec![
            actions::choice_action("fun_pack.effect", &tr("Fun pack effect"), &group, "", &options),
            button("fun_pack.plate", tr("Take the empty room")),
            button("fun_pack.clone", tr("Add a clone")),
            button("fun_pack.clean", tr("Clean up the datamosh")),
            actions::slider_action(&spec, self.sliders[4] as f64),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        let unavailable = self.effect == "mosh" && !host.packets_problem().is_empty();
        match id {
            "fun_pack.effect" => {
                let shown: Vec<String> = EFFECTS.iter().map(|(_, t)| tr(t)).collect();
                let options: Vec<Choice> = EFFECTS.iter().zip(&shown).map(|((_, t), s)| Choice { value: t, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(self.effect_index()), true)?;
                if pick {
                    self.on_effect(host, said);
                }
                Some(actions::slider_text(&tr("Fun pack effect"), &shown[said]))
            }
            "fun_pack.plate" => {
                if !fx::PLATE.contains(&self.effect) || self.counting.is_some() {
                    return None;
                }
                self.start_countdown(host, Grab::Plate);
                Some(tr("Take the empty room"))
            }
            "fun_pack.clone" => {
                let full = self.clones.as_ref().is_some_and(|c| c.count >= Clones::MAX);
                if self.effect != "clones" || self.counting.is_some() || full {
                    return None;
                }
                self.start_countdown(host, Grab::Clone);
                Some(tr("Add a clone"))
            }
            "fun_pack.clean" => {
                if self.effect != "mosh" || unavailable {
                    return None;
                }
                self.clean_up(host);
                Some(tr("Clean up the datamosh"))
            }
            "fun_pack.smear" => {
                if self.effect != "mosh" || unavailable {
                    return None;
                }
                let target = actions::slider_target(params, self.sliders[4], 1.0, SLIDERS[4].lo, SLIDERS[4].hi, Some(SLIDERS[4].default))?;
                self.set_slider(host, 4, target);
                Some(actions::slider_text(&tr("Datamosh smear"), &target.to_string()))
            }
            _ => None,
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        let effect = self.effect;
        self.stepped = Some(effect);
        if effect == "game_boy" {
            return Some(FrameStep::Shader(ShaderStep { wgsl: GAME_BOY.into(), uniforms: json!({}), textures: Vec::new() }));
        }
        if !fx::EFFECTS.contains(&effect) {
            return None;
        }
        let frozen = self.hub.state.lock().unwrap().look.clone();
        Some(FrameStep::Cpu(Box::new(FunStep { hub: self.hub.clone(), frozen, own: Runner::default() })))
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("effect".into(), json!(self.effect));
        cfg.insert("across".into(), json!(self.across));
        for (d, v) in SLIDERS.iter().zip(self.sliders) {
            cfg.insert(d.key.into(), json!(v));
        }
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        self.effect = cfg.get("effect").and_then(Value::as_str).and_then(|e| EFFECTS.iter().find(|(k, _)| *k == e)).map_or("off", |(k, _)| *k);
        for (i, d) in SLIDERS.iter().enumerate() {
            self.sliders[i] = clamped_int(cfg.get(d.key).unwrap_or(&Value::Null), d.default, d.lo, d.hi);
        }
        self.across = cfg.get("across") == Some(&Value::Bool(true));
        // once, with the whole of it: the next hook with a host does it
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Fun pack".into(), self.effect.into())]
    }
}

/// The effect's name as the card shows it, in English.
pub fn english_name(effect: &str) -> &'static str {
    effect_text(effect)
}

#[cfg(test)]
#[path = "fun_pack_tests.rs"]
mod tests;
