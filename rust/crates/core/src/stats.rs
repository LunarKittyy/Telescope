//! How streams are doing, as numbers: what arrived, was decoded and went out, what was dropped and why, how long each stage took, and what the
//! app costs. Each stream's worker counts into its own `SourceStats` (atomics only, nothing locked or allocated per frame), and once a second the
//! hub reads them into a `Snapshot` with the last minute of every series, which goes on the bus as `Event::StatsUpdated`. Measuring that costs
//! something per frame (every plugin's frame step) only runs while something asks for it with `Hub::want`, like `want_people` does for vision.

use crate::balance::{Balance, Place, Why};
use crate::event::Event;
use crate::perf::STAGES;
use crate::time::monotonic_ns;
use crate::BusSender;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// How many seconds of every series a snapshot holds.
pub const KEPT: usize = 60;
/// How often the hub makes a snapshot.
pub const EVERY_MS: u64 = 1000;

const SECOND: u64 = 1_000_000_000;

// ── Why frames go missing ────────────────────────────────────────────────────

/// Why a frame never reached the virtual camera. Each is recorded where it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// The link lost it: UDP gave up on a frame that never came whole.
    LostOnLink,
    /// A newer frame had already gone out when it finished.
    Late,
    /// The decoder failed on it.
    DecoderFailed,
    /// Skipped on the way to the next keyframe after a loss.
    WaitingForKeyframe,
    /// Thinned out on purpose: a burst or a busy decoder, so the picture stays live instead of lagging.
    Paced,
    /// A plugin's frame step broke on it.
    StepFailed,
    /// Known to be gone, with nothing above to explain it.
    Unknown,
}

impl DropReason {
    pub const ALL: [DropReason; 7] = [
        DropReason::LostOnLink, DropReason::Late, DropReason::DecoderFailed, DropReason::WaitingForKeyframe,
        DropReason::Paced, DropReason::StepFailed, DropReason::Unknown,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// How many frames were dropped for each reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drops {
    pub lost_on_link: u64,
    pub late: u64,
    pub decoder_failed: u64,
    pub waiting_for_keyframe: u64,
    pub paced: u64,
    pub step_failed: u64,
    pub unknown: u64,
}

impl Drops {
    /// What was dropped since `earlier` (an older reading of the same totals).
    pub fn since(&self, earlier: &Drops) -> Drops {
        Drops {
            lost_on_link: self.lost_on_link.saturating_sub(earlier.lost_on_link),
            late: self.late.saturating_sub(earlier.late),
            decoder_failed: self.decoder_failed.saturating_sub(earlier.decoder_failed),
            waiting_for_keyframe: self.waiting_for_keyframe.saturating_sub(earlier.waiting_for_keyframe),
            paced: self.paced.saturating_sub(earlier.paced),
            step_failed: self.step_failed.saturating_sub(earlier.step_failed),
            unknown: self.unknown.saturating_sub(earlier.unknown),
        }
    }

    pub fn total(&self) -> u64 {
        self.lost_on_link + self.late + self.decoder_failed + self.waiting_for_keyframe + self.paced + self.step_failed + self.unknown
    }

    pub fn of(&self, reason: DropReason) -> u64 {
        match reason {
            DropReason::LostOnLink => self.lost_on_link,
            DropReason::Late => self.late,
            DropReason::DecoderFailed => self.decoder_failed,
            DropReason::WaitingForKeyframe => self.waiting_for_keyframe,
            DropReason::Paced => self.paced,
            DropReason::StepFailed => self.step_failed,
            DropReason::Unknown => self.unknown,
        }
    }

    fn from_counts(counts: [u64; 7]) -> Drops {
        Drops { lost_on_link: counts[0], late: counts[1], decoder_failed: counts[2], waiting_for_keyframe: counts[3], paced: counts[4], step_failed: counts[5], unknown: counts[6] }
    }
}

// ── Where the link goes ──────────────────────────────────────────────────────

/// Which way a stream reaches the computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    #[default]
    Unknown,
    /// Over the cable, through an adb forward.
    Usb,
    /// Over the network: Wi-Fi, a cable to the router or Tailscale.
    Network,
    /// From this computer itself (a webcam, the Browser camera).
    Local,
}

