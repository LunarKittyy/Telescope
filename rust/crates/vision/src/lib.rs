//! People in the picture for the plugins that look at them: the vision pack's download (pack), MediaPipe from it (mediapipe),
//! the picture work around the models (ops) and the per-stream scheduling (service). VisionHost puts them together as core's
//! VisionService, adding the steady people tracking app.py does (core::people) and the app's one download (VisionDownload).

pub mod matting;
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
use telescope_core::image::{frame_rgb, small_rgb};
use telescope_core::people::PeopleTracking;
use telescope_core::vision::{turned, Turn, Upright, VisionPack, VisionService};

pub type WebOpener = Box<dyn Fn(&str, Duration) -> io::Result<Box<dyn Read>> + Send + Sync>;

#[derive(Default)]
struct Download {
    running: bool,
    /// The optional plugins that get switched on once it's done, in the order they asked.
    waiting: Vec<String>,
    error: String,
    failed_for: String,
    done: u64,
    size: u64,
}

/// The high quality cutout's download: where it unpacks and what the build knows of it (no entry: nothing to download).
struct CutoutPack {
    dir: PathBuf,
    info: PackInfo,
    download: Mutex<Download>,
    cancel: Arc<AtomicBool>,
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
    cutout: Mutex<Option<Arc<CutoutPack>>>,
    /// When each stream's normal mask was last asked for while the cutout gave the picture its masks (see person_mask_hq).
    warmed: Mutex<std::collections::HashMap<String, f64>>,
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

/// The frame as RGB at the size the segmenter takes (see ops::model_input), shrunk on its planes first so a big frame is never made into RGB at full size.
fn model_rgb(frame: &Frame) -> Option<Rgb> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let Some((mw, mh)) = ops::model_input_size(w, h, ops::MODEL_SIZE) else { return rgb(frame) };
    small_rgb(frame, mw, mh, true).map(|data| Rgb::new(mw, mh, data))
}

/// The frame as RGB at the size the pose model gets (see ops::people_size), shrunk on its planes first.
fn people_rgb(frame: &Frame) -> Option<Rgb> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (pw, ph) = ops::people_size(w, h, service::PEOPLE_WIDTH);
    if pw >= w {
        return rgb(frame);
    }
    small_rgb(frame, pw, ph, true).map(|data| Rgb::new(pw, ph, data))
}

/// The pack's backend from `load`: the marker only says the files matched once, and the library is native code that runs as the user.
fn find_pack(dir: &Path, info: &PackInfo, load: &dyn Fn(&Path) -> Result<Arc<dyn Backend>, LoadError>) -> Result<Arc<dyn Backend>, LoadError> {
    let folder = info.folder(dir);
    if !info.installed(dir) {
        return Err(LoadError::Missing(format!("no vision pack in {}", folder.display())));
    }
    if pack::tree_hash(&folder).ok() != info.id {
        return Err(LoadError::Failed(service::VisionError::new("RuntimeError", "the vision pack has changed since it was checked, so it wasn't loaded")));
    }
    load(&folder)
}

/// The matting download's backend, after its files are hashed again: the marker only says they matched once, and the library is native code that runs as the user.
fn find_matting(dir: &Path, info: &PackInfo) -> Result<Arc<dyn Backend>, LoadError> {
    if !info.installed(dir) {
        return Err(LoadError::Missing(format!("no matting download in {}", info.folder(dir).display())));
    }
    let folder = info.folder(dir);
    if pack::tree_hash(&folder).ok() != info.id {
        return Err(LoadError::Failed(service::VisionError::new("RuntimeError", "the matting download has changed since it was checked, so it wasn't loaded")));
    }
    matting::load(&folder)
}

impl VisionHost {
    /// MediaPipe from this version's pack in the Python app's folder (vision_pack.default_dir).
    pub fn system() -> VisionHost {
        let (dir, info) = (pack::default_dir(), PackInfo::current());
        let (find_dir, find_info) = (dir.clone(), info.clone());
        let find: Find = Box::new(move || find_pack(&find_dir, &find_info, &|folder| mediapipe::MediaPipe::load(folder).map(|b| Arc::new(b) as Arc<dyn Backend>)));
        let (there_dir, there_info) = (dir.clone(), info.clone());
        let host = VisionHost::with(find, Some(Box::new(move || there_info.installed(&there_dir))), dir, info, Box::new(pack::web));
        let (cutout_dir, cutout_info) = (pack::matting_dir(), PackInfo::current_matting());
        let (find_dir, find_info) = (cutout_dir.clone(), cutout_info.clone());
        let find: Find = Box::new(move || find_matting(&find_dir, &find_info));
        let (there_dir, there_info) = (cutout_dir.clone(), cutout_info.clone());
        host.with_cutout(find, Some(Box::new(move || there_info.installed(&there_dir))), cutout_dir, cutout_info)
    }

