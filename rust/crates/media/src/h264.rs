//! H.264 (Annex-B, in chunks of any size) to I420 (h264_reader.Decoder): on the graphics card when that's on and works,
//! else in software. A card decoder that errors, or takes in pieces of the stream and gives no picture for a moment, is swapped
//! for software on the spot without ending the stream, and not tried again this run. The stream is asked for a keyframe, so the
//! picture resumes right away instead of at the next one the phone was going to send.

use crate::ffmpeg::{Codec, Fault, Forced, Hw};
use crate::packets::{Steps, Tap};
use crate::slow::{self, Rate, Timings};
use crate::stamp::{self, Stamp};
use std::time::{Duration, Instant};
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use telescope_core::stats::SourceStats;
use telescope_core::clock::PhoneClock;
use telescope_core::frame::Frame;
use telescope_core::media::{Decoded, Decoder, DecodingStatus, Packet, TimingProbe};

/// Card decoders to try, best first: D3D11 works on any Windows card; VA-API covers AMD and Intel on Linux.
#[cfg(windows)]
const HW_TYPES: [Hw; 2] = [Hw::D3d11va, Hw::Cuda];
#[cfg(not(windows))]
const HW_TYPES: [Hw; 2] = [Hw::Vaapi, Hw::Cuda];

/// Pieces of the stream a card decoder may take in a row without a picture before software takes over: half a second at 30 fps.
const SILENT_UNITS: u32 = 15;
/// How long nothing is started beside the running decoder after the stream's conditions changed, so a view being moved doesn't restart it every frame.
const SETTLE: Duration = Duration::from_secs(3);

/// A card decoder errored this run: software from then on.
static HW_FAILED: AtomicBool = AtomicBool::new(false);
/// Streams trying software right now because their card looked too slow.
static TRYING: AtomicUsize = AtomicUsize::new(0);
/// What each live decoder uses ("vaapi", "processor"), counted.
static LIVE: Mutex<BTreeMap<&'static str, usize>> = Mutex::new(BTreeMap::new());

/// The card decoder this computer has, probed once by opening one.
pub fn hardware_device() -> Option<Hw> {
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    *found.get_or_insert_with(|| first_that_opens(|h| Codec::h264(Some(h)).is_ok()))
}

/// The probe's answer, forgotten when the frame steps move to another card so the next decoder looks again.
static FOUND: Mutex<Option<Option<Hw>>> = Mutex::new(None);

pub(crate) fn refresh_hardware_probe() {
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The first card decoder that opens, in the order they're tried. One this build or this computer doesn't have just doesn't open.
fn first_that_opens(opens: impl Fn(Hw) -> bool) -> Option<Hw> {
    HW_TYPES.into_iter().find(|&h| opens(h))
}

pub fn hardware_failed() -> bool {
    HW_FAILED.load(Ordering::SeqCst)
}

/// The card was too slow lately and software decodes until the wait is over, or is being tried to see whether it's better.
pub fn hardware_slow() -> bool {
    if TRYING.load(Ordering::SeqCst) > 0 {
        return true;
    }
    let mut all = POLICIES.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(who, _)| who.strong_count() > 0);
    all.iter().any(|(_, policy)| policy.lock().unwrap().cooldown.active())
}

/// Whether this Annex-B data holds an IDR picture, where a new decoder can start.
fn has_keyframe(data: &[u8]) -> bool {
    start_codes(data).into_iter().any(|(_, end)| data.get(end).is_some_and(|b| b & 31 == 5))
}

pub fn status() -> DecodingStatus {
    DecodingStatus {
        hardware_device: hardware_device().map(|h| h.name().to_string()),
        backends: LIVE.lock().unwrap().iter().filter(|(_, n)| **n > 0).map(|(k, _)| k.to_string()).collect(),
        hardware_failed: hardware_failed(),
        hardware_slow: hardware_slow(),
    }
}

fn backend(hw: Option<Hw>) -> &'static str {
    hw.map_or("processor", Hw::name)
}

fn count_live(hw: Option<Hw>, by: isize) {
    let mut live = LIVE.lock().unwrap();
    let n = live.entry(backend(hw)).or_insert(0);
    *n = n.saturating_add_signed(by);
}

/// Where each Annex-B start code is: (start, end) of the 00 00 01 or 00 00 00 01, left to right without overlap.
pub(crate) fn start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 2 < data.len() {
        if data[p] == 0 && data[p + 1] == 0 {
            if data[p + 2] == 1 {
                out.push((p, p + 3));
                p += 3;
                continue;
            }
            if data[p + 2] == 0 && p + 3 < data.len() && data[p + 3] == 1 {
                out.push((p, p + 4));
                p += 4;
                continue;
            }
        }
        p += 1;
    }
    out
}

/// Marks every plain Baseline SPS (profile 66) as Constrained Baseline, which FFmpeg's VA-API accepts and plain Baseline it refuses.
/// Phone encoders never use the Baseline-only tools (FMO, ASO, redundant slices), so the stream already is Constrained Baseline.
/// Only one bit of the byte after profile_idc changes: that can't make a 00 00 0x sequence, and the length stays the same.
fn mark_constrained_baseline(unit: &mut [u8]) {
    for (_, end) in start_codes(unit) {
        // NAL header, profile_idc, then the constraint flags
        if unit.len() > end + 2 && unit[end] & 31 == 7 && unit[end + 1] == 66 {
            unit[end + 2] |= 0x40;
        }
    }
}

/// An access unit delimiter, which ends the NAL unit before it and takes any stray bytes after it.
const END_OF_SETUP: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

/// The stream's SPS and PPS as last seen, kept so a fresh decoder or a new packet step can start from a keyframe.
#[derive(Default)]
struct Headers {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    tail: Vec<u8>,
}

impl Headers {
    fn remember(&mut self, data: &[u8]) {
        let data = [&self.tail[..], data].concat();
        let starts = start_codes(&data);
        for pair in starts.windows(2) {
            let (first, last) = (pair[0], pair[1]);
            if first.1 < last.0 {
                match data[first.1] & 31 {
                    7 => self.sps = Some(data[first.0..last.0].to_vec()),
                    8 => self.pps = Some(data[first.0..last.0].to_vec()),
                    _ => {}
                }
            }
        }
        // Keep split start codes and incomplete SPS/PPS, without holding on to ordinary video frames
        self.tail = match starts.last() {
            Some(&(s, e)) if e < data.len() && matches!(data[e] & 31, 7 | 8) => {
                let rest = &data[s..];
                rest[rest.len().saturating_sub(65536)..].to_vec()
            }
            _ => data[data.len().saturating_sub(4)..].to_vec(),
        };
    }

    /// The SPS and PPS closed off by a delimiter, so data that starts mid-picture can't end up stuck to the PPS.
    fn setup(&self) -> Vec<u8> {
        [self.sps.as_deref().unwrap_or_default(), self.pps.as_deref().unwrap_or_default(), END_OF_SETUP].concat()
    }
}

/// What the timing probe reads: the recent pictures' times and the decoder they came from.
struct Timed {
    times: Timings,
    backend: &'static str,
}

pub struct H264 {
    codec: Codec,
    headers: Headers,
    tap: Option<Arc<Tap>>,
    // Steps of its own: ones that fed another decoder would hold this one's keyframes back
    steps: Option<Steps>,
    /// The steps' generation last fed, so a step put on later gets SPS and PPS first.
    step_generation: u64,
    /// Decoded a frame: from then on a bad packet is the stream's fault, not the card's.
    hw_proven: bool,
    /// Pieces of the stream the card has taken since its last picture.
    silent: u32,
    /// How long its recent pictures took (decode and hand-over: on a card that includes the wait for the picture, which a card decoding in
    /// the background only does when the picture is copied off or kept there) and what it decodes on, shared with the timing probe.
    timings: Arc<Mutex<Timed>>,
    /// The frame rate the stream is meant to run at, which the stream changes while it runs.
    target_fps: Arc<AtomicU32>,
    /// What this stream has learned about its card: its waits are its own, so another stream's verdict never decides this one.
    policy: Arc<Mutex<Policy>>,
    /// Set while software decodes only because the card was too slow: the card to go back to once the wait is over.
    retry_hw: Option<Hw>,
    /// The running software decoder found errors in the piece it was last fed, which a card decoder started beside it can't tell for itself.
    piece_damaged: bool,
    /// After a trial or a candidate was cancelled because the stream's conditions changed: none starts again before this (a view being panned
    /// changes them every frame).
    settle_until: Option<Instant>,
    /// A decoder started beside the running one, which the running one keeps the picture going for until it has a picture of its own.
    candidate: Option<Candidate>,
    /// Set while software has the stream to see whether it's better than the card was: it keeps the stream only if it turns out so.
    trial: Option<Trial>,
    /// The size of the last picture the running decoder gave, which a trial's comparison is made at.
    last_size: Option<(u32, u32)>,
    /// New pictures a second that reached the virtual camera, over the last few seconds: the card's before a trial, software's during one.
    rate: Rate,
    /// Whether this decoder is counted in TRYING.
    counted: bool,
    /// Tells the stream to ask for a keyframe after a change of decoder.
    ask_keyframe: Option<Box<dyn Fn() + Send>>,
    /// The phone's clock offset, which turns the capture times in the stream into this computer's clock.
    clock: Option<Arc<PhoneClock>>,
    /// The crops a newer phone tags its pictures with, until each picture comes out.
    crops: stamp::CropTags,
    /// When the card gave up, until software has a picture.
    swapped_at: Option<Instant>,
    /// Whether the stream's frame steps start on the graphics card, which is what lets a decoded picture stay there (none: always in memory).
    #[cfg(surfaces)]
    surface_wish: Option<Arc<AtomicBool>>,
    /// Tests: the next feed fails as a broken driver would.
    #[cfg(test)]
    break_next: bool,
    /// Tests: the delays a software decoder that takes over gets, and how many pictures it refuses.
    #[cfg(test)]
    software: (Duration, Duration, u32),
    /// Tests: the delays a pretend card started beside the running decoder gets.
    #[cfg(test)]
    card: (Duration, Duration),
    /// Tests: the software decoder doesn't open.
    #[cfg(test)]
    no_software: bool,
    /// Tests: a decoder started beside the running one runs on a thread of its own, as it does for real (else it works through each piece at once).
    #[cfg(test)]
    threaded: bool,
    /// Tests: a decoder started beside the running one waits for this lock before each piece.
    #[cfg(test)]
    hold: Option<Arc<Mutex<()>>>,
}

/// What one stream has learned about its card.
struct Policy {
    /// How long software keeps the stream after beating the card, and then the card is tried again.
    cooldown: slow::Cooldown,
    /// How long a card that looked too slow, but beat software when tried, isn't judged on its speed again.
    lost: slow::Cooldown,
}

impl Policy {
    const fn new() -> Policy {
        Policy { cooldown: slow::Cooldown::new(), lost: slow::Cooldown::new() }
    }
}

/// Each stream's Policy, found again by the stream when its decoder is made anew (after a reconnect). A decoder made on a thread no stream
/// is bound to has one of its own.
static POLICIES: Mutex<Vec<(Weak<SourceStats>, Arc<Mutex<Policy>>)>> = Mutex::new(Vec::new());

fn stream_policy() -> Arc<Mutex<Policy>> {
    let Some(stream) = telescope_core::stats::current() else { return Arc::new(Mutex::new(Policy::new())) };
    let mut all = POLICIES.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(who, _)| who.strong_count() > 0);
    if let Some((_, policy)) = all.iter().find(|(who, _)| std::ptr::eq(who.as_ptr(), Arc::as_ptr(&stream))) {
        return policy.clone();
    }
    let policy = Arc::new(Mutex::new(Policy::new()));
    all.push((Arc::downgrade(&stream), policy.clone()));
    policy
}

/// How many times this thread's stream's frame steps have changed (tests: a count they move by hand).
fn steps_epoch() -> Option<u64> {
    #[cfg(test)]
    return Some(TEST_EPOCH.with(|d| d.get()));
    #[cfg(not(test))]
    telescope_core::stats::steps_epoch()
}

/// How many new pictures this thread's stream has sent to the virtual camera so far (tests: a count they move by hand).
fn delivered_total() -> Option<u64> {
    #[cfg(test)]
    return Some(TEST_DELIVERED.with(|d| d.get()));
    #[cfg(not(test))]
    telescope_core::stats::delivered_total()
}

#[cfg(test)]
thread_local! {
    static TEST_DELIVERED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static TEST_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What a stream's picture is compared under: a trial's verdict means nothing once any of these change.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Conditions {
    fps: u32,
    size: Option<(u32, u32)>,
    /// How often the stream's frame steps changed, which changes how many pictures get through.
    steps_epoch: u64,
}

impl Conditions {
    /// What changed from then to now, if anything (a size not known yet is no change).
    fn change_since(&self, then: &Conditions) -> Option<&'static str> {
        if self.fps != then.fps {
            return Some("the frame rate changed");
        }
        if self.steps_epoch != then.steps_epoch {
            return Some("the frame steps changed");
        }
        match (self.size, then.size) {
            (Some(now), Some(was)) if now != was => Some("the picture size changed"),
            _ => None,
        }
    }
}

/// What a decoder started beside the running one is for.
enum Plan {
    /// Software, to see whether it's better than the card: what the card took a picture and how many new pictures a second it delivered
    /// in the last few seconds before, and the conditions it was measured under.
    Trial { hw: Hw, card_ms: f64, card_rate: Option<f64>, conditions: Conditions },
    /// The card, which the stream goes back to.
    Card(Hw),
}

/// What a decoder started beside the running one made of the last piece of the stream it was given.
enum Report {
    /// There was no keyframe to start from: nothing was decoded.
    Waiting,
    /// Decoded, but no picture came out of it.
    Quiet,
    /// A picture came out, with no errors in it, and was taken off the decoder.
    Picture,
    /// A picture carried errors FFmpeg covered up, or a piece was refused: it starts over at the next keyframe.
    Damaged,
    /// The decoder itself is broken (a card that refuses the stream).
    Fault(String),
    /// The frame steps it was started with changed.
    Stale,
}

/// Lets a decoder started beside the running one start where a decoder can: at a whole access unit with an IDR picture in it.
struct Gate {
    waiting: bool,
    /// The stream's SPS and PPS as the skipped units showed them, put in front of the first unit it lets through.
    headers: Headers,
}

impl Gate {
    fn admit<'a>(&mut self, unit: &'a [u8]) -> Option<std::borrow::Cow<'a, [u8]>> {
        if !self.waiting {
            return Some(std::borrow::Cow::Borrowed(unit));
        }
        if !has_keyframe(unit) {
            self.headers.remember(unit);
            return None;
        }
        self.waiting = false;
        Some(std::borrow::Cow::Owned([self.headers.setup().as_slice(), unit].concat()))
    }
}

/// The decoding half of a decoder started beside the running one: on a thread of its own, so what it costs is never paid in a picture the running
/// decoder has ready.
struct Rig {
    codec: Codec,
    steps: Option<Steps>,
    step_generation: u64,
    gate: Gate,
    /// The crops its own pictures are tagged with, which become the running decoder's at the switch.
    crops: stamp::CropTags,
    #[cfg(test)]
    hold: Option<Arc<Mutex<()>>>,
}

