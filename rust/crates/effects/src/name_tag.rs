//! The Name tag plugin (plugins/name_tag.py): a lower third with your name, pronouns and an optional second line, drawn onto the
//! picture. The window's view (zoom, pan, flip and rotation) runs before every plugin's step, so the tag is drawn onto the
//! picture as apps get it: Mirror in Transforms never mirrors the text, and left means left in the call.

use crate::shared::Paired;
use crate::tag::{self, Spec, TagCache, DEFAULT_POSITION, DEFAULT_SIZE, DEFAULT_STYLE, MAX_CHARS, POSITIONS, SIZES, STYLES};
use serde_json::{json, Value};
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "name_tag";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/name_tag.slint"));

const STYLE_TEXTS: [(&str, &str); 4] = [("bar", n_("Bar")), ("pill", n_("Pill")), ("light", n_("Light")), ("shadow", n_("Shadow"))];
const POSITION_TEXTS: [(&str, &str); 5] = [
    ("bottom_left", n_("Bottom left")), ("bottom_centre", n_("Bottom centre")), ("bottom_right", n_("Bottom right")),
    ("top_left", n_("Top left")), ("top_right", n_("Top right")),
];
const SIZE_TEXTS: [(&str, &str); 3] = [("small", n_("Small")), ("medium", n_("Medium")), ("large", n_("Large"))];
const SWITCH_TIP: &str = n_("Show the name tag on the picture");
const PRONOUNS_TIP: &str = n_("Shown next to your name, like she/her or they/them");
const SECOND_TIP: &str = n_("A smaller line under your name, like your role. Leave it empty for none");

/// The tag drawn on each frame, with the cache of the rendered tile it works from (a stream of its own size has its own).
struct TagStep {
    spec: Spec,
    cache: TagCache,
}

impl CpuStep for TagStep {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        if let Some(tag) = self.cache.get(&self.spec, frame.width, frame.height) {
            tag::apply(frame, &tag);
        }
    }
}

pub struct NameTagPlugin {
    on: bool,
    spec: Spec,
    paired: Paired,
    held: Option<bool>,
    /// set_config has no host: the card hears the settings, and the frames the step, with the next hook that has one.
    pending: bool,
}

impl Default for NameTagPlugin {
    fn default() -> Self {
        NameTagPlugin::new()
    }
}

fn index_of(keys: &[&str], key: &str, default: &str) -> usize {
    keys.iter().position(|k| *k == key).or_else(|| keys.iter().position(|k| *k == default)).unwrap_or(0)
}

fn cut(text: &str) -> String {
    text.chars().take(MAX_CHARS).collect()
}

impl NameTagPlugin {
    pub fn new() -> NameTagPlugin {
        NameTagPlugin { on: false, spec: Spec::default(), paired: Paired::default(), held: None, pending: false }
    }

    fn step(&self) -> TagStep {
        TagStep { spec: self.spec.clone(), cache: TagCache::default() }
    }

    /// Whether the tag is drawn at all: on, and something to say.
    fn shows(&self) -> bool {
        self.on && !self.spec.empty()
    }

