//! A DSLR through libgphoto2 (gp.rs): one thread holds the camera, asks it for live view frames in a loop and does what the card
//! asks (focus, aperture, exposure compensation, white balance) between two frames, since only one process can hold the camera and
//! the gphoto2 command can't change a setting mid-stream. The card only gets what the camera allows right now: a setting the camera
//! marks read-only (a scene mode on the dial) isn't offered, and the list is read again now and then so a turn of the dial shows up.

use super::gphoto::{is_claim_error, Timing, CLAIM_ERROR};
use super::gphoto_stills::{notify_changed, read_photo_caps, Shared as StillsShared};
use super::quirks;
use super::Capture;
use crate::webcam_controls::{Control, Controls};
use crate::webcams::{gphoto_port, Choice, Device};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const FOCUS: &str = "dslr_focus";
pub const APERTURE: &str = "dslr_aperture";
pub const EXPOSURE_COMP: &str = "dslr_exposure_comp";
pub const WHITE_BALANCE: &str = "dslr_white_balance";
/// What the card calls them, and the camera's own name for each setting.
/// A setting can go by another name on another make (Canon's aperture is "aperture"), so each lists the names to try in turn.
const MENUS: [(&str, &[&str]); 3] = [(APERTURE, &["f-number", "aperture"]), (EXPOSURE_COMP, &["exposurecompensation"]), (WHITE_BALANCE, &["whitebalance"])];
/// How far a tap moves the focus (a click beside the jog, an arrow key), in the camera's own steps. The Nikon D5100 takes -32767 to 32767.
pub const FOCUS_STEP: i32 = 200;
pub use crate::webcam_controls::{jog_value, AUTOFOCUS, FAR, JOG_BASE, JOG_LEVELS, NEAR};
pub const JOG_SMALLEST: f64 = 50.0;
pub const JOG_LARGEST: f64 = 2000.0;

/// The jog level a control value stands for, with its sign, when it is one.
fn jog_level(value: i64) -> Option<i64> {
    (value.abs() > JOG_BASE && value.abs() <= JOG_BASE + JOG_LEVELS).then(|| value.signum() * (value.abs() - JOG_BASE))
}

/// How far a step at `level` (1 to JOG_LEVELS) moves a focus that takes a number: from 50 at the centre to 2000 at the edge.
pub fn jog_amount(level: i64) -> i32 {
    let at = (level.abs().clamp(1, JOG_LEVELS) - 1) as f64 / (JOG_LEVELS - 1) as f64;
    (JOG_SMALLEST * (JOG_LARGEST / JOG_SMALLEST).powf(at)).round() as i32
}

/// How far a focus step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Tap,
    Jog(i64),
}
/// How often the settings are read again while streaming.
/// Requests waiting for the camera's thread, and how many it does between two frames.
const MAX_QUEUED: usize = 4;
const PER_FRAME: usize = 2;
/// How long ending a stream waits for the camera's thread before leaving it to finish and let go by itself.
const END_WAIT: Duration = Duration::from_secs(6);
/// How long the camera's settings are left before they are read again. This is how a change the camera made on its own reaches the
/// cards: a turn of the zoom ring (which changes the aperture and the apertures on offer), the mode dial, the camera's own buttons.
/// Those are expected input, not an accident of polling. A read after each of the card's own requests (below) covers those, and
/// camera events (gp_camera_wait_for_event) are not used instead because the D5100's live view isn't known to send them, a changed
/// list isn't in them and a camera that does send them still needs this read as the fallback.
const REFRESH: Duration = Duration::from_millis(1500);
/// How long the card's aperture (or another setting that needs live view started again) has to stay unchanged before live view is
/// cycled, so several clicks cost one restart.
const RESTART_AFTER: Duration = Duration::from_millis(800);

pub enum Kind {
    Toggle,
    Range { min: f64, max: f64, step: f64 },
    Choice,
    Text,
    Other,
}

/// One camera setting as the camera reports it.
pub struct Setting {
    pub kind: Kind,
    pub readonly: bool,
    pub value: String,
    pub choices: Vec<String>,
}

/// A camera held open, used from one thread.
pub trait Rig: Send {
    /// The next live view frame, a JPEG.
    fn preview(&mut self) -> Result<Vec<u8>, String>;
    fn read(&mut self, name: &str) -> Option<Setting>;
    fn write(&mut self, name: &str, value: &str) -> Result<(), String>;
    /// After the camera ended live view on its own.
    fn restart_live_view(&mut self);
    /// Takes one photo to the computer and gives each file the camera made; expect is how many to wait for (RAW and JPEG are two).
    fn capture(&mut self, _expect: usize) -> Result<Vec<Shot>, Incomplete> {
        Err("this camera can't take photos".into())
    }
    /// Live view off and the camera let go.
    fn close(&mut self);
}

/// One file the camera made for a photo, as it named it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub name: String,
    pub data: Vec<u8>,
}

/// A photo that wasn't taken whole: why, and the files that did come (none when the camera failed at once).
#[derive(Debug)]
pub struct Incomplete {
    pub why: String,
    pub shots: Vec<Shot>,
}

impl From<String> for Incomplete {
    fn from(why: String) -> Incomplete {
        Incomplete { why, shots: Vec::new() }
    }
}

impl From<&str> for Incomplete {
    fn from(why: &str) -> Incomplete {
        Incomplete::from(why.to_string())
    }
}

/// One thing the camera said while the rest of a photo's files were awaited.
pub enum Said<F> {
    /// Nothing for a moment.
    Quiet,
    /// Another file is ready to fetch.
    File(F),
    /// The camera is done with the photo.
    Done,
    /// Something else (a setting changed, the status), which no photo waits on.
    Other,
}

