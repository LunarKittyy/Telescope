//! The Wait screen plugin (plugins/wait_screen.py): what the virtual camera shows while nothing streams, and whether an app is
//! reading it. The picture itself is drawn and held on the camera by wait_screen.rs; this decides when, at what size and which
//! picture, and holds the settings and the card.
//!
//! Holds the camera from app start to quit except while a stream has it (a stream with the camera off doesn't). The size follows
//! the stream (the Setup canvas, or the size the last stream opened at) so a call that's already watching keeps working when the
//! phone takes over. The extra cameras Add camera sets up show it too while nothing streams to them. Another plugin can also ask
//! for it in place of the shown stream's picture (cover_requested on the bus).

use telescope_core::i18n::Fields;
use crate::convert::rgb_to_i420;
use crate::wait_screen::{self, Loader, Rgb, DEFAULT_SIZE};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::output::{Held, Outputs, WaitScreen};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "wait_screen";
const UI: &str = include_str!("../ui/wait_screen.slint");
/// The picture in the card is this wide, and as tall as the camera's shape makes it.
const PREVIEW_W: u32 = 384;
/// The copy of the chosen image, next to the config.
const KEPT_PREFIX: &str = "wait_screen-";
/// A stand-in that couldn't be made is tried again this long after, not on every frame.
const RETRY: Duration = Duration::from_secs(10);
/// The picture in the card, when it's an animation, moves on.
const TICK: &str = "preview-tick";
/// The formats the pictures can be loaded from, as the file dialog lists them: the ones Qt reads in the Python app that this can too.
pub(crate) const PICKER_FORMATS: &str = "*.bmp *.gif *.ico *.jfif *.jpeg *.jpg *.pbm *.pgm *.png *.ppm *.tga *.webp";

// ── In place of the stream's picture ──────────────────────────────────

/// What a made stand-in is for: a picture at a size, in the format the stream's frames have, mirrored or not.
#[derive(Clone, Debug, PartialEq)]
struct CoverKey {
    size: (u32, u32),
    format: PixelFormat,
    image: Option<String>,
    mirror: bool,
}

/// The pictures of one stand-in with how long each shows, and how long they take all together.
struct Cover {
    frames: Vec<(Frame, f64)>,
    total: f64,
}

impl Cover {
    /// The picture showing `seconds` into the animation, which repeats.
    fn at(&self, seconds: f64) -> &Frame {
        let mut at = if self.total > 0.0 { seconds % self.total } else { 0.0 };
        for (frame, shown) in &self.frames {
            if at < *shown {
                return frame;
            }
            at -= shown;
        }
        &self.frames[self.frames.len() - 1].0
    }
}

#[derive(Default)]
struct CoverCache {
    /// The stand-ins made, the newest last: this picture's last two sizes are kept.
    made: Vec<(CoverKey, Arc<Cover>)>,
    /// Keys a thread is making pictures for.
    loading: Vec<CoverKey>,
    /// When each key's pictures last couldn't be made.
    failed: Vec<(CoverKey, Instant)>,
}

/// An RGB picture as a frame in the stream's format and mirrored or not; None for a format nothing can be drawn in.
fn picture_in(rgb: &Rgb, format: PixelFormat, mirror: bool) -> Option<Frame> {
    Some(match format {
        PixelFormat::I420 => rgb_to_i420(&rgb.data, rgb.width, rgb.height, 3, mirror),
        PixelFormat::Nv12 => {
            let i420 = rgb_to_i420(&rgb.data, rgb.width, rgb.height, 3, mirror);
            let luma = rgb.width as usize * rgb.height as usize;
            let (y, chroma) = i420.data.split_at(luma);
            let (u, v) = chroma.split_at(chroma.len() / 2);
            let mut data = y.to_vec();
            data.extend(u.iter().zip(v).flat_map(|(u, v)| [*u, *v]));
            Frame::new(rgb.width, rgb.height, PixelFormat::Nv12, data)
        }
        PixelFormat::Rgba => Frame::new(rgb.width, rgb.height, PixelFormat::Rgba, wait_screen::rgba(rgb, mirror)),
        PixelFormat::Bgra => {
            let mut data = wait_screen::rgba(rgb, mirror);
            data.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
            Frame::new(rgb.width, rgb.height, PixelFormat::Bgra, data)
        }
        _ => return None,
    })
}