    /// Gives the host the high quality cutout: find loads it, there says whether its download is unpacked, and dir and info are where the
    /// download goes and what it must match. Without this the cutout is missing and masks stay the normal ones.
    pub fn with_cutout(self, find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>, dir: PathBuf, info: PackInfo) -> VisionHost {
        let state_bus = self.shared.bus.clone();
        self.shared.vision.set_cutout(find, there, Box::new(move |state| emit(&state_bus, Event::MattingStateChanged { state: state.to_string() })));
        *self.shared.cutout.lock().unwrap() = Some(Arc::new(CutoutPack { dir, info, download: Mutex::new(Download::default()), cancel: Arc::new(AtomicBool::new(false)) }));
        self
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
            cutout: Mutex::new(None), warmed: Mutex::new(Default::default()),
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
        let names = {
            let mut d = shared.download.lock().unwrap();
            d.running = false;
            let names = std::mem::take(&mut d.waiting);
            if !error.is_empty() {
                d.error = error.clone();
                d.failed_for = names.first().cloned().unwrap_or_default();
            }
            names
        };
        if error.is_empty() {
            shared.vision.pack_downloaded();
        }
        // One finish per plugin that asked, so each is switched on; one with no name when nothing waited
        if names.is_empty() {
            emit(&shared.bus, Event::VisionDownloadFinished { error: error.clone(), switch_on: String::new() });
        }
        for name in names {
            emit(&shared.bus, Event::VisionDownloadFinished { error: error.clone(), switch_on: name });
        }
    }
}

impl VisionHost {
    fn cutout_pack(&self) -> Option<Arc<CutoutPack>> {
        self.shared.cutout.lock().unwrap().clone()
    }

    fn run_cutout_download(shared: Arc<Shared>, cutout: Arc<CutoutPack>) {
        let progress_bus = shared.bus.clone();
        let progress_cutout = cutout.clone();
        let mut progress = move |done: u64, size: u64| {
            {
                let mut d = progress_cutout.download.lock().unwrap();
                d.done = done;
                d.size = size;
            }
            emit(&progress_bus, Event::VisionDownloadProgress { done, size });
        };
        let cancel = cutout.cancel.clone();
        let opener = |url: &str, t: Duration| (shared.opener)(url, t);
        let result = pack::download(&cutout.info, &cutout.dir, &mut progress, &|| cancel.load(Ordering::SeqCst), &opener, None);
        let error = result.err().map(|e| e.0).unwrap_or_default();
        {
            let mut d = cutout.download.lock().unwrap();
            d.running = false;
            d.error = error.clone();
        }
        if error.is_empty() {
            shared.vision.cutout_downloaded();
        }
        emit(&shared.bus, Event::VisionDownloadFinished { error, switch_on: String::new() });
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
        let shared = frame.clone();
        self.shared.vision.newest_mask(source_id, frame.number, frame.width as usize, frame.height as usize, Box::new(move || model_rgb(&shared)))
    }

    fn person_mask_at(&self, source_id: &str, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let shared = frame.clone();
        self.shared.vision.newest_mask(source_id, frame.number, w, h, Box::new(move || model_rgb(&shared)))
    }

    fn view_epoch(&self) -> u64 {
        self.shared.vision.view_epoch()
    }

    fn person_mask_in(&self, source_id: &str, frame: &Frame, w: usize, h: usize, epoch: u64) -> Option<Arc<Vec<u8>>> {
        let shared = frame.clone();
        self.shared.vision.newest_mask_in(source_id, frame.number, w, h, Box::new(move || model_rgb(&shared)), epoch)
    }

    fn want_people(&self, owner: &str, on: bool) {
        self.shared.vision.want_people(owner, on);
    }

    fn wants_people(&self) -> bool {
        self.shared.vision.wants_people()
    }

    fn feed_raw(&self, source_id: &str, frame: &Frame, upright: Upright) {
        self.shared.vision.feed_raw(source_id, &mut || people_rgb(frame), upright);
    }

