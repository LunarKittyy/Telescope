// The renders for round 2 of the kit: MOTION_SHOTS=dir cargo nextest run -p telescope-shell --test kit_r2_film. With FILM_BEFORE=1 the grid case
// is drawn the old way (two rows of one-row choices), so before and after sit side by side. Without MOTION_SHOTS each test passes drawing nothing.
mod motion_film;

use motion_film::*;
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen::{self, Picture};

fn tag() -> &'static str {
    if std::env::var_os("FILM_BEFORE").is_some() { "before" } else { "after" }
}

fn key(ui: &slint_interpreter::ComponentInstance, k: Key) {
    let text: slint::SharedString = k.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

const CHIN: &str = r#"
import { ToggleTile, IconButton, Icons, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 56px;
    background: Theme.card;
    HorizontalLayout {
        x: 10px; y: 8px; height: 40px; width: 280px; spacing: 6px;
        ToggleTile { icon: Icons.camera; name: "Camera"; checked: true; hue: #aa9cf5; }
        ToggleTile { icon: Icons.person; name: "Skeleton"; hue: #aa9cf5; }
        ToggleTile { icon: Icons.grid; name: "Outlines"; hue: #aa9cf5; }
        IconButton { icon: Icons.pop-out; label: "Pop out"; width: 40px; height: 40px; }
        IconButton { icon: Icons.eye-off; label: "Hide"; width: 40px; height: 40px; }
    }
}
"#;

#[test]
fn chin_buttons() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(CHIN, 300.0, 56.0);
    step(FRAME);
    let pic = offscreen::draw(ui.window(), 300.0, 56.0, 6.0).unwrap();
    save(&dir, &format!("chin-buttons-{}.png", tag()), &pic);
}

const SLIDER: &str = r#"
import { Slider, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 44px;
    background: Theme.card;
    in-out property <float> v: 1;
    sl := Slider { x: 10px; y: 6px; width: 320px; label: "Steps"; minimum: 1; maximum: 5; step: 1; value: root.v; hue: #aa9cf5; readout: "\{root.v}"; moved(x) => { root.v = x; } }
}
"#;

#[test]
fn stepped_slider_mid_drag() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER, 340.0, 44.0);
    offscreen::draw(ui.window(), 340.0, 44.0, 1.0).unwrap();
    let x0 = 10.0 + 7.0;
    let stride = (320.0 - 56.0 - 8.0 - 14.0) / 4.0;
    press(&ui, x0, 22.0);
    let mut frames = Vec::new();
    // The pointer crosses two steps slowly, a few pixels a frame: the handle is held back near a step, then goes through the middle
    for i in 0..30 {
        pointer_to(&ui, x0 + i as f32 * stride * 2.0 / 30.0, 22.0);
        frames.push(offscreen::draw(ui.window(), 340.0, 44.0, 2.0).unwrap());
        step(FRAME);
    }
    release(&ui, x0 + stride * 2.0, 22.0);
    for _ in 0..14 {
        frames.push(offscreen::draw(ui.window(), 340.0, 44.0, 2.0).unwrap());
        step(FRAME);
    }
    let picks: Vec<_> = frames.into_iter().enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, p)| p).collect();
    save(&dir, &format!("slider-detent-{}.png", tag()), &strip(&picks, (0.0, 0.0, 340.0, 44.0), 2.0, 2));
}

const DROP: &str = r#"
import { ChoiceField, WindowBox, Theme } from "@ui-kit/ui-kit.slint";
export { WindowBox }
export component T inherits Window {
    width: 300px;
    height: 200px;
    background: Theme.card;
    ChoiceField { x: 20px; y: 20px; width: 180px; caption: "Look"; items: ["Calm", "Bright", "Film"]; current: 0; }
}
"#;

