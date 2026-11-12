//! Options that swap what is under them (Background: Off, Blur, Picture; Chroma key: Colour, Picture): the old option's rows and the new
//! one's cross-fade over each other and the card goes straight from the old height to the new one (no height change at all when the two are
//! the same). OPTION_SHOTS=dir draws frames mid-switch; OPTION_TAG says before or after in their names.

use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance, Value};
use std::path::{Path, PathBuf};
use telescope_core::plugin::Plugin;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::chroma_key::ChromaKeyPlugin;
use telescope_shell::offscreen;

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 500.0;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The plugin's card in a window with `prop` (an int) for the option, and the card's own height.
fn window(plugin: &mut dyn Plugin, prop: &str) -> ComponentInstance {
    offscreen::install();
    let card = plugin.card().expect("a card");
    let telescope_core::ui::SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let doc = format!(
        "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c} }} from \"plugin/card.slint\";\n\
         export component W inherits Window {{\n    width: {WIDTH}px;\n    height: {HEIGHT}px;\n    background: Theme.ground;\n    in-out property <int> option;\n    out property <length> card-h: card.preferred-height;\n    \
         card := {c} {{ hue: Theme.hue-camera; active: true; {prop}: root.option; }}\n}}",
        c = card.ui.component
    );
    let mut compiler = Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    compiler.set_file_loader(move |path: &Path| {
        let found = path.ends_with("plugin/card.slint").then(|| Ok(source.clone()));
        Box::pin(async move { found })
    });
    let result = spin_on(compiler.build_from_source(doc, Path::new("/root/wrapper.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let w = result.component("W").unwrap().create().unwrap();
    w.show().unwrap();
    w
}

fn height(w: &ComponentInstance) -> f32 {
    match w.get_property("card-h").unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{v:?}"),
    }
}

/// The card's height in every frame after the option goes from `from` to `to`, and a picture of a few of them.
fn swap(make: &dyn Fn() -> Box<dyn Plugin>, prop: &str, from: i32, to: i32, name: &str) -> Vec<f32> {
    let _still = offscreen::freeze_clock();
    let mut plugin = make();
    let w = window(plugin.as_mut(), prop);
    w.set_property("option", Value::Number(f64::from(from))).unwrap();
    offscreen::draw(w.window(), WIDTH, HEIGHT, 1.0).unwrap();
    offscreen::skip(std::time::Duration::from_millis(900));
    offscreen::draw(w.window(), WIDTH, HEIGHT, 1.0).unwrap();
    w.set_property("option", Value::Number(f64::from(to))).unwrap();
    let mut heights = Vec::new();
    let mut pics = Vec::new();
    for _ in 0..40 {
        pics.push(offscreen::draw(w.window(), WIDTH, HEIGHT, 1.0).unwrap());
        heights.push(height(&w));
        offscreen::skip(std::time::Duration::from_millis(16));
    }
    if let Some(dir) = std::env::var_os("OPTION_SHOTS").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        let tag = std::env::var("OPTION_TAG").unwrap_or_else(|_| "after".into());
        let (cw, ch, gap) = (WIDTH as usize, 260usize, 3usize);
        let picked = [0usize, 3, 6, 9, 13, 19];
        let (sw, sh) = (3 * cw + 2 * gap, 2 * ch + gap);
        let mut rgba = vec![96u8; sw * sh * 4];
        for (n, &i) in picked.iter().enumerate() {
            let (ox, oy) = ((n % 3) * (cw + gap), (n / 3) * (ch + gap));
            for y in 0..ch {
                for x in 0..cw {
                    let src = (y * pics[i].width as usize + x) * 4;
                    let dst = ((oy + y) * sw + ox + x) * 4;
                    rgba[dst..dst + 4].copy_from_slice(&pics[i].rgba[src..src + 4]);
                }
            }
        }
        offscreen::Picture { width: sw as u32, height: sh as u32, rgba }.save_png(&dir.join(format!("{name}-{tag}.png"))).unwrap();
    }
    heights
}

fn background() -> Box<dyn Plugin> {
    Box::new(BackgroundPlugin::new())
}

fn chroma() -> Box<dyn Plugin> {
    Box::new(ChromaKeyPlugin::new())
}

fn biggest_step(v: &[f32]) -> f32 {
    v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

#[test]
fn blur_to_picture_goes_straight_from_one_height_to_the_other() {
    let h = swap(&background, "mode", 1, 2, "background-blur-to-picture");
    let (start, end) = (h[0], *h.last().unwrap());
    let (low, high) = (start.min(end), start.max(end));
    assert!(h.iter().all(|v| *v >= low - 0.5 && *v <= high + 0.5), "never through a smaller or a bigger height than either: {h:?}");
    assert!(biggest_step(&h) <= (high - low) * 0.3 + 0.5, "{h:?}");
}

#[test]
fn off_to_blur_and_back_move_the_height_without_a_jump() {
    let h = swap(&background, "mode", 0, 1, "background-off-to-blur");
    let (start, end) = (h[0], *h.last().unwrap());
    assert!(end > start + 20.0, "{start} {end}");
    assert!(h.windows(2).all(|w| w[1] >= w[0] - 0.01), "only grows: {h:?}");
    assert!(biggest_step(&h) < (end - start) * 0.3, "{h:?}");
    let h = swap(&background, "mode", 1, 0, "background-blur-to-off");
    assert!(h.windows(2).all(|w| w[1] <= w[0] + 0.01), "only shrinks: {h:?}");
}

#[test]
fn colour_to_picture_in_chroma_key_never_bounces() {
    let h = swap(&chroma, "behind", 0, 1, "chroma-colour-to-picture");
    let (start, end) = (h[0], *h.last().unwrap());
    let (low, high) = (start.min(end), start.max(end));
    assert!(h.iter().all(|v| *v >= low - 0.5 && *v <= high + 0.5), "{h:?}");
    let h = swap(&chroma, "behind", 1, 0, "chroma-picture-to-colour");
    let (low, high) = (h[0].min(*h.last().unwrap()), h[0].max(*h.last().unwrap()));
    assert!(h.iter().all(|v| *v >= low - 0.5 && *v <= high + 0.5), "{h:?}");
}