/// The stand-ins, made on threads of their own: a long animation takes a while to load, and the stream never waits for it.
pub struct Covers {
    loader: Loader,
    cache: Mutex<CoverCache>,
}

impl Covers {
    fn new(loader: Loader) -> Arc<Covers> {
        Arc::new(Covers { loader, cache: Mutex::new(CoverCache::default()) })
    }

    fn find(&self, key: &CoverKey) -> Option<Arc<Cover>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).made.iter().find(|(k, _)| k == key).map(|(_, c)| c.clone())
    }

    /// Starts making the pictures for key, unless they are, or are being, or couldn't be a moment ago.
    fn make(self: &Arc<Self>, key: CoverKey) {
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.made.iter().any(|(k, _)| *k == key) || cache.loading.contains(&key) {
                return;
            }
            if cache.failed.iter().any(|(k, at)| *k == key && at.elapsed() < RETRY) {
                return;
            }
            cache.loading.push(key.clone());
        }
        let covers = self.clone();
        let spawned = std::thread::Builder::new().name("wait-screen-cover".into()).spawn({
            let key = key.clone();
            move || covers.load(key)
        });
        if let Err(e) = spawned {
            log::warn!("Couldn't start making the wait screen for the stream: {e}");
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.loading.retain(|k| *k != key);
            cache.failed.push((key, Instant::now()));
        }
    }

    fn load(&self, key: CoverKey) {
        let (w, h) = key.size;
        let mut frames = Vec::new();
        match (self.loader)(key.image.as_deref(), w, h) {
            Ok(pictures) => {
                frames = pictures.iter().filter_map(|(rgb, s)| picture_in(rgb, key.format, key.mirror).map(|f| (f, s.max(0.01)))).collect();
                if frames.is_empty() {
                    log::warn!("The wait screen for the stream came out empty");
                }
            }
            Err(e) => log::error!("Couldn't make the wait screen for the stream: {e}"),
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if frames.is_empty() {
            cache.failed.retain(|(k, _)| *k != key);
            cache.failed.push((key.clone(), Instant::now()));
        } else {
            let total = frames.iter().map(|(_, s)| s).sum();
            // This picture's other sizes stay for a moment (a stream that changes size comes back), the rest go
            cache.made.retain(|(k, _)| k.image == key.image && k.mirror == key.mirror && *k != key);
            cache.made.push((key.clone(), Arc::new(Cover { frames, total })));
            let drop = cache.made.len().saturating_sub(2);
            cache.made.drain(..drop);
            cache.failed.retain(|(k, _)| *k != key);
        }
        cache.loading.retain(|k| *k != key);
    }

    #[cfg(test)]
    fn loading(&self) -> bool {
        !self.cache.lock().unwrap().loading.is_empty()
    }
}

/// The wait screen at the stream's size, as the virtual camera shows it (animations play); black while it's being made.
struct CoverStep {
    covers: Arc<Covers>,
    image: Option<String>,
    mirror: bool,
    black: Option<Frame>,
}

impl CpuStep for CoverStep {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let size = (frame.width, frame.height);
        let key = CoverKey { size, format: frame.format, image: self.image.clone(), mirror: self.mirror };
        let picture = match self.covers.find(&key) {
            Some(cover) => cover.at(monotonic_ns() as f64 / 1e9).clone(),
            None => {
                self.covers.make(key);
                match self.black.as_ref().filter(|b| (b.width, b.height, b.format) == (frame.width, frame.height, frame.format)) {
                    Some(black) => black.clone(),
                    None => {
                        let zeros = Rgb { width: frame.width, height: frame.height, data: vec![0; frame.width as usize * frame.height as usize * 3] };
                        let Some(black) = picture_in(&zeros, frame.format, false) else { return };
                        self.black = Some(black.clone());
                        black
                    }
                }
            }
        };
        let (number, time_ns) = (frame.number, frame.time_ns);
        *frame = picture;
        frame.number = number;
        frame.time_ns = time_ns;
    }
}

