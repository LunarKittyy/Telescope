//! VisionHost: the download the cards share (vision_download.py) and the people app.py tracks, on the bus.

use super::*;
use crate::service::tests::{wait, Fake};
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
    let h = VisionHost::with(find, None, tmp.join("vision"), info, opener);
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
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = yuv_frame(w, h);
        let (pw, ph) = ops::people_size(w, h, service::PEOPLE_WIDTH);
        let old = measure(&mut || { let full = rgb(&frame).unwrap(); crate::ops::model_input(&full, ops::MODEL_SIZE).unwrap(); });
        let new = measure(&mut || { model_rgb(&frame).unwrap(); });
        let old_people = measure(&mut || { rgb(&frame).unwrap().area(pw, ph); });
        let new_people = measure(&mut || { people_rgb(&frame).unwrap(); });
        println!("vision pictures {w}x{h} (wall ms, cpu ms): segmenter input old {old:.2?} new {new:.2?}; people input old {old_people:.2?} new {new_people:.2?}");
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
    assert!(dir.join(&h.cutout_pack().unwrap().info.pack_id().unwrap()).join("models/rvm.onnx").is_file());
    // Remove download deletes both
    h.remove_pack().unwrap();
    assert!(!h.matting_pack().ready);
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
        Err(LoadError::Failed(e)) => assert!(e.message.contains("changed since"), "{}", e.message),
        Err(_) => panic!("the swap went unnoticed: the library was handed to the loader"),
        Ok(_) => panic!("loaded"),
    }
}
