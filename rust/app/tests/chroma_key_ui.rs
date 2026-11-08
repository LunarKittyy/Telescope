//! Chroma key's colour row and the kit's bubble, in a real offscreen window: where the bubble goes, how it closes, and that a pick
//! on the pad changes what the plugin stores. SHOTS=dir cargo test --test chroma_key_ui shots draws the card for a look by eye.

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use serde_json::{json, Map, Value as Json};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_shell::source::{from_json, parse_color};
use telescope_shell::{input, offscreen};

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 700.0;
const LABEL: &str = "Colour behind you";

/// The Slint type a card property of this JSON value gets in the wrapper that forwards them.
fn slint_type(key: &str, value: &Json) -> &'static str {
    match value {
        Json::Bool(_) => "bool",
        Json::Number(_) => "float",
        Json::String(s) if s.len() == 7 && s.starts_with('#') && parse_color(s).is_some() => "color",
        Json::String(_) => "string",
        Json::Array(items) if key == "channel-value" => {
            let _ = items;
            "[int]"
        }
        Json::Array(items) if items.iter().all(Json::is_string) => "[string]",
        Json::Array(_) => "[float]",
        _ => "string",
    }
}

type Moves = Rc<RefCell<Vec<UiEvent>>>;

struct Rig {
    plugin: ChromaKeyPlugin,
    host: FakeHost,
    w: ComponentInstance,
    /// What the card said to the plugin, in order
    said: Moves,
    keys: Vec<String>,
}

