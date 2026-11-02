//! Whether an app is reading the virtual camera (vcam.CameraWatch), from a thread of its own.
//!
//! Linux: v4l2loopback (0.13+) sends an event when a reader starts or stops streaming; older drivers fall back to
//! looking for other processes holding the device. Windows 11 camera: its shared memory exists while an app reads it,
//! and the camera counts each frame it hands over. UnityCapture: the filter inside an app sets a named event whenever
//! it wants a frame. on_change(watched) runs on the watch thread, only when the answer changes.

use crate::held::{Job, Stop};
use crate::mf::SharedView;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// UnityCapture's "Want" event: wait() says whether an app asked for a frame within the time.
pub trait WantEvent: Send {
    fn wait(&self, timeout: Duration) -> bool;
}

/// What the watch reads, so tests can stand in for the system.
#[derive(Clone)]
pub struct WatchDeps {
    /// Linux: the main camera's node.
    pub device: String,
    pub retry_open: Duration,
    pub proc_scan: Duration,
    /// Other processes holding the device (the fallback).
    pub holders: Arc<dyn Fn() -> Vec<u32> + Send + Sync>,
    /// Windows: whether the main camera is the Windows 11 one now.
    pub use_mf: Arc<dyn Fn() -> bool + Send + Sync>,
    pub mf_open: Arc<dyn Fn() -> Option<SharedView> + Send + Sync>,
    pub mf_period: Duration,
    pub linger: Duration,
    /// Windows: UnityCapture's Want event, once an app's filter (or a sender) has made it.
    pub uc_want: Arc<dyn Fn() -> Option<Box<dyn WantEvent>> + Send + Sync>,
}

pub struct Watcher {
    on_change: Box<dyn Fn(bool) + Send + Sync>,
    watched: Mutex<Option<bool>>,
    deps: WatchDeps,
}

impl Watcher {
    pub fn new(on_change: Box<dyn Fn(bool) + Send + Sync>, deps: WatchDeps) -> Watcher {
        Watcher { on_change, watched: Mutex::new(None), deps }
    }

    pub fn watched(&self) -> bool {
        self.watched.lock().unwrap().unwrap_or(false)
    }

