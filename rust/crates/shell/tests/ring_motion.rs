// The picked lens's outline is part of its tile: when the placeholders become the phone's lenses the outline is on the tile's own
// place and size in every frame, never at the placeholder's and then jumping. RING_SHOTS=dir (or MOTION_SHOTS) draws the film.
mod motion_film;

use motion_film::*;
use slint::{ComponentHandle, Model};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::offscreen;

const LENSES: &str = r#"
import { LensRow, LensTile, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 400px;
    height: 80px;
    background: Theme.card;
    in property <[LensTile]> lenses;
    in property <int> picked: -1;
    out property <[length]> xs: r.tile-x;
    out property <[length]> ws: r.tile-w;
    out property <length> left: r.worm-left;
    out property <length> right: r.worm-right;
    r := LensRow { x: 10px; y: 10px; width: 380px; label: "Lens"; lenses: root.lenses; current: root.picked; }
}
"#;

fn tiles(n: usize) -> Value {
    let all = [("0.5", "Ultra"), ("0.6", "Wide"), ("1", "Main"), ("2", "Zoom"), ("5", "Tele")];
    Value::Model(slint::ModelRc::new(slint::VecModel::from(all[..n].iter().enumerate().map(|(i, (n, name))| Value::Struct(slint_interpreter::Struct::from_iter([
        ("id".to_string(), Value::String(i.to_string().into())), ("number".to_string(), Value::String((*n).into())),
        ("unit".to_string(), Value::String("x".into())), ("rest".to_string(), Value::String((*name).into())), ("name".to_string(), Value::String((*name).into())),
    ]))).collect::<Vec<_>>())))
}

fn list(ui: &ComponentInstance, name: &str) -> Vec<f32> {
    match ui.get_property(name).unwrap() {
        Value::Model(m) => (0..m.row_count()).map(|i| match m.row_data(i).unwrap() { Value::Number(n) => n as f32, v => panic!("{v:?}") }).collect(),
        v => panic!("{v:?}"),
    }
}

/// How far the outline is from the picked tile's own edges, per frame, while the lenses arrive.
fn off_the_tile(ui: &ComponentInstance, pick: usize, frames: usize, mut each: impl FnMut()) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for _ in 0..frames {
        offscreen::draw(ui.window(), 400.0, 80.0, 1.0).unwrap();
        let (x, w) = (list(ui, "xs")[pick], list(ui, "ws")[pick]);
        out.push((num(ui, "left") - x, num(ui, "right") - (x + w)));
        each();
        step(FRAME);
    }
    out
}

#[test]
fn the_outline_goes_with_its_tile_when_placeholders_become_lenses() {
    let _still = offscreen::freeze_clock();
    let ui = make(LENSES, 400.0, 80.0);
    step(1000);
    offscreen::draw(ui.window(), 400.0, 80.0, 1.0).unwrap();
    // The phone says its lenses and which is picked in the same breath
    set(&ui, "lenses", tiles(4));
    set(&ui, "picked", Value::Number(1.0));
    let off = off_the_tile(&ui, 1, 40, || {});
    // The outline is only drawn once its tile is more than half there; from then on it is the tile's
    let worst = off.iter().skip(1).map(|(l, r)| l.abs().max(r.abs())).fold(0.0, f32::max);
    assert!(worst < 1.5, "the outline never leaves its tile by more than a pixel or so: {off:?}");
}

#[test]
fn the_outline_goes_with_its_tile_when_a_lens_comes_or_goes_and_with_a_pick() {
    let _still = offscreen::freeze_clock();
    let ui = make(LENSES, 400.0, 80.0);
    set(&ui, "lenses", tiles(3));
    set(&ui, "picked", Value::Number(1.0));
    step(1500);
    offscreen::draw(ui.window(), 400.0, 80.0, 1.0).unwrap();
    set(&ui, "lenses", tiles(5));
    let off = off_the_tile(&ui, 1, 40, || {});
    let worst = off.iter().map(|(l, r)| l.abs().max(r.abs())).fold(0.0, f32::max);
    assert!(worst < 1.5, "the others make room and the outline goes with its tile: {off:?}");
    // A real pick between two lenses still glides (a spring over the tiles), and lands exactly
    set(&ui, "picked", Value::Number(3.0));
    step(900);
    offscreen::draw(ui.window(), 400.0, 80.0, 1.0).unwrap();
    let (x, w) = (list(&ui, "xs")[3], list(&ui, "ws")[3]);
    assert!((num(&ui, "left") - x).abs() < 0.5 && (num(&ui, "right") - (x + w)).abs() < 0.5);
}

#[test]
fn ring_film() {
    let Some(dir) = std::env::var_os("RING_SHOTS").map(std::path::PathBuf::from).or_else(shots) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let _still = offscreen::freeze_clock();
    let ui = make(LENSES, 400.0, 80.0);
    step(1000);
    offscreen::draw(ui.window(), 400.0, 80.0, 1.0).unwrap();
    let pics = film(&ui, (400.0, 80.0), 1.0, 24, |i| {
        if i == 0 {
            set(&ui, "lenses", tiles(4));
            set(&ui, "picked", Value::Number(1.0));
        }
    });
    let every: Vec<_> = pics.into_iter().step_by(2).collect();
    save(&dir, &format!("lens-ring-{}.png", std::env::var("RING_TAG").unwrap_or_else(|_| "after".into())), &strip(&every, (0.0, 0.0, 400.0, 60.0), 1.0, 2));
}