/// How the video travels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Unknown,
    Udp,
    Tcp,
    Local,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub route: Route,
    pub transport: Transport,
}

impl LinkInfo {
    /// From the video URL a phone session opens ("https://127.0.0.1:40001/v1/video.h264", loopback being an adb forward). A plugin's
    /// source reaches nothing outside this computer, whatever its URL says.
    pub fn of_url(url: &str, plugin_source: bool) -> LinkInfo {
        if plugin_source {
            return LinkInfo { route: Route::Local, transport: Transport::Local };
        }
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let authority = rest.split(['/', '?']).next().unwrap_or("");
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = if let Some(v6) = host.strip_prefix('[') { v6.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") };
        let route = match host {
            "" => Route::Unknown,
            "127.0.0.1" | "localhost" | "::1" => Route::Usb,
            _ => Route::Network,
        };
        let path = rest.split('?').next().unwrap_or("");
        let transport = if path.ends_with(".udp") {
            Transport::Udp
        } else if route == Route::Unknown {
            Transport::Unknown
        } else {
            Transport::Tcp
        };
        LinkInfo { route, transport }
    }
}

// ── Timing ───────────────────────────────────────────────────────────────────

/// How long something took over the last second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub mean_ms: f32,
    pub max_ms: f32,
    pub count: u32,
}

/// One thing's times since it was last taken: three atomics, so any thread can add.
#[derive(Default)]
pub struct Timer {
    sum_us: AtomicU64,
    count: AtomicU32,
    max_us: AtomicU32,
}

impl Timer {
    #[inline]
    pub fn add(&self, ns: u64) {
        let us = (ns / 1000).min(u32::MAX as u64) as u32;
        // Count last, and a take reads it first: a sample that is counted has its time and its peak in too
        self.max_us.fetch_max(us, Ordering::Release);
        self.sum_us.fetch_add(us as u64, Ordering::Release);
        self.count.fetch_add(1, Ordering::Release);
    }

    /// What was added since the last take, which starts it over.
    pub fn take(&self) -> Timing {
        let count = self.count.swap(0, Ordering::AcqRel);
        let sum = self.sum_us.swap(0, Ordering::AcqRel);
        let max = self.max_us.swap(0, Ordering::AcqRel);
        if count == 0 {
            return Timing::default();
        }
        // A sample added during the take can be in the sum and not yet in the count, which mustn't put the mean past the slowest
        let max_ms = max as f32 / 1000.0;
        Timing { mean_ms: ((sum as f64 / count as f64 / 1000.0) as f32).min(max_ms), max_ms, count }
    }
}

/// Timers by name (a plugin's), made when the name first comes up and kept: whoever times looks its timer up once and keeps it.
pub struct Timers {
    on: Arc<AtomicBool>,
    list: Mutex<Vec<(String, Arc<Timer>)>>,
}

impl Timers {
    fn new(on: Arc<AtomicBool>) -> Timers {
        Timers { on, list: Mutex::new(Vec::new()) }
    }

