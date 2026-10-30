//! The Cinema tools and their scopes against what the Python app does on the same pictures. crates/effects/tests/fixtures/cinema.json
//! has a checksum of everything Python made from a few test pictures (make_cinema_fixtures.py): the scopes' counts and pictures, false
//! colour, focus peaking, zebras and the tools together, plus what the level filter answers and what the plugin saves after loading odd
//! settings. A checksum that differs names the picture, and for a scope picture the first row that does.

use serde_json::Value;
use std::path::PathBuf;
use telescope_core::host::View;
use telescope_core::plugin::Plugin;
use telescope_effects::cinema::CinemaPlugin;
use telescope_effects::cinema_tools::*;
use telescope_effects::cpu::Rgb;
use telescope_effects::scopes;

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/effects/tests/fixtures/cinema.json");
    telescope_core::json::loads(&std::fs::read_to_string(path).expect("cinema.json")).unwrap()
}

fn crc32(bytes: &[u8]) -> u32 {
    let table: Vec<u32> = (0..256u32).map(|n| (0..8).fold(n, |c, _| if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 })).collect();
    !bytes.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}

/// The picture make_cinema_fixtures.py makes, from integer arithmetic alone.
fn picture(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255) / (w - 1).max(1);
            let g = (y * 255) / (h - 1).max(1);
            let b = ((x * 3 + y * 5) ^ ((x * y) >> 3)) & 255;
            img.set(x, y, [r as u8, g as u8, b as u8]);
        }
    }
    let mut fill = |x0: usize, x1: usize, y0: usize, y1: usize, rgb: [u8; 3]| {
        for y in y0..y1 {
            for x in x0..x1 {
                img.set(x, y, rgb);
            }
        }
    };
    fill(w / 10, w / 3, h / 8, h / 3, [220, 60, 30]);
    fill(w / 2, w * 3 / 4, h / 2, h * 3 / 4, [250; 3]);
    fill(0, w / 4, h * 3 / 4, h, [0; 3]);
    fill(0, w, h / 5, h / 5 + 1, [255; 3]);
    fill(w / 3 * 2, w / 3 * 2 + 1, 0, h, [255; 3]);
    img
}

/// The bytes a numpy BGR picture has.
fn bgr(frame: &Rgb) -> Vec<u8> {
    frame.data.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect()
}

fn i64_bytes(counts: &[u32]) -> Vec<u8> {
    counts.iter().flat_map(|&c| (c as i64).to_le_bytes()).collect()
}

fn mask_bytes(mask: &[bool]) -> Vec<u8> {
    mask.iter().map(|&m| m as u8).collect()
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap()
}

fn look_of(v: &Value) -> Look {
    let mut look = Look::default();
    let flag = |k: &str| v.get(k) == Some(&Value::Bool(true));
    look.peaking = flag("peaking");
    look.zebras = flag("zebras");
    look.false_colour = flag("false_colour");
    if let Some(c) = v.get("peaking_colour").and_then(Value::as_str) {
        look.peaking_colour = PEAKING_COLOURS.iter().map(|t| t.0).find(|k| *k == c).unwrap();
    }
    if let Some(l) = v.get("peaking_level").and_then(Value::as_i64) {
        look.peaking_level = l;
    }
    if let Some(l) = v.get("zebra_level").and_then(Value::as_i64) {
        look.zebra_level = l;
    }
    if let Some(g) = v.get("guides").and_then(Value::as_str) {
        look.guides = GUIDES.iter().map(|t| t.0).find(|k| *k == g).unwrap();
    }
    look
}

