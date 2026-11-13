//! How streaming has been going, for Copy diagnostics: stage times, the worst stalls and a timeline, with nothing that names a window or program.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// The stages time::span reports, in the order a frame passes through them (the decoder first, the camera last).
pub const STAGES: [&str; 10] = ["decode", "receive", "take_frame", "transfer", "pipeline", "upload", "submit", "gpu_wait", "readback", "camera"];
/// How many of a stage's latest times its summary is taken from.
const WINDOW: usize = 512;

/// One stage's latest times in microseconds (0 is an empty slot).
struct Stage {
    ring: [AtomicU32; WINDOW],
    next: AtomicUsize,
}

#[allow(clippy::declare_interior_mutable_const)]
const NEW_STAGE: Stage = Stage { ring: [const { AtomicU32::new(0) }; WINDOW], next: AtomicUsize::new(0) };
static SEEN: [Stage; STAGES.len()] = [NEW_STAGE; STAGES.len()];

/// A stage took ns. Two atomic stores and a compare, so it's fine for every frame on every thread.
pub fn record(stage: &str, ns: u64) {
    let Some(i) = STAGES.iter().position(|s| *s == stage) else { return };
    let us = (ns / 1000).min(u32::MAX as u64 - 1) as u32;
    crate::stats::note_stage(i, ns);
    let s = &SEEN[i];
    s.ring[s.next.fetch_add(1, Ordering::Relaxed) % WINDOW].store(us + 1, Ordering::Relaxed);
    let mut active = ACTIVE.load(Ordering::Relaxed);
    while active != 0 {
        let w = active.trailing_zeros() as usize;
        PEAKS[w][i].fetch_max(us + 1, Ordering::Relaxed);
        active &= active - 1;
    }
}

/// How many streams can follow their own slowest stage at once.
const WATCHERS: usize = 8;
#[allow(clippy::declare_interior_mutable_const)]
const NO_PEAKS: [AtomicU32; STAGES.len()] = [const { AtomicU32::new(0) }; STAGES.len()];
/// Each watcher's slowest time of every stage since it last asked.
static PEAKS: [[AtomicU32; STAGES.len()]; WATCHERS] = [NO_PEAKS; WATCHERS];
/// Which watchers are following, one bit each.
static ACTIVE: AtomicU32 = AtomicU32::new(0);

/// A claim on one of the watcher slots, so a stream's peaks aren't taken by another stream's watch.
pub struct PeakWatch(usize);

impl PeakWatch {
    /// None when every slot is taken.
    pub fn claim() -> Option<PeakWatch> {
        for w in 0..WATCHERS {
            let bit = 1u32 << w;
            if ACTIVE.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
                for p in &PEAKS[w] {
                    p.store(0, Ordering::Relaxed);
                }
                return Some(PeakWatch(w));
            }
        }
        None
    }

    /// The stage that took longest since the last call, and how many milliseconds; starts this watcher's peaks over.
    pub fn take(&self) -> Option<(&'static str, f64)> {
        let mut worst: Option<(&'static str, u32)> = None;
        for (name, p) in STAGES.iter().zip(&PEAKS[self.0]) {
            let peak = p.swap(0, Ordering::Relaxed);
            if peak > worst.map_or(0, |w| w.1) {
                worst = Some((name, peak));
            }
        }
        worst.map(|(name, us)| (name, (us - 1) as f64 / 1000.0))
    }
}

impl Drop for PeakWatch {
    fn drop(&mut self) {
        ACTIVE.fetch_and(!(1u32 << self.0), Ordering::AcqRel);
    }
}

/// How a stage did over its latest times.
#[derive(Clone, Debug, PartialEq)]
pub struct StageSummary {
    pub stage: &'static str,
    /// Times seen since the program started.
    pub count: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Each stage that has run, summed up over its latest times.
pub fn stage_summaries() -> Vec<StageSummary> {
    STAGES.iter().zip(&SEEN).filter_map(|(name, s)| {
        let count = s.next.load(Ordering::Relaxed) as u64;
        let mut times: Vec<u32> = s.ring.iter().map(|t| t.load(Ordering::Relaxed)).filter(|t| *t > 0).collect();
        times.sort_unstable();
        let at = |share: f64| (times[((times.len() as f64 * share).ceil() as usize).clamp(1, times.len()) - 1] - 1) as f64 / 1000.0;
        (!times.is_empty()).then(|| StageSummary { stage: name, count, p50_ms: at(0.5), p99_ms: at(0.99), max_ms: at(1.0) })
    }).collect()
}

/// Milliseconds the short way: one decimal under 10, whole above.
pub fn ms(ms: f64) -> String {
    if ms < 10.0 { format!("{ms:.1}") } else { format!("{ms:.0}") }
}

/// One line for the log with every stage's p50/p99/max over its latest times, or None before anything has run.
pub fn stage_line() -> Option<String> {
    let summaries = stage_summaries();
    (!summaries.is_empty()).then(|| {
        let each: Vec<String> = summaries.iter().map(|s| format!("{} {}/{}/{}", s.stage, ms(s.p50_ms), ms(s.p99_ms), ms(s.max_ms))).collect();
        format!("Stage times p50/p99/max ms over the latest frames: {}", each.join(", "))
    })
}

/// What had the screen, as far as Telescope is told: never its title, program or number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    Unknown,
    /// Nothing, or the desktop.
    Nothing,
    Telescope,
    /// Another program's window, filling a screen.
    Fullscreen,
    /// Another program's window.
    Windowed,
}