    /// Whether anything asks for the expensive measuring right now (Hub::want). Checked once a frame, never held.
    #[inline]
    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn timer(&self, name: &str) -> Arc<Timer> {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, t)) = list.iter().find(|(n, _)| n == name) {
            return t.clone();
        }
        let timer = Arc::new(Timer::default());
        list.push((name.to_string(), timer.clone()));
        timer
    }

    /// Every timer that was added to, with what it saw since the last take.
    fn take_all(&self) -> Vec<NamedTiming> {
        let list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        list.iter().map(|(name, t)| NamedTiming { name: name.clone(), timing: t.take() }).filter(|t| t.timing.count > 0).collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NamedTiming {
    pub name: String,
    pub timing: Timing,
}

// ── What a stream's threads count ────────────────────────────────────────────

#[derive(Default)]
struct StageAcc(Timer);

struct Meta {
    source_id: String,
    slot: u32,
    url: String,
    plugin_source: bool,
    target_fps: u32,
    /// The transport the feed says it uses when that isn't what its URL says (UDP that fell back to TCP).
    transport: Option<Transport>,
}

/// One stream's live numbers, written by its threads and read by the hub. Everything is a relaxed atomic.
pub struct SourceStats {
    started_ns: u64,
    received: AtomicU64,
    bytes: AtomicU64,
    decoded: AtomicU64,
    delivered: AtomicU64,
    repeated: AtomicU64,
    drops: [AtomicU64; 7],
    stages: [StageAcc; STAGES.len()],
    steps: Timers,
    meta: Mutex<Meta>,
}

impl SourceStats {
    fn new(source_id: &str, slot: u32, detailed: Arc<AtomicBool>) -> SourceStats {
        SourceStats {
            started_ns: monotonic_ns(),
            received: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            repeated: AtomicU64::new(0),
            drops: [const { AtomicU64::new(0) }; 7],
            stages: std::array::from_fn(|_| StageAcc::default()),
            steps: Timers::new(detailed),
            meta: Mutex::new(Meta { source_id: source_id.into(), slot, url: String::new(), plugin_source: false, target_fps: 0, transport: None }),
        }
    }

    /// Numbers that belong to no hub, for a test of what counts into them.
    pub fn detached() -> SourceStats {
        SourceStats::new("", 0, Arc::new(AtomicBool::new(false)))
    }

    /// Frames came in from the feed, with the bytes behind them (a frame the decoder skipped to keep up counts too).
    #[inline]
    pub fn received(&self, frames: u64, bytes: u64) {
        self.received.fetch_add(frames, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// The decoder finished this many frames.
    #[inline]
    pub fn decoded(&self, frames: u64) {
        self.decoded.fetch_add(frames, Ordering::Relaxed);
    }

    /// A new frame went to the virtual camera.
    #[inline]
    pub fn delivered(&self) {
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }

    /// The last frame went to the virtual camera again, because no new one came in time.
    #[inline]
    pub fn repeated(&self) {
        self.repeated.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn dropped(&self, reason: DropReason, frames: u64) {
        self.drops[reason.index()].fetch_add(frames, Ordering::Relaxed);
    }

    /// A stage (a name from perf::STAGES, by its index) took this long on one of this stream's threads.
    #[inline]
    fn stage(&self, index: usize, ns: u64) {
        self.stages[index].0.add(ns);
    }

    /// The timers of each plugin's frame step, which the pipeline fills while Timers::on.
    pub fn steps(&self) -> &Timers {
        &self.steps
    }

    /// Where the stream is reading from and the rate it's meant to keep (both change mid-stream).
    pub fn set_link(&self, url: &str, plugin_source: bool) {
        let mut meta = self.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.url = url.into();
        meta.plugin_source = plugin_source;
        meta.transport = None;
    }

    pub fn set_target_fps(&self, fps: u32) {
        self.meta.lock().unwrap_or_else(|p| p.into_inner()).target_fps = fps;
    }

    fn totals(&self) -> Counts {
        Counts {
            received: self.received.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            decoded: self.decoded.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            repeated: self.repeated.load(Ordering::Relaxed),
        }
    }

    fn drop_counts(&self) -> [u64; 7] {
        std::array::from_fn(|i| self.drops[i].load(Ordering::Relaxed))
    }
}

thread_local! {
    static BOUND: RefCell<Option<Arc<SourceStats>>> = const { RefCell::new(None) };
}

/// Puts a stream's numbers on the thread that makes them while it lives, so stage times (perf::record) and drops further down (the link's) are
/// counted for that stream without every call between carrying it.
pub struct Bound {
    before: Option<Arc<SourceStats>>,
}

pub fn bind(stats: &Arc<SourceStats>) -> Bound {
    Bound { before: BOUND.with(|b| b.borrow_mut().replace(stats.clone())) }
}

impl Drop for Bound {
    fn drop(&mut self) {
        let before = self.before.take();
        let _ = BOUND.try_with(|b| *b.borrow_mut() = before);
    }
}

/// The stream this thread is counting for, for a thread it starts that should count for the same one.
pub fn current() -> Option<Arc<SourceStats>> {
    BOUND.try_with(|b| b.borrow().clone()).ok().flatten()
}

fn with_current(f: impl FnOnce(&SourceStats)) {
    let _ = BOUND.try_with(|b| {
        if let Ok(b) = b.try_borrow() {
            if let Some(stats) = b.as_deref() {
                f(stats);
            }
        }
    });
}

/// A stage took ns on this thread (perf::record calls this).
#[inline]
pub(crate) fn note_stage(index: usize, ns: u64) {
    with_current(|s| s.stage(index, ns));
}

/// The decoder finished this many frames on this thread's stream.
pub fn note_decoded(frames: u64) {
    with_current(|s| s.decoded(frames));
}

/// The feed on this thread's stream carries its video this way now, whatever its URL said.
pub fn note_transport(transport: Transport) {
    with_current(|s| s.meta.lock().unwrap_or_else(|p| p.into_inner()).transport = Some(transport));
}

/// Frames dropped for reason on this thread's stream. Nothing happens on a thread no stream is bound to.
pub fn note_drop(reason: DropReason, frames: u64) {
    with_current(|s| s.dropped(reason, frames));
}

// ── Snapshots ────────────────────────────────────────────────────────────────

/// Frames and bytes since the stream started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub received: u64,
    pub bytes: u64,
    pub decoded: u64,
    pub delivered: u64,
    pub repeated: u64,
}

/// Over the last second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub received_fps: f32,
    pub decoded_fps: f32,
    pub delivered_fps: f32,
    pub repeated_fps: f32,
    pub dropped_fps: f32,
    pub mbps: f32,
}

/// The last KEPT seconds of each number, oldest first, one value a second. A time is its slowest in that second, so a spike still shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceSeries {
    pub received_fps: Vec<f32>,
    pub decoded_fps: Vec<f32>,
    pub delivered_fps: Vec<f32>,
    pub repeated_fps: Vec<f32>,
    pub dropped_fps: Vec<f32>,
    pub mbps: Vec<f32>,
    pub decode_ms: Vec<f32>,
    pub step_ms: Vec<f32>,
    pub camera_ms: Vec<f32>,
    /// The drop totals at the end of each of those seconds and of the one before, so what was dropped lately can be told from the whole run.
    #[serde(default)]
    pub drop_totals: Vec<Drops>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub source_id: String,
    /// Which virtual camera it feeds.
    pub slot: u32,
    pub link: LinkInfo,
    /// The rate it's meant to keep.
    pub target_fps: u32,
    /// How long it's been running.
    pub seconds: u64,
    pub totals: Counts,
    pub drops: Drops,
    pub now: Rates,
    /// Each stage that ran in the last second (perf::STAGES), in the order a frame passes them.
    pub stages: Vec<NamedTiming>,
    /// Each plugin's frame step, while something asked for it.
    pub plugins: Vec<NamedTiming>,
    pub series: SourceSeries,
}

/// A move of the balancer: frame steps that can run either way went to the card or the CPU.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BalanceMove {
    pub seconds_ago: f32,
    pub to_card: bool,
    pub why: Why,
    /// What the frame waited for the card, which is what it moved on.
    pub wait_ms: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub on_card: bool,
    /// TELESCOPE_FRAME_STEPS holds it in place.
    pub pinned: bool,
    pub moves: u32,
    pub last_wait_ms: f32,
    /// The latest few, oldest first.
    pub recent: Vec<BalanceMove>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSeries {
    pub cpu_percent: Vec<f32>,
    pub memory_mb: Vec<f32>,
}

/// The app's own cost.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
    /// Of one core, so a busy app on a many-core computer goes past 100. None where the system can't say.
    pub cpu_percent: Option<f32>,
    pub memory_mb: Option<f32>,
    pub balance: BalanceSnapshot,
    /// How long each plugin's hooks (events, card updates, clicks) took over the last second, while something asked.
    pub hooks: Vec<NamedTiming>,
    pub series: AppSeries,
}

