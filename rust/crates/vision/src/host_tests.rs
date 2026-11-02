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