// ── The plugin ────────────────────────────────────────────────────────

/// A message for an io error as the system words it ("No space left on device"), without Rust's "(os error 28)".
fn os_message(e: &std::io::Error) -> String {
    telescope_core::system::os_words(e)
}

/// A name nobody else will pick: 16 random hex digits.
fn random_hex() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    format!("{:016x}", hasher.finish())
}

type Copier = Box<dyn Fn(&Path, &Path) -> std::io::Result<()> + Send>;

pub struct WaitScreenPlugin {
    outputs: Arc<dyn Outputs>,
    covers: Arc<Covers>,
    /// The folder the chosen image is kept in: next to the config.
    folder: Box<dyn Fn() -> PathBuf + Send>,
    /// Copies a file (fs::copy), which a test can make fail.
    copy: Copier,
    screen: Option<Box<dyn WaitScreen>>,
    /// The extra cameras' screens, kept once made (each registers for driver reloads).
    extras: BTreeMap<u32, Box<dyn WaitScreen>>,
    /// The extra cameras nothing streams to.
    idle: Vec<u32>,
    /// Nothing streams to the main camera while others stream to extra ones.
    main_idle: bool,
    watch: Option<Box<dyn Held>>,
    /// An app reads the camera, as the watch last said.
    watched: bool,
    image_path: Option<String>,
    mirror: bool,
    last_size: Option<(u32, u32)>,
    shut: bool,
    /// Who asked for it in place of the stream's picture.
    asked: BTreeSet<String>,
    covering: bool,
    /// The file dialog open, if one is.
    picking: Option<RequestId>,
    /// An animation in the card: its pictures, which one shows and which round of ticks this is.
    animation: Vec<(Rgb, f64)>,
    shown_frame: usize,
}

impl WaitScreenPlugin {
    pub fn new(outputs: Arc<dyn Outputs>) -> WaitScreenPlugin {
        let loader: Loader = Arc::new(|path, w, h| Ok(wait_screen::load_frames(path, w, h)));
        WaitScreenPlugin::with_loader(outputs, loader)
    }

    /// loader makes the stand-in's pictures for (path, width, height).
    pub fn with_loader(outputs: Arc<dyn Outputs>, loader: Loader) -> WaitScreenPlugin {
        WaitScreenPlugin {
            outputs,
            covers: Covers::new(loader),
            folder: Box::new(|| telescope_core::config::config_path().parent().map(Path::to_path_buf).unwrap_or_default()),
            copy: Box::new(|from, to| std::fs::copy(from, to).map(|_| ())),
            screen: None,
            extras: BTreeMap::new(),
            idle: Vec::new(),
            main_idle: false,
            watch: None,
            watched: false,
            image_path: None,
            mirror: false,
            last_size: None,
            shut: false,
            asked: BTreeSet::new(),
            covering: false,
            picking: None,
            animation: Vec::new(),
            shown_frame: 0,
        }
    }

    /// Where the chosen image is kept, which isn't always the config's folder (tests).
    pub fn keeping_in(mut self, folder: impl Fn() -> PathBuf + Send + 'static) -> WaitScreenPlugin {
        self.folder = Box::new(folder);
        self
    }

    /// How a file is copied into the folder (tests make it fail).
    pub fn copying_with(mut self, copy: impl Fn(&Path, &Path) -> std::io::Result<()> + Send + 'static) -> WaitScreenPlugin {
        self.copy = Box::new(copy);
        self
    }

    pub fn image_path(&self) -> Option<&str> {
        self.image_path.as_deref()
    }

    pub fn mirror(&self) -> bool {
        self.mirror
    }

    // ── Holding the camera ────────────────────────────────────────────

    /// The size the stream opens the camera at: the Setup canvas, else the size the last one opened at.
    pub fn size(&self, host: &dyn Host) -> (u32, u32) {
        match host.canvas_size() {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => self.last_size.unwrap_or(DEFAULT_SIZE),
        }
    }

    fn show(&mut self, host: &mut dyn Host) {
        if self.shut {
            return;
        }
        self.show_extras(host);
        if host.is_streaming() && host.is_camera_on() && !self.main_idle {
            return;
        }
        let (w, h) = self.size(host);
        if let Some(screen) = &self.screen {
            screen.show(w, h, self.image_path.as_deref(), self.mirror);
        }
    }