/// Everything the hub knows, once a second. Plain numbers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The monotonic clock when it was taken, in ms.
    pub at_ms: u64,
    /// Something asked for the expensive measuring (the `plugins` and `hooks` lists are filled).
    pub detailed: bool,
    pub sources: Vec<SourceSnapshot>,
    pub app: AppSnapshot,
}

// ── The hub ──────────────────────────────────────────────────────────────────

/// What a source's numbers were at the last second, to get this second's rates.
#[derive(Default)]
struct Prev {
    ns: u64,
    totals: Counts,
    dropped: u64,
}

struct Entry {
    stats: Arc<SourceStats>,
    prev: Prev,
    series: SourceSeries,
}

#[derive(Default)]
struct AppTrack {
    prev: Option<(u64, u64)>,
    series: AppSeries,
}

fn keep(series: &mut Vec<f32>, value: f32) {
    if series.len() >= KEPT {
        series.remove(0);
    }
    series.push(value);
}

/// The one place every stream's numbers meet. The app has one (`Hub::shared`); a test makes its own.
pub struct Hub {
    detailed: Arc<AtomicBool>,
    wanted: Mutex<BTreeSet<String>>,
    sources: Mutex<Vec<Entry>>,
    app: Mutex<AppTrack>,
    hooks: Timers,
    balance: Arc<Balance>,
    had_sources: AtomicBool,
    bus: Mutex<Option<BusSender>>,
    ticking: AtomicBool,
}

