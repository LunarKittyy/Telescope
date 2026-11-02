//! Notes, Name tag and Desk mode against what the Python app does. crates/effects/tests/fixtures holds what Python made
//! (make_cards_fixtures.py): what each plugin saves after loading odd settings, the layout and rendered tile of a set of name
//! tags, and the desk picture put through each desk setting.

use serde_json::Value;
use std::path::PathBuf;
use telescope_core::plugin::Plugin;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::notes::NotesPlugin;
use telescope_effects::sheet::{self, Sheet};
#[cfg(not(windows))]
use telescope_effects::tag::{self, Spec};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/effects/tests/fixtures")
}

fn json_of(name: &str) -> Value {
    telescope_core::json::loads(&std::fs::read_to_string(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))).unwrap()
}

/// A saved number is the same number, whether it was written 100 or 100.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same(va, vb)),
        _ => a == b,
    }
}

#[test]
fn settings_load_and_save_as_python_does() {
    let mut checked = 0;
    for case in json_of("cards_configs.json").as_array().unwrap() {
        let mut plugin: Box<dyn Plugin> = match case["plugin"].as_str().unwrap() {
            "notes" => Box::new(NotesPlugin::new()),
            "name_tag" => Box::new(NameTagPlugin::new()),
            "desk" => Box::new(DeskPlugin::new()),
            other => panic!("{other}"),
        };
        let loaded = plugin.set_config(&case["input"]);
        if case["output"].is_null() {
            assert!(loaded.is_err(), "{} given {} should be refused", case["plugin"], case["input"]);
            continue;
        }
        loaded.unwrap();
        let got = plugin.get_config();
        assert!(same(&got, &case["output"]), "{} given {}:\n  rust   {got}\n  python {}", case["plugin"], case["input"], case["output"]);
        checked += 1;
    }
    assert!(checked > 10);
}

#[cfg(not(windows))]
fn rect(v: &Value) -> [f64; 4] {
    let a = v.as_array().unwrap();
    [a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_f64().unwrap(), a[3].as_f64().unwrap()]
}

#[cfg(not(windows))]
fn spec_of(v: &Value) -> Spec {
    let s = |k: &str| v[k].as_str().unwrap().to_string();
    Spec { name: s("name"), pronouns: s("pronouns"), second: s("second"), style: s("style"), position: s("position"), size: s("size") }
}

#[cfg(not(windows))] // recorded with Linux's fonts
#[test]
fn name_tags_are_laid_out_and_drawn_like_python() {
    let Some(fonts) = tag::Fonts::system() else {
        eprintln!("no font on this machine: the name tag comparison is skipped");
        return;
    };
    let all = json_of("nametag.json");
    for case in all["cases"].as_array().unwrap() {
        let (w, h) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
        let spec = spec_of(&case["spec"]);
        let lay = tag::layout_with(&spec, w, h, &fonts).unwrap();
        let want = rect(&case["box"]);
        let got = [lay.box_.x, lay.box_.y, lay.box_.w, lay.box_.h];
        let t = &case["tile"];
        let (tw, th) = (t["width"].as_u64().unwrap() as usize, t["height"].as_u64().unwrap() as usize);
        let rendered = tag::render_with(&spec, w, h, &fonts).unwrap();
        let tile = std::fs::read(fixtures().join(t["file"].as_str().unwrap())).unwrap();
        // Align the two tiles on the edge the tag is anchored to
        let right = spec.position.ends_with("right");
        let (cw, ch) = (tw.min(rendered.width), th.min(rendered.height));
        let (mut sum_alpha, mut big_alpha, mut sum_ink) = (0u64, 0usize, 0u64);
        for y in 0..ch {
            for x in 0..cw {
                let (rx, px) = if right { (rendered.width - 1 - x, tw - 1 - x) } else { (x, x) };
                let (r, p) = (y * rendered.width + rx, y * tw + px);
                let (alpha_r, alpha_p) = (255 - rendered.keep[r] as i32, tile[p * 4 + 3] as i32);
                sum_alpha += (alpha_r - alpha_p).unsigned_abs() as u64;
                big_alpha += ((alpha_r - alpha_p).abs() > 64) as usize;
                for c in 0..3 {
                    sum_ink += (rendered.ink[r * 3 + c] as i32 - tile[p * 4 + c] as i32).unsigned_abs() as u64;
                }
            }
        }
        let n = (cw * ch) as f64;
        eprintln!("{:<26} box {:?} vs {:?}  tile {}x{} vs {tw}x{th}  alpha off by {:.2} on average, {:.4} of pixels by over 64; ink {:.2}",
                  case["name"].as_str().unwrap(), got.map(|v| v as i64), want.map(|v| v as i64), rendered.width, rendered.height,
                  sum_alpha as f64 / n, big_alpha as f64 / n, sum_ink as f64 / (n * 3.0));
        // Qt places glyphs in quarter pixels, so only the glyph edges differ
        assert_eq!(got, want, "{}: the box", case["name"]);
        assert!(sum_alpha as f64 / n < 4.0 && big_alpha as f64 / n < 0.02, "{}: the tile", case["name"]);
    }
}