    fn tracked_people(&self, source_id: &str) -> (Vec<Value>, u32) {
        let (tracks, count) = self.shared.tracking.lock().unwrap().last(source_id, service::monotonic());
        (tracks.iter().map(|t| t.to_value()).collect(), count)
    }

    fn view_turned(&self, old: Turn, new: Turn) {
        if old != new {
            self.shared.tracking.lock().unwrap().remap(&turned(old, new));
            // A mask kept from before says where people were in the picture as it was
            self.shared.vision.drop_masks();
        }
    }

    fn drop_stream(&self, source_id: &str) {
        self.shared.tracking.lock().unwrap().drop_stream(source_id);
        self.shared.vision.drop_stream(source_id);
        self.shared.warmed.lock().unwrap().remove(source_id);
    }

    fn set_power_saving(&self, on: bool) {
        self.shared.vision.set_power_saving(on);
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        self.shared.vision.diagnostics()
    }

    fn shutdown(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
        if let Some(cutout) = self.cutout_pack() {
            cutout.cancel.store(true, Ordering::SeqCst);
        }
        self.shared.vision.shutdown();
        *self.shared.bus.lock().unwrap() = None;
    }

    fn download_failed(&self, error: &str, plugin: &str) {
        let mut d = self.shared.download.lock().unwrap();
        d.error = error.to_string();
        d.failed_for = plugin.to_string();
    }

    fn person_mask_hq(&self, source_id: &str, frame: &Frame, w: usize, h: usize, epoch: u64) -> Option<Arc<Vec<u8>>> {
        let shared = frame.clone();
        let hq = self.shared.vision.newest_cutout_in(source_id, frame.number, w, h, Box::new(move || rgb(&shared)), epoch);
        let normal = |frame: &Frame| {
            let shared = frame.clone();
            self.shared.vision.newest_mask_nowait_in(source_id, frame.number, w, h, Box::new(move || model_rgb(&shared)), epoch)
        };
        // Not downloaded, loading, failed or late: the normal mask, so the picture never goes without one it could have had
        let Some(hq) = hq else { return normal(frame) };
        // While the cutout gives the masks the normal model still gets a frame now and then, so it has a fresh mask to fall back on the moment the cutout is late
        let now = service::monotonic();
        let due = {
            let mut warmed = self.shared.warmed.lock().unwrap();
            let last = warmed.entry(source_id.to_string()).or_insert(f64::NEG_INFINITY);
            let due = now - *last >= service::MASK_MAX_AGE_S / 2.0;
            if due {
                *last = now;
            }
            due
        };
        if due {
            normal(frame);
        }
        Some(hq)
    }

    fn matting_state(&self) -> String {
        self.shared.vision.cutout_state().to_string()
    }

    fn matting_pack(&self) -> VisionPack {
        let Some(cutout) = self.cutout_pack() else { return VisionPack::default() };
        let d = cutout.download.lock().unwrap();
        let size = cutout.info.download_size();
        VisionPack {
            ready: cutout.info.installed(&cutout.dir),
            available: cutout.info.asset_name().is_some(),
            downloading: d.running,
            size_mb: ((size as f64 / 1e6).round_ties_even() as u64).max(1),
            done: d.done,
            size: d.size,
            error: d.error.clone(),
            failed_for: String::new(),
        }
    }

    fn download_matting(&self) {
        let Some(cutout) = self.cutout_pack() else { return };
        {
            let mut d = cutout.download.lock().unwrap();
            d.error.clear();
            if d.running {
                return;
            }
            d.running = true;
            d.done = 0;
            d.size = 0;
        }
        cutout.cancel.store(false, Ordering::SeqCst);
        let (shared, again) = (self.shared.clone(), cutout.clone());
        if std::thread::Builder::new().name("matting-download".into()).spawn(move || VisionHost::run_cutout_download(shared, again)).is_err() {
            cutout.download.lock().unwrap().running = false;
        }
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
        if !switch_on.is_empty() && !d.waiting.iter().any(|n| n == switch_on) {
            d.waiting.push(switch_on.to_string());
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

    fn remove_pack(&self) -> Result<(), String> {
        let cutout = self.cutout_pack().map(|c| pack::remove(&c.dir, None));
        pack::remove(&self.shared.dir, None).and(cutout.unwrap_or(Ok(())))
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod host_tests;