static SHARED: LazyLock<Arc<Hub>> = LazyLock::new(|| Arc::new(Hub::with_balance(Balance::shared())));

impl Hub {
    pub fn shared() -> Arc<Hub> {
        SHARED.clone()
    }

    /// The same without taking a reference count, for what runs often.
    pub fn global() -> &'static Hub {
        &SHARED
    }

    pub fn new() -> Hub {
        Hub::with_balance(Arc::new(Balance::free()))
    }

    pub fn with_balance(balance: Arc<Balance>) -> Hub {
        let detailed = Arc::new(AtomicBool::new(false));
        Hub {
            hooks: Timers::new(detailed.clone()),
            detailed,
            wanted: Mutex::new(BTreeSet::new()),
            sources: Mutex::new(Vec::new()),
            app: Mutex::new(AppTrack::default()),
            balance,
            had_sources: AtomicBool::new(false),
            bus: Mutex::new(None),
            ticking: AtomicBool::new(false),
        }
    }

    /// A stream starts counting. Hand it back to `unregister` when the stream ends.
    pub fn register(&self, source_id: &str, slot: u32) -> Arc<SourceStats> {
        let stats = Arc::new(SourceStats::new(source_id, slot, self.detailed.clone()));
        let prev = Prev { ns: stats.started_ns, ..Prev::default() };
        self.sources.lock().unwrap_or_else(|p| p.into_inner()).push(Entry { stats: stats.clone(), prev, series: SourceSeries::default() });
        stats
    }

    pub fn unregister(&self, stats: &Arc<SourceStats>) {
        self.sources.lock().unwrap_or_else(|p| p.into_inner()).retain(|e| !Arc::ptr_eq(&e.stats, stats));
    }

    /// owner wants the expensive numbers (each plugin's frame step and hook times) measured, or lets go. They're only taken while someone does.
    pub fn want(&self, owner: &str, on: bool) {
        let mut wanted = self.wanted.lock().unwrap_or_else(|p| p.into_inner());
        if on {
            wanted.insert(owner.to_string());
        } else {
            wanted.remove(owner);
        }
        self.detailed.store(!wanted.is_empty(), Ordering::Relaxed);
    }

    pub fn detailed(&self) -> bool {
        self.detailed.load(Ordering::Relaxed)
    }

    /// The timers of what a plugin's hooks cost, which the host fills while `hooks().on()`.
    pub fn hooks(&self) -> &Timers {
        &self.hooks
    }

    /// A snapshot now, from what the process says about itself.
    pub fn snapshot(&self) -> Snapshot {
        let (cpu, rss) = process_reading();
        self.snapshot_at(monotonic_ns(), cpu, rss)
    }

    /// A snapshot as of now_ns on the monotonic clock, with the process's CPU time so far in ns and its memory in bytes (None: not known).
    pub fn snapshot_at(&self, now_ns: u64, cpu_ns: Option<u64>, rss_bytes: Option<u64>) -> Snapshot {
        let detailed = self.detailed();
        let sources = {
            let mut entries = self.sources.lock().unwrap_or_else(|p| p.into_inner());
            entries.iter_mut().map(|e| sample(e, now_ns)).collect::<Vec<_>>()
        };
        let mut app = self.app.lock().unwrap_or_else(|p| p.into_inner());
        let cpu_percent = cpu_ns.and_then(|cpu| {
            let (before_ns, before_cpu) = app.prev.replace((now_ns, cpu))?;
            (now_ns > before_ns && cpu >= before_cpu).then(|| ((cpu - before_cpu) as f64 / (now_ns - before_ns) as f64 * 100.0) as f32)
        });
        let memory_mb = rss_bytes.map(|b| (b as f64 / (1024.0 * 1024.0)) as f32);
        if let Some(c) = cpu_percent {
            keep(&mut app.series.cpu_percent, c);
        }
        if let Some(m) = memory_mb {
            keep(&mut app.series.memory_mb, m);
        }
        let series = app.series.clone();
        drop(app);
        Snapshot {
            at_ms: now_ns / 1_000_000,
            detailed,
            sources,
            app: AppSnapshot { cpu_percent, memory_mb, balance: self.balance_snapshot(now_ns), hooks: self.hooks.take_all(), series },
        }
    }

    fn balance_snapshot(&self, now_ns: u64) -> BalanceSnapshot {
        let recent = self.balance.moves().into_iter().map(|m| BalanceMove {
            seconds_ago: now_ns.saturating_sub(m.at_ns) as f32 / SECOND as f32,
            to_card: m.to == Place::Card,
            why: m.why,
            wait_ms: m.wait_ns as f32 / 1e6,
        }).collect();
        BalanceSnapshot {
            on_card: self.balance.place() == Place::Card,
            pinned: self.balance.is_pinned(),
            moves: self.balance.swaps(),
            last_wait_ms: self.balance.last_wait() as f32 / 1e6,
            recent,
        }
    }

    /// Whether anything is worth a snapshot: a stream is running, or one just ended and the panel has to hear it.
    fn worth_sending(&self) -> bool {
        let any = !self.sources.lock().unwrap_or_else(|p| p.into_inner()).is_empty();
        self.had_sources.swap(any, Ordering::Relaxed) || any
    }

    /// Sends a snapshot to bus every EVERY_MS from now on while a stream runs, in place of whatever bus it sent to before (one thread serves the process).
    pub fn start(self: &Arc<Self>, bus: BusSender) {
        *self.bus.lock().unwrap_or_else(|p| p.into_inner()) = Some(bus);
        if self.ticking.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = Arc::downgrade(self);
        let spawned = std::thread::Builder::new().name("stats".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(EVERY_MS));
            let Some(hub) = hub.upgrade() else { return };
            let bus = hub.bus.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(bus) = bus.filter(|_| hub.worth_sending()) {
                bus.emit(Event::StatsUpdated { snapshot: hub.snapshot() });
            }
        });
        if let Err(e) = spawned {
            self.ticking.store(false, Ordering::SeqCst);
            log::warn!("Couldn't start the thread that gathers stats: {e}");
        }
    }
}

