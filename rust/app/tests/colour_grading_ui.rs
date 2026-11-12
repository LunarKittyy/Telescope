//! test_colour_grading.py's tests of what's on screen: the card in a real offscreen window, the plugin among the real built-ins.

use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance};
use std::path::{Path, PathBuf};
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::{SlintSource, UiEvent};
use telescope_effects::colour_grading::ColourGradingPlugin;
use telescope_effects::grade::monotone_curve;
use telescope_shell::offscreen;
use telescope_shell::source::from_json;

/// The card's width; CARD_WIDTH tries another for a look at how it lays out.
fn width() -> f32 {
    std::env::var("CARD_WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(460.0)
}
const HEIGHT: f32 = 900.0;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// The card in a window; a wrapper hands down the properties these tests set.
fn window(plugin: &mut ColourGradingPlugin) -> ComponentInstance {
    offscreen::install();
    let card = plugin.card().expect("a card");
    let SlintSource::Text(source) = card.ui.source.clone() else { panic!("source text") };
    let doc = format!(
        "import {{ Theme }} from \"@ui-kit/ui-kit.slint\";\nimport {{ {c}, CurvePoint }} from \"plugin/card.slint\";\n\
         export component W inherits Window {{\n    width: {width}px;\n    height: {HEIGHT}px;\n    \
         in-out property <int> page;\n    in-out property <int> curve;\n    in-out property <int> channel;\n    in-out property <string> curve-line;\n    in-out property <[CurvePoint]> curve-points;\n    in-out property <int> curve-picked;\n    out property <length> card-h: card.preferred-height;\n    \
         card := {c} {{ hue: Theme.hue-camera; page: root.page; curve: root.curve; channel: root.channel; curve-line: root.curve-line; curve-points: root.curve-points; curve-picked: root.curve-picked; }}\n}}",
        c = card.ui.component,
        width = width()
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

fn card() -> (ColourGradingPlugin, FakeHost) {
    let (mut plugin, mut host) = (ColourGradingPlugin::new(), FakeHost::new());
    plugin.setup(&mut host);
    plugin.set_config(&json!({})).unwrap();
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    (plugin, host)
}

fn ui(plugin: &mut ColourGradingPlugin, host: &mut FakeHost, callback: &str, arg: usize) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![json!(arg)] });
}

/// What the plugin last told the card for a property.
fn told(host: &FakeHost, key: &str) -> Json {
    host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Json>(j).ok())
        .find_map(|v| v.get(key).cloned()).unwrap_or(Json::Null)
}

/// Hands the card what the plugin told it.
fn feed(w: &ComponentInstance, host: &FakeHost) {
    for key in ["page", "curve", "channel", "curve-line", "curve-points", "curve-picked"] {
        let value = told(host, key);
        if value.is_null() {
            continue;
        }
        w.set_property(key, from_json(&value)).unwrap();
    }
}

/// Hands the card what the plugin told it, and lets it lay out and finish the move a page switch starts (the pages cross-fade).
fn show(w: &ComponentInstance, host: &FakeHost) -> Vec<u8> {
    feed(w, host);
    offscreen::draw(w.window(), width(), HEIGHT, 1.0).expect("a picture");
    offscreen::skip(std::time::Duration::from_millis(900));
    offscreen::draw(w.window(), width(), HEIGHT, 1.0).expect("a picture").rgba
}

fn shows(w: &ComponentInstance, kind: &str) -> bool {
    ElementHandle::find_by_element_type_name(w, kind).next().is_some()
}

fn labelled(w: &ComponentInstance, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(w, label).next().is_some()
}

#[test]
fn pages_show_one_at_a_time_and_the_channel_row_only_for_rgb() {
    let (mut plugin, mut host) = card();
    let w = window(&mut plugin);
    show(&w, &host);
    assert!(shows(&w, "WheelCell") && !shows(&w, "CurveBox"), "Primaries is the page to begin with");

    ui(&mut plugin, &mut host, "page-picked", 1);
    show(&w, &host);
    assert!(!shows(&w, "WheelCell") && shows(&w, "CurveBox"));
    assert!(labelled(&w, "Channel"));
    assert!(!shows(&w, "Note"));

    ui(&mut plugin, &mut host, "curve-picked-by-hand", 1);
    show(&w, &host);
    assert!(!labelled(&w, "Channel"));
    assert_eq!(told(&host, "curve-points")[0], json!({"x": 0.0, "y": 0.5}), "the hue curve's neutral line");
    assert!(!shows(&w, "Note"), "no helper text under a hue curve");
}