/// Waits for the files of a RAW and JPEG photo after the first, until `expect` are in. The wait is a clock, not a count of quiet
/// moments: a camera busy with other events can't stretch it. A photo with a file missing is an error that still holds the ones that came.
pub fn gather<F>(shots: Vec<Shot>, expect: usize, limit: Duration, mut listen: impl FnMut() -> Result<Said<F>, String>,
                 mut fetch: impl FnMut(F) -> Result<Shot, String>) -> Result<Vec<Shot>, Incomplete> {
    let until = Instant::now() + limit;
    let mut shots = shots;
    let missing = |shots: Vec<Shot>, why: String| Err(Incomplete { why: format!("{} of {expect} files came: {why}", shots.len()), shots });
    while shots.len() < expect {
        if Instant::now() >= until {
            return missing(shots, "the rest didn't arrive in time".into());
        }
        match listen() {
            Err(why) => return missing(shots, why),
            Ok(Said::Done) => return missing(shots, "the camera said it was done".into()),
            Ok(Said::File(file)) => match fetch(file) {
                Ok(shot) => shots.push(shot),
                Err(why) => return missing(shots, why),
            },
            Ok(Said::Quiet | Said::Other) => {}
        }
    }
    Ok(shots)
}

/// Opens the camera on a port.
pub type Connect = Box<dyn FnMut(&str) -> Result<Box<dyn Rig>, String> + Send>;

/// Whether libgphoto2 could be loaded (bundled or the system's).
pub fn available() -> bool {
    super::gp::available()
}

/// (model, port) of each camera plugged in.
pub fn detect() -> Result<Vec<(String, String)>, String> {
    super::gp::auto_detect()
}

/// The first of these names the camera has, with its setting.
pub(super) fn resolve<'a>(rig: &mut dyn Rig, names: &[&'a str]) -> Option<(&'a str, Setting)> {
    names.iter().find_map(|n| rig.read(n).map(|s| (*n, s)))
}

/// What to write to move the focus one step toward near (-1) or far (1): a number where the camera takes one (Nikon), else one of the
/// camera's own Near and Far choices (Canon): its step 1 for a tap, and steps 1, 2 and 3 for the first, middle and last third of the jog.
fn focus_step(setting: &Setting, direction: i32, reach: Reach) -> Option<String> {
    match setting.kind {
        Kind::Range { .. } => {
            let amount = match reach {
                Reach::Tap => FOCUS_STEP,
                Reach::Jog(level) => jog_amount(level),
            };
            Some((direction * amount).to_string())
        }
        Kind::Choice => {
            let word = if direction < 0 { "Near" } else { "Far" };
            let ours: Vec<&String> = setting.choices.iter().filter(|c| c.starts_with(word)).collect();
            let rank = match reach {
                Reach::Tap => 1,
                Reach::Jog(level) => (level.abs().clamp(1, JOG_LEVELS) - 1) * 3 / JOG_LEVELS + 1,
            };
            let wanted = char::from_digit(rank as u32, 10).unwrap_or('1');
            let fallback = if rank >= 3 { ours.last() } else { ours.first() };
            ours.iter().find(|c| c.ends_with(wanted)).or(fallback).map(|c| (*c).clone())
        }
        _ => None,
    }
}

/// The controls this camera offers right now.
pub fn read_controls(rig: &mut dyn Rig) -> Vec<Control> {
    let mut out = Vec::new();
    let usable = |s: &Setting| !s.readonly;
    let drive = rig.read("autofocusdrive").is_some_and(|s| usable(&s))
        && rig.read("manualfocusdrive").is_some_and(|s| usable(&s) && focus_step(&s, -1, Reach::Tap).is_some() && focus_step(&s, 1, Reach::Tap).is_some());
    if drive {
        let mut c = Control::new(FOCUS, "buttons", -(JOG_BASE + JOG_LEVELS), JOG_BASE + JOG_LEVELS, 1, AUTOFOCUS, AUTOFOCUS);
        c.transient = true;
        out.push(c);
    }
    for (name, gnames) in MENUS {
        let Some(s) = resolve(rig, gnames).map(|(_, s)| s).filter(|s| usable(s) && matches!(s.kind, Kind::Choice) && s.choices.len() > 1) else { continue };
        let at = s.choices.iter().position(|c| *c == s.value).unwrap_or(0) as i64;
        let mut c = Control::new(name, "menu", 0, s.choices.len() as i64 - 1, 1, at, at);
        c.menu = (0..s.choices.len() as i64).collect();
        c.labels = s.choices;
        c.transient = true;
        // Aperture and exposure compensation run in order, so the card steps along them; white balance is a set of names
        (c.ordered, c.unit) = match name {
            APERTURE => (true, "fstop".into()),
            EXPOSURE_COMP => (true, "ev".into()),
            _ => (false, String::new()),
        };
        out.push(c);
    }
    out
}

/// Does what a control says: a menu takes the nth choice of the camera's list, the focus takes Near, Focus and Far (and a step of the jog).
pub fn write_control(rig: &mut dyn Rig, name: &str, value: i64) -> Result<(), String> {
    if name == FOCUS {
        return match value {
            AUTOFOCUS => rig.write("autofocusdrive", "1"),
            NEAR | FAR => {
                let drive = rig.read("manualfocusdrive").ok_or("the camera can't drive its focus")?;
                let step = focus_step(&drive, if value == NEAR { -1 } else { 1 }, Reach::Tap).ok_or("no focus step to take")?;
                rig.write("manualfocusdrive", &step)
            }
            _ => {
                let level = jog_level(value).ok_or("no such focus button")?;
                let drive = rig.read("manualfocusdrive").ok_or("the camera can't drive its focus")?;
                let step = focus_step(&drive, level.signum() as i32, Reach::Jog(level)).ok_or("no focus step to take")?;
                rig.write("manualfocusdrive", &step)
            }
        };
    }
    let Some((_, gnames)) = MENUS.iter().find(|(n, _)| *n == name) else { return Err("no such control".into()) };
    let (gname, setting) = resolve(rig, gnames).ok_or("the camera doesn't have it")?;
    if setting.readonly {
        return Err("the camera doesn't allow it now".into());
    }
    let choice = usize::try_from(value).ok().and_then(|i| setting.choices.get(i)).ok_or("not one of its choices")?;
    rig.write(gname, choice)
}

