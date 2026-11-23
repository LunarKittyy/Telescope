//! The Windows reader: system times, the user's own processes, memory, power and the graphics card's performance counters. Nothing here needs admin rights; what refuses to be opened is skipped.

use super::{gpu_load, program_name, CpuTimes, GpuLoad, RawProcess, Sample};
use std::collections::HashMap;
use std::sync::Arc;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA};
use windows::Win32::System::Power::{EffectivePowerModeBatterySaver, EffectivePowerModeBetterBattery, GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

/// A handle that is closed when it goes.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open and is closed once
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn filetime(t: FILETIME) -> u64 {
    (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64
}

/// The user's SID as the bytes a token holds it in (8-byte aligned, the SID itself is inside).
struct UserSid(Vec<u64>);

impl UserSid {
    fn of_process(process: HANDLE) -> Option<UserSid> {
        let mut token = HANDLE::default();
        // SAFETY: the process handle is open and the token handle goes to a local
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
        let token = Owned(token);
        let mut size = 0u32;
        // SAFETY: asking for the size with no buffer is how the call is used
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut size) };
        if size == 0 || size > 4096 {
            return None;
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: the buffer holds at least `size` bytes
        unsafe { GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr().cast()), size, &mut size) }.ok()?;
        Some(UserSid(buf))
    }

    fn same_as(&self, other: &UserSid) -> bool {
        // SAFETY: both buffers were filled by GetTokenInformation(TokenUser), so each starts with a TOKEN_USER whose SID points inside it
        unsafe {
            let (a, b) = (&*(self.0.as_ptr().cast::<TOKEN_USER>()), &*(other.0.as_ptr().cast::<TOKEN_USER>()));
            EqualSid(a.User.Sid, b.User.Sid).is_ok()
        }
    }
}

/// The graphics card's engine counters, opened once.
struct Gpu {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    buf: Vec<u64>,
}

// SAFETY: a PDH query and counter are plain handles that any one thread may use at a time, and the reader is only used behind the hub's lock
unsafe impl Send for Gpu {}

impl Gpu {
    fn open() -> Option<Gpu> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: the query handle goes to a local; a null data source is the live system
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: the query is open and the path is a constant wide string
        if unsafe { PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut counter) } != 0 {
            // SAFETY: the query is open and is not used again
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        let gpu = Gpu { query, counter, buf: Vec::new() };
        // A rate needs two collections, so the first one is only taken to have something to compare with
        // SAFETY: the query is open
        unsafe { PdhCollectQueryData(gpu.query) };
        Some(gpu)
    }

    fn read(&mut self) -> Option<GpuLoad> {
        // SAFETY: the query is open
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return None;
        }
        let (mut size, mut count) = (0u32, 0u32);
        // SAFETY: with no buffer the call only says how big one must be
        if unsafe { PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None) } != PDH_MORE_DATA {
            return None;
        }
        self.buf.resize((size as usize).div_ceil(8), 0);
        // SAFETY: the buffer holds `size` bytes and is 8-byte aligned, as the item array needs
        let status = unsafe { PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(self.buf.as_mut_ptr().cast())) };
        if status != 0 || count == 0 {
            return None;
        }
        // SAFETY: on success the buffer starts with `count` items whose names point further into the same buffer
        let items = unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(), count as usize) };
        let named: Vec<(String, f32)> = items
            .iter()
            // 0 and 1 are valid data and new data
            .filter(|item| item.FmtValue.CStatus <= 1)
            .filter_map(|item| {
                // SAFETY: the name is a nul-ended wide string inside the buffer and the value was asked for as a double
                let (name, value) = unsafe { (item.szName.to_string().ok()?, item.FmtValue.Anonymous.doubleValue) };
                Some((name, value as f32))
            })
            .collect();
        gpu_load(named.iter().map(|(n, v)| (n.as_str(), *v)))
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: the query is open and is not used again
        unsafe { PdhCloseQuery(self.query) };
    }
}

pub(super) struct Reader {
    me: u32,
    user: Option<UserSid>,
    /// Each process's name while the same process (pid and start) lives; None for one that isn't the user's or can't be named.
    names: HashMap<u32, (u64, Option<Arc<str>>)>,
    gpu: Option<Gpu>,
    gpu_tried: bool,
    pids: Vec<u32>,
}

impl Reader {
    pub fn new() -> Reader {
        // SAFETY: the pseudo handle of this process is always valid
        let user = UserSid::of_process(unsafe { GetCurrentProcess() });
        Reader { me: std::process::id(), user, names: HashMap::new(), gpu: None, gpu_tried: false, pids: Vec::new() }
    }

