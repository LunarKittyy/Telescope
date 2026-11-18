// Appearance's tooltip options on the kit's Tips: the wait before a tip (from instant to 1.5 s) and whether the next one skips it.
// Real pointer events through the offscreen platform, the clock moved by hand.
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::time::Duration;

const DOC: &str = r#"
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

fn open() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("tips.slint", DOC, vec![], "W").expect("the document compiles").create().unwrap();
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
    let w = open();
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
        let w = open();
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
        let w = open();
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