impl Foreground {
    pub fn label(self) -> &'static str {
        match self {
            Foreground::Unknown => "unknown",
            Foreground::Nothing => "none",
            Foreground::Telescope => "telescope",
            Foreground::Fullscreen => "fullscreen (other app)",
            Foreground::Windowed => "windowed (other app)",
        }
    }
}

/// Whether the window in front fills its screen, is another window, is Telescope's or isn't there. Linux can't say.
pub fn foreground() -> Foreground {
    #[cfg(windows)]
    {
        foreground_on_windows()
    }
    #[cfg(not(windows))]
    Foreground::Unknown
}

#[cfg(windows)]
fn foreground_on_windows() -> Foreground {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> *mut c_void;
        fn GetShellWindow() -> *mut c_void;
        fn GetDesktopWindow() -> *mut c_void;
        fn GetWindowRect(window: *mut c_void, rect: *mut Rect) -> i32;
        fn GetWindowLongW(window: *mut c_void, index: i32) -> i32;
        fn GetWindowThreadProcessId(window: *mut c_void, process: *mut u32) -> u32;
        fn GetClassNameW(window: *mut c_void, name: *mut u16, max: i32) -> i32;
        fn MonitorFromWindow(window: *mut c_void, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MonitorInfo) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessId() -> u32;
    }
    const GWL_STYLE: i32 = -16;
    const WS_CAPTION: i32 = 0x00C0_0000;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    // SAFETY: every pointer goes to a local of the type the call documents, and a window that closes meanwhile just makes a call fail
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() || window == GetShellWindow() || window == GetDesktopWindow() {
            return Foreground::Nothing;
        }
        let mut class = [0u16; 32];
        let len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "WorkerW" || class == "Progman" {
            return Foreground::Nothing;
        }
        let mut process = 0u32;
        GetWindowThreadProcessId(window, &mut process);
        if process == GetCurrentProcessId() {
            return Foreground::Telescope;
        }
        let (mut rect, mut screen) = (Rect::default(), MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, ..Default::default() });
        if GetWindowRect(window, &mut rect) == 0 || GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut screen) == 0 {
            return Foreground::Unknown;
        }
        let m = &screen.monitor;
        // A maximised window has a title bar; one that covers the screen without it is a game or a video
        let covers = rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom;
        if covers && GetWindowLongW(window, GWL_STYLE) & WS_CAPTION != WS_CAPTION { Foreground::Fullscreen } else { Foreground::Windowed }
    }
}

/// Which decoder was at work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    Unknown,
    Hardware,
    Software,
}

impl DecoderKind {
    /// From what decoding_status lists: a device name or "processor" for each decoder running.
    pub fn of(backends: &[String]) -> DecoderKind {
        match (backends.is_empty(), backends.iter().any(|b| b != "processor")) {
            (true, _) => DecoderKind::Unknown,
            (false, true) => DecoderKind::Hardware,
            (false, false) => DecoderKind::Software,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DecoderKind::Unknown => "unknown",
            DecoderKind::Hardware => "hardware",
            DecoderKind::Software => "software",
        }
    }
}

/// A frame gap this long is a stall.
pub const GAP_MS: u64 = 250;
/// How many stalls the session keeps.
pub const KEPT_STALLS: usize = 10;

/// One bad moment of a stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Stall {
    /// Local time of day it was noticed, "14:03:22".
    pub at: String,
    /// The gap between frames, or the length of the window that ran under half the rate before it.
    pub ms: u64,
    /// A gap between frames, rather than a window of slow ones.
    pub gap: bool,
    /// The slowest stage meanwhile, and its time.
    pub stage: &'static str,
    pub stage_ms: f64,
    /// Frames and Mbps a second in the window before and the one after (None while not known yet).
    pub before: Option<(f32, f32)>,
    pub after: Option<(f32, f32)>,
    pub decoder: DecoderKind,
    pub foreground: Foreground,
}