    pub fn report(&self, watched: bool) {
        let mut last = self.watched.lock().unwrap();
        if *last == Some(watched) {
            return;
        }
        *last = Some(watched);
        drop(last);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.on_change)(watched)));
        if caught.is_err() {
            log::error!("Camera watch callback failed");
        }
    }

    pub fn scan_proc(&self, stop: &Stop) {
        while !stop.is_set() {
            self.report(!(self.deps.holders)().is_empty());
            stop.wait(self.deps.proc_scan);
        }
    }

    #[cfg(target_os = "linux")]
    fn run_linux(&self, stop: &Stop) {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        const V4L2_EVENT_CLIENT_USAGE: u32 = 0x0800_0000 + 0x08E0_0000 + 1;
        const VIDIOC_SUBSCRIBE_EVENT: libc::c_ulong = (1 << 30) | (32 << 16) | ((b'V' as libc::c_ulong) << 8) | 90;
        const VIDIOC_DQEVENT: libc::c_ulong = (2 << 30) | (136 << 16) | ((b'V' as libc::c_ulong) << 8) | 89;
        const SEND_INITIAL: u32 = 1;
        while !stop.is_set() {
            let file = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(&self.deps.device);
            let Ok(file) = file else {
                stop.wait(self.deps.retry_open);
                continue;
            };
            let fd = file.as_raw_fd();
            let mut sub = [V4L2_EVENT_CLIENT_USAGE, 0, SEND_INITIAL, 0, 0, 0, 0, 0];
            // SAFETY: the ioctl reads a 32-byte v4l2_event_subscription from sub
            if unsafe { libc::ioctl(fd, VIDIOC_SUBSCRIBE_EVENT, sub.as_mut_ptr()) } < 0 {
                drop(file);
                log::info!("v4l2loopback has no usage events (older than 0.13); scanning /proc instead");
                self.scan_proc(stop);
                return;
            }
            while !stop.is_set() {
                let mut pfd = libc::pollfd { fd, events: libc::POLLPRI, revents: 0 };
                // SAFETY: one pollfd for an open descriptor
                let n = unsafe { libc::poll(&mut pfd, 1, 500) };
                if n > 0 && pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    break;
                }
                if n > 0 {
                    let mut buf = [0u8; 136];
                    // SAFETY: the ioctl writes a 136-byte v4l2_event into buf
                    if unsafe { libc::ioctl(fd, VIDIOC_DQEVENT, buf.as_mut_ptr()) } < 0 {
                        continue;
                    }
                    if u32::from_ne_bytes(buf[0..4].try_into().unwrap()) == V4L2_EVENT_CLIENT_USAGE {
                        self.report(u32::from_ne_bytes(buf[8..12].try_into().unwrap()) > 0);
                    }
                }
            }
            drop(file);
            stop.wait(self.deps.retry_open);
        }
    }

    /// Windows: which camera it is can change while Telescope runs (installed from the first-run checklist).
    pub fn run_windows(&self, stop: &Stop) {
        while !stop.is_set() {
            if (self.deps.use_mf)() {
                self.watch_mf(stop);
            } else {
                self.watch_uc(stop);
            }
        }
    }

    pub fn watch_mf(&self, stop: &Stop) {
        let (mut beat, mut beat_at) = (None, Instant::now());
        while !stop.is_set() {
            match crate::mf::peek(&*self.deps.mf_open) {
                None => {
                    self.report(false);
                    beat = None;
                }
                Some((reading, new_beat)) => {
                    let now = Instant::now();
                    if Some(new_beat) != beat {
                        beat = Some(new_beat);
                        beat_at = now;
                    }
                    // An app that stopped asking for frames without closing (one hung or paused) counts as gone
                    self.report(reading && now - beat_at <= self.deps.linger);
                }
            }
            if stop.wait(self.deps.mf_period) || !(self.deps.use_mf)() {
                return;
            }
        }
    }

    pub fn watch_uc(&self, stop: &Stop) {
        let mut event: Option<Box<dyn WantEvent>> = None;
        let mut last_seen: Option<Instant> = None;
        while !stop.is_set() {
            if event.is_none() {
                // An app's filter makes it when it opens the camera, and a sender holding it keeps it alive. Until
                // then no app has opened the camera, which is an answer too: without it an idle stop would never
                // start for a stream started before any app came along.
                event = (self.deps.uc_want)();
                if event.is_none() {
                    self.report(false);
                    if stop.wait(self.deps.retry_open) || (self.deps.use_mf)() {
                        return;
                    }
                    continue;
                }
            }
            if event.as_ref().unwrap().wait(Duration::from_secs(1)) {
                last_seen = Some(Instant::now());
                self.report(true);
                stop.wait(Duration::from_millis(500)); // one sighting is enough; don't wake for every frame
            } else if last_seen.is_none_or(|t| t.elapsed() > self.deps.linger) {
                self.report(false);
            }
            if (self.deps.use_mf)() {
                return; // the Windows 11 camera went in while an app had this one open
            }
        }
    }
}

impl Job for Watcher {
    fn run(&self, stop: &Stop) {
        #[cfg(target_os = "linux")]
        self.run_linux(stop);
        #[cfg(not(target_os = "linux"))]
        self.run_windows(stop);
    }

    fn suspended(&self) {
        self.report(false);
    }
}

/// The watch as core's Held.
pub struct CameraWatch(pub crate::held::Held<Watcher>);

impl telescope_core::output::Held for CameraWatch {
    fn start(&self) {
        self.0.start();
    }
    fn stop(&self) {
        self.0.stop();
    }
    fn running(&self) -> bool {
        self.0.running()
    }
}

#[cfg(windows)]
pub struct WinWant(pub crate::win::Owned);

