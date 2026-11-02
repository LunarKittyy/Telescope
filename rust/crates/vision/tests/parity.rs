//! The vision crate against what the Python app made of the same frames (recorded from the Python app): every step before
//! and after the models to the byte, and the real models within float tolerance when a pack is there (TELESCOPE_VISION_PACK, or
//! this version's pack in the Python app's folder).

use ring::digest::{digest, SHA256};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use telescope_core::vision::Turn;
use telescope_vision::ops::{self, Motion, Rgb};
use telescope_vision::service::Backend;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn recorded() -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join("vision.json")).unwrap()).unwrap()
}

fn sha(data: &[u8]) -> String {
    digest(&SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn pattern(w: usize, h: usize) -> Rgb {
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                data.push(((x * 7 + y * 13 + c * 50 + ((x * y) % 31) * 3) % 256) as u8);
            }
        }
    }
    Rgb::new(w, h, data)
}

fn noisy(rgb: &Rgb) -> Rgb {
    let mut out = rgb.clone();
    for y in 0..rgb.h {
        for x in 0..rgb.w {
            for c in 0..3 {
                let i = (y * rgb.w + x) * 3 + c;
                let noise = ((x * 31 + y * 17 + c * 7) % 31) as i32 - 15;
                out.data[i] = (rgb.data[i] as i32 + noise).clamp(0, 255) as u8;
            }
        }
    }
    out
}

fn astronaut() -> Rgb {
    let mut data = Vec::new();
    flate2::read::ZlibDecoder::new(std::fs::File::open(fixtures().join("astronaut.rgb.z")).unwrap()).read_to_end(&mut data).unwrap();
    Rgb::new(384, 216, data)
}

fn frame_named(name: &str) -> Rgb {
    if name == "astronaut" {
        return astronaut();
    }
    let (w, h) = name.split_once('x').unwrap();
    pattern(w.parse().unwrap(), h.parse().unwrap())
}

#[test]
fn model_input_matches_python_to_the_byte() {
    for (name, want) in recorded()["model_input"].as_object().unwrap() {
        let frame = frame_named(name);
        let small = ops::model_input(&frame, ops::MODEL_SIZE).unwrap_or(frame);
        assert_eq!((small.w as u64, small.h as u64), (want[0].as_u64().unwrap(), want[1].as_u64().unwrap()), "{name}");
        assert_eq!(sha(&small.data), want[2], "{name}");
    }
}

#[test]
fn the_motion_thumb_and_what_counts_as_moving_match_python() {
    let rec = recorded();
    let base = pattern(1280, 720);
    let mut moved = base.clone();
    for y in 300..420 {
        for x in 600..720 {
            moved.data[(y * 1280 + x) * 3..(y * 1280 + x) * 3 + 3].fill(255);
        }
    }
    assert_eq!(sha(&ops::thumb(&base)), rec["thumb"]);
    assert_eq!(sha(&ops::thumb(&astronaut())), rec["thumb_astronaut"]);
    let mut motion = Motion::default();
    let mut seq = vec![motion.moved(&base)];
    motion.used();
    seq.push(motion.moved(&noisy(&base)));
    seq.push(motion.moved(&moved));
    assert_eq!(serde_json::json!(seq), rec["moved"]);
}

#[test]
fn the_small_copy_for_people_matches_python() {
    for (name, want) in recorded()["people_small"].as_object().unwrap() {
        let frame = frame_named(name);
        let (w, h) = ops::people_size(frame.w, frame.h, 640);
        let small = frame.area(w, h);
        assert_eq!((w as u64, h as u64), (want[0].as_u64().unwrap(), want[1].as_u64().unwrap()));
        assert_eq!(sha(&small.data), want[2], "{name}");
    }
}

#[test]
fn carrying_masks_and_scaling_them_match_python() {
    let rec = recorded();
    let a: Vec<u8> = (0..65536u64).map(|i| ((i * 37 + 11) % 256) as u8).collect();
    let b: Vec<u8> = (0..65536u64).map(|i| ((i * 101 + 7) % 256) as u8).collect();
    for (carry, want) in rec["add_weighted"].as_object().unwrap() {
        let carry: f64 = carry.parse().unwrap();
        assert_eq!(sha(&ops::add_weighted(&a, 1.0 - carry, &b, carry)), *want, "carry {carry}");
    }
    let mut f: Vec<f32> = (0..65536u32).map(|i| (i as f64 / 65535.0) as f32).collect();
    f.extend((0..256).map(|k| (k as f64 / 255.0) as f32));
    f.extend((0..256).map(|k| ((k as f64 + 0.5) / 255.0) as f32));
    assert_eq!(sha(&ops::convert_scale_abs(&f, 255.0)), rec["convert_scale_abs"]);
    let mask: Vec<u8> = (0..144).flat_map(|y| (0..256).map(move |x| ((x * 3 + y * 5) % 256) as u8)).collect();
    for (size, want) in rec["mask_up"].as_object().unwrap() {
        let (w, h) = size.split_once('x').unwrap();
        assert_eq!(sha(&ops::mask_resized(&mask, 256, 144, w.parse().unwrap(), h.parse().unwrap())), *want, "{size}");
    }
}

