// The Windows splash as a real window. TELESCOPE_SPLASH_HOLD_MS keeps it up that long, to look at it.
#![cfg(windows)]

use std::time::{Duration, Instant};
use telescope_system::splash::SplashWindow;
use telescope_system::splash_windows::{since_launch, Splash};

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    fn IsWindowVisible(window: isize) -> i32;
}

fn splash_window() -> isize {
    let class: Vec<u16> = "TelescopeSplash".encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated class name and no title, which the call only reads
    unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) }
}

#[test]
fn the_splash_shows_its_text_and_goes_away() {
    let splash = Splash::open().expect("a window");
    let window = splash_window();
    assert_ne!(window, 0);
    // SAFETY: any handle is fine, a stale one just answers no
    assert_ne!(unsafe { IsWindowVisible(window) }, 0);
    splash.update_text("Starting Telescope… Räksmörgås 望远镜");
    let hold = std::env::var("TELESCOPE_SPLASH_HOLD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    std::thread::sleep(Duration::from_millis(hold));
    assert!(since_launch() > Duration::ZERO && since_launch() < Duration::from_secs(600));
    splash.close();
    let end = Instant::now() + Duration::from_secs(5);
    while splash_window() != 0 {
        assert!(Instant::now() < end, "the splash stayed up");
        std::thread::sleep(Duration::from_millis(10));
    }
}
