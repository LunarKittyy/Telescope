//! VisionHost: the download the cards share (vision_download.py) and the people app.py tracks, on the bus.

use super::*;
use crate::service::tests::{wait, Fake};
use crate::service::{PoseModel, Segmenter, VisionError};
use std::io::Cursor;
use telescope_core::bus::EventBus;
use telescope_core::people::Track;
use telescope_core::testing::scratch_dir;
use telescope_core::vision::{MISSING, OFF, READY};

fn host(opener: WebOpener) -> (VisionHost, EventBus) {
    let (h, bus, _b) = host_with(opener);
    (h, bus)
}

fn host_with(opener: WebOpener) -> (VisionHost, EventBus, Arc<Fake>) {
    let backend = Fake::new();
    let (fake, given) = (backend.clone(), backend.clone());
    let (h, bus) = host_finding(Box::new(move || Ok(given.clone() as Arc<dyn Backend>)), opener);
    (h, bus, fake)
}

fn host_finding(find: crate::service::Find, opener: WebOpener) -> (VisionHost, EventBus) {
    let files: &[(&str, &[u8])] = &[("models/a.task", b"model")];
    let tmp = scratch_dir("vision-host");
    let folder = tmp.join("expected");
    std::fs::create_dir_all(folder.join("models")).unwrap();
    std::fs::write(folder.join("models/a.task"), files[0].1).unwrap();
    let tree = pack::tree_hash(&folder).unwrap();
    let info = PackInfo::from_json(&serde_json::json!({"linux": {"id": tree, "size": 100_000}}), Some("linux"));
    let h = VisionHost::with_effort(Arc::new(Effort::free()), find, None, tmp.join("vision"), info, opener);
    let bus = EventBus::new();
    h.attach(bus.sender());
    (h, bus)
}

fn events(bus: &EventBus) -> Vec<Event> {
    std::iter::from_fn(|| bus.next()).collect()
}

fn finished(bus: &EventBus) -> Vec<(String, String)> {
    let out = Mutex::new(Vec::new());
    wait(|| {
        let mut out = out.lock().unwrap();
        out.extend(events(bus).into_iter().filter_map(|e| match e {
            Event::VisionDownloadFinished { error, switch_on } => Some((error, switch_on)),
            _ => None,
        }));
        !out.is_empty()
    });
    out.into_inner().unwrap()
}

#[test]
fn a_finished_download_makes_vision_look_for_the_pack_again() {
    let (offline, zipped) = (Arc::new(AtomicBool::new(true)), crate::pack::tests::zipped(&[("models/a.task", b"model")]));
    let o = offline.clone();
    let (h, bus) = host_finding(Box::new(|| Err(LoadError::Missing("no".into()))), Box::new(move |_, _| {
        if o.load(Ordering::SeqCst) { Err(io::Error::other("offline")) } else { Ok(Box::new(Cursor::new(zipped.clone())) as Box<dyn Read>) }
    }));
    h.load();
    wait(|| h.state() == MISSING);
    let _ = events(&bus);
    h.download("framing");
    assert!(!finished(&bus)[0].0.is_empty(), "it broke");
    assert_eq!(h.state(), MISSING);
    offline.store(false, Ordering::SeqCst);
    h.download("framing");
    let states = Mutex::new(Vec::new());
    wait(|| {
        let mut states = states.lock().unwrap();
        states.extend(events(&bus).into_iter().filter_map(|e| match e {
            Event::VisionStateChanged { state } => Some(state),
            _ => None,
        }));
        !states.is_empty()
    });
    assert_eq!(*states.lock().unwrap(), [OFF], "nobody wants people, so it loads on the next ask");
    assert_eq!(h.state(), OFF);
}

#[test]
fn test_a_failed_download_remembers_why_for_the_plugin_that_waited() {
    let (h, bus) = host(Box::new(|_, _| Err(io::Error::other("offline"))));
    h.download("framing");
    let done = finished(&bus);
    assert_eq!(done[0].1, "framing");
    assert!(done[0].0.contains("internet connection"));
    let pack = h.pack();
    assert!(!pack.downloading && pack.failed_for == "framing" && pack.error.contains("internet connection"));
}

#[test]
fn test_the_plugin_waiting_for_the_download_is_switched_on_with_no_dialog_open() {
    let zipped = crate::pack::tests::zipped(&[("models/a.task", b"model")]);
    let (h, bus) = host(Box::new(move |_, _| Ok(Box::new(Cursor::new(zipped.clone())) as Box<dyn Read>)));
    h.download("framing");
    // the shell switches the waiting plugin on when this names it with no error
    assert_eq!(finished(&bus), [(String::new(), "framing".to_string())]);
    let pack = h.pack();
    assert!(pack.ready && pack.error.is_empty() && pack.failed_for.is_empty());
    h.download_failed("The switch couldn't be saved", "framing");
    let pack = h.pack();
    assert_eq!((pack.error.as_str(), pack.failed_for.as_str()), ("The switch couldn't be saved", "framing"));
}

#[test]
fn several_plugins_asking_while_it_downloads_share_one_download_and_all_switch_on() {
    let zipped = crate::pack::tests::zipped(&[("models/a.task", b"model")]);
    let (opened, gate) = (Arc::new(std::sync::atomic::AtomicU64::new(0)),Arc::new(AtomicBool::new(false)));
    let (o, g) = (opened.clone(), gate.clone());
    let (h, bus) = host(Box::new(move |_, _| {
        o.fetch_add(1, Ordering::SeqCst);
        wait(|| g.load(Ordering::SeqCst));
        Ok(Box::new(Cursor::new(zipped.clone())) as Box<dyn Read>)
    }));
    h.download("framing");
    h.download("background");
    h.download("framing");
    gate.store(true, Ordering::SeqCst);
    let out = Mutex::new(Vec::new());
    wait(|| {
        let mut out = out.lock().unwrap();
        out.extend(events(&bus).into_iter().filter_map(|e| match e {
            Event::VisionDownloadFinished { switch_on, .. } => Some(switch_on),
            _ => None,
        }));
        out.len() >= 2
    });
    assert_eq!(out.into_inner().unwrap(), ["framing", "background"]);
    assert_eq!(opened.load(Ordering::SeqCst), 1, "one download, not one per plugin");
}

#[test]
fn leftovers_of_a_download_that_was_cut_off_do_not_get_in_the_way() {
    let zipped = crate::pack::tests::zipped(&[("models/a.task", b"model")]);
    let (h, bus) = host(Box::new(move |_, _| Ok(Box::new(Cursor::new(zipped.clone())) as Box<dyn Read>)));
    let dir = h.vision_dir().to_path_buf();
    let pid = h.shared.info.pack_id().unwrap();
    std::fs::create_dir_all(dir.join(format!(".{pid}.unpacking"))).unwrap();
    std::fs::write(dir.join(format!(".{pid}.unpacking")).join("half"), b"x").unwrap();
    std::fs::write(dir.join(format!(".{pid}.zip.part")), b"half a zip").unwrap();
    assert!(!h.pack().ready && !h.pack().downloading, "nothing reads as downloading after a restart");
    h.download("");
    assert_eq!(finished(&bus), [(String::new(), String::new())]);
    assert!(h.pack().ready);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(left, [pid], "only the pack is left");
}

