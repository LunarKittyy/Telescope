//! A clock shared by every crate, so frame and packet times compare.

/// Monotonic nanoseconds: CLOCK_MONOTONIC on Linux, the clock Python's time.monotonic() reads, for timing across processes.
pub fn monotonic_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: ts is a valid out-pointer
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(not(unix))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
    }
}
