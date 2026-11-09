//! The held bubbles on the Auto framing, Notes and Background cards, with their real plugins behind them in an offscreen window: which
//! slider gets a bubble when, that what a bubble needs runs only while it is up, and that holding a slider changes the same settings
//! it always did. SHOTS=dir cargo test --test peek_cards shots draws each card with its bubbles by eye.

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use serde_json::{json, Map, Value as Json};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_core::vision;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::framing::FramingPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_effects::peek;
use telescope_shell::source::{from_json, parse_color};
use telescope_shell::offscreen;

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 760.0;

fn slint_type(value: &Json) -> &'static str {
    match value {
        Json::Bool(_) => "bool",
        Json::Number(_) => "float",
        Json::String(s) if s.len() == 7 && s.starts_with('#') && parse_color(s).is_some() => "color",
        Json::String(_) => "string",
        Json::Array(items) if items.iter().all(Json::is_string) => "[string]",
        Json::Array(_) => "[float]",
        _ => "string",
    }
}

type Said = Rc<RefCell<Vec<UiEvent>>>;

/// A card in a window with the bubbles' layer, its plugin behind it, and what the plugin tells the card carried across.
struct Wrap {
    plugin: Box<dyn Plugin>,
    host: FakeHost,
    w: ComponentInstance,
    said: Said,
    keys: Vec<String>,
    fed: usize,
    pictures: HashMap<String, usize>,
    held_at: Option<LogicalPosition>,
}

struct Kind {
    group: &'static str,
    /// Properties the plugin sets later and the card declares, with their Slint types
    extra: &'static [(&'static str, &'static str)],
    images: &'static [&'static str],
    callbacks: &'static [(&'static str, &'static str)],
}

const FRAMING: Kind = Kind {
    group: "vision",
    extra: &[("speed-dots", "[float]"), ("framing-clip", "[float]")],
    images: &["framing-picture"],
    callbacks: &[("framing-peek-open", "bool"), ("speed-peek-open", "bool"), ("framing-moved", "float"), ("speed-moved", "float"), ("switch-toggled-by-hand", "bool"), ("hand-toggled", "bool")],
};
const NOTES: Kind = Kind { group: "on_screen", extra: &[], images: &[], callbacks: &[("size-moved", "float"), ("text-edited", "string")] };
const BACKGROUND: Kind = Kind {
    group: "look",
    extra: &[("blur-peek", "string"), ("vision-text", "string"), ("vision-download", "bool")],
    images: &["blur-picture", "picture-thumb"],
    callbacks: &[("blur-moved", "float"), ("blur-peek-open", "bool"), ("mode-picked", "int")],
};