/// Where the bright line crosses a column of the curve box: the mean row of its pixels, or None.
fn line_row(rgba: &[u8], x: usize, rows: std::ops::Range<usize>) -> Option<f32> {
    let hits: Vec<usize> = rows.filter(|&y| rgba[(y * WIDTH_PX + x) * 4..][..3].iter().map(|&c| u32::from(c)).sum::<u32>() > 3 * 150).collect();
    (!hits.is_empty()).then(|| hits.iter().sum::<usize>() as f32 / hits.len() as f32)
}

const WIDTH_PX: usize = 460;

#[test]
fn the_curve_spans_the_box_and_passes_through_its_points() {
    // A path keeps its viewbox's shape unless told to fill, and the box isn't square: the line used to stop short of the ends
    let (mut plugin, mut host) = card();
    plugin.set_config(&json!({"curves": {"all": [[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]]}})).unwrap();
    plugin.on_event(&mut host, &Event::StreamStopped);
    let w = window(&mut plugin);
    ui(&mut plugin, &mut host, "page-picked", 1);
    let rgba = show(&w, &host);
    let found = ElementHandle::find_by_element_type_name(&w, "CurveBox").next().expect("the curve box");
    let (at, size) = (found.absolute_position(), found.size());
    // The box is inset 8px each side, and 8px top and bottom for a tone curve
    let (left, top, area_w, area_h) = (at.x + 8.0, at.y + 8.0, size.width - 16.0, size.height - 16.0);
    let points = [(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)];
    for x in [0.06, 0.12, 0.25, 0.4, 0.5, 0.6, 0.75, 0.88, 0.94] {
        let column = (left + x * area_w).round() as usize;
        let want = top + (1.0 - f64::from(monotone_curve(&points, x as f64)) as f32) * area_h;
        let rows = (top as usize - 2)..((top + area_h) as usize + 3);
        let got = line_row(&rgba, column, rows).unwrap_or_else(|| panic!("no line at {x} of the width"));
        assert!((got - want).abs() < 3.0, "at {x} of the width the line is at row {got}, the curve says {want}");
    }
}

/// How many strongly coloured pixels the ring has on each side of a wheel's centre: left, right, top and bottom.
fn ring_sides(rgba: &[u8], wide: usize, at: (usize, usize), radius: usize, scale: f32) -> [usize; 4] {
    let coloured = |x: usize, y: usize| {
        let p = &rgba[(y * wide + x) * 4..][..3];
        p.iter().max().unwrap() - p.iter().min().unwrap() > 90
    };
    let (cx, cy) = at;
    let (near, far) = (radius - (15.0 * scale) as usize, radius + (15.0 * scale) as usize);
    [
        (cx - far..cx - near).filter(|&x| coloured(x, cy)).count(),
        (cx + near..cx + far).filter(|&x| coloured(x, cy)).count(),
        (cy - far..cy - near).filter(|&y| coloured(cx, y)).count(),
        (cy + near..cy + far).filter(|&y| coloured(cx, y)).count(),
    ]
}