#[test]
fn people_are_tracked_steadily_and_reach_the_bus() {
    let (h, bus, fake) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    *fake.detect.lock().unwrap() = Arc::new(|| vec![telescope_core::pose::Person::new([("nose", 0.3, 0.3, 0.99), ("left_shoulder", 0.35, 0.45, 0.99), ("right_shoulder", 0.25, 0.45, 0.99)])]);
    h.load();
    wait(|| h.state() == READY);
    h.want_people("Auto framing", true);
    let frame = Frame::new(64, 36, telescope_core::frame::PixelFormat::Rgba, vec![0; 64 * 36 * 4]);
    let mut seen = Vec::new();
    for _ in 0..40 {
        h.feed_raw("phone", &frame, Upright::default());
        std::thread::sleep(Duration::from_millis(100));
        seen.extend(events(&bus));
        if seen.iter().any(|e| matches!(e, Event::PeopleTracked { count: 1, .. })) {
            break;
        }
    }
    assert!(seen.iter().any(|e| matches!(e, Event::PeopleSeen { source_id, .. } if source_id == "phone")));
    assert!(seen.iter().any(|e| matches!(e, Event::PeopleTracked { source_id, count: 1, .. } if source_id == "phone")));
    let (tracks, count) = h.tracked_people("phone");
    assert_eq!(count, 1);
    let track = Track::from_value(&tracks[0]).unwrap();
    assert!((track.face.unwrap().0 - 0.3).abs() < 1e-9);
    assert!(track.centre().0 < 0.5);
    h.view_turned(Turn::default(), Turn { flip_h: true, ..Turn::default() });
    let (tracks, count) = h.tracked_people("phone");
    let flipped = Track::from_value(&tracks[0]).unwrap();
    assert_eq!(count, 1);
    assert_eq!(flipped.id, track.id, "the same person");
    assert_eq!(flipped.id, 1);
    assert!(flipped.centre().0 > 0.5, "mirrored");
    h.drop_stream("phone");
    assert_eq!(h.tracked_people("phone"), (vec![], 0));
    // Started again straight away, the first person seen is confirmed again, not given the old id
    let person = telescope_core::pose::Person::new([("nose", 0.3, 0.3, 0.99), ("left_shoulder", 0.35, 0.45, 0.99), ("right_shoulder", 0.25, 0.45, 0.99)]);
    assert_eq!(h.shared.tracking.lock().unwrap().update("phone", &[person], service::monotonic()).1, 0);
    h.shutdown();
}

fn yuv_frame(w: usize, h: usize) -> Frame {
    let size = telescope_core::frame::PixelFormat::I420.size(w as u32, h as u32);
    Frame::new(w as u32, h as u32, telescope_core::frame::PixelFormat::I420, (0..size).map(|i| (60 + i % 90) as u8).collect())
}

/// A smooth picture (a gradient with a lit patch), the kind of scene a camera sees, and the same with a patch of `size` pixels square moved in it.
fn scene(w: usize, h: usize, noise: u8, patch: usize) -> Frame {
    let mut frame = yuv_frame(w, h);
    let data = Arc::make_mut(&mut frame.data);
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = (40 + (x * 100 / w) + (y * 60 / h)) as u8 + if (x * 7 + y * 13) % 5 == 0 { noise } else { 0 };
        }
    }
    for y in h / 3..h / 3 + patch {
        for x in w / 3..w / 3 + patch {
            data[y * w + x] = 230;
        }
    }
    frame
}

#[test]
fn the_grey_probe_calls_a_frame_moved_when_the_colour_picture_does() {
    let (w, h) = (1920, 1080);
    let base = scene(w, h, 0, 0);
    // (frame, moved)
    let cases = [(scene(w, h, 0, 0), false), (scene(w, h, 3, 0), false), (scene(w, h, 0, 90), true), (scene(w, h, 0, 400), true)];
    for (frame, moved) in cases {
        let (mut colour, mut grey) = (ops::Motion::default(), ops::Motion::default());
        colour.moved(&people_rgb(&base).unwrap());
        colour.used();
        grey.moved_grey(people_look(&base).unwrap());
        grey.used();
        assert_eq!(colour.moved(&people_rgb(&frame).unwrap()), moved, "colour");
        assert_eq!(grey.moved_grey(people_look(&frame).unwrap()), moved, "grey");
    }
}

/// A flat luma picture whose colour is neutral, with a patch of `size` luma-sized pixels square turned strongly coloured (the luma stays put).
fn flat_luma(w: usize, h: usize, size: usize) -> Frame {
    let mut frame = yuv_frame(w, h);
    let data = Arc::make_mut(&mut frame.data);
    data[..w * h].fill(64);
    let cw = w.div_ceil(2);
    let chroma = cw * h.div_ceil(2);
    data[w * h..].fill(128);
    for y in h / 4..h / 4 + size {
        for x in w / 4..w / 4 + size {
            let i = (y / 2) * cw + x / 2;
            data[w * h + i] = 240;
            data[w * h + chroma + i] = 240;
        }
    }
    frame
}

#[test]
fn the_grey_probe_calls_a_frame_moved_when_only_the_colour_changes() {
    let (w, h) = (1920, 1080);
    let base = flat_luma(w, h, 0);
    for (size, moved) in [(0, false), (700, true)] {
        let mut look = ops::Motion::default();
        look.moved_grey(people_look(&base).unwrap());
        look.used();
        assert_eq!(look.moved_grey(people_look(&flat_luma(w, h, size)).unwrap()), moved, "patch of {size}");
    }
}

/// A packed picture of one grey with a `size` pixel square turned red of the same grey level.
fn flat_rgb(format: telescope_core::frame::PixelFormat, w: usize, h: usize, size: usize) -> Frame {
    let red = if format == telescope_core::frame::PixelFormat::Bgra { [0, 0, 255, 255] } else { [255, 0, 0, 255] };
    let mut data: Vec<u8> = [76, 76, 76, 255].repeat(w * h);
    for y in h / 4..h / 4 + size {
        for x in w / 4..w / 4 + size {
            data[(y * w + x) * 4..][..4].copy_from_slice(&red);
        }
    }
    Frame::new(w as u32, h as u32, format, data)
}

