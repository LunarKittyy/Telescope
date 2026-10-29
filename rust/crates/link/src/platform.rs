//! Which platform the Connection texts and dialogs are for: the one built for, unless something (the scenes, a test)
//! says otherwise, like Python's patched IS_WINDOWS.

use std::sync::atomic::{AtomicU8, Ordering};

// 0: the build's own platform, 1: not Windows, 2: Windows
static WINDOWS: AtomicU8 = AtomicU8::new(0);

/// Whether to behave as on Windows (where Telescope can get adb itself).
pub fn windows() -> bool {
    match WINDOWS.load(Ordering::Relaxed) {
        0 => cfg!(windows),
        n => n == 2,
    }
}

/// Behave as on Windows or not from now on; None goes back to the build's own platform.
pub fn set_windows(windows: Option<bool>) {
    WINDOWS.store(match windows { None => 0, Some(false) => 1, Some(true) => 2 }, Ordering::Relaxed);
}

// Tests that set or read the platform take this, so they don't see each other's setting
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_build_and_can_be_set() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_windows(Some(!cfg!(windows)));
        assert_eq!(windows(), !cfg!(windows));
        set_windows(None);
        assert_eq!(windows(), cfg!(windows));
    }
}
