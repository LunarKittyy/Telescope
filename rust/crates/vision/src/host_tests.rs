//! VisionHost: the download the cards share (vision_download.py) and the people app.py tracks, on the bus.

use super::*;
use crate::service::tests::{wait, Fake};
use std::io::Cursor;
use telescope_core::bus::EventBus;
use telescope_core::people::Track;
use telescope_core::testing::scratch_dir;
use telescope_core::vision::READY;

fn host(opener: WebOpener) -> (VisionHost, EventBus) {
    let (h, bus, _b) = host_with(opener);
    (h, bus)
}

fn host_with(opener: WebOpener) -> (VisionHost, EventBus, Arc<Fake>) {
    let backend = Fake::new();
    let fake = backend.clone();
    let files: &[(&str, &[u8])] = &[("models/a.task", b"model")];
    let tmp = scratch_dir("vision-host");
    let folder = tmp.join("expected");
    std::fs::create_dir_all(folder.join("models")).unwrap();
    std::fs::write(folder.join("models/a.task"), files[0].1).unwrap();
    let tree = pack::tree_hash(&folder).unwrap();
    let info = PackInfo::from_json(&serde_json::json!({"linux": {"id": tree, "size": 100_000}}), Some("linux"));
    let h = VisionHost::with(Box::new(move || Ok(backend.clone() as Arc<dyn Backend>)), None, tmp.join("vision"), info, opener);
    let bus = EventBus::new();
    h.attach(bus.sender());
    (h, bus, fake)
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
    *fake.detect.lock().unwrap() = Arc::new(|| vec![telescope_core::pose::Person::new([("nose", 0.5, 0.3, 0.9), ("left_hip", 0.45, 0.7, 0.9)])]);
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
    let (tracks, count) = h.tracked_people("phone");
    assert_eq!(count, 1);
    let face = Track::from_value(&tracks[0]).unwrap().face.unwrap();
    assert!((face.0 - 0.5).abs() < 1e-9);
    h.view_turned(Turn::default(), Turn { flip_h: true, ..Turn::default() });
    assert_eq!(h.tracked_people("phone").1, 1);
    h.drop_stream("phone");
    assert_eq!(h.tracked_people("phone"), (vec![], 0));
    h.shutdown();
}