impl Default for Hub {
    fn default() -> Self {
        Hub::new()
    }
}

fn sample(e: &mut Entry, now_ns: u64) -> SourceSnapshot {
    let totals = e.stats.totals();
    let counts = e.stats.drop_counts();
    let drops = Drops::from_counts(counts);
    let dt = (now_ns.saturating_sub(e.prev.ns)).max(1) as f64 / SECOND as f64;
    let per_second = |now: u64, before: u64| (now.saturating_sub(before) as f64 / dt) as f32;
    let rates = Rates {
        received_fps: per_second(totals.received, e.prev.totals.received),
        decoded_fps: per_second(totals.decoded, e.prev.totals.decoded),
        delivered_fps: per_second(totals.delivered, e.prev.totals.delivered),
        repeated_fps: per_second(totals.repeated, e.prev.totals.repeated),
        dropped_fps: per_second(drops.total(), e.prev.dropped),
        mbps: (totals.bytes.saturating_sub(e.prev.totals.bytes) as f64 * 8.0 / dt / 1e6) as f32,
    };
    e.prev = Prev { ns: now_ns, totals, dropped: drops.total() };
    let stages: Vec<NamedTiming> = STAGES.iter().zip(&e.stats.stages).map(|(name, acc)| NamedTiming { name: (*name).into(), timing: acc.0.take() }).collect();
    let slowest = |name: &str| stages.iter().find(|s| s.name == name).map_or(0.0, |s| s.timing.max_ms);
    let (decode, step, camera) = (slowest("decode"), slowest("pipeline"), slowest("camera"));
    let s = &mut e.series;
    keep(&mut s.received_fps, rates.received_fps);
    keep(&mut s.decoded_fps, rates.decoded_fps);
    keep(&mut s.delivered_fps, rates.delivered_fps);
    keep(&mut s.repeated_fps, rates.repeated_fps);
    keep(&mut s.dropped_fps, rates.dropped_fps);
    keep(&mut s.mbps, rates.mbps);
    keep(&mut s.decode_ms, decode);
    keep(&mut s.step_ms, step);
    keep(&mut s.camera_ms, camera);
    // One more than the other series: a window of KEPT seconds is counted from the total just before it
    if s.drop_totals.len() > KEPT {
        s.drop_totals.remove(0);
    }
    s.drop_totals.push(drops);
    let meta = e.stats.meta.lock().unwrap_or_else(|p| p.into_inner());
    let link = LinkInfo::of_url(&meta.url, meta.plugin_source);
    SourceSnapshot {
        source_id: meta.source_id.clone(),
        slot: meta.slot,
        link: LinkInfo { transport: meta.transport.unwrap_or(link.transport), ..link },
        target_fps: meta.target_fps,
        seconds: now_ns.saturating_sub(e.stats.started_ns) / SECOND,
        totals,
        drops,
        now: rates,
        stages: stages.into_iter().filter(|s| s.timing.count > 0).collect(),
        plugins: e.stats.steps.take_all(),
        series: e.series.clone(),
    }
}