    fn on_idle_outputs(&mut self, host: &mut dyn Host, slots: &[u32]) {
        self.idle = slots.iter().copied().filter(|s| *s != 0).collect();
        for (slot, screen) in &self.extras {
            if !self.idle.contains(slot) {
                screen.stop();
            }
        }
        let was_idle = std::mem::replace(&mut self.main_idle, slots.contains(&0));
        if self.main_idle && !was_idle {
            self.show(host);
        } else {
            self.show_extras(host);
        }
    }

    fn show_extras(&mut self, host: &mut dyn Host) {
        if self.shut {
            return;
        }
        // A stream to an extra camera keeps one size (the canvas, or the default), so its wait screen does too
        let size = match host.canvas_size() {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => DEFAULT_SIZE,
        };
        for slot in self.idle.clone() {
            if !self.extras.contains_key(&slot) {
                match self.outputs.wait_screen(slot) {
                    Some(screen) => {
                        self.extras.insert(slot, screen);
                    }
                    None => continue,
                }
            }
            self.extras[&slot].show(size.0, size.1, self.image_path.as_deref(), self.mirror);
        }
    }

    fn on_watched(&mut self, host: &mut dyn Host, watched: bool) {
        self.watched = watched;
        if let Some(screen) = &self.screen {
            screen.set_watched(watched);
        }
        host.emit(Event::CameraWatched { watched });
    }

    // ── In place of the stream's picture ──────────────────────────────

    fn on_cover_requested(&mut self, host: &mut dyn Host, owner: &str, on: bool) {
        if on {
            self.asked.insert(owner.to_string());
        } else {
            self.asked.remove(owner);
        }
        self.covering = !self.asked.is_empty();
        if self.covering {
            // Likely the stream's size, and what the decoder makes: ready by the first frame
            let size = self.size(host);
            self.covers.make(CoverKey { size, format: PixelFormat::I420, image: self.image_path.clone(), mirror: self.mirror });
        }
        host.refresh_frame_step();
        host.emit(Event::StreamCovered { covered: self.covering }); // every time, so whoever asked hears back straight away
    }

    // ── The image and Mirror ──────────────────────────────────────────

    /// Show path (kept as a copy next to the config, so moving the original doesn't lose it), or None for the default screen. A
    /// pick that can't be kept leaves the current image alone and says why.
    pub fn set_image(&mut self, host: &mut dyn Host, path: Option<&str>) -> Result<(), String> {
        let path = match path.filter(|p| !p.is_empty()) {
            Some(p) => match self.keep_copy(Path::new(p)) {
                Ok(kept) => Some(kept.to_string_lossy().into_owned()),
                Err(e) => {
                    log::error!("Couldn't keep a copy of the wait screen image: {e}");
                    return Err(os_message(&e));
                }
            },
            None => None,
        };
        self.image_path = path;
        host.schedule_save();
        self.cover_changed(host);
        self.show(host);
        Ok(())
    }