impl Wrap {
    fn new(mut plugin: Box<dyn Plugin>, mut host: FakeHost, kind: &Kind, top: f32, classic: bool) -> Wrap {
        offscreen::install();
        plugin.setup(&mut host);
        plugin.on_plugins_ready(&mut host);
        plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
        let card = plugin.card().expect("a card");
        let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
        let props = card.ui.props.as_object().unwrap().clone();
        let mut declared: Vec<(String, &str)> = props.iter().map(|(k, v)| (k.clone(), slint_type(v))).collect();
        for (k, t) in kind.extra {
            if !declared.iter().any(|(d, _)| d == k) {
                declared.push((k.to_string(), t));
            }
        }
        let keys: Vec<String> = declared.iter().map(|(k, _)| k.clone()).collect();
        let mut text: String = declared.iter().map(|(k, t)| format!("    in-out property <{t}> {k};\n")).collect();
        for image in kind.images {
            text.push_str(&format!("    in-out property <image> {image};\n"));
        }
        let mut forwarded: String = declared.iter().map(|(k, _)| format!("{k}: root.{k}; ")).collect();
        for image in kind.images {
            forwarded.push_str(&format!("{image}: root.{image}; "));
        }
        for (name, ty) in kind.callbacks {
            text.push_str(&format!("    callback {name}({ty});\n"));
            forwarded.push_str(&format!("{name}(a) => {{ root.{name}(a); }} "));
        }
        let doc = format!(
            "import {{ Theme, WindowBox, TipLayer, PeekLayer, Peeks }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
             export {{ Theme, WindowBox, Peeks }}\n\
             export component W inherits Window {{\n    width: {WIDTH}px;\n    height: {HEIGHT}px;\n    background: Theme.ground;\n{text}\
             out property <length> card-h: card.height;\n\
             init => {{ WindowBox.width = self.width; WindowBox.height = self.height; }}\n    \
             card := {c} {{ x: 20px; y: {top}px; width: parent.width - 40px; hue: Theme.plugin-hue(\"{g}\"); {forwarded} }}\n    \
             TipLayer {{ }}\n    PeekLayer {{ }}\n}}",
            c = card.ui.component,
            g = kind.group
        );
        let definition = telescope_shell::compile::compile("wrapper.slint", &doc, vec![("plugin/card.slint".to_string(), source)], "W").unwrap_or_else(|e| panic!("{e}"));
        let w = definition.create().unwrap();
        w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        let said: Said = Rc::default();
        for (name, _) in kind.callbacks {
            let log = said.clone();
            let name = *name;
            w.set_callback(name, move |args| {
                log.borrow_mut().push(UiEvent { ui: "card".into(), callback: name.into(), args: args.iter().map(telescope_shell::source::to_json).collect() });
                Value::Void
            })
            .unwrap();
        }
        w.show().unwrap();
        let mut wrap = Wrap { plugin, host, w, said, keys, fed: 0, pictures: HashMap::new(), held_at: None };
        // Everything the card was first given, then what the plugin said since
        let mut first: Map<String, Json> = props;
        wrap.hand_over(&mut first);
        wrap.feed();
        wrap
    }

    fn hand_over(&self, props: &mut Map<String, Json>) {
        for key in &self.keys {
            let Some(value) = props.get(key) else { continue };
            let slint = match (value, parse_color(value.as_str().unwrap_or(""))) {
                (Json::String(s), Some(c)) if s.starts_with('#') && s.len() == 7 => Value::Brush(slint::Brush::SolidColor(c)),
                _ => from_json(value),
            };
            self.w.set_property(key, slint).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        }
    }

    /// What the plugin said to the card since the last time: properties and pictures.
    fn feed(&mut self) {
        let mut update: Map<String, Json> = Map::new();
        for call in &self.host.calls[self.fed..] {
            if let Some(json) = call.strip_prefix("set_ui_props card ") {
                if let Ok(Json::Object(more)) = serde_json::from_str::<Json>(json) {
                    update.extend(more);
                }
            }
        }
        self.fed = self.host.calls.len();
        self.hand_over(&mut update);
        for ((ui, prop), frame) in &self.host.images {
            let id = Arc::as_ptr(&frame.data) as usize;
            if ui != "card" || self.pictures.get(prop) == Some(&id) {
                continue;
            }
            self.pictures.insert(prop.clone(), id);
            let image = slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&frame.data, frame.width, frame.height));
            self.w.set_property(prop, Value::Image(image)).unwrap();
        }
    }

    /// Replays what the card said to the plugin, then hands the card the result.
    fn pass_on(&mut self) {
        let said: Vec<UiEvent> = self.said.borrow_mut().drain(..).collect();
        for event in said {
            self.plugin.on_ui(&mut self.host, &event);
        }
        self.feed();
        offscreen::pump();
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
        self.feed();
    }

    fn fire(&mut self, token: &str, times: usize) {
        let owner = self.plugin.info().name;
        for _ in 0..times {
            self.event(Event::TimerFired { owner: owner.clone(), token: token.into() });
        }
    }

    fn draw(&self) -> offscreen::Picture {
        offscreen::draw(self.w.window(), WIDTH, HEIGHT, 1.0).expect("a picture")
    }

    fn slider(&self, label: &str) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.w, label).find(|e| e.accessible_role() == Some(AccessibleRole::Slider)).unwrap_or_else(|| panic!("no slider {label}"))
    }

    fn hold(&mut self, label: &str) {
        self.hold_at(label, 0.5);
    }

    /// Presses the slider this far (0 to 1) along its track and keeps the button down.
    fn hold_at(&mut self, label: &str, along: f32) {
        self.draw();
        let e = self.slider(label);
        let (p, s) = (e.absolute_position(), e.size());
        let position = LogicalPosition::new(p.x + HANDLE_INSET + along * (s.width - 2.0 * HANDLE_INSET), p.y + s.height / 2.0);
        self.held_at = Some(position);
        self.w.window().dispatch_event(WindowEvent::PointerMoved { position });
        self.w.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        self.draw();
        // A press on the track of a slider with a default waits out the double-click interval before it lands
        offscreen::skip(std::time::Duration::from_millis(400));
        self.draw();
        self.pass_on();
        self.draw();
    }

    fn let_go(&mut self) {
        let position = self.held_at.take().expect("something is held");
        self.w.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        self.draw();
        self.pass_on();
        self.draw();
    }

    fn up(&self) -> bool {
        matches!(self.w.get_global_property("Peeks", "shown"), Ok(Value::Bool(true)))
    }

    fn peek_kind(&self) -> String {
        match self.w.get_global_property("Peeks", "kind") {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("{other:?}"),
        }
    }

    fn peek_numbers(&self) -> Vec<f64> {
        match self.w.get_global_property("Peeks", "values") {
            Ok(Value::Model(m)) => (0..m.row_count()).map(|i| match m.row_data(i) {
                Some(Value::Number(n)) => n,
                other => panic!("{other:?}"),
            }).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn peek_picture(&self) -> (u32, u32) {
        match self.w.get_global_property("Peeks", "picture") {
            Ok(Value::Image(i)) => (i.size().width, i.size().height),
            other => panic!("{other:?}"),
        }
    }

    fn peek_text(&self) -> String {
        match self.w.get_global_property("Peeks", "text") {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("{other:?}"),
        }
    }

    fn card_height(&self) -> f64 {
        match self.w.get_property("card-h") {
            Ok(Value::Number(n)) => n,
            other => panic!("{other:?}"),
        }
    }

    fn said_names(&self) -> Vec<String> {
        self.host.calls.iter().filter(|c| c.starts_with("schedule ")).cloned().collect()
    }
}

/// Where the handle's centre runs from: half the handle's width in from the track's end.
const HANDLE_INSET: f32 = 7.0;

fn framing(top: f32, classic: bool) -> Wrap {
    framing_stepping(top, classic, 0.033)
}

/// The same, with each look at the clock this many seconds on.
fn framing_stepping(top: f32, classic: bool, step: f64) -> Wrap {
    let mut host = FakeHost::new();
    host.focused = Some("phone".into());
    host.vision = Some(vision::READY.into());
    host.aspect = 16.0 / 9.0;
    // Every look at the clock is a step of 33 ms on, so ticks run time forward without the test steering it
    let now = Arc::new(std::sync::Mutex::new(10.0f64));
    let mut plugin = FramingPlugin::with_clock(move || {
        let mut t = now.lock().unwrap();
        *t += step;
        *t
    });
    plugin.set_config(&json!({"on": false})).unwrap();
    Wrap::new(Box::new(plugin), host, &FRAMING, top, classic)
}

fn notes(top: f32, classic: bool) -> Wrap {
    Wrap::new(Box::new(NotesPlugin::new()), FakeHost::new(), &NOTES, top, classic)
}

fn background(top: f32, classic: bool) -> Wrap {
    let mut host = FakeHost::new();
    host.vision = Some(vision::READY.into());
    let mut plugin = BackgroundPlugin::new();
    plugin.set_config(&json!({"mode": "blur"})).unwrap();
    Wrap::new(Box::new(plugin), host, &BACKGROUND, top, classic)
}

fn scheduled(w: &Wrap, token: &str) -> bool {
    w.host.is_scheduled(token)
}

#[test]
fn the_framing_slider_gets_a_picture_bubble_only_while_the_preview_cant_show_the_crop() {
    let mut w = framing(140.0, false);
    let height = w.card_height();
    assert!(!w.up() && !scheduled(&w, "peek"));
    w.hold("Framing");
    assert!(w.up() && w.peek_kind() == "picture");
    assert!(scheduled(&w, "peek"), "the plugin makes pictures while it is up");
    assert_eq!(w.peek_picture(), (peek::SCENE_WIDTH as u32, peek::SCENE_HEIGHT as u32), "and the bubble holds one");
    assert_eq!(w.peek_numbers().len(), 4, "with the part of it to show");
    assert_eq!(w.card_height(), height, "the card did not move");
    w.let_go();
    assert!(!w.up());
    assert!(!scheduled(&w, "peek"), "closed: nothing runs");
    assert_eq!(w.card_height(), height);
}

#[test]
fn with_auto_framing_following_someone_the_preview_shows_it_and_the_slider_has_no_bubble() {
    let mut w = framing(140.0, false);
    w.plugin.on_ui(&mut w.host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    w.feed();
    let person = peek::sample_person().to_value();
    for _ in 0..40 {
        w.event(Event::PeopleSeen { source_id: "phone".into(), people: vec![person.clone()] });
        w.fire("tick", 1);
    }
    w.draw();
    w.hold("Framing");
    assert!(!w.up(), "the preview already shows the crop");
    w.let_go();
    // and the speed slider, which is about time, still has its bubble
    w.hold("Speed");
    assert!(w.up() && w.peek_kind() == "chase");
    w.let_go();
}

#[test]
fn holding_the_speed_slider_runs_the_dots_and_only_then() {
    let mut w = framing(140.0, false);
    assert!(!scheduled(&w, "chase"));
    w.hold("Speed");
    assert!(w.up() && w.peek_kind() == "chase");
    assert!(scheduled(&w, "chase"));
    let first = w.peek_numbers();
    assert_eq!(first.len(), 8, "you, the frame and its two copies");
    w.fire("chase", 100);
    w.draw();
    let later = w.peek_numbers();
    assert_eq!(later.len(), 8);
    assert_ne!(first, later, "the dots moved on their own");
    assert!(later.iter().all(|v| (0.0..=1.0).contains(v)));
    w.let_go();
    assert!(!w.up() && !scheduled(&w, "chase"));
}

#[test]
fn the_notes_size_slider_has_a_text_bubble_that_needs_nothing_running() {
    let mut w = notes(140.0, false);
    w.plugin.on_ui(&mut w.host, &UiEvent { ui: "card".into(), callback: "text-edited".into(), args: vec![json!("The first lines of what I read out loud")] });
    w.feed();
    w.hold("Text size");
    assert!(w.up() && w.peek_kind() == "text");
    // a press halfway along 16 to 120 is 68, and the bubble draws the size the slider now says
    assert_eq!(w.peek_numbers(), vec![68.0], "the size on the phone, in sp");
    assert_eq!(w.peek_text(), "The first lines of what I read out loud");
    assert!(w.said_names().is_empty(), "no timer: it is a sample drawn by the window");
    w.let_go();
    assert!(!w.up());
}

#[test]
fn a_long_set_gives_the_bubble_only_its_start() {
    let mut w = notes(140.0, false);
    let long = "word ".repeat(5000);
    w.plugin.on_ui(&mut w.host, &UiEvent { ui: "card".into(), callback: "text-edited".into(), args: vec![json!(long)] });
    w.feed();
    w.hold("Text size");
    assert_eq!(w.peek_text().chars().count(), telescope_effects::notes::SAMPLE_CHARS);
    w.let_go();
}

#[test]
fn the_blur_slider_gets_a_bubble_until_the_preview_blurs() {
    let mut w = background(200.0, false);
    // not streaming: the preview has nothing to blur
    w.hold("Blur");
    assert!(w.up() && w.peek_kind() == "picture");
    assert_eq!(w.peek_picture(), (peek::WIDTH as u32, peek::HEIGHT as u32));
    w.let_go();
    assert!(!w.up());
    // streaming with vision ready: the preview blurs, the bubble stays away
    w.host.streaming = true;
    w.event(Event::StreamStarted { url: String::new() });
    w.draw();
    w.hold("Blur");
    assert!(!w.up());
    w.let_go();
}

#[test]
fn holding_a_slider_changes_the_same_settings_as_ever() {
    // a press a quarter of the way along Blur (1 to 10) is 3, and nothing else in the settings moves
    let mut w = background(200.0, false);
    let before = w.plugin.get_config();
    w.hold_at("Blur", 0.25);
    assert!(w.up());
    w.let_go();
    let mut expected = before.clone();
    expected["blur"] = json!(3);
    assert_eq!(w.plugin.get_config(), expected);
    // the same on Framing (0 to 30 is Head to Body, so halfway is 1.5) and on the notes' size (16 to 120)
    let mut f = framing(140.0, false);
    let before = f.plugin.get_config();
    f.hold_at("Framing", 0.5);
    f.let_go();
    let mut expected = before.clone();
    expected["framing"] = json!(1.5);
    assert_eq!(f.plugin.get_config(), expected);
    let mut n = notes(140.0, false);
    let before = n.plugin.get_config();
    n.hold_at("Text size", 0.5);
    n.let_go();
    let mut expected = before.clone();
    expected["size"] = json!(68);
    assert_eq!(n.plugin.get_config(), expected);
}

/// Pictures of each card with its bubbles, in both looks: SHOTS=dir cargo test --test peek_cards shots
#[test]
fn shots() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (look, classic) in [("new", false), ("classic", true)] {
        for (name, label, make) in [
            ("framing", "Framing", framing as fn(f32, bool) -> Wrap),
            ("speed", "Speed", framing as fn(f32, bool) -> Wrap),
            ("notes", "Text size", notes as fn(f32, bool) -> Wrap),
            ("blur", "Blur", background as fn(f32, bool) -> Wrap),
        ] {
            let mut w = make(if name == "blur" { 200.0 } else { 150.0 }, classic);
            if name == "notes" {
                w.plugin.on_ui(&mut w.host, &UiEvent { ui: "card".into(), callback: "text-edited".into(), args: vec![json!("Good morning everyone and welcome to the call")] });
                w.feed();
            }
            w.draw().save_png(&dir.join(format!("{name}-{look}-closed.png"))).unwrap();
            w.hold(label);
            if name == "speed" {
                w.fire("chase", 75);
                w.draw();
            }
            w.draw().save_png(&dir.join(format!("{name}-{look}-open.png"))).unwrap();
            w.let_go();
        }
    }
}

/// The box of a held bubble in a picture of the window (x, y, width, height in pixels): the biggest block of `fill`, the colour behind what it draws.
fn find_box(p: &offscreen::Picture, fill: [u8; 3]) -> (usize, usize, usize, usize) {
    let (w, h) = (p.width as usize, p.height as usize);
    let is = |x: usize, y: usize| p.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3] == fill;
    let longest = |len: usize, count: &dyn Fn(usize) -> usize| {
        let (mut best, mut start) = ((0, 0), None);
        for i in 0..=len {
            if i < len && count(i) > 40 {
                start.get_or_insert(i);
            } else if let Some(s) = start.take() {
                if i - s > best.1 - best.0 {
                    best = (s, i);
                }
            }
        }
        best
    };
    let (x0, x1) = longest(w, &|x| (200..480.min(h)).filter(|y| is(x, *y)).count());
    let (y0, y1) = longest(h, &|y| (x0..x1).filter(|x| is(*x, y)).count());
    (x0, y0, x1 - x0, y1 - y0)
}

/// The same crop of each picture, tiled in rows of `cols` with a few pixels of grey between.
fn tile(pictures: &[offscreen::Picture], crop: (usize, usize, usize, usize), cols: usize) -> offscreen::Picture {
    let (cx, cy, cw, ch) = crop;
    let rows = pictures.len().div_ceil(cols);
    let (w, h) = (cols * cw + (cols - 1) * 4, rows * ch + (rows - 1) * 4);
    let mut rgba = vec![96u8; w * h * 4];
    for (n, p) in pictures.iter().enumerate() {
        let (ox, oy) = ((n % cols) * (cw + 4), (n / cols) * (ch + 4));
        for y in 0..ch {
            let from = ((cy + y) * p.width as usize + cx) * 4;
            let to = ((oy + y) * w + ox) * 4;
            rgba[to..to + cw * 4].copy_from_slice(&p.rgba[from..from + cw * 4]);
        }
    }
    offscreen::Picture { width: w as u32, height: h as u32, rgba }
}

fn stack(parts: &[&offscreen::Picture]) -> offscreen::Picture {
    let w = parts.iter().map(|p| p.width).max().unwrap() as usize;
    let h: usize = parts.iter().map(|p| p.height as usize + 12).sum::<usize>() - 12;
    let mut rgba = vec![40u8; w * h * 4];
    let mut y = 0;
    for p in parts {
        for row in 0..p.height as usize {
            let to = ((y + row) * w) * 4;
            rgba[to..to + p.width as usize * 4].copy_from_slice(&p.rgba[row * p.width as usize * 4..(row + 1) * p.width as usize * 4]);
        }
        y += p.height as usize + 12;
    }
    offscreen::Picture { width: w as u32, height: h as u32, rgba }
}

/// What the Speed bubble drew before: a ring for you and a dot for the framing, two sides of the box, a pause of 2 s, a move of half a second.
fn old_speed_bubble() -> ComponentInstance {
    use telescope_effects::framing::{settle, smoothed, tuning, Spring, Target};
    let _ = (settle, smoothed, tuning, Spring::new, Target::new);
    offscreen::install();
    let doc = r#"
        import { Theme } from "@ui-kit/ui-kit.slint";
        export component W inherits Window {
            width: 176px;
            height: 99px;
            background: Theme.trough;
            in property <[float]> v: [0.3, 0.5, 0.3, 0.5];
            in property <color> hue: Theme.accent;
            pure function at(n: float, extent: length) -> length { Theme.u(6) + clamp(n, 0, 1) * (extent - Theme.u(12) - Theme.u(6)) }
            Rectangle {
                width: Theme.u(6); height: Theme.u(6);
                x: at(v[0], parent.width); y: at(v[1], parent.height);
                border-radius: self.height / 2; border-width: Theme.stroke; border-color: Theme.ink2;
            }
            Rectangle {
                width: Theme.u(6); height: Theme.u(6);
                x: at(v[2], parent.width); y: at(v[3], parent.height);
                border-radius: self.height / 2; background: root.hue;
            }
        }"#;
    let w = telescope_shell::compile::compile("old.slint", doc, vec![], "W").unwrap_or_else(|e| panic!("{e}")).create().unwrap();
    w.show().unwrap();
    w
}

/// The old Chase, as it was: 33 ms ticks from the left side, the first move after 2 s.
fn old_chase(speed: i64, frames: usize, every: f64) -> Vec<offscreen::Picture> {
    use telescope_effects::framing::{settle, smoothed, tuning, Spring, Target};
    let you = |t: f64| {
        let (hold, mv, leg) = (2.0, 0.5, 2.5);
        let phase = t.rem_euclid(2.0 * leg);
        let (going, into) = if phase < leg { (true, phase) } else { (false, phase - leg) };
        let m = ((into - hold) / mv).clamp(0.0, 1.0);
        let eased = m * m * (3.0 - 2.0 * m);
        let along = if going { eased } else { 1.0 - eased };
        Target::new(0.3 + 0.4 * along, 0.5 - 0.2 * (std::f64::consts::PI * along).sin(), 2.0)
    };
    let w = old_speed_bubble();
    let (mut at, mut smooth, mut goal, mut spring) = (0.0f64, None, None, Spring::new(you(0.0)));
    let (omega, tau, scale) = tuning(speed);
    let hue = Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(0x7d, 0xc4, 0x8c)));
    let mut out = Vec::new();
    for i in 0..frames {
        // the frames are `every` seconds apart, in the 33 ms ticks the old bubble ran at
        let ticks = if i == 0 { 0 } else { (every / 0.033).round() as usize };
        let mut last = [0.3, 0.5, 0.3, 0.5];
        for _ in 0..ticks {
            at += 0.033;
            let target = you(at);
            let s = smoothed(smooth, target, 0.033, tau);
            smooth = Some(s);
            let g = settle(goal, s, 0.033, scale);
            goal = Some(g);
            let f = spring.step(g, 0.033, omega);
            last = [target.x, target.y, f.x, f.y];
        }
        if i == 0 {
            let t = you(0.0);
            last = [t.x, t.y, t.x, t.y];
        }
        w.set_property("v", Value::Model(Rc::new(slint::VecModel::from(last.iter().map(|n| Value::Number(*n)).collect::<Vec<_>>())).into())).unwrap();
        w.set_property("hue", hue.clone()).unwrap();
        out.push(offscreen::draw(w.window(), 176.0, 99.0, 2.0).expect("a picture"));
    }
    out
}