#[test]
fn the_scopes_count_and_draw_as_python_does() {
    let fx = fixture();
    let mut checked = 0;
    for (name, p) in fx["pictures"].as_object().unwrap() {
        let img = picture(n(&p["w"]) as usize, n(&p["h"]) as usize);
        assert_eq!(crc32(&scopes::luma(&img).data), n(&p["luma"]) as u32, "{name}: luma");
        let small = scopes::shrink(&img, scopes::SCOPE_WIDTH);
        let shrink = &p["shrink"];
        assert_eq!((small.w as u64, small.h as u64), (n(&shrink["w"]), n(&shrink["h"])), "{name}: shrink size");
        assert_eq!(crc32(&bgr(&small)), n(&shrink["crc"]) as u32, "{name}: shrink");
        let counts = scopes::histogram(&small);
        let want: Vec<Vec<u64>> = p["histogram"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(n).collect()).collect();
        let got: Vec<Vec<u64>> = counts.iter().map(|r| r.iter().map(|&c| c as u64).collect()).collect();
        assert_eq!(got, want, "{name}: histogram");
        let luma = scopes::luma(&small);
        assert_eq!(crc32(&i64_bytes(&scopes::waveform(&luma.data, small.w, small.h))), n(&p["waveform"]) as u32, "{name}: waveform");
        assert_eq!(crc32(&i64_bytes(&scopes::parade(&small))), n(&p["parade"]) as u32, "{name}: parade");
        assert_eq!(crc32(&i64_bytes(&scopes::vectorscope(&small, 256))), n(&p["vectorscope"]) as u32, "{name}: vectorscope");
        assert_eq!(crc32(&i64_bytes(&scopes::vectorscope(&small, 64))), n(&p["vectorscope_64"]) as u32, "{name}: vectorscope at 64");
        let (cb, cr) = scopes::chroma(&small);
        assert_eq!([crc32(&cb), crc32(&cr)], [n(&p["chroma"][0]) as u32, n(&p["chroma"][1]) as u32], "{name}: chroma");
        for scope in scopes::SCOPE_NAMES {
            let want = &p["images"][scope];
            let image = scopes::scope_image(scope, &img).unwrap();
            assert_eq!((image.w as u64, image.h as u64), (n(&want["w"]), n(&want["h"])), "{name}: {scope} size");
            if crc32(&image.data) != n(&want["crc"]) as u32 {
                let rows = want["rows"].as_array().unwrap();
                let first = (0..image.h).find(|&y| crc32(&image.data[y * image.w * 4..(y + 1) * image.w * 4]) != n(&rows[y]) as u32);
                panic!("{name}: the {scope} picture differs, first at row {first:?}");
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 20);
}

#[test]
fn the_pixel_tools_mark_what_python_marks() {
    let fx = fixture();
    let mut checked = 0;
    for (name, p) in fx["pictures"].as_object().unwrap() {
        let img = picture(n(&p["w"]) as usize, n(&p["h"]) as usize);
        assert_eq!(crc32(&bgr(&false_colour(&img, None))), n(&p["false_colour"]) as u32, "{name}: false colour");
        for (sensitivity, crc) in p["peaking"].as_object().unwrap() {
            let mask = peaking_mask(&img, sensitivity.parse().unwrap());
            assert_eq!(crc32(&mask_bytes(&mask)), n(crc) as u32, "{name}: peaking at {sensitivity}");
            checked += 1;
        }
        for (key, crc) in p["zebra"].as_object().unwrap() {
            let (level, phase) = key.split_once(':').unwrap();
            let mask = zebra_mask(&img, level.parse().unwrap(), phase.parse().unwrap(), None);
            assert_eq!(crc32(&mask_bytes(&mask)), n(crc) as u32, "{name}: zebras {key}");
            checked += 1;
        }
        for case in p["tint"].as_array().unwrap() {
            let look = look_of(&case["look"]);
            let mut phase = 0;
            for (call, crc) in case["crcs"].as_array().unwrap().iter().enumerate() {
                let mut out = img.clone();
                tint(&look, &mut out, &mut phase);
                checked += 1;
                if case["grown"] == Value::Bool(true) {
                    // OpenCV's own bilinear resize is a level off in a few pixels and a mask's edge a pixel, so only the average colour has to agree
                    let pixels = (out.w * out.h) as f64;
                    for c in 0..3 {
                        let mean = out.data.chunks_exact(3).map(|p| p[2 - c] as f64).sum::<f64>() / pixels;
                        let want = case["means"][call][c].as_f64().unwrap();
                        assert!((mean - want).abs() < 0.5, "{name}: the tools with {} (call {call}): mean {mean} vs {want}", case["look"]);
                    }
                    continue;
                }
                assert_eq!(crc32(&bgr(&out)), n(crc) as u32, "{name}: the tools with {} (call {call})", case["look"]);
            }
        }
    }
    assert_eq!(checked, 5 * (5 + 9 + 27));
}

#[test]
fn the_tables_are_the_same() {
    let fx = fixture();
    let ours: Vec<(u64, Option<[u8; 3]>, &str)> = FALSE_COLOURS.iter().map(|(u, c, name)| (*u as u64, *c, *name)).collect();
    for (python, (upper, colour, name)) in fx["false_colours"].as_array().unwrap().iter().zip(&ours) {
        assert_eq!((n(&python[0]), python[2].as_str().unwrap()), (*upper, *name));
        // Python writes B, G, R
        let theirs = python[1].as_array().map(|c| [n(&c[2]) as u8, n(&c[1]) as u8, n(&c[0]) as u8]);
        assert_eq!(theirs, *colour);
    }
    let lut = false_lut();
    for (level, entry) in fx["lut"].as_array().unwrap().iter().enumerate() {
        let b = entry.as_array().unwrap();
        assert_eq!([n(&b[2]) as u8, n(&b[1]) as u8, n(&b[0]) as u8], lut[level], "false colour at {level}");
    }
    for (key, lines) in fx["guide_lines"].as_object().unwrap() {
        let want: Vec<f64> = lines.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert_eq!(guide_lines(key).unwrap(), want, "{key}");
    }
    for (python, (key, _, share)) in fx["sizes"].as_array().unwrap().iter().zip(SIZES) {
        assert_eq!((python[0].as_str().unwrap(), python[1].as_f64().unwrap()), (key, share));
    }
    for (python, (key, _, rgb)) in fx["peaking_colours"].as_array().unwrap().iter().zip(PEAKING_COLOURS) {
        let c = python[1].as_array().unwrap();
        assert_eq!((python[0].as_str().unwrap(), [n(&c[2]) as u8, n(&c[1]) as u8, n(&c[0]) as u8]), (key, rgb));
    }
    let c = &fx["constants"];
    assert_eq!(c["scope_fps"].as_f64().unwrap() as f32, SCOPE_FPS);
    assert_eq!((n(&c["zebra_period"]) as usize, n(&c["work_width"]) as usize), (ZEBRA_PERIOD, WORK_WIDTH));
    let f = |k: &str| c[k].as_f64().unwrap();
    assert_eq!([f("level_snap"), f("level_leave"), f("pitch_snap"), f("pitch_leave"), f("smooth_s"), f("stale_s"), f("unseen_s")],
               [LEVEL_SNAP, LEVEL_LEAVE, PITCH_SNAP, PITCH_LEAVE, LEVEL_SMOOTH_S, LEVEL_STALE_S, LEVEL_UNSEEN_S]);
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

#[test]
fn the_level_follows_the_view_and_the_filter_as_python_does() {
    let fx = fixture();
    let view_of = |name: &str| match name {
        "none" => View::default(),
        "flip_h" => View { flip_h: true, ..View::default() },
        "flip_v" => View { flip_v: true, ..View::default() },
        "both" => View { flip_h: true, flip_v: true, ..View::default() },
        "cw" => View { rotation: Some(90), ..View::default() },
        "r180" => View { rotation: Some(180), ..View::default() },
        "ccw" => View { rotation: Some(270), ..View::default() },
        "flip_cw" => View { flip_h: true, rotation: Some(90), ..View::default() },
        "flip_v_ccw" => View { flip_v: true, rotation: Some(270), ..View::default() },
        other => panic!("{other}"),
    };
    for case in fx["level"]["shown_roll"].as_array().unwrap() {
        let (view, roll) = (case["view"].as_str().unwrap(), case["roll"].as_f64().unwrap());
        assert_eq!(shown_roll(roll, &view_of(view)), case["shown"].as_f64().unwrap(), "{view} at {roll}");
    }
    let mut steps = 0;
    for run in fx["level"]["runs"].as_array().unwrap() {
        let mut filter = LevelFilter::new(run["smooth"].as_f64().unwrap());
        for (i, step) in run["steps"].as_array().unwrap().iter().enumerate() {
            let out = filter.update(step["roll"].as_f64(), step["pitch"].as_f64(), step["t"].as_f64().unwrap());
            match (&step["out"], out) {
                (Value::Null, None) => {}
                (want, Some(got)) => {
                    match (want["roll"].as_f64(), got.roll) {
                        (None, None) => {}
                        (Some(a), Some(b)) => assert!(close(a, b), "smooth {} step {i}: roll {a} vs {b}", run["smooth"]),
                        (a, b) => panic!("smooth {} step {i}: roll {a:?} vs {b:?}", run["smooth"]),
                    }
                    assert!(close(want["pitch"].as_f64().unwrap(), got.pitch), "smooth {} step {i}: pitch", run["smooth"]);
                    assert_eq!((want["roll_level"].as_bool().unwrap(), want["pitch_level"].as_bool().unwrap()), (got.roll_level, got.pitch_level), "smooth {} step {i}", run["smooth"]);
                }
                (want, got) => panic!("smooth {} step {i}: python {want}, rust {got:?}", run["smooth"]),
            }
            steps += 1;
        }
    }
    assert_eq!(steps, 480);
}

/// A saved number is the same number, whether it was written 100 or 100.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => matches!((x.as_f64(), y.as_f64()), (Some(x), Some(y)) if (x - y).abs() <= 1e-9 * x.abs().max(1.0)),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same(va, vb)),
        _ => a == b,
    }
}

#[test]
fn settings_load_and_save_as_python_does() {
    let mut refused = 0;
    let cases = fixture();
    for case in cases["configs"].as_array().unwrap() {
        let mut plugin = CinemaPlugin::new();
        let loaded = plugin.set_config(&case["input"]);
        if case["output"].is_null() {
            assert!(loaded.is_err(), "given {} the plugin should refuse", case["input"]);
            refused += 1;
            continue;
        }
        loaded.unwrap();
        let got = plugin.get_config();
        assert!(same(&got, &case["output"]), "given {}:\n  rust   {got}\n  python {}", case["input"], case["output"]);
    }
    assert!(refused >= 1 && cases["configs"].as_array().unwrap().len() > 10);
}
