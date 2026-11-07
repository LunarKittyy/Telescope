//! The high quality cutout's model. What needs no files runs always; what needs the real model and library is ignored, and runs with
//! TELESCOPE_MATTING_PACK set to an unpacked matting download (tools/build_matting_pack.py --out makes the zip) and, for the pictures,
//! TELESCOPE_MATTING_CLIP set to a video: `cargo test -p telescope-vision --release matting -- --ignored --nocapture`.

use super::*;
use telescope_core::testing::scratch_dir;
use std::path::PathBuf;
use std::time::Instant;

#[test]
fn the_model_gets_the_same_number_of_pixels_whatever_shape_the_picture_is() {
    assert_eq!(input_size(1920, 1080), (640, 360));
    assert_eq!(input_size(1280, 720), (640, 360));
    // a phone held upright costs what a landscape one does
    assert_eq!(input_size(1080, 1920), (360, 640));
    assert_eq!(input_size(640, 480), (554, 416));
    for (w, h) in [(1920, 1080), (1080, 1920), (1000, 1000), (640, 480), (3840, 2160)] {
        let (iw, ih) = input_size(w, h);
        let pixels = (iw * ih) as f64;
        assert!((pixels / INPUT_PIXELS - 1.0).abs() < 0.02, "{w}x{h} -> {iw}x{ih}");
    }
}

#[test]
fn the_frame_is_shrunk_to_that_size_and_a_small_one_is_left_alone() {
    let small = model_picture(&Rgb::filled(1920, 1080, 10)).unwrap();
    assert_eq!((small.w, small.h), (640, 360));
    let upright = model_picture(&Rgb::filled(1080, 1920, 10)).unwrap();
    assert_eq!((upright.w, upright.h), (360, 640));
    assert!(model_picture(&Rgb::filled(320, 180, 10)).is_none());
}

#[test]
fn the_downsample_ratio_keeps_the_working_copy_near_320_pixels() {
    assert_eq!(downsample_ratio(640, 360), 0.5);
    assert_eq!(downsample_ratio(360, 640), 0.5);
    assert_eq!(downsample_ratio(320, 180), 1.0);
    assert_eq!(downsample_ratio(100, 100), 1.0);
    assert!((downsample_ratio(554, 416) - 320.0 / 554.0).abs() < 1e-6);
}

#[test]
fn the_library_has_the_name_its_platform_gives_it() {
    if cfg!(windows) {
        assert_eq!(library_name(), "onnxruntime.dll");
    } else {
        assert_eq!(library_name(), "libonnxruntime.so");
    }
}

#[test]
fn a_folder_without_the_files_is_missing_not_failed() {
    let folder = scratch_dir("matting-empty");
    assert!(matches!(Matting::load(&folder), Err(LoadError::Missing(_))));
    std::fs::create_dir_all(folder.join("lib")).unwrap();
    std::fs::write(folder.join("lib").join(library_name()), b"x").unwrap();
    assert!(matches!(Matting::load(&folder), Err(LoadError::Missing(_))), "the model is missing too");
}