#[test]
fn the_grey_probe_calls_an_rgb_frame_moved_when_only_the_colour_changes() {
    use telescope_core::frame::PixelFormat::{Bgra, Rgba};
    let (w, h) = (1920, 1080);
    for format in [Rgba, Bgra] {
        let base = flat_rgb(format, w, h, 0);
        for (size, moved) in [(0, false), (700, true)] {
            let mut look = ops::Motion::default();
            look.moved_grey(people_look(&base).unwrap());
            look.used();
            let changed = people_look(&flat_rgb(format, w, h, size)).unwrap();
            // the luma stays put, only the colour grids differ
            let grey = ops::THUMB.0 * ops::THUMB.1;
            assert!(people_look(&base).unwrap()[..grey].iter().zip(&changed[..grey]).all(|(a, b)| a.abs_diff(*b) <= 2), "{format:?} grey");
            assert_eq!(look.moved_grey(changed), moved, "{format:?} patch of {size}");
        }
    }
}

#[test]
fn the_grey_probe_calls_an_nv12_frame_moved_when_only_the_colour_changes() {
    let (w, h) = (1920, 1080);
    let nv12 = |size: usize| {
        let (cw, ch) = (w / 2, h / 2);
        let mut data = vec![64u8; w * h];
        data.extend(std::iter::repeat_n(128u8, cw * ch * 2));
        for y in h / 4..h / 4 + size {
            for x in w / 4..w / 4 + size {
                let i = w * h + ((y / 2) * cw + x / 2) * 2;
                data[i] = 240;
                data[i + 1] = 240;
            }
        }
        Frame::new(w as u32, h as u32, telescope_core::frame::PixelFormat::Nv12, data)
    };
    for (size, moved) in [(0, false), (700, true)] {
        let mut look = ops::Motion::default();
        look.moved_grey(people_look(&nv12(0)).unwrap());
        look.used();
        assert_eq!(look.moved_grey(people_look(&nv12(size)).unwrap()), moved, "patch of {size}");
    }
}

#[test]
fn a_colour_change_at_the_same_luma_runs_the_cutout_within_its_still_wait() {
    let (handed, ran): (Arc<Mutex<Vec<(usize, usize)>>>, Arc<Mutex<Vec<(usize, usize)>>>) = Default::default();
    let cutout = Arc::new(SeenCutout(handed.clone(), ran.clone()));
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(move || Ok(cutout.clone() as Arc<dyn Backend>)), None, scratch_dir("chroma-moved"), PackInfo::default());
    h.vision().wait_for_masks();
    h.load();
    wait(|| h.state() == READY);
    let base = flat_luma(640, 360, 0);
    let first = {
        let mut f = base.clone();
        f.number = 1;
        wait(|| h.person_mask_hq("a", &f, 160, 90, 0).is_some() && !ran.lock().unwrap().is_empty());
        ran.lock().unwrap().len()
    };
    let mut changed = flat_luma(640, 360, 240);
    changed.number = 2;
    assert!(h.person_mask_hq("a", &changed, 160, 90, 0).is_some());
    assert!(ran.lock().unwrap().len() > first, "the colour change ran the model");
    h.shutdown();
}

#[test]
fn the_models_get_small_pictures_made_from_the_planes() {
    let sizes = |f: &Frame| (model_rgb(f).map(|p| (p.w, p.h, p.data.len())), people_rgb(f).map(|p| (p.w, p.h, p.data.len())));
    assert_eq!(sizes(&yuv_frame(1920, 1080)), (Some((256, 144, 256 * 144 * 3)), Some((640, 360, 640 * 360 * 3))));
    assert_eq!(sizes(&yuv_frame(640, 480)), (Some((256, 192, 256 * 192 * 3)), Some((640, 480, 640 * 480 * 3))));
    // a frame already small enough for a model goes in as it is
    assert_eq!(sizes(&yuv_frame(200, 100)), (Some((200, 100, 200 * 100 * 3)), Some((200, 100, 200 * 100 * 3))));
}