#[test]
fn the_ring_is_as_thick_on_every_side_of_the_disc() {
    // The ring and the disc used to be placed from different pixel rules, and the ring came out a pixel thicker on some sides
    let (mut plugin, host) = card();
    let w = window(&mut plugin);
    for (card_width, scale) in [(460.0, 1.0), (461.0, 1.0), (520.0, 1.0), (460.0, 2.0), (461.0, 2.0)] {
        w.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: scale });
        feed(&w, &host);
        let rgba = offscreen::draw(w.window(), card_width, HEIGHT, scale).expect("a picture").rgba;
        let wide = (card_width * scale).round() as usize;
        for found in ElementHandle::find_by_element_type_name(&w, "ColourWheel") {
            let (at, size) = (found.absolute_position(), found.size());
            let (cx, cy) = (((at.x + size.width / 2.0) * scale).round() as usize, ((at.y + size.height / 2.0) * scale).round() as usize);
            let radius = ((size.height.min(size.width) / 2.0 - 11.0) * scale) as usize;
            let sides = ring_sides(&rgba, wide, (cx, cy), radius, scale);
            assert!(sides.iter().all(|&n| n == sides[0]) && sides[0] >= (5.0 * scale) as usize - 1, "ring thickness left, right, top, bottom at width {card_width}, scale {scale}: {sides:?}");
        }
    }
}

#[test]
fn the_curve_drawn_is_the_one_applied() {
    let points = [(0.0, 0.1), (0.3, 0.2), (1.0, 0.9)];
    let (mut plugin, mut host) = card();
    let w = window(&mut plugin);
    ui(&mut plugin, &mut host, "page-picked", 1);
    let straight = show(&w, &host);

    plugin.set_config(&json!({"curves": {"all": [[0.0, 0.1], [0.3, 0.2], [1.0, 0.9]]}})).unwrap();
    plugin.on_event(&mut host, &Event::StreamStopped);
    let bent = show(&w, &host);
    assert_ne!(straight, bent, "the card paints the new curve");

    // the line it paints is the curve, in thousandths with y going down
    let line = told(&host, "curve-line");
    let words: Vec<&str> = line.as_str().unwrap().split(' ').collect();
    assert_eq!(words.len(), 200 * 3);
    for pair in words.chunks(3) {
        let (x, y) = (pair[1].parse::<f64>().unwrap() / 1000.0, pair[2].parse::<f64>().unwrap() / 1000.0);
        assert!((1.0 - y - f64::from(monotone_curve(&points, x))).abs() < 0.001, "{pair:?}");
    }
}

#[test]
fn it_is_an_optional_built_in_that_rides_in_presets_per_phone() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/configs/linux-two-phones");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(fixture).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let snapshot: Json = serde_json::from_slice(&out.stdout).unwrap();
    assert!(snapshot["plugins"].as_array().unwrap().contains(&json!("colour_grading")), "{}", snapshot["plugins"]);
    let info = ColourGradingPlugin::new().info();
    assert!(info.optional && info.in_presets && info.device_local && !info.needs_vision);
}

/// Pictures of the card for a look by eye: SHOTS=dir cargo test --test colour_grading_ui shots, at 1x and 2x.
#[test]
fn shots() {
    let Ok(dir) = std::env::var("SHOTS") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bent = json!({
        "curves": {"all": [[0.0, 0.1], [0.3, 0.2], [0.7, 0.9], [1.0, 0.9]], "r": [[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]], "g": [[0.0, 0.2], [0.4, 0.3], [1.0, 1.0]], "b": [[0.0, 0.0], [0.6, 0.3], [1.0, 0.8]]},
        "hue_sat": [[0.0, 0.5], [0.2, 0.9], [0.5, 0.1], [0.8, 0.5]],
        "hue_hue": [[0.0, 0.5], [0.5, 0.5], [0.55, 1.0], [0.6, 0.5], [0.8, 0.0]],
    });
    for (name, config) in [("flat", json!({})), ("bent", bent)] {
        let (mut plugin, mut host) = card();
        plugin.set_config(&config).unwrap();
        plugin.on_event(&mut host, &Event::StreamStopped);
        let w = window(&mut plugin);
        let take = |host: &FakeHost, file: &str| {
            feed(&w, host);
            for scale in [1.0f32, 2.0] {
                let pic = offscreen::draw(w.window(), width(), HEIGHT, scale).expect("a picture");
                pic.save_png(&dir.join(format!("{name}-{file}-{scale}x.png"))).unwrap();
            }
        };
        take(&host, "primaries");
        if std::env::var("PRIMARIES_ONLY").is_ok() {
            continue;
        }
        ui(&mut plugin, &mut host, "page-picked", 1);
        for ch in 0..4 {
            ui(&mut plugin, &mut host, "channel-picked", ch);
            take(&host, &format!("rgb{ch}"));
        }
        for c in [1, 2] {
            ui(&mut plugin, &mut host, "curve-picked-by-hand", c);
            take(&host, &format!("hue{c}"));
        }
    }
}