impl Rig {
    /// The card `top` pixels down a window, in the look `classic` says.
    fn new(top: f32, classic: bool) -> Rig {
        offscreen::install();
        let (mut plugin, mut host) = (ChromaKeyPlugin::new(), FakeHost::new());
        plugin.setup(&mut host);
        plugin.set_config(&json!({"on": true})).unwrap();
        plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
        let card = plugin.card().expect("a card");
        let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
        let props = card.ui.props.as_object().unwrap().clone();
        let keys: Vec<String> = props.keys().cloned().collect();
        let declared: String = props.iter().map(|(k, v)| format!("    in-out property <{}> {k};\n", slint_type(k, v))).collect();
        let forwarded: String = keys.iter().map(|k| format!("{k}: root.{k}; ")).collect();
        let doc = format!(
            "import {{ Theme, WindowBox, BubbleSpot }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
             export {{ Theme, WindowBox, BubbleSpot }}\n\
             export component W inherits Window {{\n    width: {WIDTH}px;\n    height: {HEIGHT}px;\n    background: Theme.ground;\n{declared}\
             callback colour-moved(float, float, float);\n    callback hex-typed(string);\n    callback channel-typed(int, string);\n    callback pipette-clicked();\n    \
             init => {{ WindowBox.width = self.width; WindowBox.height = self.height; }}\n    \
             changed width => {{ WindowBox.width = self.width; }}\n    changed height => {{ WindowBox.height = self.height; }}\n    \
             {c} {{ x: 20px; y: {top}px; width: parent.width - 40px; hue: Theme.plugin-hue(\"look\"); {forwarded}\
             colour-moved(h, s, v) => {{ root.colour-moved(h, s, v); }} hex-typed(t) => {{ root.hex-typed(t); }} \
             channel-typed(i, t) => {{ root.channel-typed(i, t); }} pipette-clicked => {{ root.pipette-clicked(); }} }}\n}}",
            c = card.ui.component
        );
        let definition = telescope_shell::compile::compile("wrapper.slint", &doc, vec![("plugin/card.slint".to_string(), source)], "W").unwrap_or_else(|e| panic!("{e}"));
        let w = definition.create().unwrap();
        w.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        let said: Moves = Rc::new(RefCell::new(Vec::new()));
        for name in ["colour-moved", "hex-typed", "channel-typed", "pipette-clicked"] {
            let log = said.clone();
            w.set_callback(name, move |args| {
                log.borrow_mut().push(UiEvent { ui: "card".into(), callback: name.into(), args: args.iter().map(telescope_shell::source::to_json).collect() });
                Value::Void
            })
            .unwrap();
        }
        w.show().unwrap();
        let rig = Rig { plugin, host, w, said, keys };
        rig.feed();
        rig
    }

    /// Hands the card what the plugin told it: its first props, then each later update.
    fn feed(&self) {
        let mut props: Map<String, Json> = self.plugin_props();
        for call in &self.host.calls {
            if let Some(json) = call.strip_prefix("set_ui_props card ") {
                if let Ok(Json::Object(update)) = serde_json::from_str::<Json>(json) {
                    props.extend(update);
                }
            }
        }
        for key in &self.keys {
            let Some(value) = props.get(key) else { continue };
            let slint = match (value, parse_color(value.as_str().unwrap_or(""))) {
                (Json::String(s), Some(c)) if s.starts_with('#') && s.len() == 7 => Value::Brush(slint::Brush::SolidColor(c)),
                _ => from_json(value),
            };
            self.w.set_property(key, slint).unwrap();
        }
    }

    fn plugin_props(&self) -> Map<String, Json> {
        let mut plugin = ChromaKeyPlugin::new();
        plugin.set_config(&self.plugin.get_config()).unwrap();
        plugin.card().unwrap().ui.props.as_object().unwrap().clone()
    }

    /// Replays what the card said to the plugin, then hands the card the result.
    fn pass_on(&mut self) {
        let said: Vec<UiEvent> = self.said.borrow_mut().drain(..).collect();
        for event in said {
            self.plugin.on_ui(&mut self.host, &event);
        }
        self.feed();
    }

    fn draw(&self) -> offscreen::Picture {
        offscreen::draw(self.w.window(), WIDTH, HEIGHT, 1.0).expect("a picture")
    }

    fn swatch(&self) -> ElementHandle {
        self.find(LABEL, AccessibleRole::Button)
    }

    fn find(&self, label: &str, role: AccessibleRole) -> ElementHandle {
        ElementHandle::find_by_accessible_label(&self.w, label).find(|e| e.accessible_role() == Some(role)).unwrap_or_else(|| panic!("no {label} ({role:?})"))
    }

    fn bubble(&self) -> Option<ElementHandle> {
        ElementHandle::find_by_accessible_label(&self.w, LABEL).find(|e| e.accessible_role() == Some(AccessibleRole::Groupbox))
    }

    fn press(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        self.w.window().dispatch_event(WindowEvent::PointerMoved { position });
        input::pressed_at(&self.w, position);
        self.w.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    }

    fn release(&self, x: f32, y: f32) {
        self.w.window().dispatch_event(WindowEvent::PointerReleased { position: LogicalPosition::new(x, y), button: PointerEventButton::Left });
    }

    fn click(&self, x: f32, y: f32) {
        self.press(x, y);
        self.release(x, y);
        self.draw();
        // A bubble fades out before it goes
        offscreen::skip(std::time::Duration::from_millis(300));
        self.draw();
    }

    fn click_on(&self, e: &ElementHandle) {
        let (p, s) = (e.absolute_position(), e.size());
        self.click(p.x + s.width / 2.0, p.y + s.height / 2.0);
    }

    fn key(&self, key: Key) {
        let text = slint::SharedString::from(key);
        self.w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        self.w.window().dispatch_event(WindowEvent::KeyReleased { text });
        self.draw();
        // A bubble fades out before it goes
        offscreen::skip(std::time::Duration::from_millis(300));
        self.draw();
    }

    fn open(&self) {
        self.draw();
        self.click_on(&self.swatch());
        assert!(self.bubble().is_some(), "a click on the swatch opens the bubble");
    }

    fn colour(&self) -> Json {
        self.plugin.get_config()["colour"].clone()
    }
}

/// (x, y, width, height) of an element.
fn rect(e: &ElementHandle) -> (f32, f32, f32, f32) {
    (e.absolute_position().x, e.absolute_position().y, e.size().width, e.size().height)
}

fn sliders(w: &ComponentInstance) -> impl Iterator<Item = ElementHandle> {
    i_slint_backend_testing::ElementQuery::from_root(w).match_descendants().match_accessible_role(AccessibleRole::Slider).find_all().into_iter()
}

fn number(v: Value) -> f64 {
    match v {
        Value::Number(n) => n,
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_bubble_goes_below_near_the_window_top_and_inside_the_window() {
    let rig = Rig::new(0.0, false);
    let spot = |name: &str, args: &[f64]| -> Value {
        // `below` is the one boolean argument, and `top` takes it fourth
        let args: Vec<Value> = args.iter().enumerate().map(|(i, a)| if name == "top" && i == 3 { Value::Bool(*a != 0.0) } else { Value::Number(*a) }).collect();
        rig.w.invoke_global("BubbleSpot", name, &args).unwrap()
    };
    // window 400 tall, a control 30 tall, a bubble 200 tall: room above the control at 300, none at 100 or 20
    assert_eq!(spot("below", &[300.0, 30.0, 200.0, 400.0]), Value::Bool(false));
    assert_eq!(spot("below", &[100.0, 30.0, 200.0, 400.0]), Value::Bool(true));
    assert_eq!(spot("below", &[20.0, 30.0, 200.0, 400.0]), Value::Bool(true));
    // a window too small for either side: the side with more room
    assert_eq!(spot("below", &[150.0, 30.0, 300.0, 400.0]), Value::Bool(true));
    assert_eq!(spot("below", &[250.0, 30.0, 300.0, 400.0]), Value::Bool(false));
    // above the control, a gap off it; below, a gap under it
    assert_eq!(number(spot("top", &[300.0, 30.0, 200.0, 0.0, 400.0])), 300.0 - 2.0 - 200.0);
    assert_eq!(number(spot("top", &[100.0, 30.0, 200.0, 1.0, 400.0])), 100.0 + 30.0 + 2.0);
    // never past the window's edges
    assert_eq!(number(spot("top", &[0.0, 30.0, 200.0, 0.0, 400.0])), 8.0);
    assert_eq!(number(spot("top", &[390.0, 30.0, 200.0, 1.0, 400.0])), 400.0 - 200.0 - 8.0);
    // across: centred on the control, pushed in from the sides
    assert_eq!(number(spot("left", &[200.0, 40.0, 100.0, 460.0])), 170.0);
    assert_eq!(number(spot("left", &[0.0, 40.0, 100.0, 460.0])), 8.0);
    assert_eq!(number(spot("left", &[440.0, 40.0, 100.0, 460.0])), 460.0 - 100.0 - 8.0);
    // the pointer stays over the control, but off the rounded corners
    assert_eq!(number(spot("tip", &[200.0, 40.0, 170.0, 100.0])), 50.0);
    assert_eq!(number(spot("tip", &[0.0, 40.0, 8.0, 100.0])), 14.0 + 8.0);
}

#[test]
fn the_bubble_opens_above_the_colour_row_and_below_it_near_the_top() {
    // the row is ~245px down the card: with the card 60px down there is room above, with it 40px off the top there is not
    for (top, above) in [(60.0, true), (-40.0, false)] {
        let rig = Rig::new(top, false);
        rig.open();
        let (swatch, bubble) = (rect(&rig.swatch()), rect(&rig.bubble().unwrap()));
        assert!(bubble.0 >= 0.0 && bubble.1 >= 0.0 && bubble.0 + bubble.2 <= WIDTH && bubble.1 + bubble.3 <= HEIGHT, "inside the window: {bubble:?}");
        if above {
            assert!(bubble.1 + bubble.3 <= swatch.1, "above the swatch: {bubble:?} vs {swatch:?}");
        } else {
            assert!(bubble.1 >= swatch.1 + swatch.3, "below the swatch: {bubble:?} vs {swatch:?}");
        }
        let centre = |r: (f32, f32, f32, f32)| r.0 + r.2 / 2.0;
        assert!((centre(bubble) - centre(swatch)).abs() < 40.0, "roughly over the swatch");
    }
}

#[test]
fn opening_it_moves_nothing_in_the_card() {
    let rig = Rig::new(60.0, false);
    rig.draw();
    let before = rect(&rig.swatch());
    let before_sliders: Vec<_> = sliders(&rig.w).map(|e| rect(&e)).collect();
    rig.open();
    let after: Vec<_> = sliders(&rig.w).map(|e| rect(&e)).collect();
    assert_eq!(rect(&rig.swatch()), before);
    assert!(before_sliders.iter().all(|r| after.contains(r)), "the card's sliders stayed put");
}

#[test]
fn escape_and_a_press_outside_close_it_and_the_card_behind_does_not_hear_the_press() {
    let rig = Rig::new(60.0, false);
    rig.open();
    rig.key(Key::Escape);
    assert!(rig.bubble().is_none(), "Escape closes it");
    assert_eq!(rig.swatch().accessible_expanded(), Some(false));
    // the focus went back to the swatch, so Space opens it again
    for event in [WindowEvent::KeyPressed { text: " ".into() }, WindowEvent::KeyReleased { text: " ".into() }] {
        rig.w.window().dispatch_event(event);
    }
    rig.draw();
    assert!(rig.bubble().is_some(), "Space on the swatch opens it, so the focus came back to it");
    rig.key(Key::Escape);

    rig.open();
    // a press on the swatch itself, which would open it again if the card heard it
    rig.click_on(&rig.swatch());
    assert!(rig.bubble().is_none(), "a press outside closes it, and the swatch under it does not open it again");
    rig.open();
    // a press on blank window beside the card
    rig.click(4.0, 4.0);
    assert!(rig.bubble().is_none(), "a press beside the card closes it too");
    let mut rig = rig;
    rig.pass_on();
    assert_eq!(rig.plugin.get_config()["tolerance"], json!(40), "and nothing under the press changed");

    // pressing inside the frame, away from any control, leaves it open
    rig.open();
    let (x, y, w, h) = rect(&rig.bubble().unwrap());
    rig.click(x + w - 3.0, y + h / 2.0);
    assert!(rig.bubble().is_some(), "a press on its own face does nothing");
}

#[test]
fn a_press_on_the_pad_changes_the_stored_colour_and_the_pad_shows_it() {
    let mut rig = Rig::new(60.0, false);
    rig.open();
    let before = rig.colour();
    let pad = rig.find("Saturation and brightness", AccessibleRole::Slider);
    let (x, y, w, h) = rect(&pad);
    // the top right corner: full saturation, full brightness, of the hue the colour has
    rig.click(x + w - 1.0, y + 1.0);
    rig.pass_on();
    let first = rig.colour();
    assert_ne!(first, before, "picking changed the setting");
    let rgb = |c: &Json| -> Vec<i64> { c.as_array().unwrap().iter().map(|n| n.as_i64().unwrap()).collect() };
    let bgr = rgb(&first);
    assert!(bgr.iter().copied().max().unwrap() >= 250, "full brightness: some channel is full: {bgr:?}");
    assert!(bgr.iter().copied().min().unwrap() <= 5, "full saturation: some channel is empty: {bgr:?}");

    // the bottom edge is black whatever the saturation
    rig.press(x + w / 2.0, y + h - 1.0);
    rig.release(x + w / 2.0, y + h - 1.0);
    rig.draw();
    rig.pass_on();
    assert!(rgb(&rig.colour()).iter().all(|c| *c <= 4), "the bottom edge is black: {:?}", rig.colour());

    // the hue strip changes the colour again, and the strip's hue is what the card reports back
    let strip = rig.find("Hue", AccessibleRole::Slider);
    let (sx, sy, sw, sh) = rect(&strip);
    rig.click(sx + sw / 2.0, sy + sh / 2.0);
    rig.pass_on();
    assert!((told_number(&rig, "colour-h") - 0.5).abs() < 0.08, "the strip's middle is cyan: {}", told_number(&rig, "colour-h"));
}

fn told_number(rig: &Rig, key: &str) -> f64 {
    let mut found = None;
    for call in &rig.host.calls {
        if let Some(json) = call.strip_prefix("set_ui_props card ") {
            if let Some(v) = serde_json::from_str::<Json>(json).ok().and_then(|v| v.get(key).and_then(Json::as_f64)) {
                found = Some(v);
            }
        }
    }
    found.unwrap()
}

#[test]
fn the_exact_values_sit_behind_a_toggle_and_typing_one_sets_the_colour() {
    let mut rig = Rig::new(60.0, false);
    rig.open();
    assert!(ElementHandle::find_by_accessible_label(&rig.w, "Hex").next().is_none(), "values are hidden at first");
    let toggle = rig.find("Values", AccessibleRole::Switch);
    rig.click_on(&toggle);
    assert!(rig.bubble().is_some(), "still open");
    let hex = ElementHandle::find_by_accessible_label(&rig.w, "Hex").next().expect("the hex box");
    assert_eq!(hex.accessible_value().as_deref(), Some("1e2228"), "the default colour, as hex");
    // the same colour asked for through the callbacks the boxes use
    rig.said.borrow_mut().push(UiEvent { ui: "card".into(), callback: "hex-typed".into(), args: vec![json!("#ff8800")] });
    rig.pass_on();
    assert_eq!(rig.colour(), json!([0, 136, 255]), "stored as B, G, R as before");
    rig.said.borrow_mut().push(UiEvent { ui: "card".into(), callback: "channel-typed".into(), args: vec![json!(0), json!("300")] });
    rig.pass_on();
    assert_eq!(rig.colour(), json!([0, 136, 255]), "red held to 255");
    rig.said.borrow_mut().push(UiEvent { ui: "card".into(), callback: "channel-typed".into(), args: vec![json!(2), json!("10")] });
    rig.pass_on();
    assert_eq!(rig.colour(), json!([10, 136, 255]));
}

/// Pictures of the card, the bubble closed and open, in both looks: SHOTS=dir cargo test --test chroma_key_ui shots
#[test]
fn shots() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (look, classic) in [("new", false), ("classic", true)] {
        for (place, top) in [("above", 60.0), ("below", -40.0)] {
            let rig = Rig::new(top, classic);
            rig.draw().save_png(&dir.join(format!("chroma-{look}-{place}-closed.png"))).unwrap();
            rig.open();
            rig.draw().save_png(&dir.join(format!("chroma-{look}-{place}-open.png"))).unwrap();
            let toggle = rig.find("Values", AccessibleRole::Switch);
            rig.click_on(&toggle);
            rig.draw().save_png(&dir.join(format!("chroma-{look}-{place}-values.png"))).unwrap();
        }
    }
}