/// (wall ms, cpu ms) a call takes, the best wall of a few runs and the cpu over all of them (every thread of the process, so work spread over the workers counts in full).
fn measure(f: &mut dyn FnMut()) -> (f64, f64) {
    let cpu = || {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0 * 1000.0
    };
    f();
    let runs = 60;
    let before = cpu();
    let wall = (0..runs).map(|_| {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }).fold(f64::MAX, f64::min);
    (wall, (cpu() - before) / runs as f64)
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_model_pictures() {
    for (w, h) in [(1920usize, 1080usize), (2560, 1440), (3840, 2160)] {
        let frame = yuv_frame(w, h);
        let (pw, ph) = ops::people_size(w, h, service::PEOPLE_WIDTH);
        let old = measure(&mut || { let full = rgb(&frame).unwrap(); crate::ops::model_input(&full, ops::MODEL_SIZE).unwrap(); });
        let new = measure(&mut || { model_rgb(&frame).unwrap(); });
        let old_people = measure(&mut || { rgb(&frame).unwrap().area(pw, ph); });
        let new_people = measure(&mut || { people_rgb(&frame).unwrap(); });
        let old_cut = measure(&mut || { let full = rgb(&frame).unwrap(); crate::matting::model_picture(&full).unwrap(); });
        let new_cut = measure(&mut || { let p = cutout_rgb(&frame).unwrap(); crate::matting::model_picture(&p); });
        println!("vision pictures {w}x{h} (wall ms, cpu ms): segmenter input old {old:.2?} new {new:.2?}; people input old {old_people:.2?} new {new_people:.2?}; cutout input old {old_cut:.2?} new {new_cut:.2?}");
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_still_scene_probe() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = yuv_frame(w, h);
        // what a frame of a still scene cost before it was dropped: the colour picture for the model, then its tiny grey copy
        let old = measure(&mut || { let p = people_rgb(&frame).unwrap(); crate::ops::thumb(&p); });
        let new = measure(&mut || { people_look(&frame).unwrap(); });
        let rgba = flat_rgb(telescope_core::frame::PixelFormat::Rgba, w, h, 0);
        let grey_only = measure(&mut || { telescope_core::image::small_grey(&rgba, ops::THUMB.0, ops::THUMB.1).unwrap(); });
        let with_colour = measure(&mut || { people_look(&rgba).unwrap(); });
        println!("vision still probe {w}x{h} (wall ms, cpu ms): old {old:.2?} new {new:.2?}; rgba grey only {grey_only:.2?} with colour {with_colour:.2?}");
    }
}

// ── The high quality cutout ─────────────────────────────────────────────

/// A host whose cutout download serves the given files (as the matting pack would), and a cutout that finds the same fake model.
fn host_with_cutout(files: &'static [(&'static str, &'static [u8])], served: Vec<u8>, offline: Arc<AtomicBool>) -> (VisionHost, EventBus, PathBuf) {
    let opener: WebOpener = Box::new(move |_, _| {
        if offline.load(Ordering::SeqCst) { Err(io::Error::other("offline")) } else { Ok(Box::new(Cursor::new(served.clone())) as Box<dyn Read>) }
    });
    let size = 1_000_000;
    let (h, bus) = host_finding(Box::new(|| Err(LoadError::Missing("no".into()))), opener);
    let tmp = scratch_dir("matting-host");
    let expected = tmp.join("expected");
    for (name, data) in files {
        std::fs::create_dir_all(expected.join(name).parent().unwrap()).unwrap();
        std::fs::write(expected.join(name), data).unwrap();
    }
    let tree = pack::tree_hash(&expected).unwrap();
    let info = PackInfo::from_json_for(pack::Kind::Matting, &serde_json::json!({"linux": {"id": tree, "size": size}}), Some("linux"));
    let dir = tmp.join("matting");
    let (there_dir, there_info) = (dir.clone(), info.clone());
    let h = h.with_cutout(Box::new(|| Err(LoadError::Missing("no".into()))), Some(Box::new(move || there_info.installed(&there_dir))), dir.clone(), info);
    (h, bus, dir)
}

const MATTING_FILES: &[(&str, &[u8])] = &[("lib/libonnxruntime.so", b"lib"), ("models/rvm.onnx", b"model")];

#[test]
fn a_host_with_no_cutout_has_none_to_offer_and_the_normal_mask_stands_in() {
    let (h, _bus, _fake) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    assert_eq!(h.matting_state(), MISSING);
    let pack = h.matting_pack();
    assert!(!pack.available && !pack.ready && !pack.downloading);
    h.download_matting();
    assert!(!h.matting_pack().downloading, "nothing to download");
    h.load();
    wait(|| h.state() == READY);
    let frame = Frame::new(64, 36, telescope_core::frame::PixelFormat::Rgba, vec![0; 64 * 36 * 4]);
    // the first mask of a stream waits for the model: what the normal path gives is what the high quality one gives
    let normal = h.person_mask("a", &frame);
    let hq = h.person_mask_hq("a", &frame, frame.width as usize, frame.height as usize, 0);
    assert!(normal.is_some());
    assert_eq!(hq, normal);
    assert_eq!(h.matting_state(), MISSING, "and asking didn't make anything load");
    h.shutdown();
}

#[test]
fn the_cutouts_download_is_its_own_and_finishes_with_no_plugin_to_switch_on() {
    let offline = Arc::new(AtomicBool::new(true));
    let (h, bus, dir) = host_with_cutout(MATTING_FILES, crate::pack::tests::zipped(MATTING_FILES), offline.clone());
    assert_eq!(h.matting_state(), MISSING);
    let pack = h.matting_pack();
    assert!(pack.available && !pack.ready && pack.size_mb >= 1);
    assert!(!h.pack().downloading, "the vision pack is another download");
    // offline: it says why and can be tried again
    h.download_matting();
    assert!(finished(&bus)[0].0.contains("internet connection"));
    let pack = h.matting_pack();
    assert!(!pack.downloading && !pack.ready && pack.error.contains("internet connection"), "{pack:?}");
    assert!(!h.pack().ready && h.pack().error.is_empty(), "the vision pack's row never hears about it");
    offline.store(false, Ordering::SeqCst);
    h.download_matting();
    assert_eq!(finished(&bus), [(String::new(), String::new())]);
    let pack = h.matting_pack();
    assert!(pack.ready && pack.error.is_empty() && !pack.downloading, "{pack:?}");
    assert!(dir.join(h.cutout_pack().unwrap().info.pack_id().unwrap()).join("models/rvm.onnx").is_file());
    // Remove download deletes both
    h.remove_pack().unwrap();
    assert!(!h.matting_pack().ready);
}

#[test]
fn asking_for_the_cutout_again_while_it_downloads_is_one_download_and_leftovers_do_not_get_in_the_way() {
    let (h, bus, dir) = host_with_cutout(MATTING_FILES, crate::pack::tests::zipped(MATTING_FILES), Arc::new(AtomicBool::new(false)));
    let pid = h.cutout_pack().unwrap().info.pack_id().unwrap();
    std::fs::create_dir_all(dir.join(format!(".{pid}.unpacking"))).unwrap();
    std::fs::write(dir.join(format!(".{pid}.zip.part")), b"half a zip").unwrap();
    assert!(!h.matting_pack().downloading, "nothing reads as downloading after a restart");
    h.download_matting();
    h.download_matting();
    h.download_matting();
    let first = finished(&bus);
    assert_eq!(first, [(String::new(), String::new())]);
    assert!(h.matting_pack().ready);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(left, [pid], "only the pack is left");
}

#[test]
fn a_cutout_that_goes_from_loading_to_ready_or_failed_reaches_the_bus() {
    let (h, bus, _dir) = host_with_cutout(MATTING_FILES, vec![], Arc::new(AtomicBool::new(true)));
    let frame = Frame::new(64, 36, telescope_core::frame::PixelFormat::Rgba, vec![0; 64 * 36 * 4]);
    assert!(h.person_mask_hq("a", &frame, frame.width as usize, frame.height as usize, 0).is_none(), "nothing normal is ready either");
    let heard = Mutex::new(Vec::new());
    wait(|| {
        heard.lock().unwrap().extend(events(&bus).into_iter().filter_map(|e| match e {
            Event::MattingStateChanged { state } => Some(state),
            _ => None,
        }));
        heard.lock().unwrap().len() >= 2
    });
    assert_eq!(*heard.lock().unwrap(), ["loading", "missing"]);
    assert_eq!(h.matting_state(), MISSING);
    h.shutdown();
}

#[test]
fn a_matting_library_swapped_after_the_download_was_checked_is_not_loaded() {
    let tmp = scratch_dir("matting-swap");
    let expected = tmp.join("expected");
    for (name, data) in MATTING_FILES {
        std::fs::create_dir_all(expected.join(name).parent().unwrap()).unwrap();
        std::fs::write(expected.join(name), data).unwrap();
    }
    let tree = pack::tree_hash(&expected).unwrap();
    let info = PackInfo::from_json_for(pack::Kind::Matting, &serde_json::json!({"linux": {"id": tree, "size": 1}}), Some("linux"));
    let dir = tmp.join("matting");
    let folder = info.folder(&dir);
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::fs::rename(&expected, &folder).unwrap();
    std::fs::write(folder.join(pack::DONE), info.pack_id().unwrap()).unwrap();
    assert!(info.installed(&dir));
    std::fs::write(folder.join("lib/libonnxruntime.so"), b"swapped").unwrap();
    assert!(info.installed(&dir), "the marker still says it's fine");
    match find_matting(&dir, &info) {
        Err(LoadError::Missing(why)) => assert!(why.contains("damaged"), "{why}"),
        Err(_) => panic!("the swap went unnoticed: the library was handed to the loader"),
        Ok(_) => panic!("loaded"),
    }
    assert!(!info.installed(&dir), "a download that no longer matches is offered again");
}

#[test]
fn the_normal_mask_stays_warm_while_the_cutout_gives_the_masks() {
    let (h, _bus, normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let cutout = Fake::new();
    let (dir, info) = (scratch_dir("warm-cutout"), PackInfo::default());
    let h = h.with_cutout(Box::new(move || Ok(cutout.clone() as Arc<dyn Backend>)), None, dir, info);
    h.load();
    wait(|| h.state() == READY);
    let frame = |n: u64| {
        let mut f = Frame::new(64, 36, telescope_core::frame::PixelFormat::Rgba, vec![0; 64 * 36 * 4]);
        f.number = n;
        f
    };
    let n = std::cell::Cell::new(0);
    let ask = || {
        n.set(n.get() + 1);
        h.person_mask_hq("a", &frame(n.get()), 64, 36, 0)
    };
    // until the cutout's first mask the normal one stands in
    wait(|| {
        std::thread::sleep(Duration::from_millis(5));
        ask().is_some() && h.matting_state() == READY
    });
    let ran = || normal.log.lock().unwrap().segmented.len();
    let before = ran();
    let end = std::time::Instant::now() + Duration::from_millis(900);
    while std::time::Instant::now() < end {
        assert!(ask().is_some());
        std::thread::sleep(Duration::from_millis(10));
    }
    // asked on its own, not only when the cutout fell behind: its mask is never older than the cutout's is allowed to go before it's needed
    assert!(ran() >= before + 2, "the normal model ran {} times in 0.9 s of cutout masks", ran() - before);
    h.shutdown();
}

#[test]
fn a_pack_file_swapped_after_the_marker_was_written_is_not_loaded() {
    let tmp = scratch_dir("vision-swap");
    let expected = tmp.join("expected");
    for (name, data) in [("site/lib.so", &b"library"[..]), ("models/a.task", b"model")] {
        std::fs::create_dir_all(expected.join(name).parent().unwrap()).unwrap();
        std::fs::write(expected.join(name), data).unwrap();
    }
    let tree = pack::tree_hash(&expected).unwrap();
    let info = PackInfo::from_json(&serde_json::json!({"linux": {"id": tree, "size": 1}}), Some("linux"));
    let dir = tmp.join("vision");
    let folder = info.folder(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::rename(&expected, &folder).unwrap();
    std::fs::write(folder.join(pack::DONE), info.pack_id().unwrap()).unwrap();
    let loads = std::sync::atomic::AtomicUsize::new(0);
    let load = |_: &Path| -> Result<Arc<dyn Backend>, LoadError> {
        loads.fetch_add(1, Ordering::SeqCst);
        Err(LoadError::Missing("stop".into()))
    };
    assert!(matches!(find_pack(&dir, &info, &load), Err(LoadError::Missing(m)) if m == "stop"));
    assert_eq!(loads.load(Ordering::SeqCst), 1, "an untouched pack is loaded");
    std::fs::write(folder.join("site/lib.so"), b"swapped").unwrap();
    assert!(info.installed(&dir), "the marker still says it's fine");
    let found = find_pack(&dir, &info, &load);
    assert_eq!(loads.load(Ordering::SeqCst), 1, "the swapped library was handed to the loader");
    match found {
        Err(LoadError::Missing(why)) => assert!(why.contains("damaged"), "{why}"),
        _ => panic!("expected a refusal"),
    }
    assert!(!info.installed(&dir), "a pack that no longer matches is offered again");
}

const PACK_FILES: [(&str, &[u8]); 2] = [("site/lib.so", b"library"), ("models/a.task", b"model")];

/// A pack unpacked the way a download leaves it, with its marker, and the zip it came from.
fn installed_pack(name: &str) -> (PathBuf, PackInfo, PathBuf) {
    let tmp = scratch_dir(name);
    let zip = tmp.join("pack.zip");
    std::fs::write(&zip, crate::pack::tests::zipped(&PACK_FILES)).unwrap();
    let expected = tmp.join("expected");
    for (file, data) in PACK_FILES {
        std::fs::create_dir_all(expected.join(file).parent().unwrap()).unwrap();
        std::fs::write(expected.join(file), data).unwrap();
    }
    let tree = pack::tree_hash(&expected).unwrap();
    let size = std::fs::metadata(&zip).unwrap().len();
    let info = PackInfo::from_json(&serde_json::json!({"linux": {"id": tree, "size": size}}), Some("linux"));
    let dir = tmp.join("vision");
    pack::install_zip(&info, &dir, &zip).unwrap();
    assert!(info.installed(&dir));
    (dir, info, zip)
}

#[test]
fn a_pack_whose_file_security_software_removed_is_offered_again_and_installs_over_the_damaged_one() {
    let (dir, info, zip) = installed_pack("vision-removed");
    std::fs::remove_file(info.folder(&dir).join("site/lib.so")).unwrap();
    assert!(info.installed(&dir), "the marker still says it's fine");
    let loads = std::sync::atomic::AtomicUsize::new(0);
    let load = |_: &Path| -> Result<Arc<dyn Backend>, LoadError> {
        loads.fetch_add(1, Ordering::SeqCst);
        Err(LoadError::Missing("stop".into()))
    };
    assert!(matches!(find_pack(&dir, &info, &load), Err(LoadError::Missing(why)) if why.contains("damaged")));
    assert_eq!(loads.load(Ordering::SeqCst), 0, "nothing was handed to the loader");
    assert!(!info.installed(&dir));
    pack::install_zip(&info, &dir, &zip).unwrap();
    assert!(info.installed(&dir));
    assert!(matches!(find_pack(&dir, &info, &load), Err(LoadError::Missing(why)) if why == "stop"));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[test]
fn the_card_of_a_pack_with_a_missing_file_offers_the_download_and_not_a_failure() {
    let (dir, info, zip) = installed_pack("vision-card");
    std::fs::remove_file(info.folder(&dir).join("models/a.task")).unwrap();
    let (find_dir, find_info) = (dir.clone(), info.clone());
    let find: crate::service::Find = Box::new(move || find_pack(&find_dir, &find_info, &|_| Ok(Fake::new() as Arc<dyn Backend>)));
    let (there_dir, there_info) = (dir.clone(), info.clone());
    let served = std::fs::read(&zip).unwrap();
    let h = VisionHost::with(find, Some(Box::new(move || there_info.installed(&there_dir))), dir, info, Box::new(move |_, _| Ok(Box::new(Cursor::new(served.clone())) as Box<dyn Read>)));
    assert!(h.pack().ready, "before anything looks at it");
    h.load();
    wait(|| h.state() == MISSING);
    assert!(!h.pack().ready);
    // Downloading it again puts the pack back and vision loads it on the next ask
    h.download("");
    wait(|| h.pack().ready && !h.pack().downloading);
    wait(|| h.state() == OFF || h.state() == READY);
}

fn pack_zip_for(h: &VisionHost, name: &str) -> PathBuf {
    let zip = h.shared.dir.parent().unwrap().join(name);
    std::fs::write(&zip, crate::pack::tests::zipped(&[("models/a.task", b"model")])).unwrap();
    zip
}

#[test]
fn a_zip_install_and_a_download_cannot_both_hold_the_pack_folder() {
    let gate = Arc::new(AtomicBool::new(false));
    let (opened, g) = (Arc::new(std::sync::atomic::AtomicU64::new(0)), gate.clone());
    let o = opened.clone();
    let zipped = crate::pack::tests::zipped(&[("models/a.task", b"model")]);
    let (h, bus) = host(Box::new(move |_, _| {
        o.fetch_add(1, Ordering::SeqCst);
        wait(|| g.load(Ordering::SeqCst));
        Ok(Box::new(Cursor::new(zipped.clone())) as Box<dyn Read>)
    }));
    let zip = pack_zip_for(&h, &format!("Telescope-vision-linux-{}.zip", h.shared.info.pack_id().unwrap()));
    h.download("");
    wait(|| opened.load(Ordering::SeqCst) == 1);
    assert_eq!(h.install_pack_zip(&zip), PackZip::Busy);
    gate.store(true, Ordering::SeqCst);
    assert_eq!(finished(&bus).len(), 1);
    assert_eq!(h.install_pack_zip(&zip), PackZip::Started);
    assert_eq!(finished(&bus).len(), 1);
    // an install is too quick to catch while it runs, so one that is running is stood in for by the flag it holds
    h.shared.download.lock().unwrap().running = true;
    assert_eq!(h.install_pack_zip(&zip), PackZip::Busy, "a second install while one runs");
    h.shared.download.lock().unwrap().running = false;
    assert_eq!(h.install_pack_zip(Path::new("my-plugin.zip")), PackZip::NotPack);
}

fn install_with_a_plugin_waiting(content: &[(&str, &[u8])]) -> (Vec<(String, String)>, VisionPack) {
    let (h, bus) = host(Box::new(|_, _| Err(io::Error::other("unused"))));
    let id = h.shared.info.pack_id().unwrap();
    let zip = h.shared.dir.parent().unwrap().join(format!("Telescope-vision-linux-{id}.zip"));
    std::fs::write(&zip, crate::pack::tests::zipped(content)).unwrap();
    // a plugin switched on while the install runs is in the waiting list the same way
    h.shared.download.lock().unwrap().waiting.push("framing".into());
    assert_eq!(h.install_pack_zip(&zip), PackZip::Started);
    (finished(&bus), h.pack())
}

#[test]
fn a_plugin_waiting_during_a_zip_install_is_switched_on_when_it_finishes() {
    let (done, pack) = install_with_a_plugin_waiting(&[("models/a.task", b"model")]);
    assert_eq!(done, [(String::new(), "framing".to_string())]);
    assert!(pack.ready && !pack.downloading);
}

#[test]
fn a_plugin_waiting_during_a_failed_zip_install_is_told_why() {
    let (done, pack) = install_with_a_plugin_waiting(&[("models/a.task", b"other")]);
    assert_eq!(done.len(), 1);
    assert!(done[0].0.contains("didn't match") && done[0].1 == "framing", "{done:?}");
    assert!(!pack.ready && pack.failed_for == "framing" && !pack.downloading);
}

#[test]
fn background_prefers_fresh_quality_then_fresh_normal_then_newest_held_mask() {
    let now = service::monotonic();
    let snapshot = |value, age| MaskSnapshot { mask: Arc::new(vec![value]), at: now - age, frame_at: now - age, seq: value as u64 };
    let max = service::MASK_MAX_AGE_S;
    for (hq_age, normal_age, expected) in [(0.0, 0.0, 1), (max * 2.0, 0.0, 2),
        (max * 2.0, max * 3.0, 1), (max * 3.0, max * 2.0, 2)] {
        let selected = preferred_mask(Some(snapshot(1, hq_age)), Some(snapshot(2, normal_age)), false).unwrap();
        assert_eq!(*selected.mask, [expected]);
    }
    assert_eq!(*preferred_mask(None, Some(snapshot(2, max * 2.0)), false).unwrap().mask, [2]);
    assert_eq!(*preferred_mask(Some(snapshot(1, max * 2.0)), None, false).unwrap().mask, [1]);
    assert!(preferred_mask(None, None, false).is_none());
}

#[test]
fn a_cutout_mask_well_behind_a_fresher_normal_one_gives_way_to_it() {
    let now = service::monotonic();
    let snapshot = |value, age| MaskSnapshot { mask: Arc::new(vec![value]), at: now - age, frame_at: now - age, seq: value as u64 };
    let max = service::MASK_MAX_AGE_S;
    let behind = service::HQ_BEHIND_S;
    // Both are within the age limit, but by age the normal one is the picture's
    assert_eq!(*preferred_mask(Some(snapshot(1, max * 0.9)), Some(snapshot(2, 0.0)), false).unwrap().mask, [2]);
    assert_eq!(*preferred_mask(Some(snapshot(1, behind * 2.0)), Some(snapshot(2, 0.0)), false).unwrap().mask, [2]);
    // A cutout a frame or so behind keeps its place: it cuts cleaner
    assert_eq!(*preferred_mask(Some(snapshot(1, behind * 0.5)), Some(snapshot(2, 0.0)), false).unwrap().mask, [1]);
    assert_eq!(*preferred_mask(Some(snapshot(1, behind * 1.5)), Some(snapshot(2, behind * 0.6)), false).unwrap().mask, [1]);
    // The normal one older than the cutout never beats it
    assert_eq!(*preferred_mask(Some(snapshot(1, 0.1)), Some(snapshot(2, 0.3)), false).unwrap().mask, [1]);
}

#[test]
fn while_the_crop_label_is_a_guess_the_newer_mask_is_used_whichever_model_made_it() {
    let now = service::monotonic();
    let snapshot = |value, age| MaskSnapshot { mask: Arc::new(vec![value]), at: now - age, frame_at: now - age, seq: value as u64 };
    let behind = service::HQ_BEHIND_S;
    // A cutout a frame behind keeps its place when the label is sure, and gives it up when it is a guess
    assert_eq!(*preferred_mask(Some(snapshot(1, behind * 0.5)), Some(snapshot(2, 0.0)), false).unwrap().mask, [1]);
    assert_eq!(*preferred_mask(Some(snapshot(1, behind * 0.5)), Some(snapshot(2, 0.0)), true).unwrap().mask, [2]);
    // And a cutout as recent as the normal one is still the cutout
    assert_eq!(*preferred_mask(Some(snapshot(1, 0.0)), Some(snapshot(2, 0.0)), true).unwrap().mask, [1]);
}

/// A cutout model that notes the size of the picture it is first handed and of the one it runs on.
struct Seen {
    handed: Arc<Mutex<Vec<(usize, usize)>>>,
    ran: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl Segmenter for Seen {
    fn segment(&mut self, frame: &Rgb, _ts: i64) -> Result<crate::service::Gray, VisionError> {
        self.ran.lock().unwrap().push((frame.w, frame.h));
        Ok(crate::service::Gray { w: frame.w, h: frame.h, data: vec![200; frame.w * frame.h] })
    }
    fn input(&self, frame: &Rgb) -> Option<Rgb> {
        self.handed.lock().unwrap().push((frame.w, frame.h));
        crate::matting::model_picture(frame)
    }
    fn blends_last(&self) -> bool {
        false
    }
}

struct SeenCutout(Arc<Mutex<Vec<(usize, usize)>>>, Arc<Mutex<Vec<(usize, usize)>>>);

impl Backend for SeenCutout {
    fn version(&self) -> String {
        "seen".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(Seen { handed: self.0.clone(), ran: self.1.clone() }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose"))
    }
}

#[test]
fn the_cutout_is_handed_a_small_picture_made_from_the_planes() {
    let (handed, ran): (Arc<Mutex<Vec<(usize, usize)>>>, Arc<Mutex<Vec<(usize, usize)>>>) = Default::default();
    let cutout = Arc::new(SeenCutout(handed.clone(), ran.clone()));
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(move || Ok(cutout.clone() as Arc<dyn Backend>)), None, scratch_dir("small-cutout"), PackInfo::default());
    h.load();
    wait(|| h.state() == READY);
    let base = yuv_frame(3840, 2160);
    let n = std::cell::Cell::new(0);
    wait(|| {
        std::thread::sleep(Duration::from_millis(5));
        n.set(n.get() + 1);
        let mut frame = base.clone();
        frame.number = n.get();
        h.person_mask_hq("a", &frame, 3840, 2160, 0);
        !ran.lock().unwrap().is_empty()
    });
    assert_eq!(handed.lock().unwrap()[0], (640, 360), "the model was handed a full size picture");
    assert_eq!(ran.lock().unwrap()[0], (640, 360));
    h.shutdown();
}

#[test]
fn the_cutouts_picture_is_already_the_size_its_model_wants() {
    // the model's own shrink then has nothing left to do, at any shape
    for (w, h) in [(3840usize, 2160usize), (1920, 1080), (1280, 720), (640, 480), (1080, 1920), (3000, 2000), (4000, 3000), (2560, 1080), (1000, 1000), (1333, 777)] {
        let (mw, mh) = ops::model_input_size(w, h, matting::input_size(w, h)).unwrap();
        let picture = Rgb::new(mw, mh, vec![0; mw * mh * 3]);
        assert!(matting::model_picture(&picture).is_none(), "{w}x{h} came out {mw}x{mh} and was shrunk again");
    }
}

#[test]
fn masks_of_the_same_frame_keep_the_cutout_while_the_crop_is_a_guess() {
    let now = service::monotonic();
    // The normal one was asked for a moment after the cutout, so it carries a later time for the same frame
    let hq = MaskSnapshot { mask: Arc::new(vec![1]), at: now - 0.02, frame_at: now - 0.02, seq: 7 };
    let normal = MaskSnapshot { mask: Arc::new(vec![2]), at: now - 0.01, frame_at: now - 0.01, seq: 7 };
    assert_eq!(*preferred_mask(Some(hq), Some(normal), true).unwrap().mask, [1]);
    // A normal mask of a newer frame still wins
    let hq = MaskSnapshot { mask: Arc::new(vec![1]), at: now - 0.12, frame_at: now - 0.12, seq: 7 };
    let normal = MaskSnapshot { mask: Arc::new(vec![2]), at: now - 0.01, frame_at: now - 0.01, seq: 8 };
    assert_eq!(*preferred_mask(Some(hq), Some(normal), true).unwrap().mask, [2]);
}

#[test]
fn still_and_power_saving_masks_skip_colour_decode() {
    use std::sync::atomic::AtomicUsize;
    let (handed, ran): (Arc<Mutex<Vec<(usize, usize)>>>, Arc<Mutex<Vec<(usize, usize)>>>) = Default::default();
    let cutout = Arc::new(SeenCutout(handed.clone(), ran.clone()));
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(move || Ok(cutout.clone() as Arc<dyn Backend>)), None, scratch_dir("still-skip-rgb"), PackInfo::default());
    h.vision().wait_for_masks();
    h.load();
    wait(|| h.state() == READY);
    let base = yuv_frame(640, 360);
    // Load the cutout backend and run one initial frame
    let first = {
        let mut f = base.clone();
        f.number = 1;
        wait(|| h.person_mask_hq("a", &f, 160, 90, 0).is_some() && !ran.lock().unwrap().is_empty());
        handed.lock().unwrap().len()
    };
    let pic_calls = Arc::new(AtomicUsize::new(0));
    let look_calls = Arc::new(AtomicUsize::new(0));
    let view = MaskView { geometry: Some(Default::default()), ..MaskView::default() };
    for seq in 2..=6u64 {
        let (f1, f2, pc, lc) = (base.clone(), base.clone(), pic_calls.clone(), look_calls.clone());
        let mask = h.vision().newest_cutout_snapshot_with_look(
            "a", seq, 160, 90,
            Some(Box::new(move || { lc.fetch_add(1, Ordering::Relaxed); people_look(&f1) })),
            Box::new(move || { pc.fetch_add(1, Ordering::Relaxed); cutout_rgb(&f2) }),
            0, view,
        );
        assert!(mask.is_some());
    }
    assert_eq!(handed.lock().unwrap().len(), first, "still frames did not call segmenter input");
    assert_eq!(pic_calls.load(Ordering::Relaxed), 0, "still frames skipped colour picture decode");
    assert_eq!(look_calls.load(Ordering::Relaxed), 5, "each still frame probed only the small grey thumb");
    h.set_power_saving(true);
    let moving = scene(640, 360, 0, 200);
    let (f1, f2, pc, lc) = (moving.clone(), moving, pic_calls.clone(), look_calls.clone());
    let mask = h.vision().newest_cutout_snapshot_with_look(
        "a", 7, 160, 90,
        Some(Box::new(move || { lc.fetch_add(1, Ordering::Relaxed); people_look(&f1) })),
        Box::new(move || { pc.fetch_add(1, Ordering::Relaxed); cutout_rgb(&f2) }),
        0, view,
    );
    assert!(mask.is_some());
    assert_eq!(pic_calls.load(Ordering::Relaxed), 0, "power saving skipped colour decode even on movement");
    assert_eq!(look_calls.load(Ordering::Relaxed), 6, "power saving still probed the grey thumb");
    h.shutdown();
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_still_mask() {
    let (handed, ran): (Arc<Mutex<Vec<(usize, usize)>>>, Arc<Mutex<Vec<(usize, usize)>>>) = Default::default();
    let cutout = Arc::new(SeenCutout(handed.clone(), ran.clone()));
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(move || Ok(cutout.clone() as Arc<dyn Backend>)), None, scratch_dir("bench-still-mask"), PackInfo::default());
    h.vision().wait_for_masks();
    h.load();
    wait(|| h.state() == READY);
    let mut base = yuv_frame(1920, 1080);
    base.number = 1;
    // Load the cutout backend synchronously first
    wait(|| h.person_mask_hq("a", &base, 480, 270, 0).is_some() && !ran.lock().unwrap().is_empty());
    let mut seq = 10u64;
    for run in 0..3 {
        let mut frame = base.clone();
        // Prime one fresh frame so the next 60 are within CUTOUT_STILL_EVERY_S
        seq += 1;
        frame.number = seq;
        let _ = h.person_mask_hq("a", &frame, 480, 270, 0);
        let ran_before = ran.lock().unwrap().len();
        let start = std::time::Instant::now();
        for _ in 0..60 {
            seq += 1;
            frame.number = seq;
            std::hint::black_box(h.person_mask_hq("a", &frame, 480, 270, 0));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / 60.0;
        let ran_after = ran.lock().unwrap().len();
        eprintln!("still_mask 1080p run {run}: {ms:.3} ms/frame (segment calls={})", ran_after - ran_before);
    }
    h.shutdown();
}

/// A cutout model whose masks are all one value, to tell its masks from another model's.
struct ConstSeg(u8);

impl Segmenter for ConstSeg {
    fn segment(&mut self, frame: &Rgb, _ts: i64) -> Result<crate::service::Gray, VisionError> {
        Ok(crate::service::Gray { w: frame.w, h: frame.h, data: vec![self.0; frame.w * frame.h] })
    }
    fn blends_last(&self) -> bool {
        false
    }
}

struct ConstCutout(u8);

impl Backend for ConstCutout {
    fn version(&self) -> String {
        "const".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(ConstSeg(self.0)))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose"))
    }
}

#[test]
fn a_still_picture_under_power_saving_keeps_the_cutouts_mask() {
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(|| Ok(Arc::new(ConstCutout(50)) as Arc<dyn Backend>)), None, scratch_dir("still-power-saving"), PackInfo::default());
    h.vision().wait_for_masks();
    h.load();
    wait(|| h.state() == READY);
    h.set_power_saving(true);
    // The normal model's masks are 200: a still picture's held cutout mask must not give way to one of them while it waits for its next run
    let seen: Vec<Option<u8>> = (1..=45u64)
        .map(|i| {
            let mut frame = scene(640, 360, 0, 0);
            frame.number = i;
            let mask = h.person_mask_hq("a", &frame, 160, 90, 0).map(|m| m[0]);
            std::thread::sleep(Duration::from_millis(33));
            mask
        })
        .collect();
    assert!(seen[5..].iter().all(|m| *m == Some(50)), "{seen:?}");
    h.shutdown();
}

#[test]
fn at_the_lightest_effort_level_the_normal_mask_stands_in_for_the_cutouts() {
    let (h, _bus, _normal) = host_with(Box::new(|_, _| Err(io::Error::other("offline"))));
    let h = h.with_cutout(Box::new(|| Ok(Arc::new(ConstCutout(50)) as Arc<dyn Backend>)), None, scratch_dir("lightest-effort"), PackInfo::default());
    h.vision().wait_for_masks();
    h.load();
    wait(|| h.state() == READY);
    let mask_at = |i: u64| {
        let mut frame = scene(640, 360, 0, 0);
        frame.number = i;
        let mask = h.person_mask_hq("a", &frame, 160, 90, 0).map(|m| m[0]);
        std::thread::sleep(Duration::from_millis(33));
        mask
    };
    wait(|| mask_at(1) == Some(50));
    h.vision().inner.effort.set_floor(telescope_core::effort::MOST);
    let seen: Vec<Option<u8>> = (2..=40u64).map(mask_at).collect();
    assert!(seen[5..].iter().all(|m| *m == Some(200)), "{seen:?}");
    h.vision().inner.effort.set_floor(telescope_core::effort::FULL);
    wait(|| mask_at(100) == Some(50));
    h.shutdown();
}

#[test]
fn a_normal_mask_late_from_a_slow_open_does_not_lead_a_cutout_of_a_newer_frame() {
    let now = service::monotonic();
    // The normal frame was taken first, the model then opened for most of a second, so its mask is made later than the cutout's of a newer frame
    let hq = MaskSnapshot { mask: Arc::new(vec![1]), at: now - 0.3, frame_at: now - 0.3, seq: 9 };
    let normal = MaskSnapshot { mask: Arc::new(vec![2]), at: now - 0.01, frame_at: now - 0.9, seq: 8 };
    assert_eq!(*preferred_mask(Some(hq), Some(normal), false).unwrap().mask, [1]);
}

#[test]
fn a_pack_from_before_the_face_finder_still_loads_after_an_update() {
    let (dir, old, _) = installed_pack("vision-older");
    let newer = PackInfo::from_json(&serde_json::json!({"linux": {"id": "f".repeat(64), "size": 1, "older": [old.id.clone().unwrap()]}}), Some("linux"));
    assert!(!newer.installed(&dir), "this version's own pack isn't there");
    assert_eq!(newer.usable(&dir).and_then(|p| p.id), old.id, "the older one it still runs is");
    let loaded = std::sync::Mutex::new(None);
    let load = |folder: &Path| -> Result<Arc<dyn Backend>, LoadError> {
        *loaded.lock().unwrap() = Some(folder.to_path_buf());
        Ok(Fake::new() as Arc<dyn Backend>)
    };
    assert!(find_pack(&dir, &newer, &load).is_ok());
    assert_eq!(loaded.lock().unwrap().as_deref(), Some(old.folder(&dir).as_path()));
    // Without it in the list, it's a pack this version can't run
    let unrelated = PackInfo::from_json(&serde_json::json!({"linux": {"id": "f".repeat(64), "size": 1}}), Some("linux"));
    assert!(unrelated.usable(&dir).is_none());
    assert!(matches!(find_pack(&dir, &unrelated, &load), Err(LoadError::Missing(_))));
}

#[test]
fn the_card_says_ready_with_an_older_pack_this_version_still_runs() {
    let (dir, old, _) = installed_pack("vision-older-card");
    let newer = PackInfo::from_json(&serde_json::json!({"linux": {"id": "f".repeat(64), "size": 1, "older": [old.id.clone().unwrap()]}}), Some("linux"));
    let (find_dir, find_info) = (dir.clone(), newer.clone());
    let find: crate::service::Find = Box::new(move || find_pack(&find_dir, &find_info, &|_| Ok(Fake::new() as Arc<dyn Backend>)));
    let h = VisionHost::with(find, None, dir, newer, Box::new(|_, _| Err(io::Error::other("offline"))));
    assert!(h.pack().ready);
    h.load();
    wait(|| h.state() == READY);
}