/// The card's height in each frame after Primaries is switched for another page (pictured when COLOUR_SHOTS names a folder).
fn page_switch(from: usize, to: usize, shots: Option<&Path>, name: &str) -> Vec<f32> {
    let _still = offscreen::freeze_clock();
    let (mut plugin, mut host) = card();
    let w = window(&mut plugin);
    ui(&mut plugin, &mut host, "page-picked", from);
    show(&w, &host);
    let mut heights = vec![match w.get_property("card-h").unwrap() { slint_interpreter::Value::Number(n) => n as f32, v => panic!("{v:?}") }];
    let mut pics = Vec::new();
    ui(&mut plugin, &mut host, "page-picked", to);
    feed(&w, &host);
    for _ in 0..40 {
        pics.push(offscreen::draw(w.window(), width(), HEIGHT, 1.0).expect("a picture"));
        heights.push(match w.get_property("card-h").unwrap() { slint_interpreter::Value::Number(n) => n as f32, v => panic!("{v:?}") });
        offscreen::skip(std::time::Duration::from_millis(16));
    }
    if let Some(dir) = shots {
        // A few frames of the move, side by side, cut to the card's own height
        let tallest = heights.iter().cloned().fold(0.0, f32::max).ceil() + 4.0;
        let picked: Vec<_> = [0usize, 3, 6, 9, 12, 16, 22, 30].iter().map(|&i| offscreen::Picture { width: pics[i].width, height: pics[i].height, rgba: pics[i].rgba.clone() }).collect();
        let (cw, ch) = (width() as usize, (tallest as usize).min(HEIGHT as usize));
        let gap = 3;
        let (sw, sh) = (4 * cw + 3 * gap, 2 * ch + gap);
        let mut rgba = vec![96u8; sw * sh * 4];
        for (n, p) in picked.iter().enumerate() {
            let (ox, oy) = ((n % 4) * (cw + gap), (n / 4) * (ch + gap));
            for y in 0..ch {
                for x in 0..cw {
                    let from = (y * p.width as usize + x) * 4;
                    let to = ((oy + y) * sw + ox + x) * 4;
                    rgba[to..to + 4].copy_from_slice(&p.rgba[from..from + 4]);
                }
            }
        }
        offscreen::Picture { width: sw as u32, height: sh as u32, rgba }.save_png(&dir.join(name)).unwrap();
    }
    heights
}

fn biggest_step(v: &[f32]) -> f32 {
    v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

#[test]
fn switching_pages_moves_the_cards_height_from_one_page_to_the_other_in_one_go() {
    let shots = std::env::var_os("COLOUR_SHOTS").map(PathBuf::from);
    if let Some(dir) = &shots {
        std::fs::create_dir_all(dir).unwrap();
    }
    let tag = std::env::var("COLOUR_TAG").unwrap_or_else(|_| "after".into());
    for (from, to, name) in [(0, 1, "primaries-to-curves"), (1, 0, "curves-to-primaries"), (0, 2, "primaries-to-lut")] {
        let h = page_switch(from, to, shots.as_deref(), &format!("colour-grading-{name}-{tag}.png"));
        let (start, end) = (h[0], *h.last().unwrap());
        assert!((start - end).abs() > 20.0, "{name}: the pages have different heights: {start} {end}");
        let (low, high) = (start.min(end), start.max(end));
        assert!(h.iter().all(|v| *v >= low - 1.0 && *v <= high + 1.0), "{name}: straight from one height to the other, never through a smaller or a bigger one: {h:?}");
        assert!(biggest_step(&h) < (high - low) * 0.25, "{name}: no jump: {h:?}");
        assert!(h[4] != start && (h[4] - end).abs() > 3.0, "{name}: it takes a few frames: {h:?}");
    }
}
