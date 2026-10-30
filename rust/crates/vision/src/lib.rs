//! People in the picture for the plugins that look at them: the vision pack's download (pack), MediaPipe from it (mediapipe),
//! the picture work around the models (ops) and the per-stream scheduling (service). VisionHost puts them together as core's
//! VisionService, adding the steady people tracking app.py does (core::people) and the app's one download (VisionDownload).

pub mod mediapipe;
pub mod ops;
pub mod pack;
pub mod service;

use ops::Rgb;
use pack::PackInfo;
use service::{Backend, Find, LoadError, Vision};
use serde_json::Value;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::bus::BusSender;
use telescope_core::event::Event;
use telescope_core::frame::Frame;
use telescope_core::image::frame_rgb;
use telescope_core::people::PeopleTracking;
use telescope_core::vision::{turned, Turn, Upright, VisionPack, VisionService};

pub type WebOpener = Box<dyn Fn(&str, Duration) -> io::Result<Box<dyn Read>> + Send + Sync>;

#[derive(Default)]
struct Download {
    running: bool,
    /// The optional plugin that gets switched on once it's done.
    waiting: String,
    error: String,
    failed_for: String,
    done: u64,
    size: u64,
}

struct Shared {
    vision: Vision,
    bus: Arc<Mutex<Option<BusSender>>>,
    tracking: Arc<Mutex<PeopleTracking>>,
    dir: PathBuf,
    info: PackInfo,
    download: Mutex<Download>,
    cancel: Arc<AtomicBool>,
    opener: WebOpener,
}

/// The vision service the binary hands the shell.
#[derive(Clone)]
pub struct VisionHost {
    shared: Arc<Shared>,
}

fn emit(bus: &Mutex<Option<BusSender>>, event: Event) {
    if let Some(bus) = bus.lock().unwrap().as_ref() {
        bus.emit(event);
    }
}

/// The frame as RGB for the models; None for a format that has no picture.
fn rgb(frame: &Frame) -> Option<Rgb> {
    frame_rgb(frame).map(|data| Rgb::new(frame.width as usize, frame.height as usize, data))
}

impl VisionHost {
    /// MediaPipe from this version's pack in the Python app's folder (vision_pack.default_dir).
    pub fn system() -> VisionHost {
        let (dir, info) = (pack::default_dir(), PackInfo::current());
        let (find_dir, find_info) = (dir.clone(), info.clone());
        let find: Find = Box::new(move || {
            if !find_info.installed(&find_dir) {
                return Err(LoadError::Missing(format!("no vision pack in {}", find_info.folder(&find_dir).display())));
            }
            mediapipe::MediaPipe::load(&find_info.folder(&find_dir)).map(|b| Arc::new(b) as Arc<dyn Backend>)
        });
        let (there_dir, there_info) = (dir.clone(), info.clone());
        VisionHost::with(find, Some(Box::new(move || there_info.installed(&there_dir))), dir, info, Box::new(pack::web))
    }

    /// The parts given: find makes the backend, there says whether a pack is there without loading it.
    pub fn with(find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>, dir: PathBuf, info: PackInfo, opener: WebOpener) -> VisionHost {
        let bus: Arc<Mutex<Option<BusSender>>> = Arc::new(Mutex::new(None));
        let tracking = Arc::new(Mutex::new(PeopleTracking::new()));
        let (people_bus, people_tracking, state_bus) = (bus.clone(), tracking.clone(), bus.clone());
        let vision = Vision::new(
            Box::new(move |key, people| {
                emit(&people_bus, Event::PeopleSeen { source_id: key.to_string(), people: people.iter().map(|p| p.to_value()).collect() });
                let (tracks, count) = people_tracking.lock().unwrap().update(key, &people, service::monotonic());
                emit(&people_bus, Event::PeopleTracked { source_id: key.to_string(), tracks: tracks.iter().map(|t| t.to_value()).collect(), count });
            }),
            Box::new(move |state| emit(&state_bus, Event::VisionStateChanged { state: state.to_string() })),
            find,
            there,
        );
        VisionHost { shared: Arc::new(Shared {
            vision, bus, tracking, dir, info, download: Mutex::new(Download::default()), cancel: Arc::new(AtomicBool::new(false)), opener,
        }) }
    }

    /// The scheduler underneath, for tests and diagnostics.
    pub fn vision(&self) -> &Vision {
        &self.shared.vision
    }