    pub fn sample(&mut self) -> Sample {
        // SAFETY: the pseudo handle of this process is always valid
        let own = process_ticks(unsafe { GetCurrentProcess() });
        let mut sample = Sample { cpu: system_times(), own_ticks: own.map(|(_, ticks)| ticks), ..Sample::default() };
        let mut memory = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        // SAFETY: the struct is MEMORYSTATUSEX with its length set
        if unsafe { GlobalMemoryStatusEx(&mut memory) }.is_ok() {
            sample.memory_total_mb = memory.ullTotalPhys / (1024 * 1024);
            sample.memory_available_mb = Some(memory.ullAvailPhys / (1024 * 1024));
        }
        sample.on_battery = crate::machine::on_battery();
        sample.power_saver = power_saver();
        if !self.gpu_tried {
            self.gpu_tried = true;
            self.gpu = Gpu::open();
        }
        sample.gpu = self.gpu.as_mut().and_then(Gpu::read);
        sample.procs = self.processes();
        sample
    }

    /// The user's other processes with their time so far; what can't be opened without admin rights is skipped.
    fn processes(&mut self) -> Vec<RawProcess> {
        let Some(user) = &self.user else { return Vec::new() };
        let mut bytes = 0u32;
        let mut room = self.pids.len().max(1024);
        loop {
            self.pids.resize(room, 0);
            // SAFETY: the buffer holds `room` ids and the byte count is that many u32s
            if unsafe { EnumProcesses(self.pids.as_mut_ptr(), (room * 4) as u32, &mut bytes) }.is_err() {
                return Vec::new();
            }
            // A full buffer may have cut the list short, so it grows and asks again
            if (bytes as usize) < room * 4 || room >= 1 << 18 {
                break;
            }
            room *= 2;
        }
        let count = (bytes as usize / 4).min(self.pids.len());
        let mut procs = Vec::with_capacity(self.names.len());
        for &pid in &self.pids[..count] {
            if pid == 0 || pid == self.me {
                continue;
            }
            // SAFETY: a failed open is skipped
            let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { continue };
            let handle = Owned(handle);
            let Some((start, ticks)) = process_ticks(handle.0) else { continue };
            let name = match self.names.get(&pid) {
                Some((s, name)) if *s == start => name.clone(),
                _ => {
                    let ours = UserSid::of_process(handle.0).is_some_and(|sid| sid.same_as(user));
                    let name = if ours { image_name(handle.0).map(|n| Arc::<str>::from(program_name(&n))) } else { None };
                    self.names.insert(pid, (start, name.clone()));
                    name
                }
            };
            if let Some(name) = name {
                procs.push(RawProcess { pid, start, name, ticks });
            }
        }
        if self.names.len() > count + 64 {
            let live: std::collections::HashSet<u32> = self.pids[..count].iter().copied().collect();
            self.names.retain(|pid, _| live.contains(pid));
        }
        procs
    }
}

/// Idle, kernel (which holds idle) and user time of all processors, in 100 ns.
fn system_times() -> Option<CpuTimes> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: every pointer goes to a local
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    let total = filetime(kernel) + filetime(user);
    Some(CpuTimes { busy: total.saturating_sub(filetime(idle)), total })
}

/// (when the process started, its kernel plus user time so far), both in 100 ns.
fn process_ticks(process: HANDLE) -> Option<(u64, u64)> {
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: every pointer goes to a local
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some((filetime(created), filetime(kernel) + filetime(user)))
}

/// The program's full path.
fn image_name(process: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer holds `len` characters
    unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]))
}

/// The computer is saving power when battery saver is on or the power mode is one of the two that favour the battery.
pub(super) fn power_saver() -> Option<bool> {
    let mode = effective_mode::get();
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: the struct is SYSTEM_POWER_STATUS
    let saver_flag = unsafe { GetSystemPowerStatus(&mut status) }.ok().map(|_| status.SystemStatusFlag == 1);
    match (mode, saver_flag) {
        (None, None) => None,
        (mode, flag) => Some(flag == Some(true) || mode.is_some_and(|m| m == EffectivePowerModeBatterySaver || m == EffectivePowerModeBetterBattery)),
    }
}

/// The effective power mode, which Windows reports once on registering and again on every change: no polling.
mod effective_mode {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::OnceLock;
    use windows::Win32::System::Power::{PowerRegisterForEffectivePowerModeNotifications, EFFECTIVE_POWER_MODE, EFFECTIVE_POWER_MODE_V2};

    const UNKNOWN: i32 = i32::MIN;
    static MODE: AtomicI32 = AtomicI32::new(UNKNOWN);
    static REGISTERED: OnceLock<bool> = OnceLock::new();

    unsafe extern "system" fn changed(mode: EFFECTIVE_POWER_MODE, _context: *const c_void) {
        MODE.store(mode.0, Ordering::Relaxed);
    }

    pub(super) fn get() -> Option<EFFECTIVE_POWER_MODE> {
        REGISTERED.get_or_init(|| {
            let mut handle: *mut c_void = std::ptr::null_mut();
            // SAFETY: the callback is a plain function that only stores into an atomic, and the registration lives as long as the process
            unsafe { PowerRegisterForEffectivePowerModeNotifications(EFFECTIVE_POWER_MODE_V2, Some(changed), None, &mut handle) }.is_ok()
        });
        match MODE.load(Ordering::Relaxed) {
            UNKNOWN => None,
            mode => Some(EFFECTIVE_POWER_MODE(mode)),
        }
    }
}