    /// path copied to a fresh wait_screen-<id><ext> next to the config; the previous copy goes only once the new one is in place,
    /// and a new name makes the camera reload even for the same file type.
    fn keep_copy(&self, path: &Path) -> std::io::Result<PathBuf> {
        let folder = (self.folder)();
        std::fs::create_dir_all(&folder)?;
        let here = path.canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let starts = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(KEPT_PREFIX));
        if starts && here.is_some() && here == folder.canonicalize().ok() {
            return Ok(path.to_path_buf()); // already one of ours
        }
        let extension = path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
        let dest = folder.join(format!("{KEPT_PREFIX}{}{extension}", random_hex()));
        let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
        let copied = (self.copy)(path, &part).and_then(|()| std::fs::rename(&part, &dest));
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        // Copies from before: the one it replaces, and wait_screen.<ext> from older versions
        for old in std::fs::read_dir(&folder)?.flatten() {
            let name = old.file_name().to_string_lossy().into_owned();
            let ours = name.starts_with(KEPT_PREFIX) || name.starts_with("wait_screen.");
            if ours && old.path() != dest {
                let _ = std::fs::remove_file(old.path());
            }
        }
        Ok(dest)
    }

    fn set_mirror(&mut self, host: &mut dyn Host, on: bool) {
        self.mirror = on;
        host.schedule_save();
        self.cover_changed(host);
        self.show(host);
    }

    /// A stand-in in use is asked for again at the new picture or mirroring.
    fn cover_changed(&mut self, host: &mut dyn Host) {
        if self.covering {
            host.refresh_frame_step();
            let size = self.size(host);
            self.covers.make(CoverKey { size, format: PixelFormat::I420, image: self.image_path.clone(), mirror: self.mirror });
        }
    }

    // ── The card ──────────────────────────────────────────────────────

    fn props(&self, host: &dyn Host, pick_error: &str, unreadable: bool, preview: (u32, u32)) -> Value {
        let (w, h) = self.size(host);
        // A short line under the picture, with the sentence as its tooltip
        let (note, detail, kind) = if !pick_error.is_empty() {
            (tr("Couldn't use that image"), tr("{problem}. Keeping the current wait screen.").field("problem", pick_error), "status_err")
        } else if unreadable {
            (tr("Couldn't read that image"), tr("The default screen is showing. Choose another one."), "status_err")
        } else {
            (
                tr("Shown at {size}").field("size", format!("{w} \u{d7} {h}")),
                tr("The size the stream opens the camera at. Still images and GIFs work; transparent parts show the dark background."),
                "status_dim",
            )
        };
        json!({
            "preview-width": preview.0, "preview-height": preview.1, "picture-note": note, "picture-detail": detail, "picture-kind": kind,
            "default-enabled": self.image_path.is_some(), "mirror-checked": self.mirror,
        })
    }

    /// Redraws the card; pick_error is why the image just chosen couldn't be kept, if it couldn't.
    fn refresh(&mut self, host: &mut dyn Host, pick_error: &str) {
        host.cancel_schedule(TICK);
        let (w, h) = self.size(host);
        let height = (PREVIEW_W as f64 * h as f64 / w as f64).round_ties_even().max(1.0) as u32;
        let pictures = self.image_path.as_deref().map(|p| wait_screen::read_image_frames(p, PREVIEW_W, height)).unwrap_or_default();
        let unreadable = self.image_path.is_some() && pictures.is_empty();
        self.animation = if pictures.is_empty() { wait_screen::load_frames(None, PREVIEW_W, height) } else { pictures };
        self.shown_frame = 0;
        host.set_ui_props("wait-screen", &self.props(host, pick_error, unreadable, (PREVIEW_W, height)));
        self.show_frame(host);
        // An animation plays in the card as it does on the camera
        if self.animation.len() > 1 {
            host.schedule((self.animation[0].1 * 1000.0) as u64, TICK);
        }
    }

    fn show_frame(&self, host: &mut dyn Host) {
        if let Some((rgb, _)) = self.animation.get(self.shown_frame) {
            host.set_ui_image("wait-screen", "preview", &Frame::new(rgb.width, rgb.height, PixelFormat::Rgba, wait_screen::rgba(rgb, false)));
        }
    }

    fn tick(&mut self, host: &mut dyn Host) {
        if self.animation.len() < 2 {
            return;
        }
        self.shown_frame = (self.shown_frame + 1) % self.animation.len();
        self.show_frame(host);
        host.schedule((self.animation[self.shown_frame].1 * 1000.0) as u64, TICK);
    }

    fn choose(&mut self, host: &mut dyn Host) {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
        let dialog = FileDialog {
            kind: FileDialogKind::Open,
            title: tr("Choose a wait screen"),
            start: home.to_string_lossy().into_owned(),
            filters: vec![tr("Images ({formats})").field("formats", PICKER_FORMATS)],
        };
        self.picking = host.pick_file(&dialog);
    }

    fn pick(&mut self, host: &mut dyn Host, path: Option<&str>) {
        let error = self.set_image(host, path).err().unwrap_or_default();
        self.refresh(host, &error);
    }
}

