// test_widgets.py's checks of what a widget looks like, on the kit's widgets drawn by the software renderer.
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, SharedString};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::{compile, input, offscreen};

const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 200.0;

const DOC: &str = r#"
import { Theme, Icons, Icon, Slider, Switch, Button, ChoiceField } from "@ui-kit/ui-kit.slint";
export { KeyFocus } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 300px;
    height: 200px;
    background: #000;
    in property <float> value: 100;
    in property <float> floor-value: 0;
    in-out property <int> switched;
    // Every icon the kit has, one at a time (the name-to-picture path a card takes), or a file's
    in property <string> icon-name: "bell";
    in property <image> icon-file;
    out property <color> accent-hue: Theme.accent;
    out property <color> faint: Theme.text-faint;
    out property <color> accent: Theme.accent;
    out property <color> fill: Theme.border-hover;
    out property <color> sel: Theme.sel;
    out property <color> muted-red: Theme.edge(#ff0000);
    out property <color> edge-accent: Theme.edge(Theme.accent);
    out property <color> muted-accent: Theme.muted(Theme.accent);
    // Row 32px tall in a 36px box, its handle's centre running from x 7 to x 193
    Rectangle {
        x: 0;
        y: 0;
        width: 200px;
        height: 36px;
        HorizontalLayout {
            Slider { minimum: 100; maximum: 1000; value: value; marks: [700, 1000, 300, 100]; has-readout: false; hue: #ff0000; label: "Marks"; }
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
    Switch { x: 10px; y: 150px; label: "Check daily"; toggled => { switched += 1; } }
    Rectangle {
        x: 90px;
        y: 145px;
        width: 200px;
        height: 36px;
        HorizontalLayout {
            Slider { minimum: 0; maximum: 100; value: floor-value; has-readout: false; label: "Live"; }
        }
    }
    Icon { x: 240px; y: 90px; width: 24px; height: 24px; source: Icons.named(icon-name); tint: #fff; }
    Icon { x: 270px; y: 90px; width: 24px; height: 24px; source: icon-file; tint: #fff; }
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
    // Given out of order, and out from under the handle while they're read
    set(500.0);
    let faint = colour(&ui, "faint");
    let marks = runs(&draw(&ui), 16, faint);
    assert_eq!(marks.len(), 4, "{marks:?}");
    assert!(marks.windows(2).all(|m| m[0] < m[1]), "{marks:?}");
    assert!((marks[0] - 7.0).abs() <= 1.0 && (marks[3] - 193.0).abs() <= 1.0, "the handle's ends: {marks:?}");
    for (value, mark) in [(100.0, marks[0]), (300.0, marks[1]), (700.0, marks[2]), (1000.0, marks[3])] {
        set(value);
        // The handle glides to a value that was set from outside: let it arrive
        draw(&ui);
        offscreen::skip(std::time::Duration::from_millis(600));
        // The handle is a card-coloured box with a border in the card's colour, quietened at rest: its top edge is a run just below the top
        let handle = runs(&draw(&ui), 5, colour(&ui, "muted-red"));
        assert_eq!(handle.len(), 1, "{handle:?}");
        assert!((handle[0] - mark).abs() <= 1.0, "the handle at {value} is at {}, its mark at {mark}", handle[0]);
    }
}

#[test]
fn slider_fill_doesnt_stick_out_past_the_handle_at_zero() {
    let ui = rig();
    let (picture, edge, fill) = (draw(&ui), colour(&ui, "edge-accent"), colour(&ui, "muted-accent"));
    let row = 145 + 16;
    // At 0 the handle sits at the very start of the track, 14 wide, and its border is the muted card colour
    assert!(near(picture.at(91, row), edge), "the live slider's handle edge is the muted card colour: {:?}", picture.at(91, row));
    // The fill is the hue half-way to the card, which isn't the handle's edge colour
    assert!(!near(fill, edge), "the two colours are told apart by the checks below");
    assert!((80..90).all(|x| !near(picture.at(x, row), fill)), "nothing of the fill sticks out left of the handle");
    assert!((106..140).all(|x| !near(picture.at(x, row), fill)), "and at 0 none is left of its right edge either");
    // A disabled slider has a fill of its own, hidden under the handle at 0 too
    let sel = colour(&ui, "sel");
    assert!((14..30).all(|x| picture.at(x, 56) != sel));
}

#[test]
fn separator_and_all_known_vector_icons_are_constructible() {
    let ui = rig();
    let lit = |ui: &ComponentInstance, x0: u32| {
        let picture = draw(ui);
        (x0..x0 + 24).any(|x| (90..114).any(|y| picture.at(x, y) != [0, 0, 0]))
    };
    let listed = std::fs::read_to_string(telescope_ui_kit::library_path().join("icons.slint")).unwrap();
    let files: Vec<&str> = listed.split("@image-url(\"").skip(1).filter_map(|s| s.split('"').next()).collect();
    assert!(files.len() > 50, "{} icons", files.len());
    for file in files {
        let path = telescope_ui_kit::library_path().join(file);
        let image = slint::Image::load_from_path(&path).unwrap_or_else(|_| panic!("{file} doesn't load"));
        ui.set_property("icon-file", Value::Image(image)).unwrap();
        assert!(lit(&ui, 270), "{file} draws nothing");
    }
    // By name, as PluginInfo.icon gives it
    let names: Vec<&str> = listed.split("name == \"").skip(1).filter_map(|s| s.split('"').next()).collect();
    assert!(names.len() > 20, "{} names", names.len());
    for name in names.into_iter().chain(["unknown"]) {
        ui.set_property("icon-name", Value::String(name.into())).unwrap();
        assert!(lit(&ui, 240), "{name} draws nothing");
    }
}

fn press(ui: &ComponentInstance, text: SharedString) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn tab(ui: &ComponentInstance) {
    press(ui, Key::Tab.into());
}

#[test]
fn focus_ring_shows_after_tab_and_not_after_a_click() {
    let ui = rig();
    let accent = colour(&ui, "accent");
    let ringed = |x: u32, y: u32| (0..3).any(|d| near(draw(&ui).at(x - d, y), accent));
    // Just outside a box's left edge, where the ring is and the box's own border is not
    let outside = |x: u32, y: u32| (1..3).any(|d| near(draw(&ui).at(x - d, y), accent));
    assert!(!ringed(110, 106));
    tab(&ui); // the slider
    tab(&ui); // the button
    tab(&ui);
    input::focus_key(&ui);
    assert!(ringed(110, 106), "Tab reached the dropdown: its ring is just outside its left edge");
    let position = LogicalPosition::new(22.0, 160.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    input::pressed_at(&ui, position);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    assert_eq!(ui.get_property("switched"), Ok(Value::Number(1.0)));
    assert!(!ringed(110, 106), "the click took the focus from the dropdown");
    assert!(!outside(10, 162), "and shows no ring on the switch");
    press(&ui, " ".into());
    assert_eq!(ui.get_property("switched"), Ok(Value::Number(2.0)), "the keys go to the switch now");
    // By keyboard the same switch does get its ring
    tab(&ui);
    press(&ui, Key::Backtab.into());
    input::focus_key(&ui);
    assert!(outside(10, 162), "Tab back to it rings it");
}
