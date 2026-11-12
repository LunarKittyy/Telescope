// Filmstrips and curves of the kit's motions, for looking at the feel: MOTION_SHOTS=dir cargo nextest run -p telescope-shell --test kit_motion_film
// With the variable unset every test here passes without drawing anything.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

const MOTIONS: &str = r#"
import { Settle, Flick, Pop, Tone, Fade, Unfold } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 100px;
    height: 100px;
    in property <float> to;
    in property <bool> on;
    out property <float> settle: settle-m.value;
    out property <float> flick: flick-m.value;
    out property <float> pop: pop-m.scale;
    out property <float> tone: tone-m.value;
    out property <float> fade: fade-m.value;
    out property <float> unfold: unfold-m.value;
    settle-m := Settle { to: root.to; }
    flick-m := Flick { to: root.to; }
    pop-m := Pop { to: root.to; }
    tone-m := Tone { on: root.on; }
    fade-m := Fade { shown: root.on; }
    unfold-m := Unfold { open: root.on; }
}
"#;

#[test]
fn shots_of_the_curves() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(MOTIONS, 100.0, 100.0);
    let names = ["settle", "flick", "pop", "tone", "fade", "unfold"];
    let read = || {
        for n in names {
            num(&ui, n);
        }
    };
    read();
    set(&ui, "to", Value::Number(1.0));
    set(&ui, "on", Value::Bool(true));
    read();
    let mut series: Vec<Vec<f32>> = vec![Vec::new(); names.len()];
    for _ in 0..28 {
        for (i, n) in names.iter().enumerate() {
            series[i].push(num(&ui, n));
        }
        step(FRAME);
    }
    let colours = [[240, 240, 240], [120, 200, 255], [255, 170, 90], [140, 230, 140], [230, 120, 200], [250, 230, 100]];
    let text: Vec<String> = names.iter().zip(&series).map(|(n, t)| format!("{n}: {:?}", t.iter().map(|v| (v * 1000.0).round() / 1000.0).collect::<Vec<_>>())).collect();
    std::fs::write(dir.join("curves.txt"), text.join("\n")).unwrap();
    // The pop's scale runs from 0.9 to 1, so it is drawn on its own scale, 0.9 to 1.0 as 0 to 1
    let series: Vec<(Vec<f32>, [u8; 3])> = series.into_iter().enumerate().map(|(i, t)| (if i == 2 { t.iter().map(|v| (v - 0.9) / 0.1).collect() } else { t }, colours[i])).collect();
    save(&dir, "curves.png", &plot(&series, (-0.05, 1.15)));
}

const SWITCH: &str = r#"
import { Switch, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 120px;
    height: 36px;
    background: Theme.card;
    in-out property <bool> on;
    sw := Switch { x: 40px; y: 6px; checked <=> root.on; label: "Flip"; hue: Theme.accent; }
}
"#;

#[test]
fn shots_of_the_switch() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(SWITCH, 120.0, 36.0);
    let first = offscreen::draw(ui.window(), 120.0, 36.0, 3.0).unwrap();
    press(&ui, 50.0, 18.0);
    release(&ui, 50.0, 18.0);
    let mut frames = vec![first];
    frames.extend(film(&ui, (120.0, 36.0), 3.0, 17, |_| {}));
    // Back: another click, a while later
    step(500);
    press(&ui, 50.0, 18.0);
    release(&ui, 50.0, 18.0);
    frames.extend(film(&ui, (120.0, 36.0), 3.0, 16, |_| {}));
    save(&dir, "switch-on-then-off.png", &strip(&frames, (30.0, 0.0, 60.0, 36.0), 3.0, 8));
}

const SLIDER: &str = r#"
import { Slider, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 40px;
    background: Theme.card;
    in-out property <float> v: 20;
    sl := Slider { x: 10px; y: 2px; width: 320px; label: "Level"; minimum: 0; maximum: 100; value: root.v; hue: Theme.accent; moved(x) => { root.v = x; } }
}
"#;

#[test]
fn shots_of_the_slider() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER, 340.0, 40.0);
    let mut frames = vec![offscreen::draw(ui.window(), 340.0, 40.0, 2.0).unwrap()];
    // Moved from outside: 20 to 85
    set(&ui, "v", Value::Number(85.0));
    frames.extend(film(&ui, (340.0, 40.0), 2.0, 17, |_| {}));
    save(&dir, "slider-moved-from-outside.png", &strip(&frames, (0.0, 0.0, 340.0, 40.0), 2.0, 2));
    // Held and let go: the colour comes up at once and dims back
    step(500);
    let mut held = vec![offscreen::draw(ui.window(), 340.0, 40.0, 2.0).unwrap()];
    press(&ui, 100.0, 20.0);
    held.extend(film(&ui, (340.0, 40.0), 2.0, 2, |_| {}));
    release(&ui, 100.0, 20.0);
    held.extend(film(&ui, (340.0, 40.0), 2.0, 12, |_| {}));
    save(&dir, "slider-held-then-let-go.png", &strip(&held, (0.0, 0.0, 340.0, 40.0), 2.0, 2));
}