impl Rig {
    /// Decodes a piece of the stream and takes its newest picture, and says how that went. damaged: the decoder that has the stream saw errors in
    /// this piece, which a card decoder (FFmpeg flags nothing on those) can't tell.
    fn work(&mut self, data: &[u8], damaged: bool) -> Report {
        #[cfg(test)]
        if let Some(hold) = &self.hold {
            drop(hold.lock().unwrap_or_else(|e| e.into_inner()));
        }
        if self.steps.as_ref().map_or(0, Steps::generation) != self.step_generation {
            return Report::Stale;
        }
        // Nothing before the keyframe is decoded, so whatever it refuses after that is its fault, or damage in the stream
        match decode(&mut self.codec, self.steps.as_ref(), data, true, None, true, &mut self.crops, Some(&mut self.gate)) {
            Ok(fed) => {
                if self.codec.take_damaged() || damaged {
                    self.gate.waiting = true;
                    Report::Damaged
                } else if self.gate.waiting {
                    Report::Waiting
                } else if fed.newest.is_some() {
                    Report::Picture
                } else {
                    Report::Quiet
                }
            }
            Err(why) if self.codec.hw.is_some() => Report::Fault(why),
            Err(_) => {
                self.codec.take_damaged();
                self.gate.waiting = true;
                Report::Damaged
            }
        }
    }
}

/// What a decoder started beside the running one is told.
enum Work {
    /// The next piece of the stream, and whether the running decoder found errors in it.
    Piece(Vec<u8>, bool),
    /// Whether the frame steps want pictures to stay on the card.
    #[cfg(surfaces)]
    Wish(bool),
}

/// How a Rig is run: on a thread, which is how it runs for real, or (tests) at once on the caller's.
enum Engine {
    Thread { send: Option<mpsc::Sender<Work>>, reports: mpsc::Receiver<Report>, thread: Option<std::thread::JoinHandle<Rig>>, stop: Arc<AtomicBool> },
    #[cfg(test)]
    Inline { rig: Option<Box<Rig>>, reports: VecDeque<Report> },
}

impl Engine {
    fn start(rig: Rig, #[cfg(test)] threaded: bool) -> Result<Engine, String> {
        #[cfg(test)]
        if !threaded {
            return Ok(Engine::Inline { rig: Some(Box::new(rig)), reports: VecDeque::new() });
        }
        let (send, work) = mpsc::channel::<Work>();
        let (report, reports) = mpsc::channel();
        let mut rig = rig;
        let stop = Arc::new(AtomicBool::new(false));
        let quit = stop.clone();
        let thread = std::thread::Builder::new()
            .name("decoder-beside".into())
            .spawn(move || {
                while let Ok(job) = work.recv() {
                    // A decoder given up on drops what is queued instead of decoding it
                    if quit.load(Ordering::Relaxed) {
                        break;
                    }
                    match job {
                        Work::Piece(data, damaged) => {
                            if report.send(rig.work(&data, damaged)).is_err() {
                                break;
                            }
                        }
                        #[cfg(surfaces)]
                        Work::Wish(wish) => rig.codec.surfaces(wish),
                    }
                }
                rig
            })
            .map_err(|e| format!("no thread for it: {e}"))?;
        Ok(Engine::Thread { send: Some(send), reports, thread: Some(thread), stop })
    }

    fn send(&mut self, job: Work) {
        match self {
            Engine::Thread { send, .. } => {
                if let Some(send) = send {
                    // A thread that has gone shows as missing reports
                    let _ = send.send(job);
                }
            }
            #[cfg(test)]
            Engine::Inline { rig, reports } => {
                if let Some(rig) = rig {
                    match job {
                        Work::Piece(data, damaged) => reports.push_back(rig.work(&data, damaged)),
                        #[cfg(surfaces)]
                        Work::Wish(wish) => rig.codec.surfaces(wish),
                    }
                }
            }
        }
    }

    /// What it reported since the last call, or None when its thread is gone.
    fn collect(&mut self) -> Option<Vec<Report>> {
        match self {
            Engine::Thread { reports, .. } => {
                let mut out = Vec::new();
                loop {
                    match reports.try_recv() {
                        Ok(r) => out.push(r),
                        Err(mpsc::TryRecvError::Empty) => return Some(out),
                        Err(mpsc::TryRecvError::Disconnected) => return if out.is_empty() { None } else { Some(out) },
                    }
                }
            }
            #[cfg(test)]
            Engine::Inline { reports, .. } => Some(reports.drain(..).collect()),
        }
    }

