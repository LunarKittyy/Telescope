//! A live look at the whole computer (CPU, graphics card, memory, power, the user's busiest programs), read on demand and never logged, saved or sent.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) mod drm;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as os;
#[cfg(not(any(target_os = "linux", windows)))]
mod other;
#[cfg(not(any(target_os = "linux", windows)))]
use other as os;

/// Whether the computer says it is saving power (battery saver, a power-saver profile, Windows' best power efficiency mode); None where it can't tell. Cheap enough to ask every few seconds.
pub fn power_saver() -> Option<bool> {
    os::power_saver()
}

/// How many busy programs a reading names.
const TOP: usize = 3;
/// One program using more than this much of the whole computer is the likely reason for a slow stream.
const BUSY_PROGRAM: f32 = 40.0;
/// The whole computer or graphics card counts as maxed out above this.
const MAXED: f32 = 85.0;
/// Under this share of memory free, or under `LOW_MEMORY_MB`, memory is short.
const LOW_MEMORY_SHARE: f64 = 0.10;
const LOW_MEMORY_MB: u64 = 500;
/// The computer counts as busy for Telescope's own part to matter above this.
const BUSY: f32 = 60.0;

/// A program's use of the processor or graphics card, as a share of the whole computer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessUse {
    /// The program's file name without folder or extension; same-named processes (a browser's) are added together.
    pub name: String,
    pub percent: f32,
}

/// What the computer is doing right now, from the difference to the last reading. None is "can't tell (without admin rights)".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemReading {
    /// The whole computer, 0 to 100.
    pub cpu_percent: Option<f32>,
    pub cores: u32,
    /// This process's part of the whole computer.
    pub own_cpu_percent: Option<f32>,
    /// The user's other programs using the most processor, biggest first.
    pub busiest: Vec<ProcessUse>,
    pub gpu_percent: Option<f32>,
    /// This process's part of the graphics card, where the system gives per-program numbers (Windows, and Linux drivers with DRM fdinfo).
    pub own_gpu_percent: Option<f32>,
    /// The user's other programs using the most graphics card, biggest first, where the system says.
    pub gpu_busiest: Vec<ProcessUse>,
    pub memory_total_mb: u64,
    pub memory_available_mb: Option<u64>,
    pub on_battery: Option<bool>,
    pub power_saver: Option<bool>,
}

/// Why a stream may be slow, as far as a reading can tell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Cause {
    OtherApp { name: String, cpu_percent: f32 },
    GpuBusy { name: Option<String> },
    BatterySaver,
    /// Only a weak hint: many laptops are fine on battery.
    OnBattery,
    LowMemory,
    TelescopeItself,
}

/// The processor's time counters: busy and all (idle included), in whatever unit the system counts in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// One of the user's processes at a moment: its time counter is in the same unit as `CpuTimes`.
#[derive(Clone, Debug)]
pub(crate) struct RawProcess {
    pub pid: u32,
    /// When it started, which tells a new process from an old one that had its number.
    pub start: u64,
    pub name: Arc<str>,
    pub ticks: u64,
}

/// The graphics card's load: the busiest kind of work overall, and each process's share.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GpuLoad {
    pub total: f32,
    pub per_pid: Vec<(u32, f32)>,
}

/// Everything one look at the system gives, before the difference to the last one is taken.
#[derive(Default)]
pub(crate) struct Sample {
    pub cpu: Option<CpuTimes>,
    pub own_ticks: Option<u64>,
    pub procs: Vec<RawProcess>,
    pub gpu: Option<GpuLoad>,
    pub memory_total_mb: u64,
    pub memory_available_mb: Option<u64>,
    pub on_battery: Option<bool>,
    pub power_saver: Option<bool>,
}

/// What the last look saw, for the next one's rates.
struct Last {
    cpu: Option<CpuTimes>,
    own_ticks: Option<u64>,
    procs: HashMap<(u32, u64), u64>,
}

/// Reads the computer once a second or so. The first reading has no rates; each later one covers the time since the one before.
pub struct Monitor {
    reader: os::Reader,
    last: Option<Last>,
}

impl Monitor {
    pub fn new() -> Monitor {
        Monitor { reader: os::Reader::new(), last: None }
    }

    /// Whether a reading was taken before, so the next one has rates.
    #[cfg(test)]
    pub(crate) fn has_history(&self) -> bool {
        self.last.is_some()
    }

