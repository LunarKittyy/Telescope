// test_widgets.py's checks of what a widget looks like, on the kit's widgets drawn by the software renderer.
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, SharedString};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::{compile, offscreen};

const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 200.0;

const DOC: &str = r#"
import { Theme, Slider, Switch, Button, ChoiceField } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 300px;
    height: 200px;
    background: #000;
    in property <float> value: 100;
    in property <float> floor-value: 0;
    in-out property <bool> switched;
    out property <color> faint: Theme.text-faint;
    out property <color> accent: Theme.accent;
    out property <color> fill: Theme.border-hover;
    // Row 36px tall, its handle's centre running from x 10 to x 190
    Rectangle {
        x: 0;
        y: 0;
        width: 200px;
        height: 36px;
        HorizontalLayout {
            Slider { minimum: 100; maximum: 1000; value: value; marks: [300, 700]; has-readout: false; hue: #ff0000; label: "Marks"; }
        }
    }
    Rectangle {
        x: 0;
        y: 40px;
        width: 200px;
        height: 36px;
        HorizontalLayout {
            Slider { minimum: 0; maximum: 100; value: floor-value; has-readout: false; enabled: false; label: "Fill"; }
        }
    }
    Button { x: 10px; y: 90px; width: 80px; height: 30px; text: "First"; }
    ChoiceField { x: 110px; y: 90px; width: 120px; compact: true; caption: "Choice"; items: ["a", "b"]; }
    Switch { x: 10px; y: 150px; label: "Check daily"; toggled => { switched = true; } }
}
"#;

fn rig() -> ComponentInstance {
    offscreen::install();
    let ui = compile::compile("widgets.slint", DOC, Vec::new(), "Test").unwrap().create().unwrap();
    ui.show().unwrap();
    ui
}

struct Drawn(offscreen::Picture);

impl Drawn {
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.0.width + x) * 4) as usize;
        [self.0.rgba[i], self.0.rgba[i + 1], self.0.rgba[i + 2]]
    }
}

fn draw(ui: &ComponentInstance) -> Drawn {
    Drawn(offscreen::draw(ui.window(), WIDTH, HEIGHT, 1.0).expect("a picture"))
}

fn colour(ui: &ComponentInstance, name: &str) -> [u8; 3] {
    let Ok(Value::Brush(brush)) = ui.get_property(name) else { panic!("{name}") };
    let c = brush.color();
    [c.red(), c.green(), c.blue()]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 12)
}

/// Where a colour runs along row y, as the middle of each run.
fn runs(picture: &Drawn, y: u32, wanted: [u8; 3]) -> Vec<f32> {
    let (mut out, mut start) = (Vec::new(), None);
    for x in 0..picture.0.width {
        match (near(picture.at(x, y), wanted), start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push((s + x - 1) as f32 / 2.0);
                start = None;
            }
            _ => {}
        }
    }
    out
}

#[test]
fn zoom_slider_marks_sit_where_the_handle_would() {
    let ui = rig();
    let set = |v: f64| ui.set_property("value", Value::Number(v)).unwrap();
    set(1000.0);
    let faint = colour(&ui, "faint");
    let marks = runs(&draw(&ui), 18, faint);
    assert_eq!(marks.len(), 2, "{marks:?}");
    for (value, mark) in [(300.0, marks[0]), (700.0, marks[1])] {
        set(value);
        let handle = runs(&draw(&ui), 18, [255, 0, 0]);
        assert_eq!(handle.len(), 1, "{handle:?}");
        assert!((handle[0] - mark).abs() <= 1.0, "the handle at {value} is at {}, its mark at {mark}", handle[0]);
    }
}

#[test]
fn slider_fill_doesnt_stick_out_past_the_handle_at_zero() {
    let ui = rig();
    let (picture, fill) = (draw(&ui), colour(&ui, "fill"));
    let handle_left = 3; // its centre runs from 10, and it's 14 wide
    assert!((0..handle_left).all(|x| !near(picture.at(x, 58), fill)), "at 0 the handle is the leftmost thing on the groove");
    assert!((0..handle_left).all(|x| picture.at(x, 58) == [0, 0, 0]));
}

fn tab(ui: &ComponentInstance) {
    let text: SharedString = Key::Tab.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

#[test]
fn focus_ring_shows_after_tab_and_not_after_a_click() {
    let ui = rig();
    let accent = colour(&ui, "accent");
    let ringed = |x: u32, y: u32| (0..3).any(|d| near(draw(&ui).at(x - d, y), accent));
    assert!(!ringed(110, 106));
    tab(&ui); // the slider
    tab(&ui); // the button
    tab(&ui);
    assert!(ringed(110, 106), "Tab reached the dropdown: its ring is just outside its left edge");
    let position = LogicalPosition::new(22.0, 160.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    assert_eq!(ui.get_property("switched"), Ok(Value::Bool(true)));
    assert!(!ringed(10, 160), "a click shows no ring on the switch");
    assert!(ringed(110, 106), "it stays where the keyboard put it: a click on a switch doesn't take the focus");
}