#[test]
fn the_desk_makes_the_same_pictures_as_python() {
    let all = json_of("desk.json");
    let (w, h) = (all["width"].as_u64().unwrap() as usize, all["height"].as_u64().unwrap() as usize);
    let raw = std::fs::read(fixtures().join(all["frame"].as_str().unwrap())).unwrap();
    let rgb: Vec<u8> = raw.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    for case in all["cases"].as_array().unwrap() {
        let s = &case["settings"];
        let c = s["corners"].as_array().unwrap();
        let corners = [0, 1, 2, 3].map(|i| [c[i][0].as_f64().unwrap(), c[i][1].as_f64().unwrap()]);
        let stretch = [s["stretch"][0].as_i64().unwrap() as i32, s["stretch"][1].as_i64().unwrap() as i32];
        let sheet = Sheet { corners, cleanup: s["cleanup"].as_str().unwrap().into(), shape: s["shape"].as_str().unwrap().into(), stretch, geometry: Default::default() };
        let warp = sheet::warp_for(&sheet, w, h);
        let rect: Vec<u64> = case["rect"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!([warp.rect.0 as u64, warp.rect.1 as u64, warp.rect.2 as u64, warp.rect.3 as u64], rect[..], "{}: where the sheet goes", case["name"]);
        for (row, want) in warp.on_preview.iter().zip(case["on_preview"].as_array().unwrap()) {
            for (got, want) in row.iter().zip(want.as_array().unwrap()) {
                assert!((got - want.as_f64().unwrap()).abs() < 1e-6 * want.as_f64().unwrap().abs().max(1.0), "{}: the preview's warp", case["name"]);
            }
        }
        let got = sheet::straighten(&rgb, w, h, &sheet, &warp);
        let want = std::fs::read(fixtures().join(case["file"].as_str().unwrap())).unwrap();
        let (mut sum, mut over, mut max) = (0u64, 0usize, 0u32);
        for (g, p) in got.chunks(3).zip(want.chunks(4)) {
            let worst_channel = (0..3).map(|k| (g[k] as i32 - p[k] as i32).unsigned_abs()).max().unwrap();
            sum += (0..3).map(|k| (g[k] as i32 - p[k] as i32).unsigned_abs() as u64).sum::<u64>();
            over += (worst_channel > 24) as usize;
            max = max.max(worst_channel);
        }
        let (mean, over) = (sum as f64 / (w * h * 3) as f64, over as f64 / (w * h) as f64);
        eprintln!("{:<26} max {max:>3} mean {mean:.3} over {over:.4}", case["name"].as_str().unwrap());
        assert_eq!(max, 0, "{}: every pixel as OpenCV makes it", case["name"]);
    }
}
