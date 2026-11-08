// Filmstrips and curves of the kit's motions, for looking at the feel: MOTION_SHOTS=dir cargo nextest run -p telescope-shell --test kit_motion_film
// With the variable unset every test here passes without drawing anything.
#![allow(dead_code)]
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::path::PathBuf;
use std::time::Duration;
use telescope_shell::offscreen::{self, Picture};
use telescope_shell::compile;

pub const FRAME: u64 = 16;

pub fn shots() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("MOTION_SHOTS")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn make(doc: &str, w: f32, h: f32) -> ComponentInstance {
    offscreen::install();
    let def = compile::compile("t.slint", doc, Vec::new(), "T").unwrap_or_else(|e| panic!("{e}"));
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(w, h));
    ui.show().unwrap();
    offscreen::pump();
    ui
}

pub fn num(ui: &ComponentInstance, name: &str) -> f32 {
    match ui.get_property(name).unwrap_or_else(|e| panic!("{name}: {e:?}")) {
        Value::Number(n) => n as f32,
        v => panic!("{name}: {v:?}"),
    }
}

pub fn set(ui: &ComponentInstance, name: &str, v: Value) {
    ui.set_property(name, v).unwrap_or_else(|e| panic!("{name}: {e:?}"));
}

pub fn step(ms: u64) {
    offscreen::skip(Duration::from_millis(ms));
}

pub fn at(x: f32, y: f32) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

pub fn pointer_to(ui: &ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(x, y) });
}

pub fn press(ui: &ComponentInstance, x: f32, y: f32) {
    pointer_to(ui, x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position: at(x, y), button: PointerEventButton::Left });
}

pub fn release(ui: &ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerReleased { position: at(x, y), button: PointerEventButton::Left });
}

/// The window drawn now and then again every frame as the clock runs on, `each` called before each draw with the frame's number.
pub fn film(ui: &ComponentInstance, size: (f32, f32), scale: f32, frames: usize, mut each: impl FnMut(usize)) -> Vec<Picture> {
    let mut pictures = Vec::new();
    for i in 0..frames {
        each(i);
        pictures.push(offscreen::draw(ui.window(), size.0, size.1, scale).unwrap());
        step(FRAME);
    }
    pictures
}

/// The same crop (x, y, w, h in logical pixels) of every picture, side by side in rows of `cols`, 3 pixels of mid grey between.
pub fn strip(pictures: &[Picture], crop: (f32, f32, f32, f32), scale: f32, cols: usize) -> Picture {
    let (cx, cy, cw, ch) = ((crop.0 * scale) as usize, (crop.1 * scale) as usize, (crop.2 * scale) as usize, (crop.3 * scale) as usize);
    let rows = pictures.len().div_ceil(cols);
    let (w, h) = (cols * cw + (cols - 1) * 3, rows * ch + (rows - 1) * 3);
    let mut rgba = vec![96u8; w * h * 4];
    for (n, p) in pictures.iter().enumerate() {
        let (ox, oy) = ((n % cols) * (cw + 3), (n / cols) * (ch + 3));
        for y in 0..ch {
            for x in 0..cw {
                let from = ((cy + y) * p.width as usize + cx + x) * 4;
                let to = ((oy + y) * w + ox + x) * 4;
                rgba[to..to + 4].copy_from_slice(&p.rgba[from..from + 4]);
            }
        }
    }
    Picture { width: w as u32, height: h as u32, rgba }
}

pub fn dot(rgba: &mut [u8], w: usize, x: i32, y: i32, colour: [u8; 3], r: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let (px, py) = (x + dx, y + dy);
            if px >= 0 && py >= 0 && (px as usize) < w && ((py as usize * w + px as usize) * 4 + 3) < rgba.len() {
                let i = (py as usize * w + px as usize) * 4;
                rgba[i..i + 3].copy_from_slice(&colour);
            }
        }
    }
}

/// Curves against time: one series per colour, a value every frame, with lines at 0 and 1 and every 100 ms.
pub fn plot(series: &[(Vec<f32>, [u8; 3])], range: (f32, f32)) -> Picture {
    let (w, h) = (720usize, 300usize);
    let mut rgba = vec![24u8; w * h * 4];
    for px in rgba.chunks_mut(4) {
        px[3] = 255;
    }
    let frames = series.iter().map(|s| s.0.len()).max().unwrap_or(2);
    let at = |f: f32, v: f32| (20.0 + f / (frames as f32 - 1.0) * (w as f32 - 40.0), 20.0 + (1.0 - (v - range.0) / (range.1 - range.0)) * (h as f32 - 40.0));
    for tick in 0..=(frames as u32 * FRAME as u32 / 100) {
        let (x, _) = at(tick as f32 * 100.0 / FRAME as f32, 0.0);
        for y in 0..h {
            dot(&mut rgba, w, x as i32, y as i32, [52, 52, 52], 0);
        }
    }
    for level in [0.0, 1.0] {
        let (_, y) = at(0.0, level);
        for x in 0..w {
            dot(&mut rgba, w, x as i32, y as i32, if level == 1.0 { [110, 110, 110] } else { [80, 80, 80] }, 0);
        }
    }
    for (values, colour) in series {
        for (f, v) in values.iter().enumerate() {
            let (x, y) = at(f as f32, *v);
            dot(&mut rgba, w, x as i32, y as i32, *colour, 2);
        }
    }
    Picture { width: w as u32, height: h as u32, rgba }
}

/// Where a point went, drawn as a trail over a picture: the path of a handle across and down.
pub fn trail(base: &Picture, scale: f32, points: &[(f32, f32)], colour: [u8; 3]) -> Picture {
    let mut out = Picture { width: base.width, height: base.height, rgba: base.rgba.clone() };
    for (i, (x, y)) in points.iter().enumerate() {
        let r = if i == 0 || i == points.len() - 1 { 4 } else { 2 };
        dot(&mut out.rgba, base.width as usize, (x * scale) as i32, (y * scale) as i32, colour, r);
    }
    out
}

pub fn save(dir: &std::path::Path, name: &str, picture: &Picture) {
    picture.save_png(&dir.join(name)).unwrap();
}