#[cfg(windows)]
impl WantEvent for WinWant {
    fn wait(&self, timeout: Duration) -> bool {
        crate::win::wait(&self.0, timeout.as_millis() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::held::Gate;
    use crate::mf::tests::{reading, Opener};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn deps() -> WatchDeps {
        WatchDeps {
            device: "/nonexistent/video99".into(), retry_open: Duration::from_millis(10), proc_scan: Duration::from_millis(10),
            holders: Arc::new(Vec::new), use_mf: Arc::new(|| true), mf_open: Arc::new(|| None),
            mf_period: Duration::from_millis(10), linger: Duration::from_secs(3), uc_want: Arc::new(|| None),
        }
    }

    fn watcher(deps: WatchDeps) -> (Arc<Watcher>, Arc<Mutex<Vec<bool>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (Arc::new(Watcher::new(Box::new(move |w| s.lock().unwrap().push(w)), deps)), seen)
    }

    fn wait_for(what: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(2);
        while !what() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn in_thread(w: &Arc<Watcher>, run: fn(&Watcher, &Stop)) -> (Arc<Stop>, std::thread::JoinHandle<()>) {
        let stop = Arc::new(Stop::default());
        let (w, s) = (w.clone(), stop.clone());
        (stop, std::thread::spawn(move || run(&w, &s)))
    }

    // test_vcam.py::test_watch_reports_only_changes
    #[test]
    fn watch_reports_only_changes() {
        let (w, seen) = watcher(deps());
        for v in [true, true, false, false, true] {
            w.report(v);
        }
        assert_eq!(*seen.lock().unwrap(), [true, false, true]);
        assert!(w.watched());
    }

    // test_vcam.py::test_old_drivers_fall_back_to_looking_for_other_processes
    #[test]
    fn old_drivers_fall_back_to_looking_for_other_processes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let holders = Arc::new(move || if c.fetch_add(1, Ordering::SeqCst) == 1 { vec![4242] } else { vec![] });
        let (w, seen) = watcher(WatchDeps { holders, ..deps() });
        let (stop, t) = in_thread(&w, Watcher::scan_proc);
        wait_for(|| seen.lock().unwrap().len() >= 3);
        stop.set();
        t.join().unwrap();
        assert_eq!(seen.lock().unwrap()[..3], [false, true, false]);
    }

    // test_vcam.py::test_linux_watch_retries_while_the_device_is_missing
    #[test]
    fn the_watch_retries_while_the_device_is_missing_and_stops_when_asked() {
        let (w, _seen) = watcher(WatchDeps { use_mf: Arc::new(|| false), ..deps() });
        let gate = Arc::new(Gate::default());
        let held = crate::held::Held::new(Arc::try_unwrap(w).ok().unwrap(), &gate);
        held.start();
        std::thread::sleep(Duration::from_millis(50));
        held.stop();
        assert!(!held.running());
    }

    // test_vcam.py::test_windows_watch_counts_a_camera_no_app_has_opened_as_unwatched
    #[test]
    fn a_camera_no_app_has_opened_counts_as_unwatched() {
        let (w, seen) = watcher(WatchDeps { use_mf: Arc::new(|| false), ..deps() });
        let (stop, t) = in_thread(&w, Watcher::run_windows);
        std::thread::sleep(Duration::from_millis(50));
        stop.set();
        t.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), [false]);
    }

    struct NeverWants(Arc<AtomicUsize>);

    impl WantEvent for NeverWants {
        fn wait(&self, _timeout: Duration) -> bool {
            false
        }
    }

    impl Drop for NeverWants {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    // test_vcam.py::test_windows_watch_moves_to_the_windows_11_camera_while_an_app_holds_unitycapture
    #[test]
    fn the_watch_moves_to_the_windows_11_camera_while_an_app_holds_unitycapture() {
        let closed = Arc::new(AtomicUsize::new(0));
        let c = closed.clone();
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let d = WatchDeps {
            uc_want: Arc::new(move || Some(Box::new(NeverWants(c.clone())) as Box<dyn WantEvent>)),
            use_mf: Arc::new(move || a.fetch_add(1, Ordering::SeqCst) >= 2),
            ..deps()
        };
        let (w, _seen) = watcher(d);
        w.watch_uc(&Stop::default());
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    // test_vcam.py::test_the_watch_follows_the_windows_11_cameras_reader
    #[test]
    fn the_watch_follows_the_windows_11_cameras_reader() {
        let o = Opener::new(None);
        let open = o.opener();
        let open = Arc::new(Mutex::new(open));
        let d = WatchDeps { mf_open: Arc::new(move || (open.lock().unwrap())()), ..deps() };
        let (w, seen) = watcher(d);
        let (stop, t) = in_thread(&w, Watcher::watch_mf);
        wait_for(|| *seen.lock().unwrap() == [false]);
        *o.buf.lock().unwrap() = Some(reading(8, 4));
        wait_for(|| *seen.lock().unwrap() == [false, true]);
        o.set(16, 0);
        wait_for(|| *seen.lock().unwrap() == [false, true, false]);
        stop.set();
        t.join().unwrap();
        assert_eq!(o.closed.load(Ordering::SeqCst), o.opened.load(Ordering::SeqCst), "the watch never keeps it open itself");
    }

    // test_vcam.py::test_the_watch_counts_a_stalled_reader_as_gone
    #[test]
    fn the_watch_counts_a_stalled_reader_as_gone() {
        let o = Opener::new(Some(reading(8, 4)));
        let open = Arc::new(Mutex::new(o.opener()));
        let d = WatchDeps { mf_open: Arc::new(move || (open.lock().unwrap())()), linger: Duration::from_millis(50), ..deps() };
        let (w, seen) = watcher(d);
        let (stop, t) = in_thread(&w, Watcher::watch_mf);
        wait_for(|| *seen.lock().unwrap() == [true, false]);
        stop.set();
        t.join().unwrap();
    }
}