/// The Speed bubble drawn every twelfth of a second for a second after it opens, at two speeds, to see the follow by eye, with the old one
/// (twelve frames across three seconds, as nothing moves in the first two) beside it: SHOTS=dir cargo test --test peek_cards speed_film
#[test]
fn speed_film() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut after = Vec::new();
    for speed in [2.0f32, 9.0] {
        let mut w = framing_stepping(150.0, false, 1.0 / 60.0);
        w.hold_at("Speed", (speed - 1.0) / 9.0);
        // The handle has glided to its step and the bubble with it before the film starts
        for _ in 0..30 {
            offscreen::skip(std::time::Duration::from_millis(50));
            w.draw();
        }
        let mut frames = Vec::new();
        for _ in 0..12 {
            w.fire("chase", 5);
            frames.push(offscreen::draw(w.w.window(), WIDTH, HEIGHT, 2.0).expect("a picture"));
        }
        // the box inside the bubble, at twice the size (found where the bubble landed, which follows the handle)
        let (x, y, bw, bh) = find_box(&frames[0], [0x1a, 0x1a, 0x1a]);
        let strip = tile(&frames, (x, y, 352.min(bw), 198.min(bh)), 6);
        strip.save_png(&dir.join(format!("after-speed{speed}.png"))).unwrap();
        after.push(strip);
        w.let_go();
    }
    let mut before = Vec::new();
    for speed in [2i64, 9] {
        let frames = old_chase(speed, 12, 0.25);
        let strip = tile(&frames, (0, 0, 352, 198), 6);
        strip.save_png(&dir.join(format!("before-speed{speed}.png"))).unwrap();
        before.push(strip);
    }
    stack(&[&before[0], &before[1]]).save_png(&dir.join("before.png")).unwrap();
    stack(&[&after[0], &after[1]]).save_png(&dir.join("after.png")).unwrap();
}