    /// The decoder back, once it is done with what it was given.
    fn finish(&mut self) -> Option<Rig> {
        match self {
            Engine::Thread { send, thread, .. } => {
                drop(send.take());
                thread.take()?.join().ok()
            }
            #[cfg(test)]
            Engine::Inline { rig, .. } => rig.take().map(|rig| *rig),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        #[allow(irrefutable_let_patterns)]
        if let Engine::Thread { stop, .. } = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// A decoder started beside the running one and fed the same stream. The stream moves to it only when it has given a picture with no errors in
/// it, from a keyframe it started at, and has caught up with the running decoder; until then the running decoder keeps the picture going,
/// so a switch never leaves a gap, a dark frame or a picture shown twice.
struct Candidate {
    engine: Engine,
    hw: Option<Hw>,
    plan: Plan,
    began: Instant,
    /// Given the stream's SPS and PPS ahead of the first of what it is fed.
    primed: bool,
    /// Pieces of the stream it was given, and how many it has reported on.
    sent: u64,
    reported: u64,
    /// Has been given a keyframe, whatever came of it: without one, giving no picture is the phone's lateness and not the decoder's fault.
    keyframe_seen: bool,
    /// Decoding from a keyframe with no errors so far.
    started: bool,
    /// Gave a picture with no errors since it started.
    ready: bool,
    /// Pictures it has given, and when the first came (for how fast it decodes once going).
    pictures: u64,
    first_picture: Option<Instant>,
}

/// Software has the stream to be judged against the card that looked too slow.
struct Trial {
    hw: Hw,
    card_ms: f64,
    card_rate: Option<f64>,
    /// When software got the stream.
    began: Instant,
    /// What the comparison was made under, from before software started.
    conditions: Conditions,
    step_generation: u64,
}

impl H264 {
    /// On the card when hardware is asked for, one opens and none failed this run; software otherwise.
    pub fn new(hardware: bool, tap: Option<Arc<Tap>>) -> Result<H264, String> {
        H264::opening(hardware, hardware_device, Codec::h264, tap)
    }

    /// new, with how a computer finds its card decoder and opens a codec.
    fn opening(hardware: bool, device: impl FnOnce() -> Option<Hw>, open: impl Fn(Option<Hw>) -> Result<Codec, String>, tap: Option<Arc<Tap>>) -> Result<H264, String> {
        if hardware && hardware_failed() {
            log::info!("Decoding this stream in software: the graphics card failed earlier this run");
        }
        let policy = stream_policy();
        let mut wanted = if hardware && !hardware_failed() { device() } else { None };
        // The card was too slow lately: software until the wait is over, and then the card again
        let mut retry_hw = None;
        if let Some(h) = wanted {
            let now = slow::now();
            let mut policy = policy.lock().unwrap();
            if policy.cooldown.waiting(now) {
                log::info!("Decoding this stream in software: the graphics card was too slow lately");
                retry_hw = wanted.take();
            } else if policy.cooldown.active() {
                policy.cooldown.returned(now);
                log::info!("Going back to hardware decoding ({}): the wait after it was too slow is over", h.name());
            }
        }
        let opened = wanted.map(|h| {
            let forced = matches!(Forced::asked(), Some((Forced::Open, _)));
            open(Some(h)).and_then(|c| if forced { Err("forced by TELESCOPE_HW_FAULT".to_string()) } else { Ok(c) }).map_err(|why| (h, why))
        });
        let codec = match opened {
            Some(Ok(c)) => c,
            Some(Err((h, why))) => {
                HW_FAILED.store(true, Ordering::SeqCst);
                log::warn!("Hardware decoding ({}) didn't start ({why}); decoding in software from now on", h.name());
                open(None)?
            }
            None => {
                if hardware && !hardware_failed() && retry_hw.is_none() {
                    log::info!("Decoding this stream in software: no graphics card decoder here");
                }
                open(None)?
            }
        };
        let mut dec = H264::with_codec(codec, tap);
        dec.policy = policy;
        dec.retry_hw = retry_hw;
        Ok(dec)
    }

    fn with_codec(codec: Codec, tap: Option<Arc<Tap>>) -> H264 {
        let codec_hw = codec.hw;
        count_live(codec.hw, 1);
        let steps = tap.as_ref().map(|t| t.open());
        let step_generation = steps.as_ref().map_or(0, Steps::generation);
        H264 {
            codec,
            headers: Headers::default(),
            tap,
            steps,
            step_generation,
            hw_proven: false,
            silent: 0,
            timings: Arc::new(Mutex::new(Timed { times: Timings::default(), backend: backend(codec_hw) })),
            target_fps: Arc::new(AtomicU32::new(30)),
            policy: stream_policy(),
            retry_hw: None,
            piece_damaged: false,
            settle_until: None,
            candidate: None,
            trial: None,
            last_size: None,
            rate: Rate::default(),
            counted: false,
            ask_keyframe: None,
            clock: None,
            crops: stamp::CropTags::default(),
            swapped_at: None,
            #[cfg(surfaces)]
            surface_wish: None,
            #[cfg(test)]
            break_next: false,
            #[cfg(test)]
            software: Default::default(),
            #[cfg(test)]
            card: Default::default(),
            #[cfg(test)]
            no_software: false,
            #[cfg(test)]
            threaded: false,
            #[cfg(test)]
            hold: None,
        }
    }

    /// The card it decodes on, or None for software.
    pub fn hw(&self) -> Option<Hw> {
        self.codec.hw
    }

    /// The newest frame the data completed (older ones are dropped to keep latency down after a stall) and how many it completed.
    pub fn feed(&mut self, data: &[u8]) -> (Option<Frame>, u32) {
        self.feed_with(data, true)
    }

    /// feed; take false decodes the data but leaves the pictures on the decoder (nobody looks at them), so none is copied or converted.
    fn feed_with(&mut self, data: &[u8], take: bool) -> (Option<Frame>, u32) {
        self.poll_candidate();
        let out = self.feed_running(data, take);
        self.advance(data, take, out)
    }

    /// Feeds the decoder that has the stream.
    fn feed_running(&mut self, data: &[u8], take: bool) -> (Option<Frame>, u32) {
        self.headers.remember(data);
        let mut data = std::borrow::Cow::Borrowed(data);
        if let Some(steps) = &self.steps {
            let generation = steps.generation();
            if generation != self.step_generation {
                self.step_generation = generation;
                // TCP sends SPS and PPS only when joining, and a step put on later needs them
                if steps.on() {
                    data = [self.headers.setup(), data.into_owned()].concat().into();
                }
            }
        }
        // Joined the stream before its headers: nothing can decode that, so it says nothing about the card
        let strict = self.codec.hw.is_some() && !self.hw_proven && self.headers.sps.is_some() && self.headers.pps.is_some();
        #[cfg(test)]
        let result = if std::mem::take(&mut self.break_next) {
            Err("the driver failed after receiving setup".to_string())
        } else {
            decode(&mut self.codec, self.steps.as_ref(), &data, strict, self.clock.as_deref(), take, &mut self.crops, None)
        };
        #[cfg(not(test))]
        let result = decode(&mut self.codec, self.steps.as_ref(), &data, strict, self.clock.as_deref(), take, &mut self.crops, None);
        // A picture left on the decoder was never waited for (a card decodes in the background until its picture is taken), so it says nothing about keeping up
        let timed = |fed: &Fed| if take { fed.count } else { 0 };
        match result {
            Ok(fed) if self.codec.hw.is_none() => {
                // Read every feed, so what a card candidate is told is about this piece only
                self.piece_damaged = self.codec.take_damaged();
                self.note_picture_back(fed.count);
                self.time(fed.took, timed(&fed));
                (fed.newest, fed.count)
            }
            Ok(fed) => {
                self.silent = if fed.count > 0 { 0 } else { self.silent + fed.units };
                self.hw_proven |= fed.count > 0;
                if self.silent >= SILENT_UNITS {
                    let why = format!("no picture from the last {} pieces of the stream", self.silent);
                    return self.fall_back(why, &data);
                }
                if let Some(slow) = self.time(fed.took, timed(&fed)) {
                    // The card's picture is good, so it keeps the stream while software is tried beside it
                    if self.candidate.is_none() && self.settled(slow::now()) {
                        self.begin_software_trial(slow);
                    }
                }
                (fed.newest, fed.count)
            }
            Err(why) if self.codec.hw.is_none() => {
                log::warn!("H.264 decoding failed ({why}); skipping this data");
                telescope_core::stats::note_drop(telescope_core::stats::DropReason::DecoderFailed, 1);
                (None, 0)
            }
            Err(why) => self.fall_back(why, &data),
        }
    }

    /// After every feed, however it went: judges software if it has the stream on trial, starts going back to the card when that is due,
    /// and hands the decoder started beside the running one the data. out is what the running decoder gave, which goes out as it is.
    fn advance(&mut self, data: &[u8], take: bool, out: (Option<Frame>, u32)) -> (Option<Frame>, u32) {
        let now = slow::now();
        if let Some(f) = out.0.as_ref() {
            self.last_size = Some((f.width, f.height));
        }
        if take {
            if let Some(total) = delivered_total() {
                self.rate.record(now, total);
            }
        } else {
            // Nobody sees the pictures, so none is delivered, and a decoder beside this one would miss them: that says nothing about either
            self.rate = Rate::default();
            self.cancel_trial("nobody is looking at the stream");
            if let Some(cand) = self.candidate.take() {
                self.drop_candidate(cand, "nobody is looking at the stream");
            }
        }
        self.judge_trial(now);
        if take {
            self.start_card_candidate(now);
            let damaged = std::mem::take(&mut self.piece_damaged);
            self.feed_candidate(data, damaged);
        }
        out
    }

    /// Swaps the card for software mid-stream and retries data on it.
    fn fall_back(&mut self, why: String, data: &[u8]) -> (Option<Frame>, u32) {
        let hw = self.codec.hw.expect("only a card decoder falls back");
        if let Some(cand) = self.candidate.take() {
            self.drop_candidate(cand, "the card failed");
        }
        // A decoder on the picked card's render node can reject a stream the default node takes: software meanwhile, the default node at the next keyframe
        if crate::decode_card::retry_on_default_node(hw == Hw::Vaapi) {
            log::warn!("Hardware decoding ({}) failed on the picked card ({why}); trying the default render node at the next keyframe", hw.name());
            let fed = self.swap_to_software(hw, data);
            if self.codec.hw.is_none() {
                self.retry_hw = Some(hw);
            }
            return fed;
        }
        HW_FAILED.store(true, Ordering::SeqCst);
        log::warn!("Hardware decoding ({}) failed ({why}); decoding in software from now on", hw.name());
        self.swap_to_software(hw, data)
    }

    /// Whether a trial or a card candidate may start now, after the stream's conditions changed lately.
    fn settled(&self, now: Instant) -> bool {
        self.settle_until.is_none_or(|until| now >= until)
    }

    /// The stream's conditions changed under a trial or candidate: nothing starts again for a few seconds.
    fn settle(&mut self) {
        self.settle_until = Some(slow::now() + SETTLE);
    }

    /// What the stream is being compared under now.
    fn conditions(&self) -> Conditions {
        Conditions { fps: self.target_fps.load(Ordering::Relaxed), size: self.last_size, steps_epoch: steps_epoch().unwrap_or(0) }
    }

    /// Starts software beside the card because the card looks too slow. The card keeps the stream until software has a clean picture and has
    /// caught up, and then software is timed against it (judge_trial) and keeps the stream only if it turns out better. The card isn't called
    /// failed either way.
    fn begin_software_trial(&mut self, slow: Slowness) {
        let hw = self.codec.hw.expect("only a card decoder falls back");
        let now = slow::now();
        log::warn!("Hardware decoding ({}) is {}; starting software decoding beside it to see whether it's better", hw.name(), slow.why);
        // Everything the verdict compares is read here, before software starts
        let plan = Plan::Trial { hw, card_ms: slow.card_ms, card_rate: self.rate.per_second(now), conditions: self.conditions() };
        match self.open_beside(None).and_then(|codec| self.start_candidate(codec, plan)) {
            Ok(()) => self.begin_trying(),
            Err(why) => {
                // Without a pause the next picture would try again and again
                let wait = self.policy.lock().unwrap().lost.fell(now);
                log::warn!("The software decoder didn't open ({why}); staying on the card, whose speed isn't judged again for {} minutes", wait.as_secs() / 60);
            }
        }
    }

    /// Opens a decoder to run beside the one that has the stream (None: software).
    fn open_beside(&self, hw: Option<Hw>) -> Result<Codec, String> {
        #[cfg(not(test))]
        let opened = Codec::h264(hw);
        // Tests: the pretend card is software opened as a card
        #[cfg(test)]
        let opened = Codec::h264(None).and_then(|mut c| {
            if hw.is_none() && self.no_software {
                return Err("forced by the test".to_string());
            }
            c.hw = hw;
            (c.delays, c.refusals) = if hw.is_some() { (self.card, 0) } else { ((self.software.0, self.software.1), self.software.2) };
            Ok(c)
        });
        #[allow(unused_mut)]
        let mut codec = opened?;
        // The pool a card makes at its first picture is sized by this, so it is set before the first feed
        #[cfg(surfaces)]
        codec.surfaces(self.wish());
        Ok(codec)
    }

    fn start_candidate(&mut self, codec: Codec, plan: Plan) -> Result<(), String> {
        let steps = self.tap.as_ref().map(|t| t.open());
        let step_generation = steps.as_ref().map_or(0, Steps::generation);
        let hw = codec.hw;
        let rig = Rig {
            codec,
            steps,
            step_generation,
            gate: Gate { waiting: true, headers: Headers::default() },
            crops: stamp::CropTags::default(),
            #[cfg(test)]
            hold: self.hold.clone(),
        };
        let engine = Engine::start(rig, #[cfg(test)] self.threaded)?;
        // Without this the picture waits for the phone's next keyframe, up to ten seconds with intra refresh
        if let Some(ask) = &self.ask_keyframe {
            ask();
        }
        self.candidate = Some(Candidate { engine, hw, plan, began: slow::now(), primed: false, sent: 0, reported: 0, keyframe_seen: false, started: false, ready: false, pictures: 0, first_picture: None });
        Ok(())
    }

    /// Hands the decoder started beside the running one the data the running one has just had, to work on in its own time.
    fn feed_candidate(&mut self, data: &[u8], damaged: bool) {
        let Some(cand) = self.candidate.as_mut() else { return };
        let data = if cand.primed {
            data.to_vec()
        } else {
            cand.primed = true;
            [self.headers.setup().as_slice(), data].concat()
        };
        cand.sent += 1;
        cand.engine.send(Work::Piece(data, damaged));
    }

    /// Before each feed: reads what the decoder started beside the running one has made of its pieces, and moves the stream to it once it has
    /// given a clean picture from a keyframe and has seen everything the running decoder has. Nothing is exchanged then, so no picture is shown
    /// twice, goes back or comes from a decoder that missed some of the stream.
    fn poll_candidate(&mut self) {
        let Some(mut cand) = self.candidate.take() else { return };
        let now = slow::now();
        let Some(reports) = cand.engine.collect() else {
            self.candidate_failed(cand, "its thread stopped");
            return;
        };
        for report in reports {
            cand.reported += 1;
            cand.keyframe_seen |= !matches!(report, Report::Waiting);
            match report {
                Report::Waiting | Report::Damaged => (cand.started, cand.ready) = (false, false),
                Report::Quiet => cand.started = true,
                Report::Picture => {
                    (cand.started, cand.ready) = (true, true);
                    cand.pictures += 1;
                    cand.first_picture.get_or_insert(now);
                }
                Report::Fault(why) => {
                    self.candidate_failed(cand, &why);
                    return;
                }
                Report::Stale => {
                    self.settle();
                    self.drop_candidate(cand, "the frame steps changed");
                    return;
                }
            }
        }
        if let Plan::Trial { conditions, .. } = &cand.plan {
            if let Some(why) = self.conditions().change_since(conditions) {
                self.settle();
                self.drop_candidate(cand, why);
                return;
            }
        }
        if cand.ready && cand.reported == cand.sent {
            self.take_over(cand, now);
            return;
        }
        // Software that decodes but can't keep up with the stream never catches up to take over: that is its answer, and waiting out the limit only burns processor
        if let (Plan::Trial { .. }, Some(first)) = (&cand.plan, cand.first_picture) {
            let (fed, span) = (now.saturating_duration_since(cand.began), now.saturating_duration_since(first));
            if slow::falling_behind(cand.pictures, span, cand.sent, fed, cand.sent - cand.reported) {
                self.candidate_slower(cand, span, fed);
                return;
            }
        }
        if now.saturating_duration_since(cand.began) >= slow::START_LIMIT {
            if let (Plan::Trial { .. }, Some(first)) = (&cand.plan, cand.first_picture) {
                // It did give pictures, just not enough: that is a speed, and not a broken decoder
                let (fed, span) = (now.saturating_duration_since(cand.began), now.saturating_duration_since(first));
                self.candidate_slower(cand, span, fed);
                return;
            }
            let limit = slow::START_LIMIT.as_secs();
            // Software that was given a keyframe and made nothing of it has lost its trial. A card that decoded from one and gave no picture has failed;
            // one whose keyframe was damaged or never came says nothing about the card, and another is started
            let blame = match cand.plan {
                Plan::Trial { .. } => cand.keyframe_seen,
                // A card that never answered some of what it was given is stuck in its driver, and is not tried again either
                Plan::Card(_) => cand.started || cand.reported < cand.sent,
            };
            if blame {
                self.candidate_failed(cand, &format!("no picture within {limit} s"));
            } else {
                self.drop_candidate(cand, &format!("no clean keyframe to start from within {limit} s"));
            }
            return;
        }
        self.candidate = Some(cand);
    }

    /// The candidate has had every piece of the stream since a keyframe and its last picture was clean: it has the stream from now on, and the
    /// decoder it replaces is closed.
    fn take_over(&mut self, mut cand: Candidate, now: Instant) {
        let Some(rig) = cand.engine.finish() else {
            self.candidate_failed(cand, "its thread stopped");
            return;
        };
        let (from, to) = (self.codec.hw, cand.hw);
        let after = now.saturating_duration_since(cand.began).as_millis();
        count_live(from, -1);
        count_live(to, 1);
        self.codec = rig.codec;
        self.steps = rig.steps;
        self.step_generation = rig.step_generation;
        // The candidate read the tags of every picture it decoded, and the old decoder's pending ones belong to pictures that are gone
        self.crops = rig.crops;
        #[cfg(surfaces)]
        {
            let wish = self.wish();
            self.codec.surfaces(wish);
        }
        self.hw_proven = to.is_some();
        self.silent = 0;
        *self.timings.lock().unwrap() = Timed { times: Timings::default(), backend: backend(to) };
        self.rate = Rate::default();
        if let Some(total) = delivered_total() {
            self.rate.record(now, total);
        }
        match cand.plan {
            Plan::Trial { hw, card_ms, card_rate, conditions } => {
                log::info!("Software decoding has the picture after {after} ms; timing it for {} s against {}", slow::WINDOW.as_secs(), hw.name());
                let step_generation = self.steps.as_ref().map_or(0, Steps::generation);
                self.trial = Some(Trial { hw, card_ms, card_rate, began: now, conditions, step_generation });
            }
            Plan::Card(hw) => {
                self.retry_hw = None;
                log::info!("Going back to hardware decoding ({}), which has the picture after {after} ms", hw.name());
            }
        }
    }

    /// Software decoded pictures beside the card but slower than the stream came: the card keeps the stream, with the same wait as when software wasn't better.
    fn candidate_slower(&mut self, cand: Candidate, span: Duration, fed: Duration) {
        let Plan::Trial { hw, .. } = cand.plan else { return };
        self.end_trying();
        let wait = self.policy.lock().unwrap().lost.fell(slow::now());
        let per_second = |n: u64, d: Duration| if d.is_zero() { 0.0 } else { n as f64 / d.as_secs_f64() };
        log::warn!(
            "Software decoding was slower than real time beside {} ({:.1} pictures a second against {:.1} coming in); staying on the card, whose speed isn't judged again for {} minutes",
            hw.name(),
            per_second(cand.pictures.saturating_sub(1), span),
            per_second(cand.sent, fed),
            wait.as_secs() / 60
        );
    }

    /// The candidate errored or gave no picture although it had a keyframe: the running decoder keeps the stream.
    fn candidate_failed(&mut self, cand: Candidate, why: &str) {
        match cand.plan {
            Plan::Trial { hw, .. } => {
                self.end_trying();
                let wait = self.policy.lock().unwrap().lost.fell(slow::now());
                log::warn!("Software decoding beside {} gave no picture ({why}); staying on the card, whose speed isn't judged again for {} minutes", hw.name(), wait.as_secs() / 60);
            }
            Plan::Card(hw) => {
                // A decoder on the picked card's render node can reject a stream the default node takes: the next candidate is on the default node
                if crate::decode_card::retry_on_default_node(hw == Hw::Vaapi) {
                    log::warn!("Hardware decoding ({}) failed on the picked card ({why}); trying the default render node", hw.name());
                    return;
                }
                HW_FAILED.store(true, Ordering::SeqCst);
                self.retry_hw = None;
                log::warn!("Hardware decoding ({}) didn't start again ({why}); decoding in software from now on", hw.name());
            }
        }
    }

    /// The candidate isn't wanted any more, for a reason that isn't its fault: the card is still to be tried again, and a trial is no verdict.
    fn drop_candidate(&mut self, cand: Candidate, why: &str) {
        match cand.plan {
            Plan::Trial { hw, .. } => {
                log::info!("Software trial beside {} cancelled: {why}", hw.name());
                self.end_trying();
            }
            Plan::Card(hw) => log::info!("Hardware decoding ({}) is started again later: {why}", hw.name()),
        }
    }

    /// Once software has been timed for a whole window: it keeps the stream for a while if it's quicker than the card was and also gets more new
    /// pictures to the camera, and otherwise the card has it back and isn't judged on its speed for a while. A trial that outlasts its limit,
    /// whatever the feeds did meanwhile (and however late a feed that would have qualified came), ends that way too, and one whose conditions
    /// changed is cancelled without a verdict.
    fn judge_trial(&mut self, now: Instant) {
        let Some(trial) = self.trial.as_ref() else { return };
        let changed = self.conditions().change_since(&trial.conditions).or_else(|| {
            (self.steps.as_ref().map_or(0, Steps::generation) != trial.step_generation).then_some("the frame steps changed")
        });
        let (hw, card_ms, card_rate, began) = (trial.hw, trial.card_ms, trial.card_rate, trial.began);
        if let Some(why) = changed {
            self.settle();
            self.cancel_trial(why);
            return;
        }
        let rate = self.rate.per_second(now);
        let (software_ms, took) = if now.saturating_duration_since(began) >= slow::TRIAL_LIMIT {
            // Software has had long enough to show anything, and what comes after that is too late to count
            (f64::INFINITY, format!("no steady picture within {} s", slow::TRIAL_LIMIT.as_secs()))
        } else {
            let window = {
                let timed = self.timings.lock().unwrap();
                timed.times.per_picture(now).map(|ms| (ms, timed.times.stalled()))
            };
            match (window, rate) {
                (Some((_, Some(worst))), _) => (f64::INFINITY, format!("stalled with pictures up to {worst:.0} ms")),
                (Some((ms, None)), Some(_)) => (ms, format!("{ms:.1} ms a picture")),
                _ => return,
            }
        };
        self.trial = None;
        self.end_trying();
        self.retry_hw = Some(hw);
        let shown = |r: Option<f64>| r.map_or("an unknown number of".to_string(), |r| format!("{r:.1}"));
        let against = format!("{took} and {} new pictures a second, against {card_ms:.1} ms and {} on {}", shown(rate), shown(card_rate), hw.name());
        let mut policy = self.policy.lock().unwrap();
        if slow::software_quicker(card_ms, software_ms) && slow::delivers_more(card_rate, rate) {
            let wait = policy.cooldown.fell(now);
            log::warn!("Software decoding is better ({against}); decoding in software for {} minutes, then trying the card again", wait.as_secs() / 60);
            return;
        }
        let wait = policy.lost.fell(now);
        log::warn!("Software decoding was no better ({against}); back to the card, and its speed isn't judged again for {} minutes", wait.as_secs() / 60);
    }

    /// Software had the stream on trial and the trial can't be judged: the card has it back, with no verdict held against it.
    fn cancel_trial(&mut self, why: &str) {
        let Some(trial) = self.trial.take() else { return };
        log::info!("Software trial cancelled: {why}; back to the card");
        self.end_trying();
        self.retry_hw = Some(trial.hw);
    }

    /// Starts the card beside software when it's to go back: at once when no wait runs, or once the wait after it was too slow is over and
    /// software kept up meanwhile. Software keeps the stream until the card has a picture.
    fn start_card_candidate(&mut self, now: Instant) {
        let Some(hw) = self.retry_hw else { return };
        if self.trial.is_some() || self.candidate.is_some() || self.codec.hw.is_some() || !self.settled(now) {
            return;
        }
        let struggling = {
            let interval = 1000.0 / f64::from(self.target_fps.load(Ordering::Relaxed).max(1));
            let timed = self.timings.lock().unwrap();
            timed.times.slow(now, interval).is_some() || timed.times.stalled().is_some()
        };
        {
            let mut policy = self.policy.lock().unwrap();
            if policy.cooldown.waiting(now) {
                return;
            }
            if policy.cooldown.active() {
                if struggling {
                    log::info!("Staying in software a while longer: it's struggling with the stream too");
                    policy.cooldown.extend(now);
                    return;
                }
                policy.cooldown.returned(now);
            }
        }
        match self.open_beside(Some(hw)).and_then(|codec| self.start_candidate(codec, Plan::Card(hw))) {
            Ok(()) => {}
            Err(why) => {
                HW_FAILED.store(true, Ordering::SeqCst);
                self.retry_hw = None;
                log::warn!("Hardware decoding ({}) didn't start again ({why}); decoding in software from now on", hw.name());
            }
        }
    }

    fn begin_trying(&mut self) {
        if !std::mem::replace(&mut self.counted, true) {
            TRYING.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn end_trying(&mut self) {
        if std::mem::take(&mut self.counted) {
            TRYING.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Whether the stream's frame steps want pictures to stay on the card.
    #[cfg(surfaces)]
    fn wish(&self) -> bool {
        self.surface_wish.as_ref().is_some_and(|w| w.load(Ordering::Relaxed))
    }

    /// Passes the wish on to the running decoder and the one beside it (which gets it again if it takes over).
    #[cfg(surfaces)]
    fn apply_wish(&mut self) {
        let wish = self.wish();
        self.codec.surfaces(wish);
        if let Some(cand) = self.candidate.as_mut() {
            cand.engine.send(Work::Wish(wish));
        }
    }

    /// Steps of its own for a new decoder: the old ones have already seen this data.
    fn restart_steps(&mut self) {
        if let Some(tap) = &self.tap {
            let steps = tap.open();
            self.step_generation = steps.generation();
            self.steps = Some(steps);
        }
    }

    fn swap_to_software(&mut self, hw: Hw, data: &[u8]) -> (Option<Frame>, u32) {
        let codec = match Codec::h264(None) {
            #[cfg(test)]
            Ok(mut c) => {
                (c.delays, c.refusals) = ((self.software.0, self.software.1), self.software.2);
                c
            }
            #[cfg(not(test))]
            Ok(c) => c,
            Err(e) => {
                log::error!("The software decoder didn't open either: {e}");
                return (None, 0);
            }
        };
        count_live(Some(hw), -1);
        count_live(None, 1);
        self.codec = codec;
        *self.timings.lock().unwrap() = Timed { times: Timings::default(), backend: backend(None) };
        self.restart_steps();
        // Without this the picture waits for the phone's next keyframe, up to ten seconds with intra refresh
        if let Some(ask) = &self.ask_keyframe {
            ask();
        }
        // A fresh decoder can't start from a keyframe alone, and TCP sends SPS and PPS only when joining
        let setup = [&self.headers.setup(), data].concat();
        self.swapped_at = Some(Instant::now());
        let fed = decode(&mut self.codec, self.steps.as_ref(), &setup, false, self.clock.as_deref(), true, &mut self.crops, None).map_or((None, 0), |fed| (fed.newest, fed.count));
        self.note_picture_back(fed.1);
        fed
    }

    /// Notes what each picture this data finished took to decode and hand over (not the packet steps, which cost the same on any decoder),
    /// and says how the card is too slow for the stream if it has been for a whole window and software didn't lose to it lately.
    fn time(&mut self, took: Duration, count: u32) -> Option<Slowness> {
        if count == 0 {
            return None;
        }
        let now = slow::now();
        let each = took.as_secs_f64() * 1000.0 / f64::from(count);
        let fps = self.target_fps.load(Ordering::Relaxed).max(1);
        let interval = 1000.0 / f64::from(fps);
        let slow = {
            let mut timed = self.timings.lock().unwrap();
            // The first picture of a decoder pays for its setup, which says nothing about the stream
            let first = timed.times.recent().is_none();
            for _ in 0..count.min(8) {
                timed.times.record(now, each);
            }
            if !first {
                timed.times.note_hiccup(now, each);
            }
            let hw = self.codec.hw?;
            if let Some(worst) = timed.times.stalled() {
                // Software has to keep up to beat a card that stalls
                let card_ms = timed.times.recent().map_or(interval, |s| s.median_ms.max(interval));
                let why = format!("stalled: pictures took up to {worst:.0} ms, more than 500 ms once or 200 ms twice within {} s, on {}", slow::STALL_WINDOW.as_secs(), hw.name());
                Slowness { why, card_ms }
            } else {
                let s = timed.times.slow(now, interval)?;
                let why = format!(
                    "too slow for {} fps: a median of {:.1} ms, p95 {:.1} ms, max {:.1} ms a picture over the last {} s, against {interval:.1} ms between pictures on {}",
                    fps, s.median_ms, s.p95_ms, s.max_ms, slow::WINDOW.as_secs(), hw.name()
                );
                Slowness { why, card_ms: s.median_ms }
            }
        };
        // Software can only beat the card on the pictures that reach the camera, so there has to be a rate to beat
        self.rate.per_second(now)?;
        let mut policy = self.policy.lock().unwrap();
        if policy.lost.waiting(now) {
            return None;
        }
        if policy.lost.active() {
            policy.lost.returned(now);
        }
        Some(slow)
    }

    fn note_picture_back(&mut self, count: u32) {
        if count > 0 {
            if let Some(at) = self.swapped_at.take() {
                log::info!("Software decoding has the picture back after {} ms", at.elapsed().as_millis());
            }
        }
    }
}

impl Drop for H264 {
    fn drop(&mut self) {
        count_live(self.codec.hw, -1);
        let trying = self.trial.is_some() || matches!(self.candidate, Some(Candidate { plan: Plan::Trial { .. }, .. }));
        if trying {
            log::info!("Software trial cancelled: the stream ended before a verdict");
        }
        self.end_trying();
    }
}

/// How a card is too slow: why, for the log, and the time a picture software has to clearly beat to take over.
struct Slowness {
    why: String,
    card_ms: f64,
}

/// What feeding the decoder gave: the newest frame, how many finished and how many access units went in.
struct Fed {
    newest: Option<Frame>,
    count: u32,
    units: u32,
    /// How long decoding and handing over the newest picture took (the copy off a card, or keeping it there): the packet steps are not in it.
    took: Duration,
}

/// Parses data into access units, runs them through the packet steps and decodes them. strict: data the decoder
/// refuses is the decoder's fault (a card that can't take the stream), not something to skip. Each access unit's capture time
/// (the SEI the phone puts in front of a picture) is read before the steps change the unit and goes with it as the packet's pts, so the
/// picture that comes out has its own time however the decoder delays or reorders; clock turns it into this computer's.
fn decode(codec: &mut Codec, steps: Option<&Steps>, data: &[u8], strict: bool, clock: Option<&PhoneClock>, take: bool, crops: &mut stamp::CropTags, mut gate: Option<&mut Gate>) -> Result<Fed, String> {
    let mut newest = None;
    let mut count = 0;
    let mut units_in = 0;
    let mut took = Duration::ZERO;
    let mut fault = None;
    let steps = steps.filter(|s| s.on());
    codec.parse(data, |codec, unit| {
        if fault.is_some() {
            return;
        }
        // A decoder that has to start at a keyframe skips everything before one
        let unit = match gate.as_mut() {
            Some(gate) => match gate.admit(unit) {
                Some(unit) => unit,
                None => return,
            },
            None => std::borrow::Cow::Borrowed(unit),
        };
        let unit = &unit[..];
        let (found, crop) = stamp::read(unit);
        let pts = found.and_then(Stamp::to_pts);
        crops.note(pts, crop);
        let units = match steps {
            Some(s) => s.step(unit),
            None => vec![unit.to_vec()],
        };
        for mut u in units {
            if codec.hw.is_some() {
                mark_constrained_baseline(&mut u);
            }
            units_in += 1;
            let began = slow::now();
            let decoded = codec.decode_newest_at(&u, pts);
            took += slow::now().saturating_duration_since(began);
            match decoded {
                Ok(n) => count += n,
                // Joined mid-GOP, say: skip until a keyframe
                Err(Fault::Invalid) if !strict => {}
                Err(Fault::Invalid) => fault = Some("the decoder refused the stream".to_string()),
                Err(Fault::Broken(why)) => fault = Some(why),
            }
            if fault.is_some() {
                return;
            }
        }
    });
    // Only the newest picture is downloaded and converted; the ones it replaced never were, and none is while nobody looks
    if fault.is_none() && take {
        let began = slow::now();
        let taken = codec.take_newest_stamped();
        took += slow::now().saturating_duration_since(began);
        match taken {
            Ok(taken) => {
                newest = taken.map(|(mut frame, pts)| {
                    frame.capture_ns = pts.and_then(Stamp::from_pts).and_then(|s| clock?.to_desktop(s.clock, s.ns));
                    frame.phone_share = crops.take(pts);
                    frame
                })
            }
            // A picture that couldn't be taken doesn't count as finished
            Err(Fault::Invalid) if !strict => count = count.saturating_sub(1),
            Err(Fault::Invalid) => fault = Some("the decoder refused the stream".to_string()),
            Err(Fault::Broken(why)) => fault = Some(why),
        }
    }
    codec.drop_newest();
    match fault {
        Some(why) => Err(why),
        None => Ok(Fed { newest, count, units: units_in, took }),
    }
}

impl Decoder for H264 {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        #[cfg(surfaces)]
        self.apply_wish();
        let (mut newest, count) = self.feed(&packet.data);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { newest, count }
    }

    fn decode_unseen(&mut self, packet: &Packet) -> Decoded {
        #[cfg(surfaces)]
        self.apply_wish();
        // A swap to software or back to the card mid-way may hand a picture over anyway; nobody wants it
        let (_, count) = self.feed_with(&packet.data, false);
        Decoded { newest: None, count }
    }

    fn hardware(&self) -> bool {
        self.codec.hw.is_some()
    }

    fn set_phone_clock(&mut self, clock: Arc<PhoneClock>) {
        self.clock = Some(clock);
    }

    fn set_surface_wish(&mut self, wish: Arc<AtomicBool>) {
        #[cfg(surfaces)]
        { self.surface_wish = Some(wish); }
        #[cfg(not(surfaces))]
        let _ = wish;
    }

    fn on_keyframe_needed(&mut self, ask: Box<dyn Fn() + Send>) {
        self.ask_keyframe = Some(ask);
    }

    fn target_fps_cell(&self) -> Option<Arc<AtomicU32>> {
        Some(self.target_fps.clone())
    }

    fn timing_probe(&self) -> Option<TimingProbe> {
        let timed = self.timings.clone();
        Some(Arc::new(move || {
            let timed = timed.lock().unwrap();
            let s = timed.times.recent()?;
            Some(format!("Decoding on {}: p95 {:.1} ms, max {:.1} ms a picture (decode and hand-over) over the last {} s", timed.backend, s.p95_ms, s.max_ms, slow::WINDOW.as_secs()))
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::packets::tests::{maker, FnStep};
    use crate::testenc;
    use std::sync::atomic::AtomicUsize;
    use telescope_core::media::PacketTap;

    pub(crate) const AUD: &[u8] = b"\x00\x00\x00\x01\x09\xf0";

    fn mean(f: &Frame) -> f64 {
        let y = &f.data[..(f.width * f.height) as usize];
        y.iter().map(|&v| v as f64).sum::<f64>() / y.len() as f64
    }

    /// The luma a flat grey of value v ends up as (BT.601 video range).
    fn luma(v: u8) -> f64 {
        16.0 + v as f64 * 219.0 / 255.0
    }

    fn software() -> H264 {
        H264::new(false, None).unwrap()
    }

    #[test]
    fn each_packet_shows_its_frame_straight_away() {
        let Some(packets) = testenc::h264(&[20, 60, 100, 140, 180, 220], 64, 48, 5) else { return };
        let mut dec = software();
        for (p, v) in packets.iter().zip([20, 60, 100, 140, 180, 220]) {
            let (frame, count) = dec.feed(p);
            let frame = frame.expect("a frame for every packet");
            // Frame i, not frame i - 1
            assert!(count == 1 && (mean(&frame) - luma(v)).abs() <= 8.0, "{} vs {}", mean(&frame), luma(v));
        }
    }

    #[test]
    fn pictures_nobody_looks_at_are_decoded_but_not_handed_over_and_the_next_needs_no_keyframe() {
        // One keyframe, then P-frames only: the picture after the unseen ones can only be right if they went through the decoder
        let values = [20, 60, 100, 140, 180, 220];
        let Some(packets) = testenc::h264(&values, 64, 48, 100) else { return };
        let mut dec = software();
        assert!(dec.decode(&Packet { data: packets[0].clone(), time_ns: 0 }).newest.is_some());
        for p in &packets[1..5] {
            let unseen = dec.decode_unseen(&Packet { data: p.clone(), time_ns: 0 });
            assert!(unseen.newest.is_none() && unseen.count == 1, "decoded and counted, but no picture");
        }
        let last = dec.decode(&Packet { data: packets[5].clone(), time_ns: 0 }).newest.expect("the picture after the pause");
        assert!((mean(&last) - luma(220)).abs() <= 8.0, "{} vs {}", mean(&last), luma(220));
    }

    #[test]
    fn pieces_of_any_size_decode_in_order() {
        let Some(packets) = testenc::h264(&[20, 60, 100, 140, 180, 220], 64, 48, 5) else { return };
        let data = packets.concat();
        let mut dec = software();
        let means: Vec<f64> = data.chunks(97).filter_map(|c| dec.feed(c).0).map(|f| mean(&f)).collect();
        assert!(means.windows(2).all(|w| w[0] <= w[1]) && *means.last().unwrap() > luma(200));
    }

    #[test]
    fn a_burst_shows_its_newest_frame_but_counts_every_frame_that_arrived() {
        let Some(packets) = testenc::h264(&[30, 60, 90, 120, 150, 180], 64, 48, 5) else { return };
        let (frame, count) = software().feed(&packets.concat());
        assert_eq!(count, 6);
        assert!((mean(&frame.unwrap()) - luma(180)).abs() <= 8.0);
    }

    #[test]
    fn decoding_stays_in_software_unless_hardware_is_on() {
        assert!(software().hw().is_none());
    }

    #[test]
    fn sps_and_pps_are_remembered_across_split_chunks_without_keeping_frames() {
        let Some(packets) = testenc::h264(&[50, 60], 64, 48, 5) else { return };
        let data = packets.concat();
        let mut h = Headers::default();
        for c in data.chunks(3) {
            h.remember(c);
        }
        let setup = h.setup();
        let kinds: Vec<u8> = start_codes(&setup).iter().map(|&(_, e)| setup[e] & 31).collect();
        assert_eq!(kinds, [7, 8, 9]);
        assert!(h.tail.len() <= 4);
    }

    #[test]
    fn a_baseline_sps_is_marked_constrained_baseline_and_nothing_else_changes() {
        let sps = [0, 0, 0, 1, 0x67, 66, 0x00, 0x1f, 0xe9, 0x40];
        let pps = [0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80];
        let mut unit = [&sps[..], &pps[..]].concat();
        let before = unit.clone();
        mark_constrained_baseline(&mut unit);
        assert_eq!(unit[6], 0x40);
        let diff: Vec<usize> = (0..unit.len()).filter(|&i| unit[i] != before[i]).collect();
        assert_eq!(diff, [6]);
        // Already marked, other profiles and other NAL units stay as they are
        let again = unit.clone();
        mark_constrained_baseline(&mut unit);
        assert_eq!(unit, again);
        let mut high = vec![0, 0, 1, 0x67, 100, 0, 40, 0xac];
        let copy = high.clone();
        mark_constrained_baseline(&mut high);
        assert_eq!(high, copy);
        let mut cut = vec![0, 0, 1, 0x67, 66];
        mark_constrained_baseline(&mut cut);
        assert_eq!(cut, [0, 0, 1, 0x67, 66]);
    }

    #[test]
    fn start_codes_match_three_and_four_byte_forms() {
        assert_eq!(start_codes(b"\x00\x00\x01\x67\x00\x00\x00\x01\x68"), [(0, 3), (4, 8)]);
        assert_eq!(start_codes(b"\x00\x00\x00\x00\x01\x65"), [(1, 5)]);
        assert!(start_codes(b"\x00\x00\x02\x00\x00").is_empty());
    }

    fn tap_with(make: Arc<dyn telescope_core::host::PacketStepFactory>) -> Arc<Tap> {
        let tap = Tap::new();
        tap.set(vec![("test".into(), make)]);
        tap
    }

    #[test]
    fn nothing_extra_runs_while_no_plugin_changes_packets() {
        let Some(packets) = testenc::h264(&[50; 8], 64, 48, 5) else { return };
        let mut dec = H264::new(false, Some(Tap::new())).unwrap();
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 8);
        assert!(!dec.steps.as_ref().unwrap().on());
        assert_eq!(dec.steps.as_ref().unwrap().asked(), 0, "no unit was copied out for a step");
    }

    #[test]
    fn a_step_sees_every_access_unit_and_what_it_hands_back_is_decoded() {
        let Some(packets) = testenc::h264(&[50; 6], 64, 48, 5) else { return };
        let seen = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            Box::new(FnStep(move |u: &[u8]| {
                let mut seen = s.lock().unwrap();
                seen.push(u.to_vec());
                // The third twice
                if seen.len() == 3 { vec![u.to_vec(), u.to_vec()] } else { vec![u.to_vec()] }
            }))
        }));
        let (_, count) = H264::new(false, Some(tap)).unwrap().feed(&packets.concat());
        let seen = seen.lock().unwrap();
        assert!(seen.len() == 6 && seen.iter().all(|u| u.starts_with(b"\x00\x00")));
        assert_eq!(count, 7);
    }

    #[test]
    fn a_step_put_on_mid_stream_gets_the_streams_sps_and_pps_first() {
        // SPS and PPS only at the start, as TCP sends them
        let Some(packets) = testenc::h264(&[50; 12], 64, 48, 100) else { return };
        let tap = Tap::new();
        let mut dec = H264::new(false, Some(tap.clone())).unwrap();
        dec.feed(&packets[..6].concat());
        let units = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let u = units.clone();
        tap.set(vec![("test".into(), maker(move || {
            let u = u.clone();
            Box::new(FnStep(move |unit: &[u8]| {
                u.lock().unwrap().push(unit.to_vec());
                vec![unit.to_vec()]
            }))
        }))]);
        let (_, count) = dec.feed(&packets[6..].concat());
        let units = units.lock().unwrap();
        let first: Vec<u8> = start_codes(&units[0]).iter().map(|&(_, e)| units[0][e] & 31).collect();
        assert!(count > 0 && first.contains(&7) && first.contains(&8));
    }

    #[test]
    fn a_step_can_drop_a_frame() {
        let Some(packets) = testenc::h264(&[50; 10], 64, 48, 5) else { return };
        let n = Arc::new(AtomicUsize::new(0));
        let tap = tap_with(maker(move || {
            let n = n.clone();
            Box::new(FnStep(move |u: &[u8]| {
                // Drop every other non-keyframe picture
                let p_frame = start_codes(u).iter().any(|&(_, e)| u[e] & 31 == 1);
                if p_frame && n.fetch_add(1, Ordering::SeqCst) % 2 == 1 { vec![] } else { vec![u.to_vec()] }
            }))
        }));
        let (_, count) = H264::new(false, Some(tap)).unwrap().feed(&packets.concat());
        assert!(count < 10);
    }

    #[test]
    fn a_new_decoder_starts_its_steps_afresh() {
        let made = Arc::new(AtomicUsize::new(0));
        let m = made.clone();
        let tap = tap_with(maker(move || {
            m.fetch_add(1, Ordering::SeqCst);
            Box::new(FnStep(|u: &[u8]| vec![u.to_vec()]))
        }));
        let _a = H264::new(false, Some(tap.clone())).unwrap();
        let _b = H264::new(false, Some(tap)).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    /// HW_FAILED and the live count are one per run: tests that touch them take turns, and leave them as they found them.
    struct CardTest(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    fn card_test() -> CardTest {
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        HW_FAILED.store(false, Ordering::SeqCst);
        CardTest(turn)
    }

    impl Drop for CardTest {
        fn drop(&mut self) {
            HW_FAILED.store(false, Ordering::SeqCst);
        }
    }

    fn hardware_in_use() -> bool {
        status().backends.iter().any(|b| b != "processor")
    }

    #[test]
    fn diagnostics_follow_the_live_decoders() {
        let dec = software();
        assert!(status().backends.contains(&"processor".to_string()));
        drop(dec);
    }

    #[test]
    fn diagnostics_follow_the_live_decoder_not_the_next_preference() {
        let _turn = card_test();
        let card = pretend_hardware(None);
        // Hardware decoding switched off: the next connection asks for software, and the running decoder doesn't hear of it
        let next = H264::new(false, None).unwrap();
        assert!(next.hw().is_none() && card.hw() == Some(Hw::Cuda));
        assert!(status().backends.contains(&"cuda".to_string()));
        assert!(hardware_in_use());
        drop(card);
        assert!(!hardware_in_use());
    }

    #[test]
    fn a_card_that_opens_decodes_or_hands_over_to_software() {
        // On a machine with VA-API this decodes on the card; anywhere else it's software, and either way frames come
        let Some(packets) = testenc::h264(&[60, 120, 180], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = H264::new(true, None).unwrap();
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count >= 1, "on {:?}", dec.hw());
    }

    /// A card decoder stands in for the test: software opened as if it were hardware, so the fallback logic runs.
    fn pretend_hardware(tap: Option<Arc<Tap>>) -> H264 {
        let mut codec = Codec::h264(None).unwrap();
        codec.hw = Some(Hw::Cuda);
        H264::with_codec(codec, tap)
    }

    #[test]
    fn a_graphics_card_that_fails_hands_over_to_software_and_stays_out() {
        let Some(packets) = testenc::h264(&[60, 120, 180], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.break_next = true;
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        // The same input again in software
        let (frame, count) = dec.feed(&packets[0]);
        assert!(frame.is_some() && count == 1, "{count}");
        assert!(dec.hw().is_none() && hardware_failed() && status().hardware_failed);
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 3, "{count}");
        // Not tried again this run, even on a computer that has a card that opens
        let next = H264::opening(true, || Some(Hw::Cuda), |hw| {
            let mut codec = Codec::h264(None)?;
            codec.hw = hw;
            Ok(codec)
        }, None).unwrap();
        assert!(next.hw().is_none());
        assert!(!hardware_in_use());
    }

    #[test]
    fn no_graphics_card_decoder_means_software() {
        let Some(packets) = testenc::h264(&[90], 64, 48, 5) else { return };
        let _turn = card_test();
        let tried = Arc::new(AtomicUsize::new(0));
        let t = tried.clone();
        // The computer says it has one, and it doesn't open
        let mut dec = H264::opening(true, || Some(Hw::Cuda), move |hw| {
            if hw.is_some() {
                t.fetch_add(1, Ordering::SeqCst);
                return Err("no card here".into());
            }
            Codec::h264(None)
        }, None).unwrap();
        assert_eq!(tried.load(Ordering::SeqCst), 1);
        assert!(dec.hw().is_none() && hardware_failed(), "and it isn't tried again this run");
        assert!(dec.feed(&packets.concat()).0.is_some());
    }

    #[test]
    fn a_computer_without_a_card_decoder_isnt_a_card_that_failed() {
        let _turn = card_test();
        let dec = H264::opening(true, || None, Codec::h264, None).unwrap();
        assert!(dec.hw().is_none() && !hardware_failed());
    }

    /// Runs a real stream through a card decoder that goes wrong in the given way after two good frames.
    fn fails_mid_stream(kind: Forced) {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.codec.forced = Some((kind, 2));
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        dec.on_keyframe_needed(Box::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
        }));
        let shown = packets.iter().filter(|p| dec.feed(p).0.is_some()).count();
        assert!(dec.hw().is_none() && hardware_failed(), "{kind:?} left it on the card");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "{kind:?}");
        assert!(shown > 2, "{kind:?}: the picture came back after the swap, {shown} shown");
        assert!(status().backends.contains(&"processor".to_string()));
    }

    #[test]
    fn a_card_that_stops_giving_pictures_hands_over_mid_stream() {
        fails_mid_stream(Forced::Silent);
    }

    #[test]
    fn a_card_that_starts_refusing_every_packet_hands_over_mid_stream() {
        fails_mid_stream(Forced::Reject);
    }

    #[test]
    fn a_card_that_errors_mid_stream_hands_over() {
        fails_mid_stream(Forced::Broken);
    }

    #[test]
    fn a_moment_with_no_picture_shorter_than_the_limit_stays_on_the_card() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        for p in &packets[..2] {
            dec.feed(p);
        }
        dec.codec.forced = Some((Forced::Silent, 0));
        for p in &packets[2..2 + SILENT_UNITS as usize - 1] {
            dec.feed(p);
        }
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        dec.codec.forced = None;
        dec.feed(&packets[2 + SILENT_UNITS as usize - 1]);
        assert_eq!(dec.silent, 0, "a picture clears the count");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
    }

    #[test]
    fn joining_before_the_headers_says_nothing_about_the_card() {
        let Some(packets) = testenc::h264(&[60, 61], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // A slice with no SPS or PPS seen yet: nothing could decode this
        dec.feed(b"\x00\x00\x00\x01\x41\x9a\x00\x00\x00\x01\x09\xf0");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
        assert!(dec.feed(&packets.concat()).0.is_some());
    }

    /// Every way a real card goes wrong while copying its picture out: only VA-API can get here, so this skips elsewhere.
    #[test]
    fn a_real_card_that_gives_bad_pictures_hands_over() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 320, 240, 5) else { return };
        for kind in [Forced::Copy, Forced::Empty, Forced::Blank, Forced::Silent, Forced::Reject, Forced::Broken] {
            let _turn = card_test();
            let mut dec = H264::new(true, None).unwrap();
            if dec.hw().is_none() {
                return eprintln!("no graphics card decoder; skipped");
            }
            dec.codec.forced = Some((kind, 3));
            assert!(packets[..3].iter().all(|p| dec.feed(p).0.is_some()) && dec.hw().is_some(), "{kind:?}: good frames first, on the card");
            let shown = packets[3..].iter().filter(|p| dec.feed(p).0.is_some()).count();
            assert!(dec.hw().is_none() && hardware_failed(), "{kind:?} left it on the card");
            assert!(shown > 0, "{kind:?}: no picture after the swap");
        }
    }

    /// What a chunk of several access units gave before only the newest picture was extracted: every one converted, the last kept.
    fn newest_the_old_way(codec: &mut Codec, data: &[u8]) -> (Option<Frame>, u32) {
        let (mut newest, mut count) = (None, 0);
        codec.parse(data, |c, unit| count += c.decode(unit, |f| newest = Some(f)).unwrap());
        (newest, count)
    }

    #[test]
    fn a_burst_keeps_the_picture_extracting_each_one_would_have_kept() {
        let Some(packets) = testenc::h264_moving(40, 128, 96, 10, "high") else { return };
        let _turn = card_test();
        for hardware in [false, true] {
            for group in [1usize, 3, 4, 7] {
                let mut new = H264::new(hardware, None).unwrap();
                if hardware && new.hw().is_none() {
                    eprintln!("no graphics card decoder; skipped");
                    continue;
                }
                let mut old = Codec::h264(if hardware { new.hw() } else { None }).unwrap();
                let mut kept = 0;
                for chunk in packets.chunks(group) {
                    let data = chunk.concat();
                    let (got, count) = new.feed(&data);
                    let (want, want_count) = newest_the_old_way(&mut old, &data);
                    assert_eq!(count, want_count, "{group} units a chunk, hardware {hardware}");
                    assert_eq!(got.is_some(), want.is_some());
                    if let (Some(got), Some(want)) = (got, want) {
                        assert!(got.data == want.data, "{group} units a chunk, hardware {hardware}: a different picture was kept");
                        kept += 1;
                    }
                }
                assert!(kept > 5, "pictures came out");
            }
        }
    }

    #[test]
    fn a_real_card_gives_the_same_picture_as_software() {
        let Some(packets) = testenc::h264(&[40, 90, 140, 200], 320, 240, 5) else { return };
        let _turn = card_test();
        let mut card = H264::new(true, None).unwrap();
        if card.hw().is_none() {
            return eprintln!("no graphics card decoder; skipped");
        }
        let mut soft = H264::new(false, None).unwrap();
        for p in &packets {
            let (a, b) = (card.feed(p).0.unwrap(), soft.feed(p).0.unwrap());
            let worst = a.data.iter().zip(b.data.iter()).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
            assert!(worst <= 2, "off by {worst}");
        }
        assert!(card.hw().is_some(), "still on the card");
    }

    #[test]
    fn a_plain_baseline_stream_decodes_on_the_card() {
        let Some(mut packets) = testenc::h264_moving(8, 320, 240, 5, "baseline") else { return };
        let _turn = card_test();
        let mut card = H264::new(true, None).unwrap();
        if card.hw().is_none() {
            return eprintln!("no graphics card decoder; skipped");
        }
        // As the phone sends it: x264 marks its Baseline as constrained, the phone's encoder doesn't
        for p in &mut packets {
            for (_, end) in start_codes(&p.clone()) {
                if p[end] & 31 == 7 {
                    assert_eq!(p[end + 1], 66);
                    p[end + 2] &= !0x40;
                }
            }
        }
        let mut pictures = 0;
        for p in &packets {
            pictures += card.feed(&[&p[..], END_OF_SETUP].concat()).1;
        }
        assert!(pictures >= 1 && card.hw().is_some(), "{pictures} pictures, on {:?}", card.hw());
    }

    #[test]
    fn a_hardware_decoder_refusing_the_stream_falls_back_too() {
        let Some(packets) = testenc::h264(&[100], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.codec.refusals = u32::MAX;
        let (frame, count) = dec.feed(&packets.concat());
        assert!(frame.is_some() && count == 1, "{count}");
        assert!(dec.hw().is_none() && hardware_failed());
    }

    #[test]
    fn garbage_a_card_refuses_before_its_first_frame_counts_as_the_card_failing() {
        // Real refusal from FFmpeg: a card decoder that hasn't decoded anything yet takes that as the card failing
        let Some(packets) = testenc::h264(&[60, 120], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        let bad = [b"\x00\x00\x00\x01\x65".as_slice(), &[0xFF; 64], AUD].concat();
        let (frame, count) = dec.feed(&[bad, packets.concat()].concat());
        assert!(dec.hw().is_none() && hardware_failed());
        assert!(frame.is_some() && count == 2);
    }

    #[test]
    fn the_software_fallback_gives_steps_each_unit_once() {
        let Some(packets) = testenc::h264(&[60, 120], 64, 48, 5) else { return };
        let _turn = card_test();
        let seen = Arc::new(Mutex::new(Vec::<usize>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            let index = { let mut v = s.lock().unwrap(); v.push(0); v.len() - 1 };
            Box::new(FnStep(move |u: &[u8]| {
                s.lock().unwrap()[index] += 1;
                vec![u.to_vec()]
            }))
        }));
        let mut dec = pretend_hardware(Some(tap));
        dec.codec.refusals = u32::MAX;
        assert_eq!(dec.hw(), Some(Hw::Cuda));
        let (frame, _) = dec.feed(&packets.concat());
        assert!(frame.is_some() && dec.hw().is_none());
        // A fresh step for the software decoder, given each unit once
        let seen = seen.lock().unwrap();
        assert_eq!((seen.len(), seen[1]), (2, 2), "{seen:?}");
    }

    #[test]
    fn a_bad_packet_after_hardware_decoded_frames_is_skipped_not_a_fallback() {
        let Some(packets) = testenc::h264(&[60, 61, 62, 63, 64], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        assert!(dec.feed(&packets[0]).0.is_some());
        dec.codec.refusals = 1;
        dec.feed(&packets[1]);
        assert_eq!(dec.codec.refusals, 0, "the packet reached the decoder");
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
        assert!(dec.feed(&packets[2..].concat()).0.is_some());
    }

    #[test]
    fn failure_after_setup_recovers_at_an_idr_without_repeated_headers() {
        let Some(packets) = testenc::h264(&(60..75).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        assert!(dec.feed(&packets[0]).0.is_some());
        dec.break_next = true;
        // Every later packet without its SPS and PPS, as TCP sends them after joining
        let recovered: Vec<bool> = packets[1..].iter().map(|p| {
            let codes = start_codes(p);
            let mut stripped = Vec::new();
            for (i, &(s, e)) in codes.iter().enumerate() {
                let end = codes.get(i + 1).map_or(p.len(), |c| c.0);
                if e < end && !matches!(p[e] & 31, 7 | 8) {
                    stripped.extend_from_slice(&p[s..end]);
                }
            }
            dec.feed(&stripped).0.is_some()
        }).collect();
        assert!(recovered.iter().any(|&r| r) && dec.hw().is_none());
    }

    /// The NAL units of an Annex-B packet, each with its start code.
    fn nal_units(p: &[u8]) -> Vec<&[u8]> {
        let codes = start_codes(p);
        codes.iter().enumerate().map(|(i, &(s, _))| &p[s..codes.get(i + 1).map_or(p.len(), |c| c.0)]).collect()
    }

    #[test]
    fn software_taking_over_is_given_a_pps_with_nothing_stuck_to_it() {
        // The card refuses a keyframe that arrived in pieces, so software gets its tail after the setup: that tail must not stick to the PPS
        let Some(packets) = testenc::h264_noisy(&[60, 120, 180], 128, 96, 5) else { return };
        let _turn = card_test();
        let seen = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let s = seen.clone();
        let tap = tap_with(maker(move || {
            let s = s.clone();
            Box::new(FnStep(move |u: &[u8]| {
                s.lock().unwrap().push(u.to_vec());
                vec![u.to_vec()]
            }))
        }));
        let mut dec = pretend_hardware(Some(tap));
        dec.codec.refusals = u32::MAX;
        let units = nal_units(&packets[0]);
        let pps_len = units.iter().find(|u| u[start_codes(u)[0].1] & 31 == 8).unwrap().len();
        let (setup, idr): (Vec<&[u8]>, Vec<&[u8]>) = units.into_iter().partition(|u| matches!(u[start_codes(u)[0].1] & 31, 7 | 8));
        let (setup, idr) = (setup.concat(), idr.concat());
        dec.feed(&setup);
        dec.feed(&idr[..idr.len() / 2]);
        dec.feed(&idr[idr.len() / 2..]);
        assert!(dec.hw().is_none());
        dec.feed(&packets[1..].concat());
        for unit in seen.lock().unwrap().iter() {
            for nal in nal_units(unit).into_iter().filter(|n| n[start_codes(n)[0].1] & 31 == 8) {
                assert_eq!(nal.len(), pps_len, "the PPS has {} bytes after it", nal.len() as isize - pps_len as isize);
            }
        }
    }

    /// A card that took ms a picture for the last few seconds at 30 fps, as the decoder would have timed it, and that delivered 20 new
    /// pictures a second to the camera over the same time.
    fn card_that_took(dec: &mut H264, ms: f64) {
        let now = slow::now();
        let mut times = slow::Timings::default();
        for i in 0..100u32 {
            times.record(now - Duration::from_millis(3400 - u64::from(i) * 33), ms);
        }
        dec.timings.lock().unwrap().times = times;
        let mut rate = Rate::default();
        for i in 0..=34u64 {
            rate.record(now - Duration::from_millis(3400 - i * 100), i * 2);
        }
        dec.rate = rate;
        TEST_DELIVERED.with(|d| d.set(68));
    }

    #[test]
    fn a_card_too_slow_for_the_stream_tries_software_beside_it() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        // A slow integrated card: 45 ms a picture against 33 ms between pictures
        card_that_took(&mut dec, 45.0);
        assert!(dec.feed(&packets[1]).0.is_some(), "the card keeps the picture going");
        let beside = matches!(&dec.candidate, Some(Candidate { plan: Plan::Trial { card_ms, .. }, .. }) if *card_ms == 45.0);
        assert!(beside && dec.hw() == Some(Hw::Cuda) && !hardware_failed() && hardware_slow());
        // At the next keyframe software has a picture and takes the stream over
        for p in &packets[2..8] {
            assert!(dec.feed(p).0.is_some());
        }
        assert!(dec.hw().is_none() && dec.trial.as_ref().is_some_and(|t| t.card_ms == 45.0) && dec.candidate.is_none());
        drop(dec);
        assert!(!hardware_slow());
    }

    /// How a run of pictures went: the decoder it ended on, whether software holds the stream for a while and whether the card isn't
    /// judged for a while.
    #[derive(Debug, PartialEq)]
    struct Ended {
        hw: Option<Hw>,
        software_waits: bool,
        card_not_judged: bool,
    }

    /// Feeds one picture every 50 ms on the speed check's clock for `seconds`, each one delivered to the camera. The card's decode call
    /// takes `card.0` ms and its hand-over (the copy off the card, which waits for a card decoding in the background) `card.1` ms,
    /// software's the same with `software`, and a packet step on each picture `step` ms.
    fn pictures(dec: &mut H264, seconds: u64, card: (u64, u64), software: (u64, u64), step: u64, take: bool) -> Ended {
        pictures_out(dec, seconds, card, software, step, take, (1.0, 1.0))
    }

    /// pictures, with the share of the pictures each decoder gets to the camera, `out` (the card's, software's): what the frame steps and the
    /// camera leave of them.
    fn pictures_out(dec: &mut H264, seconds: u64, card: (u64, u64), software: (u64, u64), step: u64, take: bool, out: (f64, f64)) -> Ended {
        let ms = Duration::from_millis;
        let values: Vec<u8> = (0..seconds * 20).map(|i| 40 + (i % 100) as u8).collect();
        let packets = testenc::h264(&values, 64, 48, 5).expect("libx264");
        dec.card = (ms(card.0), ms(card.1));
        dec.codec.delays = (ms(card.0), ms(card.1));
        dec.software = (ms(software.0), ms(software.1), dec.software.2);
        let mut owed = 0.0;
        for p in &packets {
            if dec.hw().is_some() {
                dec.codec.delays = (ms(card.0), ms(card.1));
            }
            slow::advance(ms(50));
            slow::advance(ms(step));
            owed += if dec.hw().is_some() { out.0 } else { out.1 };
            TEST_DELIVERED.with(|d| d.set(d.get() + owed as u64));
            owed -= owed.floor();
            dec.feed_with(p, take);
        }
        let now = slow::now();
        let policy = dec.policy.lock().unwrap();
        Ended { hw: dec.hw(), software_waits: policy.cooldown.waiting(now), card_not_judged: policy.lost.waiting(now) }
    }

    /// Feeds a card that waits 45 ms in the copy beside software that takes 5 ms until software has the stream on trial.
    fn into_trial(dec: &mut H264) {
        let ms = Duration::from_millis;
        let values: Vec<u8> = (0..400u32).map(|i| 90 + (i % 100) as u8).collect();
        let packets = testenc::h264(&values, 64, 48, 5).expect("libx264");
        dec.card = (ms(0), ms(45));
        dec.codec.delays = (ms(0), ms(45));
        dec.software = (ms(5), ms(0), 0);
        for p in &packets {
            slow::advance(ms(50));
            TEST_DELIVERED.with(|d| d.set(d.get() + 1));
            dec.feed(p);
            if dec.trial.is_some() {
                return;
            }
        }
        panic!("software never got the stream on trial");
    }

    /// A pretend card fed through a step that takes `step` ms.
    fn card_with_step(step: u64) -> H264 {
        let tap = tap_with(maker(move || {
            Box::new(FnStep(move |u: &[u8]| {
                slow::advance(Duration::from_millis(step));
                vec![u.to_vec()]
            }))
        }));
        pretend_hardware(Some(tap))
    }

    #[test]
    fn what_the_steps_cost_is_not_part_of_the_decoders_speed() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = card_with_step(60);
        let ended = pictures(&mut dec, 6, (5, 0), (1, 0), 60, true);
        let median = dec.timings.lock().unwrap().times.recent().map(|s| s.median_ms);
        assert_eq!(ended, Ended { hw: Some(Hw::Cuda), software_waits: false, card_not_judged: false }, "median {median:?}");
        assert!(median.is_some_and(|m| m < 10.0), "{median:?}");
    }

    #[test]
    fn pictures_left_on_the_card_are_not_timed() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // Nobody looks, so nothing waits for the pictures: the decode calls alone say nothing about a card that decodes in the background
        let ended = pictures(&mut dec, 6, (45, 0), (1, 0), 0, false);
        let timed = dec.timings.lock().unwrap().times.recent();
        assert_eq!(ended.hw, Some(Hw::Cuda));
        assert!(timed.is_none(), "{timed:?}");
    }

    #[test]
    fn a_card_slowed_by_the_copy_keeps_the_stream_when_software_is_slower() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // 4K on integrated graphics with the frame steps on the processor: 5 ms to decode and 25 ms to copy each picture into memory,
        // over the 26.7 ms line at 30 fps, while software takes 60 ms
        let ended = pictures(&mut dec, 25, (5, 25), (60, 0), 0, true);
        assert_eq!(ended, Ended { hw: Some(Hw::Cuda), software_waits: false, card_not_judged: true });
        assert!(!hardware_slow());
    }

    #[test]
    fn a_card_that_waits_in_the_copy_loses_to_quicker_software() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // A card that decodes in the background: its decode call returns at once and the copy waits 45 ms for the picture, so it
        // delivers about 10 pictures a second where software delivers 18
        let ended = pictures(&mut dec, 12, (0, 45), (5, 0), 0, true);
        assert_eq!(ended, Ended { hw: None, software_waits: true, card_not_judged: false });
        assert_eq!(dec.retry_hw, Some(Hw::Cuda));
    }

    #[test]
    fn software_that_decodes_quicker_but_delivers_no_more_pictures_hands_the_card_back() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        // Software is quicker per picture, but the processor it shares with the frame steps leaves the camera fewer new pictures
        for software_share in [0.4, 0.0] {
            let _turn = card_test();
            let mut dec = pretend_hardware(None);
            let ended = pictures_out(&mut dec, 25, (0, 45), (5, 0), 0, true, (1.0, software_share));
            assert_eq!(ended, Ended { hw: Some(Hw::Cuda), software_waits: false, card_not_judged: true }, "software delivering {software_share} of its pictures");
        }
    }

    #[test]
    fn a_card_slow_with_no_pictures_to_compare_is_left_alone() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        // Nothing is known about what the card delivers, so software couldn't be shown to beat it
        dec.rate = Rate::default();
        dec.feed(&packets[1]);
        assert!(dec.candidate.is_none() && dec.hw() == Some(Hw::Cuda) && !hardware_slow());
    }

