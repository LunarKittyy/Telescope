//! When a freshly updated copy counts as having started fine (main.py's started_fine and its timer).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use telescope_shell::ShownHook;

/// Runs the window with `run`, and calls `confirm` `delay` after the window has been shown, or once it has closed normally. Never when
/// the window couldn't be built or run, so a broken build isn't confirmed and still gets rolled back.
pub fn run_confirming(delay: Duration, confirm: Arc<dyn Fn() + Send + Sync>, run: impl FnOnce(ShownHook) -> Result<(), String>) -> Result<(), String> {
    let failed = Arc::new(AtomicBool::new(false));
    let shown = {
        let (confirm, failed) = (confirm.clone(), failed.clone());
        ShownHook(Arc::new(move || {
            let (confirm, failed) = (confirm.clone(), failed.clone());
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                if !failed.load(Ordering::SeqCst) {
                    confirm();
                }
            });
        }))
    };
    let result = run(shown);
    match &result {
        Ok(()) => confirm(),
        Err(_) => failed.store(true, Ordering::SeqCst),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const DELAY: Duration = Duration::from_millis(40);
    const AFTER: Duration = Duration::from_millis(150);

    fn counter() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        (n, Arc::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }))
    }

    #[test]
    fn a_window_that_could_not_be_built_is_not_confirmed() {
        let (n, confirm) = counter();
        let result = run_confirming(DELAY, confirm, |_shown| Err("no GPU".to_string()));
        std::thread::sleep(AFTER);
        assert_eq!(result, Err("no GPU".to_string()));
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_wait_starts_when_the_window_is_shown_not_before() {
        let (n, confirm) = counter();
        let seen = n.clone();
        let result = run_confirming(DELAY, confirm, |shown| {
            // Building the window takes longer than the wait, and nothing is confirmed meanwhile
            std::thread::sleep(AFTER);
            assert_eq!(seen.load(Ordering::SeqCst), 0);
            (shown.0)();
            std::thread::sleep(AFTER);
            assert_eq!(seen.load(Ordering::SeqCst), 1);
            Ok(())
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn a_window_that_fails_after_showing_is_not_confirmed_late() {
        let (n, confirm) = counter();
        let result = run_confirming(Duration::from_millis(80), confirm, |shown| {
            (shown.0)();
            Err("died".to_string())
        });
        std::thread::sleep(AFTER * 2);
        assert!(result.is_err());
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn closing_normally_confirms_straight_away() {
        let (n, confirm) = counter();
        let result = run_confirming(Duration::from_secs(60), confirm, |shown| {
            (shown.0)();
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
