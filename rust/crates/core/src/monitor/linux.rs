//! The Linux reader: /proc for processor, memory and the user's own processes, /sys for the graphics card and power profile. Nothing here needs root.

use super::{parse_meminfo, parse_pid_stat, parse_proc_stat, RawProcess, Sample};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

pub(super) struct Reader {
    uid: Option<u32>,
    me: u32,
    /// Each process's name, kept while the same process (pid and start) lives so a name is only made once.
    names: HashMap<u32, (u64, Arc<str>)>,
    path: String,
    buf: Vec<u8>,
}

impl Reader {
    pub fn new() -> Reader {
        let uid = std::fs::metadata("/proc/self").ok().map(|m| m.uid());
        Reader { uid, me: std::process::id(), names: HashMap::new(), path: String::new(), buf: Vec::new() }
    }

    pub fn sample(&mut self) -> Sample {
        let mut sample = Sample { cpu: read_text("/proc/stat").as_deref().and_then(parse_proc_stat), ..Sample::default() };
        sample.own_ticks = read_text("/proc/self/stat").as_deref().and_then(parse_pid_stat).map(|s| s.ticks);
        if let Some(mem) = read_text("/proc/meminfo").as_deref().and_then(parse_meminfo) {
            sample.memory_total_mb = mem.total_kb / 1024;
            sample.memory_available_mb = mem.available_kb.map(|kb| kb / 1024);
        }
        sample.on_battery = crate::machine::on_battery();
        sample.power_saver = power_saver();
        sample.gpu = gpu_busy().map(|total| super::GpuLoad { total, per_pid: Vec::new() });
        sample.procs = self.processes();
        sample
    }

    /// The user's other processes with their time so far; someone else's are never opened.
    fn processes(&mut self) -> Vec<RawProcess> {
        let (Some(uid), Ok(dir)) = (self.uid, std::fs::read_dir("/proc")) else { return Vec::new() };
        let mut procs = Vec::with_capacity(self.names.len());
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            if pid == self.me || entry.metadata().ok().map(|m| m.uid()) != Some(uid) {
                continue;
            }
            self.path.clear();
            let _ = write!(self.path, "/proc/{pid}/stat");
            self.buf.clear();
            if std::fs::File::open(&self.path).and_then(|mut f| f.read_to_end(&mut self.buf)).is_err() {
                continue;
            }
            let Some(stat) = std::str::from_utf8(&self.buf).ok().and_then(parse_pid_stat) else { continue };
            let name = match self.names.get(&pid) {
                Some((start, name)) if *start == stat.start => name.clone(),
                _ => {
                    let name: Arc<str> = stat.comm.into();
                    self.names.insert(pid, (stat.start, name.clone()));
                    name
                }
            };
            procs.push(RawProcess { pid, start: stat.start, name, ticks: stat.ticks });
        }
        // Names of processes that ended go, so the table follows what runs
        if self.names.len() > procs.len() + 64 {
            let live: std::collections::HashSet<u32> = procs.iter().map(|p| p.pid).collect();
            self.names.retain(|pid, _| live.contains(pid));
        }
        procs
    }
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The platform profile is "low-power" when the user picked the power saver; None where the file isn't there or can't be read.
fn power_saver() -> Option<bool> {
    let profile = std::fs::read_to_string("/sys/firmware/acpi/platform_profile").ok()?;
    Some(profile.trim() == "low-power")
}

/// The busiest graphics card's load in percent, for drivers that say it (AMD and some others); None otherwise.
fn gpu_busy() -> Option<f32> {
    let mut busiest: Option<f32> = None;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        // card0 is a card; card0-DP-1 is one of its outputs
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let Some(percent) = std::fs::read_to_string(entry.path().join("device/gpu_busy_percent")).ok().and_then(|t| t.trim().parse::<f32>().ok()) else { continue };
        busiest = Some(busiest.map_or(percent, |b| b.max(percent)));
    }
    busiest.map(|b| b.clamp(0.0, 100.0))
}
