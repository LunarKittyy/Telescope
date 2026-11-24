//! The Linux reader: /proc for processor, memory and the user's own processes, /sys for the graphics card and power profile. Nothing here needs root.

use super::drm::{client_shares, gpu_load_from_clients, parse_drm_fdinfo, ClientKey, DrmClient};
use super::{parse_meminfo, parse_pid_stat, parse_proc_stat, RawProcess, Sample};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::Instant;

/// A process with no graphics fd is looked at again after this many looks, so a program that starts using the card is seen within a few seconds.
const IDLE_LOOKS: u8 = 4;

pub(super) struct Reader {
    uid: Option<u32>,
    me: u32,
    /// Each process's name, kept while the same process (pid and start) lives so a name is only made once.
    names: HashMap<u32, (u64, Arc<str>)>,
    path: String,
    buf: Vec<u8>,
    /// The user's pids (this process included) as last listed, for the graphics scan.
    pids: Vec<u32>,
    /// The graphics clients and the time of the last look, for the next one's rates.
    gpu_last: Option<(Instant, HashMap<ClientKey, DrmClient>)>,
    /// Processes found with no graphics fd, and how many looks to leave them alone, since listing every fd of every process is the costly part.
    gpu_skip: HashMap<u32, u8>,
}

impl Reader {
    pub fn new() -> Reader {
        let uid = std::fs::metadata("/proc/self").ok().map(|m| m.uid());
        Reader { uid, me: std::process::id(), names: HashMap::new(), path: String::new(), buf: Vec::new(), pids: Vec::new(), gpu_last: None, gpu_skip: HashMap::new() }
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
        sample.procs = self.processes();
        sample.gpu = self.gpu(gpu_busy());
        sample
    }

    /// The user's other processes with their time so far; someone else's are never opened.
    pub(super) fn processes(&mut self) -> Vec<RawProcess> {
        let (Some(uid), Ok(dir)) = (self.uid, std::fs::read_dir("/proc")) else { return Vec::new() };
        let mut procs = Vec::with_capacity(self.names.len());
        self.pids.clear();
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            if entry.metadata().ok().map(|m| m.uid()) != Some(uid) {
                continue;
            }
            self.pids.push(pid);
            if pid == self.me {
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

    /// The graphics card's load: the driver's own total if it gives one, and each process's share from DRM fdinfo. A client's counters need a second look to give a rate.
    pub(super) fn gpu(&mut self, system_total: Option<f32>) -> Option<super::GpuLoad> {
        let now = Instant::now();
        let clients = self.drm_clients();
        let shares = match &self.gpu_last {
            Some((at, before)) => client_shares(before, &clients, now.duration_since(*at).as_nanos() as u64),
            None => Vec::new(),
        };
        self.gpu_last = Some((now, clients.into_iter().map(|(key, (_, client))| (key, client)).collect()));
        gpu_load_from_clients(&shares, system_total)
    }

    /// The graphics clients of the user's processes, each once under the first process (lowest pid) that has it open. Only fds pointing into /dev/dri are read.
    fn drm_clients(&mut self) -> HashMap<ClientKey, (u32, DrmClient)> {
        let mut clients = HashMap::new();
        self.pids.sort_unstable();
        for i in 0..self.pids.len() {
            let pid = self.pids[i];
            if let Some(skip) = self.gpu_skip.get_mut(&pid).filter(|s| **s > 0) {
                *skip -= 1;
                continue;
            }
            let mut found = false;
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            for fd in fds.flatten() {
                let Some(fd) = fd.file_name().to_str().map(str::to_owned) else { continue };
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/fd/{fd}");
                let is_drm = std::fs::read_link(&self.path).is_ok_and(|target| target.as_os_str().as_bytes().starts_with(b"/dev/dri/"));
                if !is_drm {
                    continue;
                }
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/fdinfo/{fd}");
                let Some(client) = std::fs::read_to_string(&self.path).ok().as_deref().and_then(parse_drm_fdinfo) else { continue };
                found = true;
                // A client without an id (an old kernel) can't be told from a duplicate, so it is left out
                let Some(key) = client.key() else { continue };
                clients.entry(key).or_insert((pid, client));
            }
            if found {
                self.gpu_skip.remove(&pid);
            } else {
                self.gpu_skip.insert(pid, IDLE_LOOKS);
            }
        }
        let pids = &self.pids;
        self.gpu_skip.retain(|pid, _| pids.binary_search(pid).is_ok());
        clients
    }
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The platform profile is "low-power" when the user picked the power saver, or power-profiles-daemon says it is on its power-saver profile;
/// None where neither can be read.
pub(super) fn power_saver() -> Option<bool> {
    let platform = std::fs::read_to_string("/sys/firmware/acpi/platform_profile").ok().map(|p| p.trim() == "low-power");
    let daemon = profiles_daemon::active().map(|p| p == "power-saver");
    match (platform, daemon) {
        (None, None) => None,
        (a, b) => Some(a == Some(true) || b == Some(true)),
    }
}

/// power-profiles-daemon's active profile over the system bus. It is asked only when the daemon is already running, so asking never starts it.
mod profiles_daemon {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use zbus::blocking::{connection::Builder, Connection};
    use zbus::zvariant::OwnedValue;

    const NAME: &str = "net.hadess.PowerProfiles";
    const PATH: &str = "/net/hadess/PowerProfiles";
    /// A bus or daemon that doesn't answer in this long is left alone for a while, so a stuck one can't hold up whatever asks.
    const ANSWER_WITHIN: Duration = Duration::from_millis(300);
    const LEAVE_ALONE: Duration = Duration::from_secs(60);

    struct Bus {
        conn: Option<Connection>,
        leave_alone_until: Option<Instant>,
    }

    static BUS: Mutex<Bus> = Mutex::new(Bus { conn: None, leave_alone_until: None });

    pub(super) fn active() -> Option<String> {
        let mut bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
        if bus.leave_alone_until.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        if bus.conn.is_none() {
            bus.conn = Builder::system().and_then(|b| b.method_timeout(ANSWER_WITHIN).build()).ok();
        }
        let answer = bus.conn.as_ref().map(ask);
        match answer {
            Some(Ok(profile)) => profile,
            // No system bus, or one that failed: try again later with a new connection
            Some(Err(_)) | None => {
                bus.conn = None;
                bus.leave_alone_until = Some(Instant::now() + LEAVE_ALONE);
                None
            }
        }
    }

    /// Ok(None) is "no daemon running", which is nothing to retry soon.
    fn ask(conn: &Connection) -> zbus::Result<Option<String>> {
        let running: bool = conn.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "NameHasOwner", &(NAME,))?.body().deserialize()?;
        if !running {
            return Ok(None);
        }
        let reply = conn.call_method(Some(NAME), PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(NAME, "ActiveProfile"))?;
        let value: OwnedValue = reply.body().deserialize()?;
        Ok(String::try_from(value).ok())
    }
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
