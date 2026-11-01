//! A clock shared by every crate, so frame and packet times compare.

/// Monotonic nanoseconds since boot: the clock Python's time.monotonic() reads, for timing across processes.
pub fn monotonic_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: ts is a valid out-pointer
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn QueryPerformanceCounter(count: *mut i64) -> i32;
            fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
        }
        let (mut count, mut frequency) = (0i64, 0i64);
        // SAFETY: both are valid out-pointers, and neither call can fail since Windows XP
        unsafe {
            QueryPerformanceCounter(&mut count);
            QueryPerformanceFrequency(&mut frequency);
        }
        (count as u128 * 1_000_000_000 / frequency.max(1) as u128) as u64
    }
    #[cfg(not(any(unix, windows)))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
    }
}

/// One line per new frame the virtual camera got, for rust/tools/stress: its number, when its packet arrived, when it was ready and when it went out.
/// TELESCOPE_STRESS_LOG names the file; without it there's nothing here to pay for but a None to check.
pub struct DeliveryLog(std::fs::File);

impl DeliveryLog {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("TELESCOPE_STRESS_LOG").filter(|p| !p.is_empty())?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(Self)
    }

    pub fn delivered(&mut self, number: u64, arrived_ns: u64, ready_ns: u64) {
        use std::io::Write;
        let _ = writeln!(self.0, "{number} {arrived_ns} {ready_ns} {}", monotonic_ns());
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn delivery_log_writes_a_line_per_frame() {
        let path = std::env::temp_dir().join(format!("telescope-delivery-{}", std::process::id()));
        let mut log = DeliveryLog(std::fs::File::create(&path).unwrap());
        let before = monotonic_ns();
        log.delivered(7, 123, 456);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let fields: Vec<u64> = text.split_whitespace().map(|f| f.parse().unwrap()).collect();
        assert_eq!(fields[..3], [7, 123, 456]);
        assert!(fields[3] >= before);
    }
}