    pub fn vision_dir(&self) -> &Path {
        &self.shared.dir
    }

    fn run_download(shared: Arc<Shared>) {
        let progress_bus = shared.bus.clone();
        let progress_shared = shared.clone();
        let mut progress = move |done: u64, size: u64| {
            {
                let mut d = progress_shared.download.lock().unwrap();
                d.done = done;
                d.size = size;
            }
            emit(&progress_bus, Event::VisionDownloadProgress { done, size });
        };
        let cancel = shared.cancel.clone();
        let opener = |url: &str, t: Duration| (shared.opener)(url, t);
        let result = pack::download(&shared.info, &shared.dir, &mut progress, &|| cancel.load(Ordering::SeqCst), &opener, None);
        let error = result.err().map(|e| e.0).unwrap_or_default();
        let name = {
            let mut d = shared.download.lock().unwrap();
            d.running = false;
            let name = std::mem::take(&mut d.waiting);
            if !error.is_empty() {
                d.error = error.clone();
                d.failed_for = name.clone();
            }
            name
        };
        if error.is_empty() {
            shared.vision.pack_downloaded();
        }
        emit(&shared.bus, Event::VisionDownloadFinished { error, switch_on: name });
    }
}

impl VisionService for VisionHost {
    fn attach(&self, bus: BusSender) {
        *self.shared.bus.lock().unwrap() = Some(bus);
    }

    fn state(&self) -> String {
        self.shared.vision.state().to_string()
    }

    fn load(&self) {
        self.shared.vision.load();
    }

    fn person_mask(&self, source_id: &str, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.shared.vision.person_mask(source_id, frame.number, frame.width as usize, frame.height as usize, &mut || rgb(frame))
    }

    fn want_people(&self, owner: &str, on: bool) {
        self.shared.vision.want_people(owner, on);
    }

    fn wants_people(&self) -> bool {
        self.shared.vision.wants_people()
    }

    fn feed_raw(&self, source_id: &str, frame: &Frame, upright: Upright) {
        self.shared.vision.feed_raw(source_id, &mut || rgb(frame), upright);
    }

    fn tracked_people(&self, source_id: &str) -> (Vec<Value>, u32) {
        let (tracks, count) = self.shared.tracking.lock().unwrap().last(source_id, service::monotonic());
        (tracks.iter().map(|t| t.to_value()).collect(), count)
    }

    fn view_turned(&self, old: Turn, new: Turn) {
        if old != new {
            self.shared.tracking.lock().unwrap().remap(&turned(old, new));
        }
    }

    fn drop_stream(&self, source_id: &str) {
        self.shared.tracking.lock().unwrap().drop_stream(source_id);
        self.shared.vision.drop_stream(source_id);
    }

    fn set_power_saving(&self, on: bool) {
        self.shared.vision.set_power_saving(on);
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        self.shared.vision.diagnostics()
    }

    fn shutdown(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
        self.shared.vision.shutdown();
        *self.shared.bus.lock().unwrap() = None;
    }

    fn pack(&self) -> VisionPack {
        let d = self.shared.download.lock().unwrap();
        let size = self.shared.info.download_size();
        VisionPack {
            ready: self.shared.info.installed(&self.shared.dir),
            available: self.shared.info.asset_name().is_some(),
            downloading: d.running,
            size_mb: ((size as f64 / 1e6).round_ties_even() as u64).max(1),
            done: d.done,
            size: d.size,
            error: d.error.clone(),
            failed_for: d.failed_for.clone(),
        }
    }

    fn download(&self, switch_on: &str) {
        let mut d = self.shared.download.lock().unwrap();
        if !switch_on.is_empty() {
            d.waiting = switch_on.to_string();
        }
        d.error.clear();
        d.failed_for.clear();
        if d.running {
            return;
        }
        d.running = true;
        d.done = 0;
        d.size = 0;
        drop(d);
        self.shared.cancel.store(false, Ordering::SeqCst);
        let shared = self.shared.clone();
        if std::thread::Builder::new().name("vision-download".into()).spawn(move || VisionHost::run_download(shared)).is_err() {
            self.shared.download.lock().unwrap().running = false;
        }
    }

    fn cancel_download(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }

    fn remove_pack(&self) {
        pack::remove(&self.shared.dir, None);
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod host_tests;