#[test]
fn a_library_that_wont_load_fails_with_its_type_and_no_path() {
    let folder = scratch_dir("matting-broken");
    std::fs::create_dir_all(folder.join("lib")).unwrap();
    std::fs::create_dir_all(folder.join("models")).unwrap();
    std::fs::write(folder.join("lib").join(library_name()), b"this is not a library").unwrap();
    std::fs::write(folder.join("models").join(MODEL), b"x").unwrap();
    match Matting::load(&folder) {
        Err(LoadError::Failed(e)) => assert_eq!(e.kind, "OSError"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("loaded a text file"),
    }
}

// What needs the real files

fn pack() -> PathBuf {
    PathBuf::from(std::env::var("TELESCOPE_MATTING_PACK").expect("TELESCOPE_MATTING_PACK is the folder of an unpacked matting download"))
}

fn segmenter() -> Box<dyn Segmenter> {
    Matting::load(&pack()).map_err(|e| format!("{e:?}")).unwrap().segmenter().map_err(|e| e.message).unwrap()
}

/// The first `count` frames of the clip at 1920 x 1080, every `every`th one.
fn clip_frames(count: usize, every: usize) -> Vec<Rgb> {
    let clip = std::env::var("TELESCOPE_MATTING_CLIP").expect("TELESCOPE_MATTING_CLIP is a video");
    let (w, h) = (1920usize, 1080usize);
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", &clip, "-vf", &format!("select='not(mod(n\\,{every}))',scale={w}:{h}"), "-vsync", "0", "-frames:v", &count.to_string(), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact(w * h * 3).map(|f| Rgb::new(w, h, f.to_vec())).collect()
}

fn small_of(seg: &dyn Segmenter, frame: &Rgb) -> Rgb {
    seg.input(frame).unwrap_or_else(|| frame.clone())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "needs the real model and library (TELESCOPE_MATTING_PACK) and a clip (TELESCOPE_MATTING_CLIP)"]
fn real_masks_have_the_pictures_size_and_a_crisp_range() {
    let mut seg = segmenter();
    let frames = clip_frames(6, 2);
    let mut last = None;
    for frame in &frames {
        let small = small_of(&*seg, frame);
        assert_eq!((small.w, small.h), (640, 360));
        let mask = seg.segment(&small, 0).map_err(|e| e.message).unwrap();
        assert_eq!((mask.w, mask.h, mask.data.len()), (640, 360, 640 * 360));
        last = Some(mask);
    }
    let mask = last.unwrap();
    let n = mask.data.len() as f64;
    let person = mask.data.iter().filter(|v| **v > 127).count() as f64 / n;
    let unsure = mask.data.iter().filter(|v| (16..240).contains(*v)).count() as f64 / n;
    println!("person {person:.3} of the picture, {unsure:.4} of it neither person nor room");
    assert!(person > 0.02 && person < 0.9, "{person}");
    assert!(unsure < 0.08, "edges should be thin, {unsure}");
}

#[test]
#[ignore = "needs the real model and library (TELESCOPE_MATTING_PACK) and a clip (TELESCOPE_MATTING_CLIP)"]
fn real_state_starts_again_on_a_reset_and_at_another_size() {
    let mut seg = segmenter();
    let frames = clip_frames(4, 2);
    let (a, b) = (small_of(&*seg, &frames[0]), small_of(&*seg, &frames[1]));
    let first = seg.segment(&a, 0).map_err(|e| e.message).unwrap();
    let carried = seg.segment(&a, 0).map_err(|e| e.message).unwrap();
    assert_ne!(first.data, carried.data, "the second run of a picture sees what the first left behind");
    seg.reset();
    assert_eq!(seg.segment(&a, 0).map_err(|e| e.message).unwrap().data, first.data, "after a reset it is the first frame again");
    // Another size: the same as a model that has never seen anything else
    let tiny = frames[2].area(320, 180);
    let mut fresh = segmenter();
    let expected = fresh.segment(&tiny, 0).map_err(|e| e.message).unwrap();
    seg.segment(&b, 0).map_err(|e| e.message).unwrap();
    let got = seg.segment(&tiny, 0).map_err(|e| e.message).unwrap();
    assert_eq!((got.w, got.h), (320, 180));
    assert_eq!(got.data, expected.data, "a new size is a new picture");
}

#[test]
#[ignore = "needs the real model and library (TELESCOPE_MATTING_PACK) and a clip (TELESCOPE_MATTING_CLIP)"]
fn real_masks_stay_still_where_nothing_moves() {
    // 15 masks a second, as the app makes them from 30 frames a second
    let mut seg = segmenter();
    let frames = clip_frames(60, 2);
    let masks: Vec<Gray> = frames.iter().map(|f| seg.segment(&small_of(&*seg, f), 0).map_err(|e| e.message).unwrap()).collect();
    // Pixels that are room in both of two neighbouring masks and person in neither's edge: how much they differ is flicker
    let mut flicker = Vec::new();
    for pair in masks[5..].windows(2) {
        let (mut sum, mut count) = (0u64, 0u64);
        for (a, b) in pair[0].data.iter().zip(&pair[1].data) {
            if *a < 8 || *a > 247 {
                sum += a.abs_diff(*b) as u64;
                count += 1;
            }
        }
        flicker.push(sum as f64 / count.max(1) as f64);
    }
    let typical = median(flicker.clone());
    println!("flicker where it was sure: median {typical:.3} levels, worst {:.3}", flicker.iter().cloned().fold(0.0, f64::max));
    assert!(typical < 2.0, "{typical}");
}

#[test]
#[ignore = "needs the real model and library (TELESCOPE_MATTING_PACK) and a clip (TELESCOPE_MATTING_CLIP)"]
fn real_cost_of_one_mask_at_1080p() {
    let mut seg = segmenter();
    let frames = clip_frames(30, 1);
    let (mut shrink, mut run, mut scale) = (Vec::new(), Vec::new(), Vec::new());
    for (i, frame) in frames.iter().enumerate() {
        let t = Instant::now();
        let small = small_of(&*seg, frame);
        let a = t.elapsed();
        let mask = seg.segment(&small, 0).map_err(|e| e.message).unwrap();
        let b = t.elapsed() - a;
        let _full = crate::ops::mask_resized(&mask.data, mask.w, mask.h, frame.w, frame.h);
        let c = t.elapsed() - a - b;
        if i >= 5 {
            shrink.push(a.as_secs_f64() * 1000.0);
            run.push(b.as_secs_f64() * 1000.0);
            scale.push(c.as_secs_f64() * 1000.0);
        }
    }
    let (s, r, u) = (median(shrink), median(run), median(scale));
    println!("1080p mask: shrink {s:.1} ms, model {r:.1} ms, back up to 1080p {u:.1} ms, in all {:.1} ms", s + r + u);
}