#[test]
fn dropdown_closing() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(DROP, 300.0, 200.0);
    ui.set_global_property("WindowBox", "width", Value::Number(300.0)).unwrap();
    ui.set_global_property("WindowBox", "height", Value::Number(200.0)).unwrap();
    offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
    press(&ui, 100.0, 44.0);
    release(&ui, 100.0, 44.0);
    for _ in 0..14 {
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
    }
    // A click outside: it fades out over frames (before: gone in one)
    press(&ui, 260.0, 160.0);
    release(&ui, 260.0, 160.0);
    let mut frames = vec![offscreen::draw(ui.window(), 300.0, 200.0, 2.0).unwrap()];
    for _ in 0..9 {
        step(FRAME);
        frames.push(offscreen::draw(ui.window(), 300.0, 200.0, 2.0).unwrap());
    }
    save(&dir, &format!("dropdown-closing-{}.png", tag()), &strip(&frames, (0.0, 0.0, 300.0, 140.0), 2.0, 5));
}

fn grid_doc() -> String {
    if std::env::var_os("FILM_BEFORE").is_some() {
        r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 120px;
    background: Theme.card;
    in-out property <int> top: 0;
    in-out property <int> bottom: -1;
    PictureSegmented { x: 10px; y: 8px; width: 280px; label: "A"; items: ["Bar", "Pill"]; disabled: [false, false]; current: root.top; hue: #aa9cf5; show-names: true; picked(i) => { root.top = i; root.bottom = -1; } Rectangle { } }
    PictureSegmented { x: 10px; y: 62px; width: 280px; label: "B"; items: ["Light", "Shadow"]; disabled: [false, false]; current: root.bottom; hue: #aa9cf5; show-names: true; picked(i) => { root.bottom = i; root.top = -1; } Rectangle { } }
}
"#.to_string()
    } else {
        r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 120px;
    background: Theme.card;
    PictureSegmented { x: 10px; y: 8px; width: 280px; label: "Style"; items: ["Bar", "Pill", "Light", "Shadow"]; disabled: [false, false, false, false]; current: 0; columns: 2; hue: #aa9cf5; show-names: true; Rectangle { } }
}
"#.to_string()
    }
}

/// The tile centres of the 2x2 grid: (x, y) of Bar, Pill, Light, Shadow.
fn tile(i: usize) -> (f32, f32) {
    (10.0 + 70.0 + (i % 2) as f32 * 142.0 + 0.0, 8.0 + 24.0 + (i / 2) as f32 * 54.0)
}

#[test]
fn the_worm_in_a_grid() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for (name, from, to) in [("light-to-pill", 2usize, 1usize), ("bar-to-shadow", 0, 3)] {
        let ui = make(&grid_doc(), 300.0, 120.0);
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
        let go = |i: usize| {
            let (x, y) = tile(i);
            press(&ui, x, y);
            release(&ui, x, y);
        };
        go(from);
        for _ in 0..30 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
        }
        go(to);
        let mut frames = vec![offscreen::draw(ui.window(), 300.0, 120.0, 1.5).unwrap()];
        for _ in 0..11 {
            step(FRAME);
            frames.push(offscreen::draw(ui.window(), 300.0, 120.0, 1.5).unwrap());
        }
        save(&dir, &format!("grid-{name}-{}.png", tag()), &strip(&frames, (0.0, 0.0, 300.0, 120.0), 1.5, 4));
    }
    // Rapid clicking across all four
    let ui = make(&grid_doc(), 300.0, 120.0);
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
    let mut frames = Vec::new();
    for n in 0..24usize {
        let i = [0usize, 3, 1, 2, 0, 1, 3, 2][n % 8];
        if n % 2 == 0 {
            let (x, y) = tile(i);
            press(&ui, x, y);
            release(&ui, x, y);
        }
        frames.push(offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap());
        step(FRAME);
    }
    save(&dir, &format!("grid-rapid-{}.png", tag()), &strip(&frames, (0.0, 0.0, 300.0, 120.0), 1.0, 4));
}

const LAYER: &str = r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 70px;
    background: Theme.card;
    in-out property <int> cur: 0;
    PictureSegmented {
        x: 10px; y: 8px; width: 280px; label: "Rotate"; items: ["0", "90", "180", "270"]; disabled: [false, false, false, false]; current: root.cur; hue: #aa9cf5; show-names: true; tile-height: 54px;
        for i in 4: Rectangle { x: i * 72px + 20px; y: 6px; width: 24px; height: 24px; background: #5a5a5a; border-radius: 4px; }
    }
}
"#;