#[test]
fn turning_a_frame_upright_matches_python() {
    let frame = astronaut();
    for (key, want) in recorded()["turned"].as_object().unwrap() {
        let b = key.as_bytes();
        let rotation = match &key[2..] { "0" => None, d => Some(d.parse().unwrap()) };
        let turn = Turn { flip_h: b[0] == b'1', flip_v: b[1] == b'1', rotation };
        assert_eq!(sha(&ops::turned(&frame, turn).data), *want, "{key}");
    }
}

fn pack() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TELESCOPE_VISION_PACK") {
        return Some(PathBuf::from(dir));
    }
    let (dir, info) = (telescope_vision::pack::default_dir(), telescope_vision::pack::PackInfo::current());
    info.installed(&dir).then(|| info.folder(&dir))
}

// The Python tests' TELESCOPE_VISION_PACK ones, plus what the models make of the astronaut against Python's recording.
#[test]
fn test_the_real_models_find_a_person_shaped_blob() {
    let Some(folder) = pack() else {
        eprintln!("no vision pack here: set TELESCOPE_VISION_PACK to a pack folder");
        return;
    };
    let backend = telescope_vision::mediapipe::MediaPipe::load(&folder).unwrap_or_else(|e| panic!("{e:?}"));
    let mut segment = backend.segmenter().unwrap();
    let grey = Rgb::filled(1280, 720, 90);
    let full = segment.segment(&grey, 1).unwrap();
    assert_eq!((full.w, full.h, full.data.len()), (1280, 720, 1280 * 720));
    drop(segment);
    let mut segment = backend.segmenter().unwrap();
    let small = ops::model_input(&grey, ops::MODEL_SIZE).unwrap();
    let m = segment.segment(&small, 1).unwrap();
    assert_eq!((m.w, m.h), (256, 144));
    drop(segment);
    let mut pose = backend.pose().unwrap();
    assert!(pose.detect(&grey.area(640, 360), 1).unwrap().is_empty());
    drop(pose);

    let rec = recorded();
    let mut segment = backend.segmenter().unwrap();
    let small = ops::model_input(&astronaut(), ops::MODEL_SIZE).unwrap();
    let mask = segment.segment(&small, 1).unwrap();
    let want = std::fs::read(fixtures().join("astronaut_mask.u8")).unwrap();
    assert_eq!(serde_json::json!([mask.w, mask.h]), rec["mask_shape"]);
    let worst = mask.data.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    let off = mask.data.iter().zip(&want).filter(|(a, b)| a != b).count();
    eprintln!("mask: {off} of {} bytes differ, by at most {worst}", want.len());
    assert!(worst <= 2, "the mask is {worst} levels off Python's");
    let mut pose = backend.pose().unwrap();
    let people = pose.detect(&astronaut(), 1).unwrap();
    let want = rec["people"].as_array().unwrap();
    assert_eq!(people.len(), want.len());
    let mut worst = 0f64;
    for (p, w) in people.iter().zip(want) {
        for (name, point) in w.as_object().unwrap() {
            let got = p.points[name];
            for (g, e) in [got.0, got.1, got.2].iter().zip(point.as_array().unwrap()) {
                worst = worst.max((g - e.as_f64().unwrap()).abs());
            }
        }
    }
    eprintln!("people: at most {worst} off");
    assert!(worst < 1e-4, "people are {worst} off Python's");
}

#[test]
fn test_the_real_pack_has_no_address_to_report_usage_to() {
    let Some(folder) = pack() else { return };
    let script = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/build_vision_pack.py")).unwrap();
    let report_to = script.split("TELEMETRY_URL = b\"").nth(1).and_then(|rest| rest.split('"').next()).expect("the address").as_bytes();
    fn libraries(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                libraries(&path, found);
            } else if path.extension().is_some_and(|e| e == "so" || e == "dll") {
                found.push(path);
            }
        }
    }
    let mut libs = Vec::new();
    libraries(&folder.join("site").join("mediapipe"), &mut libs);
    assert!(!libs.is_empty());
    for lib in libs {
        let data = std::fs::read(&lib).unwrap();
        assert!(!data.windows(report_to.len()).any(|w| w == report_to), "{} reports usage", lib.display());
    }
}