    #[test]
    fn a_switch_never_leaves_a_gap_or_a_dark_picture_in_either_direction() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        let ms = Duration::from_millis;
        let values: Vec<u8> = (0..1000u32).map(|i| 90 + (i % 100) as u8).collect();
        let packets = testenc::h264(&values, 64, 48, 7).expect("libx264");
        dec.card = (ms(0), ms(45));
        dec.codec.delays = (ms(0), ms(45));
        dec.software = (ms(5), ms(0), 0);
        let mut ran_on = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            if i == 300 {
                // The wait after software won is over, and the card is slow still
                slow::advance(slow::FIRST_WAIT);
            }
            if dec.hw().is_some() {
                dec.codec.delays = (ms(0), ms(45));
            }
            slow::advance(ms(50));
            let owed = TEST_DELIVERED.with(|d| d.get()) + 1;
            TEST_DELIVERED.with(|d| d.set(owed));
            let (frame, _) = dec.feed(p);
            let frame = frame.unwrap_or_else(|| panic!("no picture for packet {i}, on {:?}", dec.hw()));
            assert!(mean(&frame) > luma(60), "a dark picture for packet {i}: {}", mean(&frame));
            if ran_on.last() != Some(&dec.hw()) {
                ran_on.push(dec.hw());
            }
        }
        assert!(ran_on.starts_with(&[Some(Hw::Cuda), None]), "{ran_on:?}");
    }

    #[test]
    fn a_trial_still_ends_when_every_feed_of_software_fails() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        // Software takes the stream, and then every piece it is given errors
        into_trial(&mut dec);
        dec.codec.forced = Some((Forced::Broken, 0));
        let packets = testenc::h264(&[60; 80], 64, 48, 5).unwrap();
        for (i, p) in packets.iter().take(60).enumerate() {
            slow::advance(Duration::from_millis(400));
            let (_, count) = dec.feed(p);
            if i < 5 {
                assert_eq!(count, 0, "software is failing");
            }
        }
        // Past the limit: no verdict for software, the card back, and no wait that keeps software
        let policy = dec.policy.lock().unwrap();
        assert!(dec.trial.is_none() && dec.hw() == Some(Hw::Cuda), "the trial outlived its limit");
        assert!(policy.lost.waiting(slow::now()) && !policy.cooldown.active());
        assert!(!hardware_slow());
    }

    #[test]
    fn software_that_cannot_open_is_not_tried_again_and_again() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.no_software = true;
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        assert!(dec.feed(&packets[1]).0.is_some());
        assert!(dec.hw() == Some(Hw::Cuda) && dec.candidate.is_none() && !hardware_slow());
        assert!(dec.policy.lock().unwrap().lost.waiting(slow::now()), "the card isn't judged again for a while");
    }

    #[test]
    fn a_trial_is_cancelled_when_the_frame_rate_changes() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        into_trial(&mut dec);
        dec.target_fps.store(15, Ordering::Relaxed);
        let packets = testenc::h264(&[60; 8], 64, 48, 5).unwrap();
        dec.feed(&packets[0]);
        // The card starts beside software once the stream has settled, from the next keyframe on
        slow::advance(SETTLE);
        for p in &packets[1..] {
            dec.feed(p);
        }
        // No verdict either way
        let policy = dec.policy.lock().unwrap();
        assert!(dec.trial.is_none() && dec.hw() == Some(Hw::Cuda) && dec.retry_hw.is_none());
        assert!(!policy.lost.active() && !policy.cooldown.active());
        drop(policy);
        assert!(!hardware_slow());
    }

    #[test]
    fn each_stream_has_its_own_waits_and_keeps_them_across_a_new_decoder() {
        // A live wait shows in hardware_slow(), which the tests that check it must have to themselves
        let _turn = card_test();
        let hub = telescope_core::stats::Hub::new();
        let (a, b) = (hub.register("phone", 1), hub.register("phone", 2));
        let policy_of = |stream: &Arc<SourceStats>| {
            let _counting = telescope_core::stats::bind(stream);
            stream_policy()
        };
        let (pa, pb) = (policy_of(&a), policy_of(&b));
        assert!(!Arc::ptr_eq(&pa, &pb) && Arc::ptr_eq(&pa, &policy_of(&a)));
        // One stream losing to software, or keeping it, says nothing about the other
        let now = slow::now();
        pa.lock().unwrap().lost.fell(now);
        pa.lock().unwrap().cooldown.fell(now);
        let (lost, kept) = {
            let b = pb.lock().unwrap();
            (b.lost.waiting(now), b.cooldown.active())
        };
        assert!(!lost && !kept);
        // A decoder made for the stream again (after a reconnect) finds what the stream learned
        let _counting = telescope_core::stats::bind(&a);
        assert!(Arc::ptr_eq(&H264::new(false, None).unwrap().policy, &pa));
        drop(_counting);
        let unbound = H264::new(false, None).unwrap();
        assert!(!Arc::ptr_eq(&unbound.policy, &pa) && !Arc::ptr_eq(&unbound.policy, &pb));
    }

    #[test]
    fn a_stream_still_in_the_wait_after_its_win_shows_as_slow() {
        let _turn = card_test();
        let hub = telescope_core::stats::Hub::new();
        let stream = hub.register("phone", 7);
        let _counting = telescope_core::stats::bind(&stream);
        let dec = H264::new(false, None).unwrap();
        assert!(!hardware_slow());
        dec.policy.lock().unwrap().cooldown.fell(slow::now());
        assert!(hardware_slow());
        drop(_counting);
        hub.unregister(&stream);
        drop(stream);
        assert!(!hardware_slow(), "a stream that is gone doesn't count");
    }

    #[test]
    fn inside_the_wait_after_a_win_no_card_is_started_beside_software() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        let ended = pictures(&mut dec, 12, (0, 45), (5, 0), 0, true);
        assert!(ended.software_waits && ended.hw.is_none());
        pictures(&mut dec, 4, (0, 45), (5, 0), 0, true);
        assert!(dec.candidate.is_none() && dec.hw().is_none());
    }

    #[test]
    fn a_card_that_beat_software_is_judged_again_after_the_wait() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        assert!(pictures(&mut dec, 25, (5, 25), (60, 0), 0, true).card_not_judged);
        // Still slow five minutes on, and now software would be quicker
        slow::advance(slow::FIRST_WAIT);
        let ended = pictures(&mut dec, 14, (5, 25), (5, 0), 0, true);
        assert_eq!(ended, Ended { hw: None, software_waits: true, card_not_judged: false });
    }

    #[test]
    fn software_that_gives_no_picture_hands_the_card_back() {
        if testenc::h264(&[60], 64, 48, 5).is_none() { return }
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.software.2 = u32::MAX;
        // It never gets a picture beside the card, so the card keeps the stream
        let ended = pictures(&mut dec, 20, (5, 40), (1, 0), 0, true);
        assert_eq!(ended, Ended { hw: Some(Hw::Cuda), software_waits: false, card_not_judged: true });
        assert!(dec.candidate.is_none() && !hardware_slow());
    }

    #[test]
    fn a_card_that_keeps_up_stays_on_the_card() {
        let Some(packets) = testenc::h264(&(60..90).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 6.0);
        assert!(dec.feed(&packets[1]).0.is_some());
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_slow(), "left the card for {:?}", dec.hw());
    }

    #[test]
    fn linux_tries_va_api_before_cuda_when_ffmpeg_has_it() {
        #[cfg(not(windows))]
        {
            assert_eq!(first_that_opens(|_| true), Some(Hw::Vaapi));
            // FFmpeg built without VA-API, or a card without it
            assert_eq!(first_that_opens(|h| h == Hw::Cuda), Some(Hw::Cuda));
        }
        #[cfg(windows)]
        {
            assert_eq!(first_that_opens(|_| true), Some(Hw::D3d11va));
            assert_eq!(first_that_opens(|h| h == Hw::Cuda), Some(Hw::Cuda));
        }
        assert_eq!(first_that_opens(|_| false), None);
    }

    #[test]
    fn a_hand_over_decoder_doesnt_take_the_running_decoders_steps() {
        // A step that lets only the first keyframe through, like a datamosh: one shared between decoders would give
        // the new route's decoder nothing to start from
        let Some(packets) = testenc::h264(&(40..120).step_by(5).collect::<Vec<u8>>(), 64, 48, 5) else { return };
        let tap = tap_with(maker(|| {
            let mut seen_key = false;
            Box::new(FnStep(move |u: &[u8]| {
                let key = start_codes(u).iter().any(|&(_, e)| u[e] & 31 == 5);
                if key && std::mem::replace(&mut seen_key, true) { vec![] } else { vec![u.to_vec()] }
            }))
        }));
        let stream = packets.concat();
        let mut running = H264::new(false, Some(tap.clone())).unwrap();
        assert!(running.feed(&stream).1 > 0);
        let mut new = H264::new(false, Some(tap)).unwrap();
        running.feed(&stream);
        assert!(new.feed(&stream).1 > 0);
    }

    fn rig() -> Rig {
        Rig { codec: Codec::h264(None).unwrap(), steps: None, step_generation: 0, gate: Gate { waiting: true, headers: Headers::default() }, crops: stamp::CropTags::default(), hold: None }
    }

    fn named(report: &Report) -> &'static str {
        match report {
            Report::Waiting => "waiting",
            Report::Quiet => "quiet",
            Report::Picture => "picture",
            Report::Damaged => "damaged",
            Report::Fault(_) => "fault",
            Report::Stale => "stale",
        }
    }

    /// A pretend card beside software that keeps the picture going: the stream's pictures are flat greys that rise by one a packet.
    fn rising(count: usize, gop: u32) -> Option<Vec<Vec<u8>>> {
        testenc::h264(&(0..count).map(|i| 60 + i as u8).collect::<Vec<u8>>(), 64, 48, gop)
    }

    #[test]
    fn a_keyframe_cut_short_is_not_a_clean_start() {
        let Some(packets) = testenc::h264_noisy(&[200; 12], 128, 96, 6) else { return };
        // A fresh decoder given the first keyframe cut in the middle of its picture still gives one, with the missing part made up
        let cut = packets[0][..packets[0].len() * 2 / 3].to_vec();
        let mut damaged = rig();
        let report = damaged.work(&[cut, packets[1].clone()].concat(), false);
        assert!(matches!(report, Report::Damaged), "a cut keyframe gave {}", named(&report));
        assert!(damaged.gate.waiting, "it starts over at the next keyframe");
        // The same decoder starts from the next whole one
        let report = damaged.work(&packets[6], false);
        assert!(matches!(report, Report::Picture), "a whole keyframe gave {}", named(&report));
    }

    #[test]
    fn damage_the_running_decoder_saw_resets_a_card_beside_it() {
        // A card flags no errors itself, so it is told: a whole keyframe the running decoder found damaged is no clean start
        let Some(packets) = testenc::h264(&[100; 12], 64, 48, 6) else { return };
        let mut r = rig();
        assert!(matches!(r.work(&packets[0], true), Report::Damaged), "told of damage, it still counted");
        assert!(r.gate.waiting, "it starts over at the next keyframe");
        assert!(matches!(r.work(&packets[6], false), Report::Picture));
    }

    #[test]
    fn a_running_decoders_damage_is_passed_on_once() {
        let Some(packets) = testenc::h264_noisy(&[200; 12], 128, 96, 6) else { return };
        let _turn = card_test();
        let mut dec = software();
        dec.feed(&packets[0]);
        dec.retry_hw = Some(Hw::Cuda);
        let cut = packets[6][..packets[6].len() * 2 / 3].to_vec();
        for p in &packets[1..6] {
            dec.feed(p);
        }
        dec.feed(&cut);
        assert!(!dec.piece_damaged, "the mark is handed to the candidate with the piece it belongs to");
        let cand = dec.candidate.as_ref().expect("a card started beside software");
        assert!(!cand.ready, "a card took the cut keyframe as a start");
    }

    #[test]
    fn a_view_moving_every_frame_does_not_restart_the_trial_every_frame() {
        let Some(packets) = rising(60, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        dec.on_keyframe_needed(Box::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
        }));
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        for p in &packets[1..40] {
            TEST_EPOCH.with(|e| e.set(e.get() + 1));
            dec.feed(p);
        }
        assert!(asked.load(Ordering::SeqCst) <= 2, "{} keyframes asked for while the steps kept changing", asked.load(Ordering::SeqCst));
        // Once it has been still for a while, a slow card is looked at again
        slow::advance(SETTLE);
        card_that_took(&mut dec, 45.0);
        let before = asked.load(Ordering::SeqCst);
        dec.feed(&packets[40]);
        assert!(asked.load(Ordering::SeqCst) > before, "the trial never came back");
        TEST_EPOCH.with(|e| e.set(0));
    }

    #[test]
    fn a_card_stuck_in_its_driver_is_a_failure_not_a_retry() {
        let Some(packets) = testenc::h264(&[80; 40], 64, 48, 5) else { return };
        let _turn = card_test();
        let mut dec = software();
        dec.threaded = true;
        let stuck = Arc::new(Mutex::new(()));
        dec.hold = Some(stuck.clone());
        let lock = stuck.lock().unwrap();
        dec.feed(&packets[0]);
        dec.retry_hw = Some(Hw::Cuda);
        dec.feed(&packets[1]);
        assert!(dec.candidate.is_some());
        slow::advance(slow::START_LIMIT);
        dec.feed(&packets[2]);
        assert!(dec.candidate.is_none() && hardware_failed(), "another decoder would be opened beside the stuck one every few seconds");
        drop(lock);
    }

    #[test]
    fn a_decoder_beside_waits_for_a_whole_keyframe_before_decoding_anything() {
        let Some(packets) = testenc::h264(&[100; 12], 64, 48, 6) else { return };
        let mut r = rig();
        // Setup first, then pictures that need a keyframe's references
        assert!(matches!(r.work(&packets[1], false), Report::Waiting));
        assert!(matches!(r.work(&packets[2], false), Report::Waiting));
        assert!(matches!(r.work(&packets[0], false), Report::Picture));
        assert!(matches!(r.work(&packets[1], false), Report::Picture));
    }

    #[test]
    fn a_picture_never_goes_back_or_comes_twice_across_a_switch() {
        // Each packet's grey is two steps brighter than the one before it, so a picture shown from behind or twice is a visible step off
        let Some(packets) = testenc::h264(&(0..70).map(|i| 40 + 2 * i as u8).collect::<Vec<u8>>(), 64, 48, 7) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.software = (Duration::from_millis(5), Duration::ZERO, 0);
        let mut on = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            if i == 1 {
                card_that_took(&mut dec, 45.0);
            }
            slow::advance(Duration::from_millis(50));
            TEST_DELIVERED.with(|d| d.set(d.get() + 1));
            let (frame, _) = dec.feed(p);
            let shown = mean(&frame.unwrap_or_else(|| panic!("no picture for packet {i}")));
            assert!((shown - luma(40 + 2 * i as u8)).abs() < 1.0, "packet {i} showed {shown}, want {}", luma(40 + 2 * i as u8));
            if on.last() != Some(&dec.hw()) {
                on.push(dec.hw());
            }
        }
        assert!(on.starts_with(&[Some(Hw::Cuda), None]), "no switch happened: {on:?}");
    }

    #[test]
    fn the_running_decoder_keeps_giving_pictures_while_the_one_beside_it_is_stuck() {
        let Some(packets) = rising(60, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.threaded = true;
        let stuck = Arc::new(Mutex::new(()));
        dec.hold = Some(stuck.clone());
        let lock = stuck.lock().unwrap();
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        for (i, p) in packets[1..40].iter().enumerate() {
            // Returns although the decoder beside it hasn't finished anything, with the card's own picture
            let (frame, _) = dec.feed(p);
            assert!(frame.is_some(), "no picture for packet {i} while the other decoder was stuck");
        }
        assert!(dec.candidate.is_some() && dec.hw() == Some(Hw::Cuda), "the card kept the stream");
        drop(lock);
        // Free again, it catches up and takes the stream over without having held anything up
        for p in &packets[40..] {
            std::thread::sleep(Duration::from_millis(20));
            assert!(dec.feed(p).0.is_some());
            if dec.hw().is_none() {
                break;
            }
        }
        assert!(dec.hw().is_none() && dec.trial.is_some(), "software never took over");
    }

    #[test]
    fn a_decoder_that_has_not_caught_up_does_not_take_over() {
        let Some(packets) = rising(40, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        // Up to and including the keyframe at packet 5, which the decoder beside gives a clean picture for
        for p in &packets[1..=5] {
            dec.feed(p);
        }
        assert!(dec.candidate.is_some() && dec.hw() == Some(Hw::Cuda));
        // As if it still had three pieces to get through: a picture it made from behind would be one already shown
        dec.candidate.as_mut().unwrap().sent += 3;
        dec.feed(&packets[6]);
        assert!(dec.candidate.as_ref().is_some_and(|c| c.ready) && dec.hw() == Some(Hw::Cuda), "took over while behind");
        dec.candidate.as_mut().unwrap().sent -= 3;
        dec.feed(&packets[7]);
        assert!(dec.hw().is_none(), "it never took over once it had caught up");
    }

    #[test]
    fn a_late_keyframe_is_not_the_cards_failure() {
        // One keyframe and then only P-frames: the card started beside software has none to start from
        let Some(packets) = testenc::h264(&[80; 40], 64, 48, 100) else { return };
        let _turn = card_test();
        let mut dec = software();
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        dec.on_keyframe_needed(Box::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
        }));
        dec.feed(&packets[0]);
        dec.retry_hw = Some(Hw::Cuda);
        for p in &packets[1..] {
            slow::advance(Duration::from_millis(500));
            dec.feed(p);
        }
        assert!(!hardware_failed(), "a keyframe that was late made the card a failure for the whole run");
        assert!(dec.retry_hw == Some(Hw::Cuda) && dec.hw().is_none());
        assert!(asked.load(Ordering::SeqCst) >= 2, "another card decoder was started and asked for its own keyframe");
        // The keyframe comes at last
        let more = testenc::h264(&[90; 8], 64, 48, 100).unwrap();
        for p in &more {
            dec.feed(p);
        }
        assert!(dec.hw() == Some(Hw::Cuda) && !hardware_failed());
    }

    #[test]
    fn a_card_that_was_given_a_keyframe_and_made_nothing_of_it_has_failed() {
        let Some(packets) = testenc::h264(&[80; 40], 64, 48, 100) else { return };
        let _turn = card_test();
        let mut dec = software();
        dec.card = (Duration::ZERO, Duration::ZERO);
        dec.feed(&packets[0]);
        dec.retry_hw = Some(Hw::Cuda);
        dec.feed(&packets[1]);
        let cand = dec.candidate.as_mut().expect("a card started beside software");
        // It decoded from a keyframe without error and gave no picture for as long as it was allowed
        cand.began = slow::now() - slow::START_LIMIT;
        if let Engine::Inline { reports, .. } = &mut cand.engine {
            *reports = VecDeque::from([Report::Quiet]);
        }
        cand.reported = cand.sent - 1;
        dec.poll_candidate();
        assert!(dec.candidate.is_none() && hardware_failed());
    }

    #[test]
    fn software_decoding_slower_than_the_stream_ends_the_trial_early_as_a_speed_verdict() {
        let Some(packets) = rising(120, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        dec.feed(&packets[1]);
        let cand = dec.candidate.as_mut().expect("a trial started");
        // 25 pictures in 2 s of a 30 a second stream, with a second or more of it still unanswered
        let now = slow::now();
        cand.began = now - Duration::from_secs(3);
        cand.first_picture = Some(now - Duration::from_secs(2));
        (cand.pictures, cand.sent, cand.reported, cand.started, cand.ready) = (25, 90, 30, true, true);
        if let Engine::Inline { reports, .. } = &mut cand.engine {
            reports.clear();
        }
        dec.poll_candidate();
        assert!(dec.candidate.is_none(), "the trial ran on although software was clearly too slow");
        assert!(!hardware_failed(), "a slow decoder was counted as a broken one");
        let policy = dec.policy.lock().unwrap();
        assert!(policy.lost.active() && !policy.cooldown.active(), "the card wasn't kept with the usual wait");
    }

    #[test]
    fn a_trial_that_gave_no_picture_at_all_still_counts_as_failed() {
        let Some(packets) = rising(120, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        dec.feed(&packets[1]);
        let cand = dec.candidate.as_mut().expect("a trial started");
        cand.began = slow::now() - slow::START_LIMIT;
        (cand.keyframe_seen, cand.pictures) = (true, 0);
        if let Engine::Inline { reports, .. } = &mut cand.engine {
            reports.clear();
        }
        cand.sent = cand.reported + 3;
        dec.poll_candidate();
        assert!(dec.candidate.is_none());
        assert!(dec.policy.lock().unwrap().lost.active());
    }

    #[test]
    fn a_trial_is_not_judged_when_the_stream_changed_while_software_started() {
        let Some(packets) = rising(120, 5) else { return };
        let of = |dec: &H264| match (&dec.candidate, &dec.trial) {
            (Some(Candidate { plan: Plan::Trial { conditions, .. }, .. }), _) => Some(*conditions),
            (_, Some(trial)) => Some(trial.conditions),
            _ => None,
        };
        for change in ["fps", "size", "steps"] {
            let _turn = card_test();
            let mut dec = pretend_hardware(None);
            dec.feed(&packets[0]);
            card_that_took(&mut dec, 45.0);
            dec.feed(&packets[1]);
            assert!(dec.candidate.is_some(), "no trial started for {change}");
            match change {
                "fps" => dec.target_fps.store(15, Ordering::Relaxed),
                // The stream was at another size when the card was measured
                "size" => {
                    if let Some(Candidate { plan: Plan::Trial { conditions, .. }, .. }) = &mut dec.candidate {
                        conditions.size = Some((128, 96));
                    }
                }
                _ => TEST_EPOCH.with(|e| e.set(e.get() + 1)),
            }
            let before = of(&dec).unwrap();
            for p in &packets[2..20] {
                dec.feed(p);
            }
            // A card still too slow may get a new trial, but under the conditions it has now: the old comparison is gone and nothing was decided by it
            let now = of(&dec);
            assert!(now.is_none_or(|c| c != before), "{change}: the comparison made under other conditions is still going");
            let policy = dec.policy.lock().unwrap();
            assert!(!policy.lost.active() && !policy.cooldown.active(), "{change}: software was judged on other terms");
            drop(policy);
            TEST_EPOCH.with(|e| e.set(0));
        }
    }

    #[test]
    fn a_feed_that_would_qualify_after_the_limit_does_not_win_the_trial() {
        let Some(packets) = rising(8, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        let now = slow::now();
        // Software timed 40 pictures in the last three seconds at 5 ms, delivering 30 a second, against a card at 45 ms and 20 a second: a win,
        // if it had come in time
        let mut times = slow::Timings::default();
        for i in 0..40u64 {
            times.record(now - Duration::from_millis(3000 - i * 70), 5.0);
        }
        dec.timings.lock().unwrap().times = times;
        let mut rate = Rate::default();
        for i in 0..=34u64 {
            rate.record(now - Duration::from_millis(3400 - i * 100), i * 3);
        }
        dec.rate = rate;
        TEST_DELIVERED.with(|d| d.set(102));
        let conditions = dec.conditions();
        dec.trial = Some(Trial { hw: Hw::Cuda, card_ms: 45.0, card_rate: Some(20.0), began: now - slow::TRIAL_LIMIT - Duration::from_secs(1), conditions, step_generation: dec.steps.as_ref().map_or(0, Steps::generation) });
        dec.begin_trying();
        dec.judge_trial(now);
        let policy = dec.policy.lock().unwrap();
        assert!(dec.trial.is_none() && policy.lost.waiting(now) && !policy.cooldown.active(), "software won after its time was up");
    }

    #[test]
    fn a_candidate_is_dropped_when_nobody_looks_and_never_takes_over_unseen() {
        let Some(packets) = rising(30, 5) else { return };
        let _turn = card_test();
        let mut dec = pretend_hardware(None);
        dec.feed(&packets[0]);
        card_that_took(&mut dec, 45.0);
        dec.feed(&packets[1]);
        assert!(dec.candidate.is_some());
        dec.feed_with(&packets[2], false);
        assert!(dec.candidate.is_none() && dec.hw() == Some(Hw::Cuda) && !hardware_slow());
        for p in &packets[3..10] {
            dec.feed_with(p, false);
        }
        assert!(dec.candidate.is_none() && dec.hw() == Some(Hw::Cuda), "nothing is started beside it while nobody looks");
    }

    // ── The capture time each picture comes out with ──

    /// How far the fake phone's boot clock is ahead of this computer's.
    const AHEAD_NS: i64 = 7_000_000_000_000;

    /// When picture i was taken on the phone's boot clock, and on this computer's once the offset is taken off.
    fn phone_ns(i: usize) -> u64 {
        9_000_000_000_000 + i as u64 * 33_333_333
    }

    fn desktop_ns(i: usize) -> u64 {
        (phone_ns(i) as i64 - AHEAD_NS) as u64
    }

    fn known_clock() -> Arc<PhoneClock> {
        let clock = Arc::new(PhoneClock::default());
        clock.set(telescope_core::clock::ClockOffset { boot_ns: Some(AHEAD_NS), mono_ns: Some(AHEAD_NS + 5), error_ns: 1_000 });
        clock
    }

    /// The packet with a capture time SEI in front of its first slice, as the phone sends it (after any SPS and PPS).
    fn stamped(packet: &[u8], clock: u8, ns: u64) -> Vec<u8> {
        let at = start_codes(packet).into_iter().find(|&(_, e)| matches!(packet[e] & 31, 1 | 5)).map(|(s, _)| s).expect("a slice");
        [&packet[..at], &crate::stamp::sei(clock, ns), &packet[at..]].concat()
    }

    /// Which of the flat greys in values a frame shows (they're far enough apart to tell).
    fn which(frame: &Frame, values: &[u8]) -> usize {
        let m = mean(frame);
        values.iter().position(|&v| (luma(v) - m).abs() <= 8.0).unwrap_or_else(|| panic!("no grey is {m}"))
    }

    const GREYS: [u8; 6] = [20, 60, 100, 140, 180, 220];

    fn stamped_greys() -> Option<Vec<Vec<u8>>> {
        let packets = testenc::h264(&GREYS, 64, 48, 5)?;
        Some(packets.iter().enumerate().map(|(i, p)| stamped(p, 0, phone_ns(i))).collect())
    }

    #[test]
    fn each_frame_comes_out_with_the_crop_its_packet_was_tagged_with() {
        let Some(packets) = testenc::h264(&GREYS, 64, 48, 5) else { return };
        let crop = |i: usize| telescope_core::view::PhoneZoom { ratio: 1.0, crop: 1.0 + i as f64 / 4.0, x: 0.5, y: 0.5 };
        let mut dec = software();
        for (i, p) in packets.iter().enumerate() {
            let at = start_codes(p).into_iter().find(|&(_, e)| matches!(p[e] & 31, 1 | 5)).map(|(s, _)| s).expect("a slice");
            // Fed in pieces, so a picture comes out after the next packet's tag has been read
            let tagged = [&p[..at], &crate::stamp::sei_with_crop(0, phone_ns(i), crop(i)), &p[at..]].concat();
            let frame = tagged.chunks(97).filter_map(|c| dec.feed(c).0).last().or_else(|| dec.feed(AUD).0).expect("a frame for every packet");
            assert_eq!((which(&frame, &GREYS), frame.phone_share), (i, Some(crop(i))));
        }
        // An older phone tags nothing
        let mut dec = software();
        let frame = dec.feed(&stamped(&packets[0], 0, phone_ns(0))).0.expect("a frame");
        assert_eq!(frame.phone_share, None);
    }

    #[test]
    fn each_frame_comes_out_with_the_time_its_packet_was_stamped_with() {
        let Some(packets) = stamped_greys() else { return };
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        for (i, p) in packets.iter().enumerate() {
            let frame = dec.decode(&Packet { data: p.clone(), time_ns: 5 }).newest.expect("a frame for every packet");
            assert_eq!((which(&frame, &GREYS), frame.capture_ns), (i, Some(desktop_ns(i))));
            assert_eq!(frame.time_ns, 5, "the time it arrived is its own");
        }
    }

    #[test]
    fn pieces_of_any_size_give_every_frame_its_own_time_in_order() {
        let Some(packets) = stamped_greys() else { return };
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        let got: Vec<(usize, Option<u64>)> = packets.concat().chunks(97).filter_map(|c| dec.feed(c).0).map(|f| (which(&f, &GREYS), f.capture_ns)).collect();
        assert!(got.len() >= 5, "{got:?}");
        assert!(got.iter().all(|&(i, t)| t == Some(desktop_ns(i))), "{got:?}");
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn a_burst_keeps_the_time_of_the_newest_frame_it_shows() {
        let Some(packets) = stamped_greys() else { return };
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        let (frame, count) = dec.feed(&packets.concat());
        let frame = frame.unwrap();
        assert_eq!((count, which(&frame, &GREYS), frame.capture_ns), (6, 5, Some(desktop_ns(5))));
    }

    #[test]
    fn the_phones_other_clock_is_read_against_its_own_offset() {
        let Some(packets) = testenc::h264(&[60, 120], 64, 48, 5) else { return };
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        let times: Vec<_> = packets.iter().map(|p| dec.feed(&stamped(p, 1, 9_000_000_000_000)).0.unwrap().capture_ns).collect();
        assert_eq!(times, [Some(9_000_000_000_000 - AHEAD_NS as u64 - 5); 2]);
    }

    #[test]
    fn without_a_stamp_or_a_known_offset_a_frame_has_no_capture_time() {
        let Some(packets) = stamped_greys() else { return };
        let Some(plain) = testenc::h264(&GREYS, 64, 48, 5) else { return };
        // Stamped, but the phone hasn't answered a round trip yet
        let mut waiting = software();
        waiting.set_phone_clock(Arc::new(PhoneClock::default()));
        assert!(waiting.feed(&packets[0]).0.unwrap().capture_ns.is_none());
        // An older phone: no stamps at all
        let mut old = software();
        old.set_phone_clock(known_clock());
        assert!(plain.iter().all(|p| old.feed(p).0.unwrap().capture_ns.is_none()));
        // And a decoder nobody gave a clock
        assert!(software().feed(&packets[0]).0.unwrap().capture_ns.is_none());
    }

    #[test]
    fn a_stream_without_delimiters_between_packets_keeps_times_with_their_pictures() {
        let Some(packets) = testenc::h264(&GREYS, 64, 48, 5) else { return };
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        // As UDP delivers them: a whole frame a packet and no delimiter, so the parser only knows a picture is over when the next one starts
        let got: Vec<(usize, Option<u64>)> = packets.iter().enumerate()
            .filter_map(|(i, p)| dec.feed(&stamped(&p[..p.len() - AUD.len()], 0, phone_ns(i))).0)
            .map(|f| (which(&f, &GREYS), f.capture_ns)).collect();
        assert!(got.len() >= 4, "{got:?}");
        assert!(got.iter().all(|&(i, t)| t == Some(desktop_ns(i))), "{got:?}");
    }

    #[test]
    fn a_decoder_that_holds_pictures_back_still_hands_each_one_its_own_time() {
        let Some(packets) = stamped_greys() else { return };
        // Whole-picture threads: a picture comes out two packets after the one that made it
        let codec = Codec::h264_threaded(crate::ffmpeg::Threads { count: 3, whole_frames: true }).unwrap();
        let mut dec = H264::with_codec(codec, None);
        dec.set_phone_clock(known_clock());
        let got: Vec<(usize, usize, Option<u64>)> = packets.iter().enumerate()
            .filter_map(|(fed, p)| dec.feed(p).0.map(|f| (fed, which(&f, &GREYS), f.capture_ns))).collect();
        assert!(got.iter().any(|&(fed, shown, _)| shown < fed), "the decoder held pictures back: {got:?}");
        assert!(got.iter().all(|&(_, shown, t)| t == Some(desktop_ns(shown))), "a time went to the wrong picture: {got:?}");
    }

    #[test]
    fn pictures_decoded_out_of_taking_order_keep_the_time_they_were_taken_at() {
        let values = [20, 60, 100, 140, 180, 220, 40, 80, 120, 160];
        let Some(packets) = testenc::h264_reordered(&values, 64, 48) else { return };
        // The fixture has B-frames: packets aren't in the order the pictures were taken
        assert!(packets.windows(2).any(|w| w[1].1 < w[0].1), "no reordering in {:?}", packets.iter().map(|p| p.1).collect::<Vec<_>>());
        let mut dec = software();
        dec.set_phone_clock(known_clock());
        let mut seen = Vec::new();
        // The phone stamps each packet with the time its picture was taken, which is what the packet's pts is
        for (p, taken) in &packets {
            if let Some(f) = dec.feed(&stamped(p, 0, phone_ns(*taken))).0 {
                seen.push((which(&f, &values), f.capture_ns));
            }
        }
        assert!(seen.len() >= packets.len() - 3, "{seen:?}");
        assert!(seen.iter().all(|&(shown, t)| t == Some(desktop_ns(shown))), "a time went to the wrong picture: {seen:?}");
    }

    #[test]
    fn a_card_decoder_gives_each_frame_its_time_too() {
        let Some(packets) = testenc::h264(&GREYS, 320, 240, 5) else { return };
        let _turn = card_test();
        let mut card = H264::new(true, None).unwrap();
        if card.hw().is_none() {
            return eprintln!("no graphics card decoder; skipped");
        }
        card.set_phone_clock(known_clock());
        for (i, p) in packets.iter().enumerate() {
            let frame = card.feed(&stamped(p, 0, phone_ns(i))).0.expect("a frame");
            assert_eq!(frame.capture_ns, Some(desktop_ns(i)), "picture {i} on {:?}", card.hw());
        }
        assert!(card.hw().is_some(), "still on the card");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::testenc;
    use std::time::Instant;

    /// cargo test -p telescope-media --release -- --ignored --nocapture decode_speed
    #[test]
    #[ignore]
    fn decode_speed() {
        let Some(packets) = testenc::h264(&(0..120).map(|i| (i * 2) as u8).collect::<Vec<_>>(), 1920, 1080, 30) else { return };
        for hardware in [false, true] {
            let mut dec = H264::new(hardware, None).unwrap();
            let start = Instant::now();
            let mut last = None;
            let frames: u32 = packets.iter().map(|p| {
                let (f, n) = dec.feed(p);
                last = f.or(last.take());
                n
            }).sum();
            let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            // The same picture either way: luma and chroma of the last frame
            let f = last.unwrap();
            let (y, u, v) = f.i420_planes().unwrap();
            let avg = |p: &[u8]| p.iter().map(|&b| b as u64).sum::<u64>() / p.len() as u64;
            println!("{:?}: {frames} frames at 1080p, {ms:.2} ms each, hardware failed: {}, last frame Y/U/V {}/{}/{}",
                     dec.hw(), hardware_failed(), avg(y), avg(u), avg(v));
        }
    }

    /// Process CPU time in seconds (user and system, every thread) from /proc.
    fn cpu_seconds() -> f64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0
    }

    /// Decode time and CPU of a whole clip a frame at a time, software against the card.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture card_against_software
    #[test]
    #[ignore = "a measurement, needs the clips and a VA-API card"]
    fn card_against_software() {
        use crate::ffmpeg::decode_bench::packets;
        for name in ["p1080.h264", "p4k.h264"] {
            let Some(packets) = packets(name) else { continue };
            for hardware in [false, true, false, true] {
                let mut dec = H264::new(hardware, None).unwrap();
                let (cpu, began) = (cpu_seconds(), Instant::now());
                let frames: u32 = packets.iter().map(|p| dec.feed(p).1).sum();
                let (wall, cpu) = (began.elapsed().as_secs_f64(), cpu_seconds() - cpu);
                println!("{name:11} {:>9?}: {frames} pictures, {:.2} ms each, {:.2} ms of cpu each, hardware failed: {}",
                         dec.hw(), wall * 1e3 / frames as f64, cpu * 1e3 / frames as f64, hardware_failed());
            }
        }
    }

    /// A stream fed in chunks of several access units, as after a stall: only the newest picture of each chunk is kept.
    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture burst_feeding
    #[test]
    #[ignore = "a measurement, needs the clips"]
    fn burst_feeding() {
        use crate::ffmpeg::decode_bench::{digest, packets};
        for hardware in [false, true] {
            for name in ["p1080.h264", "p4k.h264"] {
                let Some(packets) = packets(name) else { continue };
                for group in [1usize, 2, 4, 8] {
                    let mut best = f64::MAX;
                    let (mut hash, mut backend, mut finished) = (0, None, 0);
                    for _ in 0..3 {
                        let mut dec = H264::new(hardware, None).unwrap();
                        let (mut newest, mut times) = (Vec::new(), Vec::new());
                        finished = 0;
                        for chunk in packets.chunks(group) {
                            let data = chunk.concat();
                            let began = Instant::now();
                            let (f, n) = dec.feed(&data);
                            times.push(began.elapsed().as_secs_f64() * 1e3);
                            finished += n;
                            newest.extend(f);
                        }
                        assert!(newest.len() > 10, "no pictures kept");
                        let total: f64 = times.iter().sum();
                        if total < best {
                            best = total;
                            hash = digest(&newest);
                            backend = dec.hw();
                        }
                    }
                    println!("{:>7?} {name:11} {group} access units per feed: {:6.2} ms per picture, {finished} pictures, kept hash {hash:016x}", backend, best / finished.max(1) as f64);
                }
            }
        }
    }
}