/// Sets a menu control to the choice the camera words as `label`, looked up in the list as it is now.
pub fn write_choice(rig: &mut dyn Rig, name: &str, label: &str) -> Result<(), String> {
    let Some((_, gnames)) = MENUS.iter().find(|(n, _)| *n == name) else { return Err("no such control".into()) };
    let (gname, setting) = resolve(rig, gnames).ok_or("the camera doesn't have it")?;
    if setting.readonly {
        return Err("the camera doesn't allow it now".into());
    }
    if !setting.choices.iter().any(|c| c == label) {
        return Err(format!("{label} isn't one of its choices now"));
    }
    rig.write(gname, label)
}

enum Cmd {
    Set(String, i64),
    /// A menu to the choice with this wording, whatever its place in the camera's list is by now.
    Pick(String, String),
}

/// What a running camera shares: where to send it work, and the controls it last read.
struct Hub {
    id: u64,
    cmds: SyncSender<Cmd>,
    controls: Arc<Mutex<Vec<Control>>>,
    stills: Arc<StillsShared>,
    /// Requests handed over that the camera hasn't finished, the list read again after each included.
    pending: Arc<AtomicUsize>,
}

/// What the photo service of the camera on a port shares with its thread, while that streams.
pub(super) fn stills_of(port: &str) -> Option<Arc<StillsShared>> {
    hubs().lock().unwrap().get(port).map(|h| h.stills.clone())
}

/// The cameras streaming now, by port.
fn hubs() -> &'static Mutex<HashMap<String, Hub>> {
    static HUBS: OnceLock<Mutex<HashMap<String, Hub>>> = OnceLock::new();
    HUBS.get_or_init(Mutex::default)
}

/// The Webcam card's way to a DSLR: only while the camera streams, since only the stream's thread can talk to it.
pub struct GphotoControls {
    port: String,
}

impl GphotoControls {
    pub fn new(location: &str) -> GphotoControls {
        GphotoControls { port: gphoto_port(location).to_string() }
    }
}

impl GphotoControls {
    fn send(&self, cmd: Cmd) -> bool {
        let hubs = hubs().lock().unwrap();
        let Some(h) = hubs.get(&self.port) else { return false };
        h.pending.fetch_add(1, Ordering::SeqCst);
        let sent = h.cmds.try_send(cmd).is_ok();
        if !sent {
            h.pending.fetch_sub(1, Ordering::SeqCst);
        }
        sent
    }
}

impl Controls for GphotoControls {
    fn list(&mut self) -> Vec<Control> {
        hubs().lock().unwrap().get(&self.port).map(|h| h.controls.lock().unwrap().clone()).unwrap_or_default()
    }

    /// Handed to the camera's thread to do between two frames (a focus run can take a second), so it says yes at once. Only a few wait at a
    /// time: a press with that many ahead of it is dropped, not queued behind seconds of focusing.
    fn set(&mut self, name: &str, value: i64) -> bool {
        self.send(Cmd::Set(name.into(), value))
    }

    fn set_labelled(&mut self, name: &str, _value: i64, label: &str) -> bool {
        self.send(Cmd::Pick(name.into(), label.into()))
    }

    fn busy(&mut self) -> bool {
        hubs().lock().unwrap().get(&self.port).is_some_and(|h| h.pending.load(Ordering::SeqCst) > 0)
    }
}

enum Msg {
    Frame(Vec<u8>),
    Failed(String),
}

pub struct LibCapture {
    stills: Arc<StillsShared>,
    rx: Option<Receiver<Msg>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    last_frame: Instant,
    silent: Duration,
}

struct Stopper(Arc<AtomicBool>);

impl telescope_core::media::FeedStopper for Stopper {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Starts live view through libgphoto2 on device's camera: Err((opened at all, why)). The first frame comes from read().
pub fn open(device: &Device) -> Result<Box<dyn Capture>, (bool, String)> {
    open_model(&device.location, &device.name, Timing::default(), Box::new(|port| super::gp::GpCamera::open(port).map(|c| Box::new(c) as Box<dyn Rig>)))
}

pub fn open_with(location: &str, timing: Timing, connect: Connect) -> Result<Box<dyn Capture>, (bool, String)> {
    open_model(location, "", timing, connect)
}

/// As open_with, for a camera of this model (as libgphoto2 lists it), whose quirks (quirks.rs) are then dealt with.
pub fn open_model(location: &str, model: &str, timing: Timing, connect: Connect) -> Result<Box<dyn Capture>, (bool, String)> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel(4);
    let (port, s, model) = (gphoto_port(location).to_string(), stop.clone(), model.to_string());
    let stills = Arc::new(StillsShared::default());
    let shared = stills.clone();
    let worker = std::thread::Builder::new().name("gphoto2".into()).spawn(move || run(connect, &port, &model, timing, &s, &tx, &shared))
        .map_err(|e| (false, e.to_string()))?;
    Ok(Box::new(LibCapture { stills, rx: Some(rx), stop, worker: Some(worker), last_frame: Instant::now(), silent: timing.silent }))
}

/// Hands over a last message once there's room, without holding up a stop.
fn deliver(tx: &SyncSender<Msg>, stop: &AtomicBool, msg: Msg) {
    let mut msg = msg;
    loop {
        match tx.try_send(msg) {
            Err(TrySendError::Full(back)) if !stop.load(Ordering::SeqCst) => {
                msg = back;
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => return,
        }
    }
}

fn pause(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10).min(d));
    }
}