/// The Framing bubble held along the slider, from the tightest to the widest, each alone and all in one strip, at twice the size:
/// SHOTS=dir cargo test --test peek_cards framing_levels
#[test]
fn framing_levels() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut single = Vec::new();
    for (n, along) in [0.0f32, 1.0 / 3.0, 0.5, 2.0 / 3.0, 5.0 / 6.0, 1.0].into_iter().enumerate() {
        let mut w = framing(150.0, false);
        w.hold_at("Framing", along);
        let shot = offscreen::draw(w.w.window(), WIDTH, HEIGHT, 2.0).expect("a picture");
        // the bubble's outline is the accent colour above the card's header; the picture sits 20 pixels inside it
        let accent = |x: usize, y: usize| {
            let p = &shot.rgba[(y * shot.width as usize + x) * 4..];
            p[1] as i32 > p[0] as i32 + 50 && p[1] as i32 > p[2] as i32 + 30
        };
        let on: Vec<(usize, usize)> = (100..300).flat_map(|y| (0..shot.width as usize).map(move |x| (x, y))).filter(|&(x, y)| accent(x, y)).collect();
        let (x0, y0) = (on.iter().map(|p| p.0).min().unwrap(), on.iter().map(|p| p.1).min().unwrap());
        let one = tile(std::slice::from_ref(&shot), (x0 + 20, y0 + 20, 352, 198), 1);
        one.save_png(&dir.join(format!("framing-{n}.png"))).unwrap();
        single.push(one);
        w.let_go();
    }
    tile(&single, (0, 0, 352, 198), 3).save_png(&dir.join("framing-levels.png")).unwrap();
}