// ── The app's own cost ───────────────────────────────────────────────────────

/// (CPU time this process has used so far in ns, its memory in bytes) from the system; None for what it can't tell.
pub fn process_reading() -> (Option<u64>, Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        // utime and stime are the 14th and 15th fields, counted after the program's name, which may hold spaces and brackets
        let cpu = std::fs::read_to_string("/proc/self/stat").ok().and_then(|text| {
            let after = text.rsplit_once(')')?.1;
            let mut fields = after.split_whitespace().skip(11);
            let (user, system): (u64, u64) = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
            // SAFETY: sysconf only reads a system constant
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
            Some((user + system) * (SECOND / ticks))
        });
        let memory = std::fs::read_to_string("/proc/self/statm").ok().and_then(|text| {
            let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
            // SAFETY: sysconf only reads a system constant
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
            Some(pages * page)
        });
        (cpu, memory)
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[repr(C)]
        #[derive(Default)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct MemoryCounters {
            size: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            rest: [usize; 6],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut c_void;
            fn GetProcessTimes(process: *mut c_void, created: *mut FileTime, exited: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
            fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut MemoryCounters, size: u32) -> i32;
        }
        let ticks = |t: &FileTime| ((t.high as u64) << 32 | t.low as u64) * 100;
        let (mut created, mut exited, mut kernel, mut user) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
        let mut counters = MemoryCounters { size: std::mem::size_of::<MemoryCounters>() as u32, ..Default::default() };
        // SAFETY: the pseudo handle of this process is always valid, and every pointer goes to a local of the type the call documents
        let (cpu, memory) = unsafe {
            let me = GetCurrentProcess();
            (GetProcessTimes(me, &mut created, &mut exited, &mut kernel, &mut user) != 0, K32GetProcessMemoryInfo(me, &mut counters, counters.size) != 0)
        };
        (cpu.then(|| ticks(&kernel) + ticks(&user)), memory.then_some(counters.working_set as u64))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    (None, None)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
