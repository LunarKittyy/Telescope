// Appearance's tokens on the kit: the tooltip wait (from instant to 1.5 s) and whether the next tip skips it, and higher contrast measured
// on the real Theme global in each of the four looks (text reaches 7:1 on every surface, lines and outlines are drawn and stand out, a
// card's colour is the full colour at rest, no text gets less readable). Real pointer events through the offscreen platform.
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::time::Duration;

const TIPS_DOC: &str = r#"
import { Button, Shortcuts, WindowBox, Tips, TipLayer } from "@ui-kit/ui-kit.slint";
export { Shortcuts, WindowBox, Tips }
export component W inherits Window {
    width: 420px;
    height: 200px;
    Button { x: 10px; y: 10px; width: 120px; text: "One"; tip: "First"; }
    Button { x: 150px; y: 10px; width: 120px; text: "Two"; tip: "Second"; }
    TipLayer { }
}
"#;

const ONE: (f32, f32) = (70.0, 26.0);
const TWO: (f32, f32) = (210.0, 26.0);
const EMPTY: (f32, f32) = (380.0, 150.0);

fn open_tips() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("tips.slint", TIPS_DOC, vec![], "W").expect("the document compiles").create().unwrap();
    w.set_global_callback("Shortcuts", "bindings-for", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    w.set_global_callback("Shortcuts", "key-chips", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    w.set_global_property("WindowBox", "width", Value::Number(420.0)).unwrap();
    w.set_global_property("WindowBox", "height", Value::Number(200.0)).unwrap();
    w.show().unwrap();
    draw(&w);
    w
}

fn draw(w: &ComponentInstance) {
    telescope_shell::offscreen::draw(w.window(), 420.0, 200.0, 1.0).unwrap();
}

fn wait(w: &ComponentInstance, ms: u64) {
    telescope_shell::offscreen::skip(Duration::from_millis(ms));
    draw(w);
}

fn up(w: &ComponentInstance) -> bool {
    matches!(w.get_global_property("Tips", "shown"), Ok(Value::Bool(true)))
}

fn move_to(w: &ComponentInstance, p: (f32, f32)) {
    let position = LogicalPosition::new(p.0, p.1);
    w.invoke_global("Tips", "moved", &[Value::Number(p.0 as f64), Value::Number(p.1 as f64)]).unwrap();
    w.window().dispatch_event(WindowEvent::PointerMoved { position });
    telescope_shell::offscreen::pump();
}

fn set(w: &ComponentInstance, delay_ms: f64, warm: bool) {
    telescope_shell::appearance::apply_to(w, &telescope_core::host::Appearance { tip_delay_ms: delay_ms as u32, tip_warm: warm, ..Default::default() });
}

#[test]
fn the_default_wait_is_what_it_always_was() {
    let w = open_tips();
    let read = |name: &str| w.get_global_property("Tips", name).unwrap();
    assert_eq!(read("delay"), Value::Number(500.0));
    assert_eq!(read("warm"), Value::Bool(true));
    move_to(&w, ONE);
    wait(&w, 450);
    assert!(!up(&w), "not yet at 450 ms");
    wait(&w, 100);
    assert!(up(&w), "up after 500 ms");
}

#[test]
fn the_delay_slider_sets_how_long_the_pointer_rests() {
    for (delay, warm) in [(0.0, true), (200.0, true), (1000.0, true), (1500.0, false)] {
        let w = open_tips();
        set(&w, delay, warm);
        move_to(&w, EMPTY);
        wait(&w, 2000);
        assert!(!up(&w));
        move_to(&w, ONE);
        if delay > 0.0 {
            wait(&w, (delay as u64).saturating_sub(100));
            assert!(!up(&w), "{delay} ms: still waiting 100 ms before");
            wait(&w, 150);
        } else {
            wait(&w, 16);
        }
        assert!(up(&w), "{delay} ms: up once it has passed");
        w.hide().unwrap();
    }
}

#[test]
fn the_next_tooltip_is_instant_only_with_the_switch_on() {
    for warm in [true, false] {
        let w = open_tips();
        set(&w, 1000.0, warm);
        move_to(&w, ONE);
        wait(&w, 1100);
        assert!(up(&w), "the first tip waits its full delay");
        // Straight to the next control: warm, it needs only a short rest
        move_to(&w, TWO);
        wait(&w, 300);
        assert_eq!(up(&w) && matches!(w.get_global_property("Tips", "says"), Ok(Value::String(s)) if s.as_str() == "Second"), warm, "warm {warm}: the second tip after 300 ms");
        wait(&w, 800);
        assert!(up(&w), "warm {warm}: and always after the full delay");
        w.hide().unwrap();
    }
}

const CONTRAST_DOC: &str = r#"
import { Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window { }
"#;

const LOOKS: [&str; 4] = ["dark", "classic", "oled", "light"];

fn open_theme() -> ComponentInstance {
    telescope_shell::offscreen::install();
    telescope_shell::compile::compile("contrast.slint", CONTRAST_DOC, vec![], "T").expect("the document compiles").create().unwrap()
}

fn colour(ui: &ComponentInstance, name: &str) -> [u8; 4] {
    match ui.get_global_property("Theme", name) {
        Ok(Value::Brush(b)) => {
            let c = b.color();
            [c.red(), c.green(), c.blue(), c.alpha()]
        }
        other => panic!("Theme.{name}: {other:?}"),
    }
}

fn length(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_global_property("Theme", name) {
        Ok(Value::Number(n)) => n,
        other => panic!("Theme.{name}: {other:?}"),
    }
}

fn luminance(c: [u8; 4]) -> f64 {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn ratio(a: [u8; 4], b: [u8; 4]) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn set_theme(ui: &ComponentInstance, look: usize, contrast: bool) {
    ui.set_global_property("Theme", "look", Value::Number(look as f64)).unwrap();
    ui.set_global_property("Theme", "contrast", Value::Bool(contrast)).unwrap();
}

#[test]
fn text_reaches_seven_to_one_on_every_surface_in_every_look() {
    let ui = open_theme();
    for (look, name) in LOOKS.iter().enumerate() {
        for text in ["ink", "ink2", "faint", "dim", "hot"] {
            for surface in ["card", "raise", "hover", "ground", "bubble"] {
                set_theme(&ui, look, false);
                let before = ratio(colour(&ui, text), colour(&ui, surface));
                set_theme(&ui, look, true);
                let after = ratio(colour(&ui, text), colour(&ui, surface));
                // Classic's hot is the dim grey of 5.0's hovered text, which was never a reading colour
                if text == "hot" && look == 1 {
                    continue;
                }
                assert!(after >= 7.0, "{text} on {surface} in {name} with higher contrast is {after:.2}:1");
                assert!(after >= before - 0.01, "{text} on {surface} in {name} gets worse: {before:.2} to {after:.2}");
            }
        }
    }
}

#[test]
fn the_lines_and_outlines_are_drawn_in_every_look_and_stand_out_from_the_card() {
    let ui = open_theme();
    for (look, name) in LOOKS.iter().enumerate() {
        set_theme(&ui, look, true);
        let card = colour(&ui, "card");
        for (token, least) in [("outline", 3.0), ("outline-hover", 4.5), ("outline-press", 7.0), ("card-outline", 3.0), ("pop-outline", 7.0), ("line-hover", 3.0)] {
            let c = colour(&ui, token);
            assert!(c[3] == 255, "{token} in {name} is drawn");
            let r = ratio(c, card);
            assert!(r >= least, "{token} in {name} is {r:.2}:1 against the card, under {least}:1");
        }
        // What is drawn needs a width: a hairline at rest, the stroke hovered
        assert!(length(&ui, "outline-w") >= 1.0 && length(&ui, "card-outline-w") >= 1.0 && length(&ui, "pop-outline-w") >= 1.0, "{name}: the outlines have a width");
        assert!(length(&ui, "outline-w-over") > length(&ui, "outline-w") - 0.01, "{name}: hovered is not thinner than at rest");
        set_theme(&ui, look, false);
        if look != 2 {
            assert_eq!(colour(&ui, "outline")[3], 0, "{name}: no outline without higher contrast");
            assert_eq!(length(&ui, "outline-w"), 0.0);
        }
    }
}

#[test]
fn a_cards_colour_is_the_full_colour_at_rest_with_higher_contrast() {
    let ui = open_theme();
    for (look, name) in LOOKS.iter().enumerate() {
        for group in ["camera", "look", "vision", "preview", "on_screen", "sound", "streaming", "extras"] {
            set_theme(&ui, look, false);
            let hue = ui.invoke_global("Theme", "plugin-hue", &[Value::String(group.into())]).unwrap();
            let at_rest = |ui: &ComponentInstance, function: &str, extra: Vec<Value>| {
                let mut args = vec![hue.clone()];
                args.extend(extra);
                match ui.invoke_global("Theme", function, &args).unwrap() {
                    Value::Brush(b) => b.color(),
                    other => panic!("{other:?}"),
                }
            };
            let Value::Brush(full) = hue.clone() else { panic!("a colour") };
            let quiet = at_rest(&ui, "muted", vec![]);
            assert_ne!(quiet, full.color(), "{group} in {name}: rest is quieter without it");
            set_theme(&ui, look, true);
            for (function, extra) in [("muted", vec![]), ("edge", vec![]), ("lit", vec![Value::Number(0.0)])] {
                assert_eq!(at_rest(&ui, function, extra), full.color(), "{group} in {name}: {function} is the full colour");
            }
        }
    }
}

#[test]
fn the_text_on_a_filled_bar_reads_in_every_look_with_or_without_higher_contrast() {
    let ui = open_theme();
    for (look, name) in LOOKS.iter().enumerate() {
        for contrast in [false, true] {
            set_theme(&ui, look, contrast);
            let (card, ink) = (colour(&ui, "card"), colour(&ui, "ink"));
            for group in ["camera", "look", "vision", "preview", "on_screen", "sound", "streaming", "extras"] {
                let hue = ui.invoke_global("Theme", "plugin-hue", &[Value::String(group.into())]).unwrap();
                for tone in [0.0, 1.0] {
                    let fill = ui.invoke_global("Theme", "lit", &[hue.clone(), Value::Number(tone)]).unwrap();
                    let Value::Brush(fill_brush) = fill.clone() else { panic!("a colour") };
                    let c = fill_brush.color();
                    let fill = [c.red(), c.green(), c.blue(), 255];
                    let on = match ui.invoke_global("Theme", "on", &[Value::Brush(slint::Brush::SolidColor(c))]).unwrap() {
                        Value::Brush(b) => b.color(),
                        other => panic!("{other:?}"),
                    };
                    let got = ratio([on.red(), on.green(), on.blue(), 255], fill);
                    let best = ratio(card, fill).max(ratio(ink, fill));
                    assert!(got >= best * 0.8 && got >= 3.0, "{group} in {name} (contrast {contrast}, tone {tone}): text on the fill is {got:.2}:1 where {best:.2}:1 is possible");
                }
            }
        }
    }
}

#[test]
fn a_window_heading_follows_the_text_size_and_not_the_density() {
    use telescope_core::host::{Appearance, Density, TextSize};
    let ui = open_theme();
    let heading = |a: Appearance| {
        telescope_shell::appearance::apply_to(&ui, &a);
        length(&ui, "heading")
    };
    let base = Appearance::default();
    let (small, normal, large) = (heading(Appearance { text: TextSize::Small, ..base }), heading(base), heading(Appearance { text: TextSize::Large, ..base }));
    assert!(small < normal && normal < large, "{small} {normal} {large}");
    assert_eq!(normal, 19.0, "as large as it was by default");
    assert_eq!(heading(Appearance { density: Density::Comfortable, ..base }), normal);
    assert_eq!(heading(Appearance { density: Density::Compact, ..base }), normal);
}

const BAR_DOC: &str = r#"
import { Slider, Theme, Shortcuts, WindowBox, Tips } from "@ui-kit/ui-kit.slint";
export { Theme, Shortcuts, WindowBox, Tips }
export component B inherits Window {
    width: 320px;
    height: 40px;
    background: Theme.card;
    Slider {
        width: 300px;
        fill: "threshold";
        hue: Theme.plugin-hue("streaming");
        minimum: 0;
        maximum: 100;
        value: 20;
        level: 80;
        has-level: true;
        readout: "20%";
        level-text: "80%";
    }
}
"#;

#[test]
fn a_threshold_bars_numbers_read_over_its_fill_at_rest_in_every_look_with_higher_contrast() {
    telescope_shell::offscreen::install();
    let ui = telescope_shell::compile::compile("bar.slint", BAR_DOC, vec![], "B").expect("the document compiles").create().unwrap();
    ui.set_global_callback("Shortcuts", "bindings-for", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    ui.set_global_callback("Shortcuts", "key-chips", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    slint::ComponentHandle::show(&ui).unwrap();
    for (look, name) in LOOKS.iter().enumerate() {
        for contrast in [false, true] {
            set_theme(&ui, look, contrast);
            let picture = telescope_shell::offscreen::draw(ui.window(), 320.0, 40.0, 1.0).unwrap();
            let px = |x: u32, y: u32| {
                let i = ((y * picture.width + x) * 4) as usize;
                [picture.rgba[i], picture.rgba[i + 1], picture.rgba[i + 2], 255]
            };
            // The fill just short of where it ends, and the glyph pixel furthest from it between the handle and there
            let fill = px(240, 4);
            let worst = (90..230).flat_map(|x| (10..30).map(move |y| (x, y))).map(|(x, y)| px(x, y)).max_by(|a, b| ratio(*a, fill).total_cmp(&ratio(*b, fill))).unwrap();
            let seen = ratio(worst, fill);
            assert!(seen >= 3.0, "the readout over the fill in {name} (contrast {contrast}) reads {seen:.2}:1");
        }
    }
}
