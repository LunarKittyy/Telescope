// Round 1 of the motion pass: the bubble's one outline, the held bubble following the handle, the switch's ease-in, the dropdown and the
// button's press. With MOTION_SHOTS=dir set it also draws pictures of them (both looks) for looking at.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

const BUBBLES: &str = r#"
import { BubbleFrame, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 360px;
    height: 330px;
    background: Theme.card;
    // Mid-window, a quarter in, hard against the left edge, hard against the right edge; the last opens below
    BubbleFrame { ax: 170px; ay: 130px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 14px; ay: 200px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 335px; ay: 270px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 70px; ay: 10px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
}
"#;

#[test]
fn shots_of_the_bubble_outline() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = make(BUBBLES, 360.0, 330.0);
        ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        let pic = offscreen::draw(ui.window(), 360.0, 330.0, 3.0).unwrap();
        save(&dir, if classic { "bubble-outline-classic.png" } else { "bubble-outline-new.png" }, &pic);
    }
}

const HELD: &str = r#"
import { Slider, PeekLayer, Peeks, Theme } from "@ui-kit/ui-kit.slint";
export { Peeks, Theme }
export component T inherits Window {
    width: 420px;
    height: 200px;
    background: Theme.ground;
    in-out property <float> v: 50;
    sl := Slider { x: 10px; y: 100px; width: 300px; label: "Level"; minimum: 0; maximum: 100; value: root.v; peek: "text"; peek-text: "Aa"; peek-values: [14]; moved(x) => { root.v = x; } }
    PeekLayer { }
}
"#;

fn global_length(ui: &slint_interpreter::ComponentInstance, name: &str) -> f32 {
    match ui.get_global_property("Peeks", name).unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{name}: {v:?}"),
    }
}

#[test]
fn a_held_bubble_points_at_the_handle_and_follows_it() {
    let _still = offscreen::freeze_clock();
    let ui = make(HELD, 420.0, 200.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    // The handle is 14 px wide and its left edge is 111 px along the track at the middle; the track starts at x 10
    press(&ui, 10.0 + 7.0 + 111.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    assert_eq!(ui.get_global_property("Peeks", "shown").unwrap(), Value::Bool(true));
    let (ax, aw) = (global_length(&ui, "ax"), global_length(&ui, "aw"));
    assert!((ax - (10.0 + 111.0)).abs() < 1.5 && (aw - 14.0).abs() < 0.1, "it points at the handle: {ax} {aw}");
    // Dragged to the right, the thing it points at goes with the handle
    pointer_to(&ui, 10.0 + 7.0 + 0.8 * 222.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    let now = global_length(&ui, "ax");
    assert!((now - (10.0 + 0.8 * 222.0)).abs() < 1.5, "it follows the handle: {now}");
    release(&ui, 10.0 + 7.0 + 0.8 * 222.0, 116.0);
}
