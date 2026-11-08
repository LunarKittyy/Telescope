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