    /// A reading now. Rates (the CPU's and each program's) are None or empty on the first call.
    pub fn read(&mut self) -> SystemReading {
        let now = self.reader.sample();
        let mut reading = SystemReading {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            memory_total_mb: now.memory_total_mb,
            memory_available_mb: now.memory_available_mb,
            on_battery: now.on_battery,
            power_saver: now.power_saver,
            ..SystemReading::default()
        };
        if let Some(last) = &self.last {
            if let (Some(before), Some(after)) = (last.cpu, now.cpu) {
                reading.cpu_percent = cpu_percent(before, after);
                if let Some(total) = after.total.checked_sub(before.total).filter(|t| *t > 0) {
                    reading.own_cpu_percent = last.own_ticks.zip(now.own_ticks).map(|(b, a)| share(a.saturating_sub(b), total));
                    reading.busiest = busiest(&last.procs, &now.procs, total, TOP);
                }
            }
        }
        if let Some(gpu) = &now.gpu {
            reading.gpu_percent = Some(gpu.total.min(100.0));
            let me = std::process::id();
            // Per-process shares exist only where the system gives them; a process absent from them is idle on the card
            if !gpu.per_pid.is_empty() {
                reading.own_gpu_percent = Some(gpu.per_pid.iter().filter(|(pid, _)| *pid == me).map(|(_, v)| *v).fold(0.0, f32::max));
            }
            reading.gpu_busiest = gpu_busiest(&gpu.per_pid, &now.procs, TOP);
        }
        let procs = now.procs.iter().map(|p| ((p.pid, p.start), p.ticks)).collect();
        self.last = Some(Last { cpu: now.cpu, own_ticks: now.own_ticks, procs });
        reading
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor::new()
    }
}

/// A share of the whole computer, in percent, from a counter's change over the processor's total change.
fn share(delta: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    ((delta as f64 / total as f64 * 100.0) as f32).clamp(0.0, 100.0)
}

/// How busy the whole processor was between two looks; None when no time passed or the counters went backwards.
pub(crate) fn cpu_percent(before: CpuTimes, after: CpuTimes) -> Option<f32> {
    let total = after.total.checked_sub(before.total).filter(|t| *t > 0)?;
    Some(share(after.busy.saturating_sub(before.busy), total))
}

/// The programs that used the most processor between two looks, same-named ones added together; processes that weren't there before have no rate yet.
pub(crate) fn busiest(before: &HashMap<(u32, u64), u64>, now: &[RawProcess], total: u64, keep: usize) -> Vec<ProcessUse> {
    let mut used: Vec<(&str, u64)> = now
        .iter()
        .filter_map(|p| Some((&*p.name, p.ticks.checked_sub(*before.get(&(p.pid, p.start))?)?)))
        .filter(|(_, delta)| *delta > 0)
        .collect();
    used.sort_by(|a, b| a.0.cmp(b.0));
    let mut by_name: Vec<(&str, u64)> = Vec::new();
    for (name, delta) in used {
        match by_name.last_mut() {
            Some((last, sum)) if *last == name => *sum += delta,
            _ => by_name.push((name, delta)),
        }
    }
    by_name.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    by_name.into_iter().take(keep).map(|(name, delta)| ProcessUse { name: name.to_string(), percent: share(delta, total) }).collect()
}

/// The user's programs that used the most graphics card, same-named ones added together; a process we can't name (someone else's) is left out.
pub(crate) fn gpu_busiest(per_pid: &[(u32, f32)], procs: &[RawProcess], keep: usize) -> Vec<ProcessUse> {
    let mut used: Vec<(&str, f32)> = per_pid.iter().filter(|(_, v)| *v > 0.0).filter_map(|(pid, v)| Some((&*procs.iter().find(|p| p.pid == *pid)?.name, *v))).collect();
    used.sort_by(|a, b| a.0.cmp(b.0));
    let mut by_name: Vec<(&str, f32)> = Vec::new();
    for (name, v) in used {
        match by_name.last_mut() {
            Some((last, sum)) if *last == name => *sum += v,
            _ => by_name.push((name, v)),
        }
    }
    by_name.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
    by_name.into_iter().take(keep).map(|(name, v)| ProcessUse { name: name.to_string(), percent: v.min(100.0) }).collect()
}

/// The program's name from its path: the file name without folders or extension.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn program_name(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 4 => stem.to_string(),
        _ => file.to_string(),
    }
}

/// The first line of /proc/stat ("cpu  user nice system idle iowait irq softirq steal ...") as busy and total jiffies.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    // guest time is already inside user time, so only the first eight columns are added
    let fields: Vec<u64> = line.split_whitespace().skip(1).take(8).map(|f| f.parse().ok()).collect::<Option<Vec<u64>>>()?;
    if fields.len() < 4 {
        return None;
    }
    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some(CpuTimes { busy: total.saturating_sub(idle), total })
}

/// What one /proc/<pid>/stat holds that is used here.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, PartialEq)]
pub(crate) struct PidStat<'a> {
    pub comm: &'a str,
    /// utime plus stime in jiffies.
    pub ticks: u64,
    /// starttime in jiffies since boot.
    pub start: u64,
}

