//! People in the picture for the plugins that look at them: the vision pack's download (pack), MediaPipe from it (mediapipe),
//! the picture work around the models (ops) and the per-stream scheduling (service). VisionHost puts them together as core's
//! VisionService, adding the steady people tracking (core::people) and the app's one download (VisionDownload).

pub mod heads;
pub mod matting;
pub mod mediapipe;
pub mod ops;
pub mod pack;
pub mod service;

use ops::Rgb;
use pack::PackInfo;
use service::{Backend, Find, LoadError, MaskSnapshot, MaskView, Vision};
use serde_json::Value;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::bus::BusSender;
use telescope_core::effort::Effort;
use telescope_core::event::Event;
use telescope_core::frame::{Frame, MaskSpace, PixelFormat};
use telescope_core::image::{frame_rgb, small_grey, small_rgb};
use telescope_core::people::PeopleTracking;
use telescope_core::vision::{turned, PackZip, Turn, Upright, VisionPack, VisionService};

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

/// The frame as RGB at the size the high quality cutout takes (see matting::model_picture). A frame at least four times that on each side is shrunk on its planes first like model_rgb, so a 4K frame never becomes a full-size picture. A smaller one (1080p) goes whole and the model shrinks it, the old way, where the half-size colour planes don't divide into whole blocks and shrinking them is the slower route.
fn cutout_rgb(frame: &Frame) -> Option<Rgb> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let Some((mw, mh)) = ops::model_input_size(w, h, matting::input_size(w, h)).filter(|&(mw, mh)| w >= mw * 4 && h >= mh * 4) else { return rgb(frame) };
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

/// The tiny grey copy a frame is told to have moved by (ops::THUMB), followed by a coarse grid of each chroma plane (U, then V), so a change of colour at the same luma moves it too. A YUV frame gives its grey from the luma plane alone, never making its decoder's BGR.
fn people_look(frame: &Frame) -> Option<Vec<u8>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (tw, th) = ops::THUMB;
    if w < tw || h < th {
        return rgb(frame).map(|picture| ops::thumb(&picture));
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12) {
        let mut look = small_grey(frame, tw, th)?;
        if frame.format == PixelFormat::I420 && frame.data.len() >= PixelFormat::I420.size(frame.width, frame.height) {
            if let Some((_, u, v)) = frame.i420_planes() {
                look.extend(chroma_grid(u, cw, ch, 1, 0));
                look.extend(chroma_grid(v, cw, ch, 1, 0));
            }
        } else if frame.format == PixelFormat::Nv12 && frame.data.len() >= w * h + 2 * cw * ch {
            let uv = &frame.data[w * h..];
            look.extend(chroma_grid(uv, cw, ch, 2, 0));
            look.extend(chroma_grid(uv, cw, ch, 2, 1));
        }
        return Some(look);
    }
    // A frame with colour of its own is shrunk once, and its grey and chroma both come from that
    let small = small_rgb(frame, tw, th, true)?;
    let [mut look, u, v] = telescope_core::image::rgb_planes(&small);
    for (grey, p) in look.iter_mut().zip(small.as_chunks::<3>().0) {
        *grey = telescope_core::image::grey_of(p[0], p[1], p[2]);
    }
    for plane in [u, v] {
        look.extend(halved(&plane, tw, th));
    }
    Some(look)
}

/// A plane of w x h halved by the mean of each 2 x 2 block (ops::CHROMA_GRID cells when it is THUMB sized).
fn halved(plane: &[u8], w: usize, h: usize) -> Vec<u8> {
    let at = |x: usize, y: usize| plane[y * w + x] as u32;
    (0..h / 2).flat_map(|y| (0..w / 2).map(move |x| (x, y))).map(|(x, y)| ((at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1) + 2) / 4) as u8).collect()
}

