//! When the main camera's stream can stop running the frame steps and vision: no app has read the virtual camera for a while and nothing
//! else needs the frames. The shell works out both and keeps the answer here, for the stream's worker to look at once per frame.
//!
//! Only the main camera (slot 0) has anyone watching it, so any other slot, and a computer where nothing says whether an app reads the
//! camera (the Wait screen plugin is off, or the driver can't tell), never pauses. Decoding and the phone's link are not touched.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// How long nobody has read the camera, and nothing has needed frames, before the steps are paused (nanoseconds).
pub const IDLE_AFTER_NS: u64 = 8_000_000_000;

#[derive(Default)]
struct Inputs {
    /// Whether an app reads the camera; None until the watch has said, or when nothing can.
    watched: Option<bool>,
    /// Whether "nobody reads" can be trusted: the watch hears about readers (v4l2loopback usage events, the Windows 11 camera's heartbeat),
    /// where the /proc scan and UnityCapture can miss one and leave it a frozen picture. Never trusted until said.
    reliable: bool,
    /// Something needs the frames: a window someone can see, a plugin that asked (Host::want_frames), a motion or stats watch.
    wanted: bool,
}

pub struct IdleGate {
    inputs: Mutex<Inputs>,
    /// Since when it has been quiet (nobody reads, nothing wanted), or 0 while it isn't. Read by the worker without a lock.
    quiet_since: AtomicU64,
    /// How long the quiet has to last.
    after_ns: u64,
}

impl Default for IdleGate {
    fn default() -> IdleGate {
        IdleGate::after(IDLE_AFTER_NS)
    }
}

impl IdleGate {
    /// A gate that pauses once it has been quiet for after_ns nanoseconds.
    pub fn after(after_ns: u64) -> IdleGate {
        IdleGate { inputs: Mutex::default(), quiet_since: AtomicU64::new(0), after_ns }
    }

    /// How long the quiet has to last (nanoseconds), which a stream that has only just started waits out too.
    pub fn quiet_needed_ns(&self) -> u64 {
        self.after_ns
    }

    /// The watch says an app started or stopped reading the camera.
    pub fn watched(&self, watched: bool) {
        self.watched_at(watched, telescope_core::time::monotonic_ns());
    }

    /// Whether something needs the frames now.
    pub fn wanted(&self, wanted: bool) {
        self.wanted_at(wanted, telescope_core::time::monotonic_ns());
    }

    /// Whether the steps are paused now: quiet for IDLE_AFTER_NS. A reader or a want ends it at once.
    pub fn idle(&self) -> bool {
        self.idle_at(telescope_core::time::monotonic_ns())
    }

    /// Whether the watch's answer can be trusted to mean nobody reads; without that the steps never pause.
    pub fn reliable(&self, reliable: bool) {
        self.reliable_at(reliable, telescope_core::time::monotonic_ns());
    }

    pub fn reliable_at(&self, reliable: bool, now: u64) {
        let mut inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        inputs.reliable = reliable;
        self.settle(&inputs, now);
    }

    pub fn watched_at(&self, watched: bool, now: u64) {
        let mut inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        inputs.watched = Some(watched);
        self.settle(&inputs, now);
    }

    pub fn wanted_at(&self, wanted: bool, now: u64) {
        let mut inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        inputs.wanted = wanted;
        self.settle(&inputs, now);
    }

    pub fn idle_at(&self, now: u64) -> bool {
        let since = self.quiet_since.load(Ordering::SeqCst);
        since != 0 && now.saturating_sub(since) >= self.after_ns
    }

    /// Quiet starts when both conditions come true, and keeps its start while they stay true.
    fn settle(&self, inputs: &Inputs, now: u64) {
        let quiet = inputs.reliable && inputs.watched == Some(false) && !inputs.wanted;
        if !quiet {
            self.quiet_since.store(0, Ordering::SeqCst);
        } else if self.quiet_since.load(Ordering::SeqCst) == 0 {
            // 0 means not quiet, so a clock that reads 0 is moved on a nanosecond
            self.quiet_since.store(now.max(1), Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn nothing_pauses_before_the_watch_has_said_anything() {
        let gate = IdleGate::default();
        gate.reliable_at(true, 0);
        gate.wanted_at(false, S);
        assert!(!gate.idle_at(100 * S), "no signal, no pause");
    }

    #[test]
    fn it_pauses_after_the_quiet_has_lasted_long_enough() {
        let gate = IdleGate::default();
        gate.reliable_at(true, 0);
        gate.watched_at(false, 10 * S);
        assert!(!gate.idle_at(17 * S));
        assert!(gate.idle_at(18 * S));
    }

    #[test]
    fn a_reader_ends_the_pause_at_once() {
        let gate = IdleGate::default();
        gate.reliable_at(true, 0);
        gate.watched_at(false, 0);
        assert!(gate.idle_at(20 * S));
        gate.watched_at(true, 20 * S);
        assert!(!gate.idle_at(20 * S));
        gate.watched_at(false, 21 * S);
        assert!(!gate.idle_at(25 * S), "the count starts again once they leave");
        assert!(gate.idle_at(29 * S));
    }

    #[test]
    fn something_that_wants_frames_ends_the_pause_and_the_count_starts_again_after_it() {
        let gate = IdleGate::default();
        gate.reliable_at(true, 0);
        gate.watched_at(false, 0);
        gate.wanted_at(true, 30 * S);
        assert!(!gate.idle_at(30 * S));
        gate.wanted_at(false, 40 * S);
        assert!(!gate.idle_at(47 * S));
        assert!(gate.idle_at(48 * S));
    }

    #[test]
    fn saying_the_same_thing_again_does_not_restart_the_count() {
        let gate = IdleGate::default();
        gate.reliable_at(true, 0);
        gate.watched_at(false, 5 * S);
        gate.watched_at(false, 12 * S);
        gate.wanted_at(false, 13 * S);
        assert!(gate.idle_at(13 * S));
    }

    #[test]
    fn a_watch_that_can_miss_readers_never_pauses() {
        let gate = IdleGate::default();
        gate.watched_at(false, 0);
        assert!(!gate.idle_at(100 * S), "reliability not said yet");
        gate.reliable_at(false, 1);
        assert!(!gate.idle_at(100 * S), "the /proc scan or UnityCapture");
        gate.reliable_at(true, 100 * S);
        assert!(!gate.idle_at(107 * S), "the count starts once the signal can be trusted");
        assert!(gate.idle_at(108 * S));
        gate.reliable_at(false, 108 * S);
        assert!(!gate.idle_at(200 * S), "losing the reliable signal ends the pause");
    }
}