/// /proc/<pid>/stat; the name sits in brackets and may hold spaces and brackets itself, so the last bracket ends it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_pid_stat(text: &str) -> Option<PidStat<'_>> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?;
    let mut fields = text.get(close + 1..)?.split_whitespace();
    // after the name: state is field 0, utime 11, stime 12, starttime 19
    let ticks = fields.clone().nth(11)?.parse::<u64>().ok()? + fields.clone().nth(12)?.parse::<u64>().ok()?;
    let start = fields.nth(19)?.parse().ok()?;
    Some(PidStat { comm, ticks, start })
}

/// /proc/meminfo's total and available memory in kB.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, PartialEq)]
pub(crate) struct MemInfo {
    pub total_kb: u64,
    pub available_kb: Option<u64>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let kb = |key: &str| -> Option<u64> { text.lines().find_map(|l| l.strip_prefix(key))?.split_whitespace().next()?.parse().ok() };
    Some(MemInfo { total_kb: kb("MemTotal:")?, available_kb: kb("MemAvailable:") })
}

/// The process id and engine type in a Windows graphics counter's instance name ("pid_1234_luid_0x0_0x1_phys_0_eng_0_engtype_3D").
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn parse_gpu_instance(name: &str) -> Option<(u32, &str)> {
    let pid = name.strip_prefix("pid_")?.split('_').next()?.parse().ok()?;
    let kind = name.rsplit_once("engtype_")?.1;
    Some((pid, kind))
}

/// The card's load from its engine counters (instance name and percent): each process's busiest engine type, and overall the busiest engine type's total, capped at 100.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn gpu_load<'a>(instances: impl IntoIterator<Item = (&'a str, f32)>) -> Option<GpuLoad> {
    let mut per_kind: HashMap<&str, f32> = HashMap::new();
    let mut per_pid_kind: HashMap<(u32, &str), f32> = HashMap::new();
    for (name, value) in instances {
        let Some((pid, kind)) = parse_gpu_instance(name) else { continue };
        let value = if value.is_finite() { value.max(0.0) } else { 0.0 };
        *per_kind.entry(kind).or_default() += value;
        *per_pid_kind.entry((pid, kind)).or_default() += value;
    }
    if per_kind.is_empty() {
        return None;
    }
    let mut per_pid: HashMap<u32, f32> = HashMap::new();
    for ((pid, _), value) in per_pid_kind {
        let best = per_pid.entry(pid).or_default();
        *best = best.max(value);
    }
    let mut per_pid: Vec<(u32, f32)> = per_pid.into_iter().map(|(pid, v)| (pid, v.min(100.0))).collect();
    per_pid.sort_by_key(|(pid, _)| *pid);
    Some(GpuLoad { total: per_kind.values().copied().fold(0.0, f32::max).min(100.0), per_pid })
}

/// The most likely reason a stream is slow, if the reading points at one. Weak hints come last.
pub fn likely_cause(reading: &SystemReading) -> Option<Cause> {
    let cpu = reading.cpu_percent.unwrap_or(0.0);
    let own = reading.own_cpu_percent.unwrap_or(0.0);
    if let Some(top) = reading.busiest.first() {
        if top.percent > BUSY_PROGRAM || (cpu > MAXED && own < cpu / 2.0) {
            return Some(Cause::OtherApp { name: top.name.clone(), cpu_percent: top.percent });
        }
    }
    if let Some(gpu) = reading.gpu_percent.filter(|g| *g > MAXED) {
        let own_gpu = reading.own_gpu_percent;
        match reading.gpu_busiest.first() {
            Some(top) if top.percent > own_gpu.unwrap_or(0.0) => return Some(Cause::GpuBusy { name: Some(top.name.clone()) }),
            // Busy, and not Telescope's doing, with the program not one we may name (another user's)
            None if own_gpu.is_some_and(|o| o < gpu / 2.0) => return Some(Cause::GpuBusy { name: None }),
            _ => {}
        }
    }
    if reading.power_saver == Some(true) {
        return Some(Cause::BatterySaver);
    }
    if let Some(free) = reading.memory_available_mb {
        if reading.memory_total_mb > 0 && (free < LOW_MEMORY_MB || (free as f64) < reading.memory_total_mb as f64 * LOW_MEMORY_SHARE) {
            return Some(Cause::LowMemory);
        }
    }
    let gpu_is_ours = reading.gpu_percent.is_some_and(|g| g > MAXED) && reading.own_gpu_percent.is_some_and(|o| o >= reading.gpu_percent.unwrap_or(0.0) / 2.0);
    if (cpu > BUSY && own >= cpu / 2.0) || gpu_is_ours {
        return Some(Cause::TelescopeItself);
    }
    (reading.on_battery == Some(true)).then_some(Cause::OnBattery)
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