/// A plane of w x h samples (`step` bytes apart, from byte `offset`) as a CHROMA_GRID of cells, each the mean of a few samples around its centre (a few thousand reads however big the frame).
fn chroma_grid(plane: &[u8], w: usize, h: usize, step: usize, offset: usize) -> Vec<u8> {
    let (gw, gh) = ops::CHROMA_GRID;
    let mut out = Vec::with_capacity(gw * gh);
    for gy in 0..gh {
        for gx in 0..gw {
            let (cx, cy) = ((gx * 2 + 1) * w / (gw * 2), (gy * 2 + 1) * h / (gh * 2));
            let (mut sum, mut n) = (0u32, 0u32);
            for y in cy.saturating_sub(2)..(cy + 2).min(h) {
                let row = &plane[y * w * step..(y + 1) * w * step];
                for x in cx.saturating_sub(2)..(cx + 2).min(w) {
                    sum += row[x * step + offset] as u32;
                    n += 1;
                }
            }
            out.push((sum / n.max(1)) as u8);
        }
    }
    out
}

/// Whether a folder with its marker still holds the pack: None when a file can't be read right now (security software scanning it), which proves nothing.
fn whole(folder: &Path, info: &PackInfo) -> Option<bool> {
    match pack::tree_hash(folder) {
        Ok(hash) => Some(Some(&hash) == info.id.as_ref()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// A pack whose files no longer match was damaged after it was checked (security software took a library, say). Its marker goes, so it counts as
/// not installed and its card offers the download again; left as it was, it would say "installed" and fail to load until the folder was removed by hand.
fn damaged(folder: &Path, what: &str) -> LoadError {
    log::warn!("The {what} in {} no longer matches what was checked; it needs to be downloaded again", folder.display());
    let _ = std::fs::remove_file(folder.join(pack::DONE));
    LoadError::Missing(format!("the {what} in {} was damaged", folder.display()))
}

/// The backend from `load` for this version's pack or an older one it still runs, hashed again first: the marker only says the files matched once, and the library is native code that runs as the user.
/// A pack that can't be used (damaged, or a file locked for a moment) leaves the next older one to be tried.
fn find_pack(dir: &Path, info: &PackInfo, load: &dyn Fn(&Path) -> Result<Arc<dyn Backend>, LoadError>) -> Result<Arc<dyn Backend>, LoadError> {
    let mut first = None;
    for pack in info.installed_packs(dir) {
        let folder = pack.folder(dir);
        let refused = match whole(&folder, &pack) {
            Some(true) => return load(&folder),
            Some(false) => damaged(&folder, "vision pack"),
            None => LoadError::Failed(service::VisionError::new("RuntimeError", "the vision pack couldn't be checked, so it wasn't loaded")),
        };
        first.get_or_insert(refused);
    }
    Err(first.unwrap_or_else(|| LoadError::Missing(format!("no vision pack in {}", info.folder(dir).display()))))
}

/// The matting download's backend, after its files are hashed again: the marker only says they matched once, and the library is native code that runs as the user.
fn find_matting(dir: &Path, info: &PackInfo) -> Result<Arc<dyn Backend>, LoadError> {
    let folder = info.folder(dir);
    if !info.installed(dir) {
        return Err(LoadError::Missing(format!("no matting download in {}", folder.display())));
    }
    match whole(&folder, info) {
        Some(true) => matting::load(&folder),
        Some(false) => Err(damaged(&folder, "matting download")),
        None => Err(LoadError::Failed(service::VisionError::new("RuntimeError", "the matting download couldn't be checked, so it wasn't loaded"))),
    }
}

impl VisionHost {
    /// MediaPipe from this version's pack in the Python app's folder (vision_pack.default_dir).
    pub fn system() -> VisionHost {
        let (dir, info) = (pack::default_dir(), PackInfo::current());
        let (find_dir, find_info) = (dir.clone(), info.clone());
        let find: Find = Box::new(move || find_pack(&find_dir, &find_info, &|folder| mediapipe::MediaPipe::load(folder).map(|b| Arc::new(b) as Arc<dyn Backend>)));
        let (there_dir, there_info) = (dir.clone(), info.clone());
        let host = VisionHost::with(find, Some(Box::new(move || there_info.usable(&there_dir).is_some())), dir, info, Box::new(pack::web));
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
        VisionHost::with_effort(Effort::shared(), find, there, dir, info, opener)
    }

    /// The same, reading how hard to work from effort instead of the app's own.
    pub fn with_effort(effort: Arc<Effort>, find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>, dir: PathBuf, info: PackInfo, opener: WebOpener) -> VisionHost {
        let bus: Arc<Mutex<Option<BusSender>>> = Arc::new(Mutex::new(None));
        let tracking = Arc::new(Mutex::new(PeopleTracking::new()));
        let (people_bus, people_tracking, state_bus) = (bus.clone(), tracking.clone(), bus.clone());
        let vision = Vision::with_effort(
            effort,
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
        VisionHost::finish_download(&shared, error);
    }

    /// The download or zip install is over: free the slot, tell every plugin that waited, and make vision look for the pack again.
    fn finish_download(shared: &Shared, error: String) {
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

    fn finish_cutout(shared: &Shared, cutout: &CutoutPack, error: String) {
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

fn slot<'a>(shared: &'a Shared, cutout: &'a Option<Arc<CutoutPack>>) -> &'a Mutex<Download> {
    match cutout {
        Some(c) => &c.download,
        None => &shared.download,
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
        VisionHost::finish_cutout(&shared, &cutout, error);
    }
}

impl VisionHost {
    fn mask_normal(&self, source_id: &str, frame: &Frame, (w, h): (usize, usize), epoch: u64, keep_late: bool) -> Option<Arc<Vec<u8>>> {
        let (look_frame, shared) = (frame.clone(), frame.clone());
        let view = MaskView { geometry: Some(frame.geometry.as_deref().copied().unwrap_or_default()), keep_late, space: MaskSpace { crop_unsure: false, ..frame.mask_space }, unsure: frame.mask_space.crop_unsure };
        self.shared.vision.newest_normal_snapshot_with_look(source_id, !keep_late, frame.number, w, h, Some(Box::new(move || people_look(&look_frame))), Box::new(move || model_rgb(&shared)), epoch, view).map(|m| m.mask)
    }

    fn mask_hq(&self, source_id: &str, frame: &Frame, size: (usize, usize), epoch: u64, keep_late: bool) -> Option<Arc<Vec<u8>>> {
        let (w, h) = size;
        let view = MaskView { geometry: Some(frame.geometry.as_deref().copied().unwrap_or_default()), keep_late, space: MaskSpace { crop_unsure: false, ..frame.mask_space }, unsure: frame.mask_space.crop_unsure };
        let (look_frame, shared) = (frame.clone(), frame.clone());
        let hq = self.shared.vision.newest_cutout_snapshot_with_look(source_id, frame.number, w, h, Some(Box::new(move || people_look(&look_frame))), Box::new(move || cutout_rgb(&shared)), epoch, view);
        let normal = |frame: &Frame| {
            let (look_frame, shared) = (frame.clone(), frame.clone());
            self.shared.vision.newest_normal_snapshot_with_look(source_id, false, frame.number, w, h, Some(Box::new(move || people_look(&look_frame))), Box::new(move || model_rgb(&shared)), epoch, view)
        };
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
        // The normal mask is asked for as soon as the cutout falls behind, so it has something newer to give
        let behind = hq_may_be_behind(view.unsure);
        let normal = (due || hq.as_ref().is_none_or(|m| m.age() > behind)).then(|| normal(frame)).flatten();
        preferred_mask(hq, normal, view.unsure).map(|m| m.mask)
    }
}

/// How far behind the normal mask the cutout's can be and still stand in for it: the cutout cuts cleaner, so a frame or so doesn't matter. Not at all
/// while the crop label is a guess, since a held mask is moved by that guess and an older one is moved further by it.
fn hq_may_be_behind(unsure: bool) -> f64 {
    if unsure { 0.0 } else { service::HQ_BEHIND_S }
}

/// The mask a frame uses out of the cutout's and the normal model's newest: by how recent each is, not by which model made it. The cutout while it
/// is within the age limit and no more than hq_may_be_behind behind a normal mask that is too; else the normal one while it is within the limit; else whichever was made last.
fn preferred_mask(hq: Option<MaskSnapshot>, normal: Option<MaskSnapshot>, unsure: bool) -> Option<MaskSnapshot> {
    let behind = hq_may_be_behind(unsure);
    let normal_leads = |hq: &MaskSnapshot| normal.as_ref().is_some_and(|n| n.fresh() && n.seq != hq.seq && n.frame_at - hq.frame_at > behind);
    if hq.as_ref().is_some_and(|h| h.fresh() && !normal_leads(h)) {
        return hq;
    }
    if normal.as_ref().is_some_and(MaskSnapshot::fresh) {
        return normal;
    }
    match (hq, normal) {
        (Some(hq), Some(normal)) => Some(if hq.at >= normal.at { hq } else { normal }),
        (hq, normal) => hq.or(normal),
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
        self.mask_normal(source_id, frame, (frame.width as usize, frame.height as usize), self.view_epoch(), false)
    }

    fn person_mask_at(&self, source_id: &str, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        self.mask_normal(source_id, frame, (w, h), self.view_epoch(), false)
    }

    fn view_epoch(&self) -> u64 {
        self.shared.vision.view_epoch()
    }

    fn person_mask_in(&self, source_id: &str, frame: &Frame, w: usize, h: usize, epoch: u64) -> Option<Arc<Vec<u8>>> {
        self.mask_normal(source_id, frame, (w, h), epoch, false)
    }

    fn want_people(&self, owner: &str, on: bool) {
        self.shared.vision.want_people(owner, on);
    }

    fn want_heads(&self, owner: &str, on: bool) {
        self.shared.vision.want_heads(owner, on);
    }

    fn wants_people(&self) -> bool {
        self.shared.vision.wants_people()
    }

    fn people_due(&self, source_id: &str) -> bool {
        self.shared.vision.people_due(source_id)
    }

    fn feed_raw(&self, source_id: &str, frame: &Frame, upright: Upright) {
        self.shared.vision.feed_raw(source_id, &mut || people_look(frame), &mut || people_rgb(frame), upright);
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
        self.mask_hq(source_id, frame, (w, h), epoch, false)
    }

    fn background_mask(&self, source_id: &str, frame: &Frame, size: (usize, usize), epoch: u64, high_quality: bool) -> Option<Arc<Vec<u8>>> {
        if high_quality { self.mask_hq(source_id, frame, size, epoch, true) } else { self.mask_normal(source_id, frame, size, epoch, true) }
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
            ready: self.shared.info.usable(&self.shared.dir).is_some(),
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

    fn install_pack_zip(&self, archive: &Path) -> PackZip {
        let Some(kind) = archive.file_name().and_then(|n| pack::pack_zip_kind(&n.to_string_lossy())) else { return PackZip::NotPack };
        let (info, dir, state, cancel) = match kind {
            pack::Kind::Vision => (self.shared.info.clone(), self.shared.dir.clone(), None, self.shared.cancel.clone()),
            pack::Kind::Matting => match self.cutout_pack() {
                Some(c) => (c.info.clone(), c.dir.clone(), Some(c.clone()), c.cancel.clone()),
                None => return PackZip::NotPack,
            },
        };
        // Check and claim in one lock, so a download or another install can't slip in between
        {
            let mut d = slot(&self.shared, &state).lock().unwrap();
            if d.running {
                return PackZip::Busy;
            }
            d.running = true;
            d.error.clear();
            d.failed_for.clear();
            d.done = 0;
            d.size = 0;
        }
        cancel.store(false, Ordering::SeqCst);
        let (shared, archive, again) = (self.shared.clone(), archive.to_path_buf(), state.clone());
        let started = std::thread::Builder::new().name("pack-install".into()).spawn(move || {
            let error = pack::install_zip(&info, &dir, &archive).err().map(|e| e.0).unwrap_or_default();
            match again {
                Some(c) => VisionHost::finish_cutout(&shared, &c, error),
                None => VisionHost::finish_download(&shared, error),
            }
        });
        if started.is_err() {
            slot(&self.shared, &state).lock().unwrap().running = false;
        }
        PackZip::Started
    }

    fn remove_pack(&self) -> Result<(), String> {
        let cutout = self.cutout_pack().map(|c| pack::remove(&c.dir, None));
        pack::remove(&self.shared.dir, None).and(cutout.unwrap_or(Ok(())))
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod host_tests;