/// Keeps live view going until stopped: opens the camera, passes on its frames, does the card's requests between them, and
/// starts live view (or the whole camera) again when it stops sending.
fn run(mut connect: Connect, port: &str, model: &str, timing: Timing, stop: &Arc<AtomicBool>, tx: &SyncSender<Msg>, stills: &Arc<StillsShared>) {
    static IDS: AtomicU64 = AtomicU64::new(1);
    let id = IDS.fetch_add(1, Ordering::SeqCst);
    let (cmd_tx, cmds) = sync_channel::<Cmd>(MAX_QUEUED);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let pending = Arc::new(AtomicUsize::new(0));
    hubs().lock().unwrap().insert(port.to_string(), Hub { id, cmds: cmd_tx, controls: shared.clone(), stills: stills.clone(), pending: pending.clone() });
    let mut rig: Option<Box<dyn Rig>> = None;
    let (mut fails, mut delay) = (0u32, timing.restart);
    let mut refreshed = Instant::now();
    // When live view is due to be cycled for a setting this model only applies that way (quirks.rs), pushed back by each further change
    let mut restart_at: Option<Instant> = None;
    while !stop.load(Ordering::SeqCst) {
        let Some(r) = rig.as_mut() else {
            match connect(port) {
                Ok(mut r) => {
                    // Live view on, for the focus drive, which only works with it
                    if r.read("viewfinder").is_some_and(|s| s.value == "false") {
                        let _ = r.write("viewfinder", "1");
                    }
                    *shared.lock().unwrap() = read_controls(r.as_mut());
                    if stills.set_caps(Some(read_photo_caps(r.as_mut()))) {
                        notify_changed(port);
                    }
                    refreshed = Instant::now();
                    rig = Some(r);
                }
                Err(why) => {
                    fails += 1;
                    log::info!("couldn't open the camera ({fails}): {why}");
                    if is_claim_error(&why) {
                        deliver(tx, stop, Msg::Failed(CLAIM_ERROR.into()));
                        break;
                    }
                    if fails >= timing.give_up_runs {
                        deliver(tx, stop, Msg::Failed(format!("gphoto2 stopped: {why}")));
                        break;
                    }
                    pause(stop, delay);
                    delay = (delay * 2).min(timing.restart_max);
                }
            }
            continue;
        };
        // A photo asked for: live view is paused for it and comes back after
        if stills.run_pending(r.as_mut()) {
            refreshed = Instant::now().checked_sub(REFRESH).unwrap_or_else(Instant::now);
            notify_changed(port);
            continue;
        }
        let mut done = 0;
        for _ in 0..PER_FRAME {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(cmd) = cmds.try_recv() else { break };
            let (name, done_it) = match cmd {
                Cmd::Set(name, value) => {
                    let done_it = write_control(r.as_mut(), &name, value);
                    if let Err(why) = &done_it {
                        log::debug!("the camera refused {name} = {value}: {why}");
                    }
                    (name, done_it)
                }
                Cmd::Pick(name, label) => {
                    let done_it = write_choice(r.as_mut(), &name, &label);
                    if let Err(why) = &done_it {
                        log::debug!("the camera refused {name} = {label}: {why}");
                    }
                    (name, done_it)
                }
            };
            if done_it.is_ok() && quirks::needs_live_view_restart(model, &name) {
                restart_at = Some(Instant::now() + RESTART_AFTER);
            }
            done += 1;
        }
        if done > 0 || refreshed.elapsed() >= REFRESH {
            *shared.lock().unwrap() = read_controls(r.as_mut());
            if stills.set_caps(Some(read_photo_caps(r.as_mut()))) {
                notify_changed(port);
            }
            refreshed = Instant::now();
        }
        // Only now, with the list up to date: whoever waits for the camera to be idle then reads where it really is
        if done > 0 {
            pending.fetch_sub(done, Ordering::SeqCst);
        }
        // The dial has been still for a moment: live view off and on, which is when this camera's lens takes the setting. The last
        // frame stays on the virtual camera meanwhile, as for a photo. The list is read again after it, as the camera may have moved.
        if restart_at.is_some_and(|t| Instant::now() >= t) && pending.load(Ordering::SeqCst) == 0 {
            restart_at = None;
            log::debug!("live view is cycled for the setting just changed");
            r.restart_live_view();
            refreshed = Instant::now().checked_sub(REFRESH).unwrap_or_else(Instant::now);
        }
        match r.preview() {
            Ok(frame) => {
                fails = 0;
                delay = timing.restart;
                match tx.try_send(Msg::Frame(frame)) {
                    Ok(()) | Err(TrySendError::Full(_)) => {} // a reader that falls behind skips frames rather than lagging
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Err(why) => {
                fails += 1;
                log::info!("no live view frame ({fails}): {why}");
                if fails >= timing.give_up_runs {
                    deliver(tx, stop, Msg::Failed(format!("gphoto2 stopped: {why}")));
                    break;
                }
                pause(stop, delay);
                delay = (delay * 2).min(timing.restart_max);
                if fails >= 3 {
                    // Live view starting again didn't help: the camera itself is opened again
                    if let Some(mut r) = rig.take() {
                        r.close();
                    }
                    // Nothing to shoot with until it's open again
                    if stills.set_caps(None) {
                        notify_changed(port);
                    }
                } else if let Some(r) = rig.as_mut() {
                    r.restart_live_view();
                }
            }
        }
    }
    {
        // Out of the list first, so nothing is sent to a camera that is going
        let mut h = hubs().lock().unwrap();
        if h.get(port).is_some_and(|h| h.id == id) {
            h.remove(port);
        }
    }
    stills.close();
    notify_changed(port);
    if let Some(mut r) = rig {
        r.close();
    }
}

impl LibCapture {
    /// Ends it for good: the camera's live view off and the camera let go before this returns.
    fn shutdown(&mut self) {
        self.rx = None;
        self.stop.store(true, Ordering::SeqCst);
        // A camera that stalls mid-call can't be interrupted, so past the wait the thread is left to finish and close the camera itself
        if let Some(w) = self.worker.take() {
            let until = Instant::now() + END_WAIT;
            while !w.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if w.is_finished() {
                let _ = w.join();
            } else {
                log::warn!("the camera is slow to let go; carrying on without waiting for it");
            }
        }
    }
}

impl Capture for LibCapture {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Err("stopped".into());
            }
            let Some(rx) = &self.rx else { return Err("stopped".into()) };
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Frame(f)) => {
                    self.last_frame = Instant::now();
                    return Ok(f);
                }
                Ok(Msg::Failed(why)) => return Err(why),
                // A photo is being taken, which can last a minute and sends no frames meanwhile
                Err(RecvTimeoutError::Timeout) if self.stills.capturing() => self.last_frame = Instant::now(),
                Err(RecvTimeoutError::Timeout) if self.last_frame.elapsed() > self.silent => {
                    self.shutdown();
                    return Err(format!("no frame within {:.1} s", self.silent.as_secs_f64()));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("gphoto2 ended".into()),
            }
        }
    }

    fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
        Box::new(Stopper(self.stop.clone()))
    }

    fn negotiated(&self) -> Option<Choice> {
        None
    }
}