impl Stall {
    /// One line of the report.
    pub fn line(&self) -> String {
        let rates = |w: Option<(f32, f32)>| w.map_or("?/?".to_string(), |(fps, mbps)| format!("{fps:.0}/{mbps:.1}"));
        let stage = if self.stage.is_empty() { String::new() } else { format!(", slowest {} {} ms", self.stage, ms(self.stage_ms)) };
        format!("{} {} ms {}{stage}, fps/Mbps {} then {}, {} decoder, front: {}", self.at, self.ms, if self.gap { "gap" } else { "slow window" },
                rates(self.before), rates(self.after), self.decoder.label(), self.foreground.label())
    }
}

static STALLS: Mutex<Vec<Stall>> = Mutex::new(Vec::new());

/// Keeps stall if it's among the worst KEPT_STALLS of the session.
pub fn keep_stall(stall: Stall) {
    let mut kept = STALLS.lock().unwrap_or_else(|e| e.into_inner());
    kept.push(stall);
    if kept.len() > KEPT_STALLS {
        let smallest = (0..kept.len()).min_by_key(|&i| kept[i].ms).unwrap_or(0);
        kept.remove(smallest);
    }
}

/// The worst stalls so far, worst first.
pub fn worst_stalls() -> Vec<Stall> {
    let mut kept = STALLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    kept.sort_by_key(|s| std::cmp::Reverse(s.ms));
    kept
}

/// One stream's watch for stalls: fed each new frame and each two-second window by the stream's own thread.
pub struct StallWatch {
    last_frame_ns: Option<u64>,
    /// The window before the current one.
    before: Option<(f32, f32)>,
    /// A stall waiting for the window after it, and the number of the window it happened in.
    pending: Option<(Stall, u64)>,
    window: u64,
    gap_in_window: bool,
    window_peak: Option<(&'static str, f64)>,
    /// This stream's own view of the slowest stage.
    peaks: Option<PeakWatch>,
    /// Where a finished stall goes: keep_stall, or a test's own.
    sink: fn(Stall),
}

impl Default for StallWatch {
    fn default() -> Self {
        StallWatch::new()
    }
}

/// What a stall notes about the machine, asked for only when there is one.
pub type Context<'a> = &'a dyn Fn() -> (DecoderKind, Foreground);

impl StallWatch {
    pub fn new() -> StallWatch {
        StallWatch::into(keep_stall)
    }

    /// A watch that hands each finished stall to sink.
    pub fn into(sink: fn(Stall)) -> StallWatch {
        StallWatch { last_frame_ns: None, before: None, pending: None, window: 0, gap_in_window: false, window_peak: None, peaks: PeakWatch::claim(), sink }
    }

    /// No frames are made on purpose for a while (the stream is paused while nobody watches): the gap before the next one is no stall.
    pub fn rest(&mut self) {
        self.last_frame_ns = None;
    }

    /// A new frame came out at now_ns (time::monotonic_ns).
    pub fn frame(&mut self, now_ns: u64, context: Context) {
        let peak = self.peaks.as_ref().and_then(PeakWatch::take);
        if peak.is_some_and(|p| p.1 > self.window_peak.map_or(0.0, |w| w.1)) {
            self.window_peak = peak;
        }
        let Some(last) = self.last_frame_ns.replace(now_ns) else { return };
        let gap_ms = now_ns.saturating_sub(last) / 1_000_000;
        if gap_ms < GAP_MS {
            return;
        }
        self.gap_in_window = true;
        if self.pending.as_ref().is_some_and(|(p, _)| p.ms >= gap_ms) {
            return;
        }
        let (decoder, foreground) = context();
        let (stage, stage_ms) = peak.unwrap_or(("", 0.0));
        self.pending = Some((Stall { at: now_text(), ms: gap_ms, gap: true, stage, stage_ms, before: self.before, after: None, decoder, foreground }, self.window));
    }

    /// A window of secs seconds ended at this rate; judged says whether it says anything about the link (not while settling).
    pub fn window(&mut self, secs: f64, fps: f32, mbps: f32, judged: bool, context: Context) {
        let now = (fps, mbps);
        if self.pending.as_ref().is_some_and(|(_, w)| *w < self.window) {
            let (mut stall, _) = self.pending.take().expect("checked just above");
            stall.after = Some(now);
            (self.sink)(stall);
        }
        let dropped = self.before.is_some_and(|(before, _)| judged && before >= 10.0 && fps < before * 0.5);
        if dropped && !self.gap_in_window {
            let (decoder, foreground) = context();
            let (stage, stage_ms) = self.window_peak.unwrap_or(("", 0.0));
            let stall = Stall { at: now_text(), ms: (secs * 1000.0) as u64, gap: false, stage, stage_ms, before: self.before, after: None, decoder, foreground };
            self.pending = Some((stall, self.window));
        }
        self.before = Some(now);
        self.window += 1;
        self.gap_in_window = false;
        self.window_peak = None;
    }