impl Plugin for WaitScreenPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Wait screen").into(),
            about: n_("The picture apps see on the virtual camera while nothing is streaming.").into(),
            when_off: n_("Without it Wait screen isn't in Settings and between streams apps might see a black picture or nothing at all.").into(),
            category: "on_screen".into(),
            icon: "moon".into(),
            frame_stage: FrameStage::Cover,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.screen = self.outputs.wait_screen(0);
        if let Some(poster) = host.poster() {
            self.watch = self.outputs.watch(Box::new(move |watched| poster.post("watched", json!(watched))));
            if let Some(watch) = &self.watch {
                watch.start();
            }
        }
    }

    /// Once every plugin's config is in, for the canvas size.
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.show(host);
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        self.shut = true;
        if let Some(screen) = &self.screen {
            screen.stop();
        }
        for screen in self.extras.values() {
            screen.stop();
        }
        if let Some(watch) = &self.watch {
            watch.stop();
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::IdleOutputs { slots } => self.on_idle_outputs(host, slots),
            Event::VcamOpened { width, height } => {
                if self.last_size != Some((*width, *height)) {
                    self.last_size = Some((*width, *height));
                    host.schedule_save();
                }
            }
            Event::CoverRequested { owner, on } => self.on_cover_requested(host, owner, *on),
            Event::Posted { owner, token, data } if owner == NAME && token == "watched" => self.on_watched(host, data.as_bool().unwrap_or(false)),
            Event::TimerFired { owner, token } if owner == NAME && token == TICK => self.tick(host),
            Event::RequestDone { request, ok, data, .. } if self.picking == Some(*request) => {
                self.picking = None;
                if *ok {
                    if let Some(path) = data.get("path").and_then(Value::as_str) {
                        self.pick(host, Some(path));
                    }
                }
            }
            _ => {}
        }
    }

    fn on_stream_starting(&mut self, _host: &mut dyn Host) {
        if let Some(screen) = &self.screen {
            screen.stop();
        }
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.show(host);
    }

    /// At the size the stream had, so an app reading the camera keeps its picture.
    fn on_camera_off(&mut self, host: &mut dyn Host) {
        self.show(host);
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        self.covering.then(|| FrameStep::Cpu(Box::new(CoverStep { covers: self.covers.clone(), image: self.image_path.clone(), mirror: self.mirror, black: None })))
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        // Built once the settings window first opens; the host has no say here, so its first look is the window's own "shown"
        vec![SettingsSection {
            id: "wait-screen".into(),
            tab: "streaming".into(),
            ui: UiSpec {
                source: SlintSource::Text(UI.into()),
                component: "WaitScreenSettings".into(),
                props: json!({"picture-note": "", "default-enabled": self.image_path.is_some(), "mirror-checked": self.mirror}),
            },
            order: 10,
            tab_title: String::new(),
            keywords: tr_ctx("search", "idle standby placeholder picture image gif black screen frozen not streaming mirror flip custom background logo"),
            footer: None,
        }]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "wait-screen" {
            return;
        }
        match event.callback.as_str() {
            "shown" => self.refresh(host, ""),
            "choose-clicked" => self.choose(host),
            "default-clicked" => self.pick(host, None),
            "mirror-toggled" => self.set_mirror(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            _ => {}
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![
            ("Wait screen".into(), if self.image_path.is_some() { "own image" } else { "default" }.into()),
            ("An app is reading the camera".into(), if self.watched { "yes" } else { "no" }.into()),
        ]
    }

    fn get_config(&self) -> Value {
        json!({"image": self.image_path, "mirror": self.mirror, "last_size": self.last_size.map(|(w, h)| vec![w, h])})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.image_path = cfg.get("image").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
        self.mirror = cfg.get("mirror") == Some(&Value::Bool(true));
        let whole = |v: &Value| v.as_i64().filter(|_| v.is_i64() || v.is_u64()).filter(|n| (16..=8192).contains(n)).map(|n| n as u32);
        self.last_size = cfg.get("last_size").and_then(Value::as_array).filter(|a| a.len() == 2)
            .and_then(|a| Some((whole(&a[0])?, whole(&a[1])?)));
        Ok(())
    }
}

#[cfg(test)]
#[path = "wait_screen_plugin_tests.rs"]
mod tests;