const PAD: &str = r#"
import { Pad, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 230px;
    background: Theme.card;
    in property <float> sx: 2000;
    in property <float> sy: -40;
    out property <length> hx: pad.handle-x;
    out property <length> hy: pad.handle-y;
    pad := Pad {
        x: 10px; y: 10px; width: 300px; height: 200px;
        label: "Balance";
        x-minimum: 2000; x-maximum: 10000; step-x: 100;
        y-minimum: -50; y-maximum: 50; step-y: 1;
        value-x: sx; value-y: sy;
        y-up: true;
        hue: Theme.accent;
    }
}
"#;

#[test]
fn shots_of_the_pad_path() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(PAD, 340.0, 230.0);
    let first = offscreen::draw(ui.window(), 340.0, 230.0, 2.0).unwrap();
    num(&ui, "hx");
    set(&ui, "sx", Value::Number(10000.0));
    set(&ui, "sy", Value::Number(40.0));
    offscreen::draw(ui.window(), 340.0, 230.0, 1.0);
    let mut points = Vec::new();
    for _ in 0..24 {
        points.push((num(&ui, "hx") + 10.0, num(&ui, "hy") + 10.0));
        step(FRAME);
    }
    let end = offscreen::draw(ui.window(), 340.0, 230.0, 2.0).unwrap();
    let _ = first;
    save(&dir, "pad-path-across-and-down.png", &trail(&end, 2.0, &points, [255, 160, 90]));
    // And back up and left
    set(&ui, "sx", Value::Number(2000.0));
    set(&ui, "sy", Value::Number(-40.0));
    offscreen::draw(ui.window(), 340.0, 230.0, 1.0);
    let mut back = Vec::new();
    for _ in 0..24 {
        back.push((num(&ui, "hx") + 10.0, num(&ui, "hy") + 10.0));
        step(FRAME);
    }
    let end = offscreen::draw(ui.window(), 340.0, 230.0, 2.0).unwrap();
    save(&dir, "pad-path-back.png", &trail(&end, 2.0, &back, [120, 200, 255]));
}

const FOLD: &str = r#"
import { Card, Folds, Icons, Theme, Slider } from "@ui-kit/ui-kit.slint";
export { Folds }
export component T inherits Window {
    width: 300px;
    height: 220px;
    background: Theme.ground;
    VerticalLayout {
        alignment: start;
        x: 10px;
        y: 10px;
        width: 280px;
        card := Card {
            title: "Colour";
            icon: Icons.sun;
            fold-key: "p/Colour";
            Slider { label: "Contrast"; value: 30; }
            Slider { label: "Gamma"; value: 70; }
            Slider { label: "Tint"; value: 50; }
        }
    }
}
"#;

#[test]
fn shots_of_a_card_folding() {
    use std::cell::Cell;
    use std::rc::Rc;
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(FOLD, 300.0, 220.0);
    let folded = Rc::new(Cell::new(false));
    let version = Rc::new(Cell::new(1.0));
    let f = folded.clone();
    ui.set_global_callback("Folds", "is-folded", move |_| Value::Bool(f.get())).unwrap();
    let (f, v, weak) = (folded, version, ui.as_weak());
    ui.set_global_callback("Folds", "toggle", move |_| {
        f.set(!f.get());
        v.set(v.get() + 1.0);
        weak.unwrap().set_global_property("Folds", "version", Value::Number(v.get())).unwrap();
        Value::Void
    })
    .unwrap();
    ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
    let mut frames = vec![offscreen::draw(ui.window(), 300.0, 220.0, 1.5).unwrap()];
    let title = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Colour").find(|e| e.accessible_checkable() == Some(true)).unwrap();
    let (p, s) = (title.absolute_position(), title.size());
    press(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    release(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    frames.extend(film(&ui, (300.0, 220.0), 1.5, 11, |_| {}));
    step(300);
    press(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    release(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    frames.extend(film(&ui, (300.0, 220.0), 1.5, 11, |_| {}));
    save(&dir, "card-fold-then-unfold.png", &strip(&frames, (0.0, 0.0, 300.0, 200.0), 1.5, 8));
}

const BUBBLE: &str = r#"
import { Slider, PeekLayer, TipLayer, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 200px;
    background: Theme.ground;
    Slider { x: 10px; y: 130px; width: 320px; label: "Level"; value: 50; peek: "text"; peek-text: "Aa Bb"; peek-values: [14]; }
    PeekLayer { }
}
"#;

#[test]
fn shots_of_a_held_bubble() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(BUBBLE, 340.0, 200.0);
    let mut frames = vec![offscreen::draw(ui.window(), 340.0, 200.0, 1.5).unwrap()];
    press(&ui, 100.0, 146.0);
    frames.extend(film(&ui, (340.0, 200.0), 1.5, 9, |_| {}));
    release(&ui, 100.0, 146.0);
    frames.extend(film(&ui, (340.0, 200.0), 1.5, 8, |_| {}));
    save(&dir, "bubble-in-and-out.png", &strip(&frames, (0.0, 20.0, 340.0, 180.0), 1.5, 6));
}