#[test]
fn the_outline_stays_over_the_tiles_in_slow_motion() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(LAYER, 300.0, 70.0);
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 70.0, 1.0).unwrap();
    press(&ui, 30.0, 35.0);
    release(&ui, 30.0, 35.0);
    for _ in 0..30 {
        step(FRAME);
    }
    // Four times slower than real: a frame every 4 ms
    press(&ui, 250.0, 35.0);
    release(&ui, 250.0, 35.0);
    let mut frames = Vec::new();
    for _ in 0..24 {
        frames.push(offscreen::draw(ui.window(), 300.0, 70.0, 1.5).unwrap());
        step(8);
    }
    save(&dir, &format!("worm-layers-slow-{}.png", tag()), &strip(&frames, (0.0, 0.0, 300.0, 70.0), 1.5, 4));
}

const PAN: &str = r#"
import { Pad, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 230px;
    background: Theme.card;
    in property <length> bw: 300px;
    in property <length> bh: 200px;
    in property <float> vx: 0;
    pad := Pad {
        x: 20px; y: 15px; width: 300px; height: 200px; label: "Pan";
        x-minimum: -1; x-maximum: 1; step-x: 0.01; y-minimum: -1; y-maximum: 1; step-y: 0.01; y-up: false;
        value-x: root.vx; value-y: 0; box-w: root.bw; box-h: root.bh; hue: #aa9cf5; has-default: true;
        Rectangle { background: #2a5a6a; border-radius: Theme.r-ctl; }
    }
}
"#;

#[test]
fn the_pan_map() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(PAN, 340.0, 230.0);
    step(FRAME);
    let at_1 = offscreen::draw(ui.window(), 340.0, 230.0, 3.0).unwrap();
    save(&dir, &format!("pan-1.0x-{}.png", tag()), &strip(&[at_1], (10.0, 5.0, 320.0, 220.0), 3.0, 1));
    // Zoom in to 1.5x: the frame is created and closes in on the middle from the pad's own size
    set(&ui, "bw", Value::Number(200.0));
    set(&ui, "bh", Value::Number(133.0));
    let mut frames = Vec::new();
    for _ in 0..16 {
        frames.push(offscreen::draw(ui.window(), 340.0, 230.0, 1.0).unwrap());
        step(FRAME);
    }
    save(&dir, &format!("pan-zooming-in-{}.png", tag()), &strip(&frames, (10.0, 5.0, 320.0, 220.0), 1.0, 4));
    step(600);
    set(&ui, "vx", Value::Number(0.5));
    step(600);
    let at_15 = offscreen::draw(ui.window(), 340.0, 230.0, 3.0).unwrap();
    save(&dir, &format!("pan-1.5x-{}.png", tag()), &strip(&[at_15], (10.0, 5.0, 320.0, 220.0), 3.0, 1));
    let _ = key;
}