impl Drop for LibCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct State {
        settings: HashMap<String, (bool, Vec<String>, String)>,
        log: Vec<String>,
        previews: u32,
        fail_previews: u32,
        live_view_restarts: u32,
        closed: bool,
        opens: u32,
    }

    /// A camera in manual mode: (read-only, choices, value) per setting.
    fn camera(readonly: bool) -> Arc<Mutex<State>> {
        let choice = |l: &[&str], v: &str| (readonly, l.iter().map(|s| s.to_string()).collect::<Vec<_>>(), v.to_string());
        let mut s = State::default();
        s.settings.insert("f-number".into(), choice(&["f/3,5", "f/4", "f/5,6"], "f/4"));
        s.settings.insert("exposurecompensation".into(), choice(&["-1", "0", "1"], "0"));
        s.settings.insert("whitebalance".into(), choice(&["Automatic", "Daylight"], "Automatic"));
        s.settings.insert("autofocusdrive".into(), (readonly, vec![], "false".into()));
        s.settings.insert("manualfocusdrive".into(), (readonly, vec![], "0".into()));
        s.settings.insert("viewfinder".into(), (false, vec![], "false".into()));
        Arc::new(Mutex::new(s))
    }

    struct Fake(Arc<Mutex<State>>);

    impl Rig for Fake {
        fn preview(&mut self) -> Result<Vec<u8>, String> {
            let mut s = self.0.lock().unwrap();
            if s.fail_previews > 0 {
                s.fail_previews -= 1;
                return Err("Device Busy".into());
            }
            s.previews += 1;
            drop(s);
            std::thread::sleep(Duration::from_millis(2));
            Ok(vec![0xFF, 0xD8, 0xFF, 0xD9])
        }
        fn read(&mut self, name: &str) -> Option<Setting> {
            let s = self.0.lock().unwrap();
            let (ro, choices, value) = s.settings.get(name)?.clone();
            let kind = if !choices.is_empty() { Kind::Choice } else if name == "manualfocusdrive" { Kind::Range { min: -100.0, max: 100.0, step: 1.0 } } else { Kind::Toggle };
            Some(Setting { kind, readonly: ro, value, choices })
        }
        fn write(&mut self, name: &str, value: &str) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            s.log.push(format!("{name}={value}"));
            match s.settings.get_mut(name) {
                Some((false, _, v)) => {
                    *v = value.into();
                    Ok(())
                }
                Some(_) => Err("Access Denied".into()),
                None => Err("no such setting".into()),
            }
        }
        fn restart_live_view(&mut self) {
            self.0.lock().unwrap().live_view_restarts += 1;
        }
        fn close(&mut self) {
            self.0.lock().unwrap().closed = true;
        }
    }

    fn fast() -> Timing {
        Timing { restart: Duration::from_millis(5), restart_max: Duration::from_millis(20), give_up_runs: 5, silent: Duration::from_secs(5), grace: Duration::from_secs(1) }
    }

    fn connecting(state: &Arc<Mutex<State>>) -> Connect {
        let state = state.clone();
        Box::new(move |_| {
            state.lock().unwrap().opens += 1;
            state.lock().unwrap().closed = false;
            Ok(Box::new(Fake(state.clone())) as Box<dyn Rig>)
        })
    }

    // By hand with a camera plugged in: cargo nextest run real_camera --run-ignored only --no-capture
    #[test]
    #[ignore]
    fn real_camera_session() {
        let (model, port) = detect().expect("the lookup runs").into_iter().next().expect("a camera");
        println!("camera {model} on {port}");
        let started = Instant::now();
        let mut cap = open_with(&format!("gphoto2:{port}"), Timing::default(), Box::new(|p| super::super::gp::GpCamera::open(p).map(|c| Box::new(c) as Box<dyn Rig>))).ok().unwrap();
        let first = cap.read().expect("a first frame");
        println!("first frame after {:?}, {} bytes", started.elapsed(), first.len());
        let mut controls = GphotoControls::new(&format!("gphoto2:{port}"));
        std::thread::sleep(Duration::from_millis(500));
        for c in controls.list() {
            println!("control {} {} value {} labels {:?}", c.name, c.kind, c.value, c.labels);
        }
        let (mut frames, t0) = (0u32, Instant::now());
        while t0.elapsed() < Duration::from_secs(5) {
            cap.read().expect("a frame");
            frames += 1;
        }
        println!("{frames} frames in 5 s, {:.1} fps", frames as f64 / t0.elapsed().as_secs_f64());
        if let Some(ap) = controls.list().into_iter().find(|c| c.name == APERTURE) {
            let other = if ap.value == 0 { 1 } else { 0 };
            println!("aperture {} -> {}", ap.labels[ap.value as usize], ap.labels[other as usize]);
            assert!(controls.set(APERTURE, other));
            std::thread::sleep(Duration::from_millis(2500));
            let now = controls.list().into_iter().find(|c| c.name == APERTURE).unwrap();
            println!("aperture reads {}", now.labels[now.value as usize]);
            assert_eq!(now.value, other);
            assert!(controls.set(APERTURE, ap.value));
            std::thread::sleep(Duration::from_millis(2500));
            let back = controls.list().into_iter().find(|c| c.name == APERTURE).unwrap();
            println!("aperture restored to {}", back.labels[back.value as usize]);
            assert_eq!(back.value, ap.value);
        }
        if controls.list().iter().any(|c| c.name == FOCUS) {
            for (label, button) in [("Focus", 1), ("Near", 0), ("Far", 2)] {
                let t = Instant::now();
                assert!(controls.set(FOCUS, button));
                let mut got = 0;
                while t.elapsed() < Duration::from_secs(3) {
                    cap.read().expect("a frame during focus");
                    got += 1;
                }
                println!("pressed {label}: {got} frames in 3 s after");
            }
        }
        drop(cap);
        println!("closed");
    }

    // Where libgphoto2 is installed this runs the real library's camera lookup (finding no camera is fine); elsewhere it's skipped
    #[test]
    fn the_real_library_lists_cameras_when_it_is_there() {
        assert!(available() || std::env::var_os(super::super::gp::BUNDLE_ENV).is_none(), "the bundled copy didn't load");
        if available() {
            let found = detect().expect("the lookup runs");
            assert!(found.iter().all(|(_, port)| port.contains(':')));
            // Run with the bundle's folder set, it is the bundled copy that got loaded
            if let Some(dir) = std::env::var_os(super::super::gp::BUNDLE_ENV) {
                let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
                let core = std::path::Path::new(&dir).join("libgphoto2.so.6");
                assert!(maps.contains(&*core.to_string_lossy()), "libgphoto2 came from somewhere else:\n{maps}");
            }
        }
    }

    #[test]
    fn manual_mode_offers_focus_and_the_cameras_own_choices() {
        let state = camera(false);
        let controls = read_controls(&mut Fake(state));
        let names: Vec<&str> = controls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, [FOCUS, APERTURE, EXPOSURE_COMP, WHITE_BALANCE]);
        let aperture = &controls[1];
        assert_eq!((aperture.kind.as_str(), aperture.value, aperture.labels.clone()), ("menu", 1, vec!["f/3,5".to_string(), "f/4".into(), "f/5,6".into()]));
        assert!(controls.iter().all(|c| c.transient));
    }

    #[test]
    fn a_scene_mode_that_refuses_everything_offers_nothing() {
        assert!(read_controls(&mut Fake(camera(true))).is_empty());
    }

    #[test]
    fn a_missing_or_one_choice_setting_isnt_offered() {
        let state = camera(false);
        state.lock().unwrap().settings.remove("autofocusdrive");
        state.lock().unwrap().settings.get_mut("whitebalance").unwrap().1 = vec!["Automatic".into()];
        let names: Vec<String> = read_controls(&mut Fake(state)).into_iter().map(|c| c.name).collect();
        assert_eq!(names, [APERTURE, EXPOSURE_COMP]);
    }

    // A Canon names its aperture "aperture" and takes Near and Far as choices of a radio, not numbers
    #[test]
    fn a_canon_style_camera_works_through_its_own_names_and_choices() {
        let state = camera(false);
        {
            let mut s = state.lock().unwrap();
            let f = s.settings.remove("f-number").unwrap();
            s.settings.insert("aperture".into(), f);
            let choices = ["Near 1", "Near 2", "Near 3", "None", "Far 1", "Far 2", "Far 3"].map(String::from).to_vec();
            s.settings.insert("manualfocusdrive".into(), (false, choices, "None".into()));
        }
        let mut rig = Fake(state.clone());
        let names: Vec<String> = read_controls(&mut rig).into_iter().map(|c| c.name).collect();
        assert_eq!(names, [FOCUS, APERTURE, EXPOSURE_COMP, WHITE_BALANCE]);
        write_control(&mut rig, APERTURE, 2).unwrap();
        write_control(&mut rig, FOCUS, 0).unwrap();
        write_control(&mut rig, FOCUS, 2).unwrap();
        assert_eq!(state.lock().unwrap().log, ["aperture=f/5,6", "manualfocusdrive=Near 1", "manualfocusdrive=Far 1"]);
    }

    #[test]
    fn requests_waiting_for_the_camera_are_few_and_the_rest_are_dropped() {
        let (tx, rx) = sync_channel::<Cmd>(MAX_QUEUED);
        let sent = (0..10).filter(|i| tx.try_send(Cmd::Set("dslr_focus".into(), *i)).is_ok()).count();
        assert_eq!(sent, MAX_QUEUED);
        drop(rx);
    }

    #[test]
    fn controls_write_the_camera_settings() {
        let state = camera(false);
        let mut rig = Fake(state.clone());
        write_control(&mut rig, APERTURE, 2).unwrap();
        write_control(&mut rig, FOCUS, 1).unwrap();
        write_control(&mut rig, FOCUS, 0).unwrap();
        write_control(&mut rig, FOCUS, 2).unwrap();
        assert_eq!(state.lock().unwrap().log, ["f-number=f/5,6", "autofocusdrive=1", "manualfocusdrive=-200", "manualfocusdrive=200"]);
        assert!(write_control(&mut rig, APERTURE, 7).is_err());
        assert!(write_control(&mut rig, "nonsense", 0).is_err());
        // A refusal from the camera comes back as an error and changes nothing
        state.lock().unwrap().settings.get_mut("f-number").unwrap().0 = true;
        assert!(write_control(&mut rig, APERTURE, 0).is_err());
        assert_eq!(state.lock().unwrap().settings["f-number"].2, "f/5,6");
    }

    #[test]
    fn a_tap_is_a_small_step_and_a_jog_step_grows_with_how_far_out_the_handle_is() {
        // Where the handle just leaves the middle and where it is at the end, on a camera that takes a number
        assert_eq!((jog_amount(1), jog_amount(JOG_LEVELS)), (50, 2000));
        let amounts: Vec<i32> = (1..=JOG_LEVELS).map(jog_amount).collect();
        assert!(amounts.windows(2).all(|w| w[0] < w[1]), "each level asks for more than the one before: {amounts:?}");
        // Spread evenly in size, so no level is a lurch
        let ratios: Vec<f64> = amounts.windows(2).map(|w| w[1] as f64 / w[0] as f64).collect();
        assert!(ratios.iter().all(|r| (r - ratios[0]).abs() < 0.08), "{ratios:?}");
        assert!(FOCUS_STEP > jog_amount(1) && FOCUS_STEP < jog_amount(JOG_LEVELS), "a tap sits inside the jog's range");
        let state = camera(false);
        let mut rig = Fake(state.clone());
        for level in [-1, -JOG_LEVELS, 6, JOG_LEVELS] {
            write_control(&mut rig, FOCUS, jog_value(level).unwrap()).unwrap();
        }
        let log = state.lock().unwrap().log.clone();
        let mid = format!("manualfocusdrive={}", jog_amount(6));
        assert_eq!(log, ["manualfocusdrive=-50", "manualfocusdrive=-2000", mid.as_str(), "manualfocusdrive=2000"]);
    }

    #[test]
    fn the_jog_at_rest_asks_for_nothing_and_a_value_that_is_no_step_is_refused() {
        assert_eq!(jog_value(0), None);
        assert_eq!((jog_value(1), jog_value(-1), jog_value(99), jog_value(-99)), (Some(JOG_BASE + 1), Some(-JOG_BASE - 1), Some(JOG_BASE + JOG_LEVELS), Some(-JOG_BASE - JOG_LEVELS)));
        let mut rig = Fake(camera(false));
        assert!(write_control(&mut rig, FOCUS, JOG_BASE).is_err() && write_control(&mut rig, FOCUS, JOG_BASE + JOG_LEVELS + 1).is_err() && write_control(&mut rig, FOCUS, 17).is_err());
    }

    #[test]
    fn a_canon_takes_its_own_near_and_far_steps_one_by_distance_and_a_tap_is_step_one() {
        let state = camera(false);
        let mut rig = Fake(state.clone());
        let choices = ["Near 1", "Near 2", "Near 3", "None", "Far 1", "Far 2", "Far 3"].map(String::from).to_vec();
        state.lock().unwrap().settings.insert("manualfocusdrive".into(), (false, choices, "None".into()));
        write_control(&mut rig, FOCUS, NEAR).unwrap();
        write_control(&mut rig, FOCUS, FAR).unwrap();
        for level in [-1, -4, -5, -8, -9, -JOG_LEVELS, JOG_LEVELS] {
            write_control(&mut rig, FOCUS, jog_value(level).unwrap()).unwrap();
        }
        assert_eq!(state.lock().unwrap().log, ["manualfocusdrive=Near 1", "manualfocusdrive=Far 1", "manualfocusdrive=Near 1", "manualfocusdrive=Near 1", "manualfocusdrive=Near 2",
                                                "manualfocusdrive=Near 2", "manualfocusdrive=Near 3", "manualfocusdrive=Near 3", "manualfocusdrive=Far 3"]);
        // The control takes a tap each way and every jog step, with Focus in the middle as before
        let focus = read_controls(&mut Fake(camera(false))).remove(0);
        assert_eq!((focus.minimum, focus.maximum, focus.value), (-(JOG_BASE + JOG_LEVELS), JOG_BASE + JOG_LEVELS, AUTOFOCUS));
    }

    #[test]
    fn aperture_and_exposure_compensation_are_dials_and_white_balance_is_not() {
        let controls = read_controls(&mut Fake(camera(false)));
        let of = |name: &str| controls.iter().find(|c| c.name == name).map(|c| (c.ordered, c.unit.clone())).unwrap();
        assert_eq!((of(APERTURE), of(EXPOSURE_COMP), of(WHITE_BALANCE)), ((true, "fstop".to_string()), (true, "ev".to_string()), (false, String::new())));
    }

    #[test]
    fn focus_steps_wait_for_the_last_one_to_finish() {
        let state = camera(false);
        let mut cap = open_with("gphoto2:usb:T5", fast(), connecting(&state)).ok().unwrap();
        cap.read().unwrap();
        let mut controls = GphotoControls::new("gphoto2:usb:T5");
        wait_for("the list", || !controls.list().is_empty());
        assert!(!controls.busy());
        assert!(controls.set(FOCUS, NEAR));
        // Counted until the camera has done it and the list is read again: a held button sends nothing more before that
        wait_for("the step", || !controls.busy());
        let log = state.lock().unwrap().log.clone();
        assert_eq!(log.iter().filter(|l| l.starts_with("manualfocusdrive")).collect::<Vec<_>>(), ["manualfocusdrive=-200"]);
        assert!(controls.list().iter().any(|c| c.name == FOCUS));
        // A request that wasn't taken (the camera is gone) leaves nothing counted
        drop(cap);
        assert!(!controls.set(FOCUS, NEAR) && !controls.busy());
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn live_view_runs_controls_apply_between_frames_and_stopping_closes_the_camera() {
        let state = camera(false);
        let mut cap = open_with("gphoto2:usb:T1", fast(), connecting(&state)).ok().unwrap();
        assert_eq!(cap.read().unwrap(), vec![0xFF, 0xD8, 0xFF, 0xD9]);
        // Live view was switched on for the focus drive
        assert_eq!(state.lock().unwrap().settings["viewfinder"].2, "1");
        let mut controls = GphotoControls::new("gphoto2:usb:T1");
        assert_eq!(controls.list().len(), 4);
        assert!(controls.set(APERTURE, 2));
        wait_for("the aperture", || state.lock().unwrap().settings["f-number"].2 == "f/5,6");
        wait_for("the list to follow", || controls.list().iter().any(|c| c.name == APERTURE && c.value == 2));
        // Frames keep coming
        for _ in 0..3 {
            cap.read().unwrap();
        }
        // The dial turned to a scene mode: the list empties without anyone asking for it
        for (_, (ro, _, _)) in state.lock().unwrap().settings.iter_mut() {
            *ro = true;
        }
        wait_for("the list to empty", || controls.list().is_empty());
        drop(cap);
        assert!(state.lock().unwrap().closed);
        // Not streaming: no controls, and a set goes nowhere
        assert!(controls.list().is_empty() && !controls.set(APERTURE, 0));
    }

    #[test]
    fn a_camera_that_ends_live_view_is_started_again() {
        let state = camera(false);
        let mut cap = open_with("gphoto2:usb:T2", fast(), connecting(&state)).ok().unwrap();
        cap.read().unwrap();
        state.lock().unwrap().fail_previews = 2;
        wait_for("live view to restart", || state.lock().unwrap().live_view_restarts == 2);
        let before = state.lock().unwrap().previews;
        wait_for("frames again", || state.lock().unwrap().previews > before + 2);
        assert_eq!(state.lock().unwrap().opens, 1);
        // Three misses in a row: the camera is opened again, and the feed carries on
        state.lock().unwrap().fail_previews = 3;
        wait_for("a new open", || state.lock().unwrap().opens == 2);
        let before = state.lock().unwrap().previews;
        wait_for("frames again", || state.lock().unwrap().previews > before + 2);
        cap.read().unwrap();
    }

    #[test]
    fn a_camera_that_never_comes_back_fails_and_a_claimed_one_says_so() {
        let state = camera(false);
        state.lock().unwrap().fail_previews = u32::MAX;
        let mut cap = open_with("gphoto2:usb:T3", fast(), connecting(&state)).ok().unwrap();
        assert!(cap.read().unwrap_err().starts_with("gphoto2 stopped"));
        let mut cap = open_with("gphoto2:usb:T4", fast(), Box::new(|_| Err("Could not claim the USB device".into()))).ok().unwrap();
        assert!(is_claim_error(&cap.read().unwrap_err()));
    }

    /// Aperture changes asked for through the card, one by one with a short wait between, as a person clicking would.
    fn click_aperture(controls: &mut GphotoControls, picks: &[i64]) {
        for pick in picks {
            assert!(controls.set(APERTURE, *pick));
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    #[test]
    fn a_d5100_cycles_live_view_once_after_a_run_of_aperture_clicks() {
        let state = camera(false);
        let mut cap = open_model("gphoto2:usb:Q1", "Nikon DSC D5100 (PTP mode)", fast(), connecting(&state)).ok().unwrap();
        cap.read().unwrap();
        let mut controls = GphotoControls::new("gphoto2:usb:Q1");
        click_aperture(&mut controls, &[0, 1, 2, 1, 0]);
        wait_for("live view to be cycled", || state.lock().unwrap().live_view_restarts == 1);
        assert_eq!(state.lock().unwrap().settings["f-number"].2, "f/3,5");
        std::thread::sleep(RESTART_AFTER + Duration::from_millis(500));
        assert_eq!(state.lock().unwrap().live_view_restarts, 1, "five clicks, one cycle");
        // The frames carry on after it
        let before = state.lock().unwrap().previews;
        wait_for("frames again", || state.lock().unwrap().previews > before + 2);
        // A later click starts the count again
        click_aperture(&mut controls, &[2]);
        wait_for("a second cycle", || state.lock().unwrap().live_view_restarts == 2);
    }

    #[test]
    fn nothing_but_aperture_on_a_d5100_cycles_live_view_and_no_other_model_does() {
        // Other settings on the D5100
        let state = camera(false);
        let mut cap = open_model("gphoto2:usb:Q2", "Nikon DSC D5100", fast(), connecting(&state)).ok().unwrap();
        cap.read().unwrap();
        let mut controls = GphotoControls::new("gphoto2:usb:Q2");
        assert!(controls.set(EXPOSURE_COMP, 2) && controls.set(WHITE_BALANCE, 1));
        wait_for("both applied", || state.lock().unwrap().settings["whitebalance"].2 == "Daylight" && state.lock().unwrap().settings["exposurecompensation"].2 == "1");
        std::thread::sleep(RESTART_AFTER + Duration::from_millis(500));
        assert_eq!(state.lock().unwrap().live_view_restarts, 0);
        drop(cap);
        // Aperture on another model, a Nikon too, and on a camera whose model isn't known
        for (i, model) in ["Nikon DSC D7500", "Canon EOS 600D", ""].into_iter().enumerate() {
            let state = camera(false);
            let location = format!("gphoto2:usb:Q3{i}");
            let mut cap = open_model(&location, model, fast(), connecting(&state)).ok().unwrap();
            cap.read().unwrap();
            let mut controls = GphotoControls::new(&location);
            click_aperture(&mut controls, &[0, 2]);
            wait_for("applied", || state.lock().unwrap().settings["f-number"].2 == "f/5,6");
            std::thread::sleep(RESTART_AFTER + Duration::from_millis(500));
            assert_eq!(state.lock().unwrap().live_view_restarts, 0, "{model:?}");
        }
    }

    #[test]
    fn a_camera_changing_aperture_and_its_choices_on_its_own_reaches_the_card_without_breaking_a_pick() {
        let state = camera(false);
        let mut cap = open_with("gphoto2:usb:Q4", fast(), connecting(&state)).ok().unwrap();
        cap.read().unwrap();
        let mut controls = GphotoControls::new("gphoto2:usb:Q4");
        let aperture = |controls: &mut GphotoControls| controls.list().into_iter().find(|c| c.name == APERTURE).unwrap();
        assert_eq!((aperture(&mut controls).labels.len(), aperture(&mut controls).value), (3, 1));
        // The zoom ring turns: the lens narrows the widest aperture, so both the value and the list on offer change under the card
        {
            let mut s = state.lock().unwrap();
            let f = s.settings.get_mut("f-number").unwrap();
            f.1 = vec!["f/4".into(), "f/4,5".into(), "f/5,6".into()];
            f.2 = "f/4,5".into();
        }
        wait_for("the card to see the new list and value", || {
            let a = aperture(&mut controls);
            a.labels == ["f/4", "f/4,5", "f/5,6"] && a.value == 1
        });
        // A click made on the old list: f/4 was place 1 there, and it is place 0 now. It goes by its name
        assert!(controls.set_labelled(APERTURE, 1, "f/4"));
        wait_for("the pick", || state.lock().unwrap().settings["f-number"].2 == "f/4");
        // One that the lens can no longer do is refused, and nothing else is written in its place
        let writes = state.lock().unwrap().log.len();
        assert!(controls.set_labelled(APERTURE, 0, "f/3,5"));
        wait_for("the camera to be done", || !controls.busy());
        assert_eq!(state.lock().unwrap().settings["f-number"].2, "f/4");
        assert_eq!(state.lock().unwrap().log.len(), writes, "no write for a choice that isn't offered");
        // And what is asked by number still works for the cameras with fixed lists
        assert!(controls.set(APERTURE, 2));
        wait_for("the number", || state.lock().unwrap().settings["f-number"].2 == "f/5,6");
    }
}