    fn texts(list: &[(&str, &str)]) -> Vec<String> {
        list.iter().map(|(_, t)| tr(t)).collect()
    }

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("Name tag"), "switch-help": tr(SWITCH_TIP), "pronouns-tip": tr(PRONOUNS_TIP), "second-tip": tr(SECOND_TIP),
            "style-items": Self::texts(&STYLE_TEXTS), "position-items": Self::texts(&POSITION_TEXTS), "size-items": Self::texts(&SIZE_TEXTS),
        })
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let Value::Object(o) = &mut all {
            o.extend([
                ("on".to_string(), json!(self.on)),
                ("name".to_string(), json!(self.spec.name)),
                ("pronouns".to_string(), json!(self.spec.pronouns)),
                ("second".to_string(), json!(self.spec.second)),
                ("style-current".to_string(), json!(index_of(&STYLES, &self.spec.style, DEFAULT_STYLE))),
                ("position-current".to_string(), json!(index_of(&POSITIONS, &self.spec.position, DEFAULT_POSITION))),
                ("size-current".to_string(), json!(index_of(&SIZES.map(|(k, _)| k), &self.spec.size, DEFAULT_SIZE))),
            ]);
        }
        all
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.all_props());
            host.refresh_frame_step();
        }
    }

    /// The settings changed: the frames get a new step, and the file a save.
    fn changed(&mut self, host: &mut dyn Host) {
        host.refresh_frame_step();
        host.schedule_save();
    }

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        host.set_ui_props("card", &json!({"on": on}));
        self.changed(host);
    }

    fn edit(&mut self, host: &mut dyn Host, field: usize, text: &str) {
        let kept = cut(text);
        let slot = match field {
            0 => &mut self.spec.name,
            1 => &mut self.spec.pronouns,
            _ => &mut self.spec.second,
        };
        *slot = kept.clone();
        // A box that took more than it holds (a paste) shows what was kept
        if kept != text {
            let key = ["name", "pronouns", "second"][field.min(2)];
            host.set_ui_props("card", &Value::Object([(key.to_string(), json!(kept))].into_iter().collect()));
        }
        self.changed(host);
    }

    fn pick(&mut self, host: &mut dyn Host, which: usize, index: usize) {
        match which {
            0 => self.spec.style = STYLES.get(index).copied().unwrap_or(DEFAULT_STYLE).into(),
            1 => self.spec.position = POSITIONS.get(index).copied().unwrap_or(DEFAULT_POSITION).into(),
            _ => self.spec.size = SIZES.get(index).map_or(DEFAULT_SIZE, |(k, _)| k).into(),
        }
        self.changed(host);
    }
}

impl Plugin for NameTagPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Name tag").into(),
            about: n_("Puts your name and pronouns in the corner of the picture.").into(),
            when_off: n_("Without it there's no Name tag card and nothing is drawn on the picture.").into(),
            category: "on_screen".into(),
            icon: "bookmark".into(),
            frame_stage: FrameStage::Overlay,
            optional: true,
            // A preset can be a scene: the tag for a talk, none for a chat
            in_presets: true,
            // Each camera has its own tag, or none (the desk camera usually doesn't want one)
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.all_props());
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        self.paired.hear(event);
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "NameTagCard".into(), props: self.all_props() }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.set_on(host, on);
            }
            "text-edited" => {
                let text = event.args.get(1).and_then(Value::as_str).unwrap_or("").to_string();
                self.edit(host, n(0) as usize, &text);
            }
            "choice-picked" => self.pick(host, n(0) as usize, n(1).max(0.0) as usize),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        // The card's search words (settings_search.keywords in Python) find its shortcut here
        let words = tr_ctx("search", "lower third caption title label overlay text name pronouns role");
        vec![actions::toggle_action("name_tag.on", &tr("Name tag"), &tr("Name tag"), &words, &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if id != "name_tag.on" {
            return None;
        }
        let want = actions::toggle_wanted(params, self.on, &mut self.held);
        if want != self.on {
            self.set_on(host, want);
        }
        Some(actions::toggle_text(&tr("Name tag"), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "name_tag.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on {
                self.set_on(host, before);
            }
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        // A cache of its own: the streams can differ in size
        self.shows().then(|| FrameStep::Cpu(Box::new(self.step())))
    }

    fn get_config(&self) -> Value {
        json!({
            "on": self.on, "name": self.spec.name, "pronouns": self.spec.pronouns, "second": self.spec.second,
            "style": self.spec.style, "position": self.spec.position, "size": self.spec.size,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let text = |key: &str| cfg.get(key).and_then(Value::as_str).map(cut).unwrap_or_default();
        let pick = |key: &str, choices: &[&str], default: &str| -> String {
            cfg.get(key).and_then(Value::as_str).filter(|v| choices.contains(v)).unwrap_or(default).to_string()
        };
        // Size is looked up in a dict, so a list or a dict there can't even be compared
        if matches!(cfg.get("size"), Some(Value::Array(_) | Value::Object(_))) {
            return Err(ConfigRejected("size".into()));
        }
        self.spec = Spec {
            name: text("name"),
            pronouns: text("pronouns"),
            second: text("second"),
            style: pick("style", &STYLES, DEFAULT_STYLE),
            position: pick("position", &POSITIONS, DEFAULT_POSITION),
            size: pick("size", &SIZES.map(|(k, _)| k), DEFAULT_SIZE),
        };
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let state = if self.shows() { format!("on ({}, {})", self.spec.style, self.spec.size) } else { "off".into() };
        vec![("Name tag".into(), state)]
    }
}

#[cfg(test)]
#[path = "name_tag_tests.rs"]
mod tests;