// The pan map as the Transforms card builds it: the pad's range is the crop's centre, which narrows to a point at 1.0x, and the frame is the pad's size over the zoom.
const ZOOM_MAP: &str = r#"
import { Pad, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 230px;
    background: Theme.card;
    in property <float> zoom: 1;
    in property <float> pan-x: 0;
    in property <float> pan-y: 0;
    property <float> span: 1 - 1 / max(1, zoom);
    out property <length> hx: pad.handle-x;
    out property <length> hy: pad.handle-y;
    pad := Pad {
        x: 20px; y: 15px; width: 300px; height: 200px; label: "Pan";
        x-minimum: 0.5 * (1 - root.span); x-maximum: 0.5 * (1 + root.span);
        y-minimum: 0.5 * (1 - root.span); y-maximum: 0.5 * (1 + root.span);
        value-x: 0.5 + root.pan-x * root.span / 2; value-y: 0.5 + root.pan-y * root.span / 2;
        step-x: 0.005; step-y: 0.005;
        box-w: self.width / max(1, root.zoom); box-h: self.height / max(1, root.zoom);
        has-centre: false; has-default: true; default-x: 0.5; default-y: 0.5; hue: #aa9cf5;
        Rectangle { background: #2a5a6a; }
    }
}
"#;

/// The frame's centre every frame, in the pad's pixels, while `each` moves the zoom; it must never jump from one frame to the next and
/// never leave the pad. Returns the pictures for the film too.
fn pan_frames(ui: &slint_interpreter::ComponentInstance, frames: usize, mut each: impl FnMut(usize)) -> Vec<Picture> {
    let mut last: Option<(f32, f32)> = None;
    film(ui, (340.0, 230.0), 1.0, frames, |i| {
        each(i);
        let now = (num(ui, "hx"), num(ui, "hy"));
        assert!(now.0 >= -0.5 && now.0 <= 300.5 && now.1 >= -0.5 && now.1 <= 200.5, "frame {i}: the frame's centre {now:?} is outside the pad");
        if let Some(before) = last {
            let jump = (now.0 - before.0).abs().max((now.1 - before.1).abs());
            assert!(jump < 40.0, "frame {i}: the frame's centre jumped {jump} px, from {before:?} to {now:?}");
        }
        last = Some(now);
    })
}

#[test]
fn the_pan_map_zooming_out_fast() {
    let shots = shots();
    let _still = offscreen::freeze_clock();
    let ui = make(ZOOM_MAP, 340.0, 230.0);
    set(&ui, "pan-x", Value::Number(0.8));
    set(&ui, "pan-y", Value::Number(-0.6));
    set(&ui, "zoom", Value::Number(1.5));
    step(800);
    // To 1.0x at once, before the frame has finished growing: it grows from where it is, to the pad's size
    set(&ui, "zoom", Value::Number(1.0));
    let pictures = pan_frames(&ui, 24, |_| {});
    if let Some(dir) = &shots {
        save(dir, &format!("pan-zoom-out-{}.png", tag()), &strip(&pictures, (10.0, 5.0, 320.0, 220.0), 1.0, 4));
    }
}

#[test]
fn the_pan_map_zooming_back_and_forth() {
    let shots = shots();
    let _still = offscreen::freeze_clock();
    let ui = make(ZOOM_MAP, 340.0, 230.0);
    set(&ui, "pan-x", Value::Number(0.5));
    set(&ui, "pan-y", Value::Number(0.5));
    // 1.0x, in to 1.5x and out again before either has finished, three times as fast as a hand does it
    let pictures = pan_frames(&ui, 36, |i| {
        let zoom = if (i / 6) % 2 == 0 { 1.5 } else { 1.0 };
        set(&ui, "zoom", Value::Number(zoom));
    });
    if let Some(dir) = &shots {
        save(dir, &format!("pan-back-and-forth-{}.png", tag()), &strip(&pictures, (10.0, 5.0, 320.0, 220.0), 1.0, 6));
    }
}

const DIAL: &str = r#"
import { Dial, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 60px;
    background: Theme.card;
    in-out property <int> at: 1;
    Dial { x: 10px; y: 10px; width: 280px; stops: ["100", "160", "200", "250", "320", "400"]; current: root.at; label: "ISO"; hue: #aa9cf5; }
}
"#;

#[test]
fn the_dial_scrolling_slowly() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = make(DIAL, 300.0, 60.0);
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 60.0, 2.0).unwrap();
    // A slow drag: the strip follows the finger, a few pixels a frame
    press(&ui, 150.0, 36.0);
    let mut frames = Vec::new();
    for i in 0..24 {
        pointer_to(&ui, 150.0 - i as f32 * 2.0, 36.0);
        frames.push(offscreen::draw(ui.window(), 300.0, 60.0, 2.0).unwrap());
        step(FRAME);
    }
    release(&ui, 150.0 - 46.0, 36.0);
    save(&dir, &format!("dial-slow-{}.png", tag()), &strip(&frames, (0.0, 0.0, 300.0, 60.0), 2.0, 3));
}
