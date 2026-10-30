//! What glibc keeps after frames are freed (platform/linux.py limit_malloc_arenas and release_freed_memory). Only glibc has either
//! knob; anywhere else both do nothing.

/// Caps glibc's per-thread memory pools; with the decoder's threads it made dozens that keep freed frames. True when glibc took it.
pub fn limit_malloc_arenas(count: i32) -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallopt only sets a tuning value
        unsafe { libc::mallopt(libc::M_ARENA_MAX, count) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        let _ = count;
        false
    }
}

/// Hands memory glibc kept after frames were freed back to the system.
pub fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only returns free memory
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn glibc_takes_the_arena_cap_and_trimming_is_harmless() {
        let taken = super::limit_malloc_arenas(4);
        assert_eq!(taken, cfg!(all(target_os = "linux", target_env = "gnu")));
        let kept: Vec<Vec<u8>> = (0..64).map(|_| vec![1u8; 1 << 16]).collect();
        drop(kept);
        super::release_freed_memory();
    }
}