    /// The stream ended: a stall still waiting for the window after it is kept as it is.
    pub fn finish(&mut self) {
        if let Some((stall, _)) = self.pending.take() {
            (self.sink)(stall);
        }
    }
}

impl Drop for StallWatch {
    fn drop(&mut self) {
        self.finish();
    }
}

fn now_text() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// One window of one stream, for the timeline.
#[derive(Clone, Copy)]
struct Sample {
    ns: u64,
    slot: u32,
    fps: f32,
    mbps: f32,
}

/// Seconds a character of the timeline stands for, and how many there are: two minutes.
pub const STEP_S: u64 = 2;
pub const COLUMNS: usize = 60;
const RAMP: &[u8] = b" .:-=+*#";

static TIMELINE: Mutex<VecDeque<Sample>> = Mutex::new(VecDeque::new());

/// A window of stream slot ended at now_ns with this rate (time::monotonic_ns).
pub fn note_window(slot: u32, now_ns: u64, fps: f32, mbps: f32) {
    let mut line = TIMELINE.lock().unwrap_or_else(|e| e.into_inner());
    line.push_back(Sample { ns: now_ns, slot, fps, mbps });
    while line.len() > COLUMNS * 2 {
        line.pop_front();
    }
}

/// A line of text for values, a character each from the oldest on the left, and the smallest and largest: space is none, "#" the most, "~" no stream then.
fn ramp(values: &[Option<f32>]) -> String {
    let seen: Vec<f32> = values.iter().flatten().copied().collect();
    let top = seen.iter().copied().fold(0.0f32, f32::max);
    values.iter().map(|v| match v {
        None => '~',
        Some(v) if top <= 0.0 || *v <= 0.0 => ' ',
        Some(v) => RAMP[((v / top * (RAMP.len() - 1) as f32).round() as usize).clamp(1, RAMP.len() - 1)] as char,
    }).collect()
}

/// The last two minutes of every stream with a recent window, two lines each (frames and Mbps), or nothing when it's quiet.
/// With more than one stream each gets a "Stream N" line first, N being its output slot, never a device's name.
pub fn timeline_lines(now_ns: u64) -> Vec<String> {
    let samples: Vec<Sample> = TIMELINE.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
    timeline_from(&samples, now_ns)
}

fn timeline_from(samples: &[Sample], now_ns: u64) -> Vec<String> {
    let mut slots: Vec<u32> = samples.iter().map(|s| s.slot).collect();
    slots.sort_unstable();
    slots.dedup();
    let drawn: Vec<(u32, Vec<String>)> = slots.into_iter().filter_map(|slot| {
        let own: Vec<Sample> = samples.iter().filter(|s| s.slot == slot).copied().collect();
        let lines = stream_lines(&own, now_ns);
        (!lines.is_empty()).then_some((slot, lines))
    }).collect();
    let several = drawn.len() > 1;
    drawn.into_iter().flat_map(|(slot, lines)| several.then(|| format!("Stream {slot}:")).into_iter().chain(lines)).collect()
}

fn stream_lines(samples: &[Sample], now_ns: u64) -> Vec<String> {
    let mut columns: [(Option<f32>, Option<f32>); COLUMNS] = [(None, None); COLUMNS];
    for s in samples {
        let ago = (now_ns.saturating_sub(s.ns) / 1_000_000_000 / STEP_S) as usize;
        if ago < COLUMNS {
            columns[COLUMNS - 1 - ago] = (Some(s.fps), Some(s.mbps));
        }
    }
    if columns.iter().all(|c| c.0.is_none()) {
        return Vec::new();
    }
    let part = |pick: fn(&(Option<f32>, Option<f32>)) -> Option<f32>, name: &str| {
        let values: Vec<Option<f32>> = columns.iter().map(pick).collect();
        let seen: Vec<f32> = values.iter().flatten().copied().collect();
        let (low, high) = (seen.iter().copied().fold(f32::MAX, f32::min), seen.iter().copied().fold(0.0f32, f32::max));
        format!("{name} |{}| min {low:.1} max {high:.1}", ramp(&values))
    };
    vec![part(|c| c.0, "fps "), part(|c| c.1, "Mbps")]
}

#[cfg(test)]
#[path = "perf_tests.rs"]
mod tests;
