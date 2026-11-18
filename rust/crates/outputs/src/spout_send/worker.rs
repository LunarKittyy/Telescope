//! The thread that shares one stream's pictures: it takes the newest picture the frame step left (older ones are dropped, the stream never waits for it), packs it and hands it
//! to the backend's sender, which it opens when the first picture comes and again when the size changes, and closes when the pictures stop.

use super::pack;
use super::{Backend, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use telescope_core::frame::Frame;

/// Without a new picture for this long the sender is taken down, so a stream that stopped doesn't leave one behind.
pub const IDLE: Duration = Duration::from_secs(3);
/// After a sender couldn't be opened or written, the time before the next try.
pub const RETRY: Duration = Duration::from_secs(2);

/// What one stream's sender is doing, for the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Sending pictures of this size.
    Sending(u32, u32),
    /// Couldn't open or write the sender.
    Failed(String),
    /// Nothing to send right now.
    Idle,
}

struct Slot {
    latest: Mutex<Option<Frame>>,
    ready: Condvar,
    stop: AtomicBool,
    alive: AtomicBool,
}

/// One running thread's handle: put pictures in, and it ends by itself when they stop.
pub struct Worker {
    slot: Arc<Slot>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(name: String, backend: Arc<dyn Backend>, tell: Arc<dyn Fn(State) + Send + Sync>, idle: Duration, retry: Duration) -> Option<Worker> {
        let slot = Arc::new(Slot { latest: Mutex::new(None), ready: Condvar::new(), stop: AtomicBool::new(false), alive: AtomicBool::new(true) });
        let held = slot.clone();
        let thread = std::thread::Builder::new().name("spout-send".into()).spawn(move || run(&name, &*backend, &*tell, &held, idle, retry)).ok()?;
        Some(Worker { slot, thread: Some(thread) })
    }

    /// Whether the thread still takes pictures (it ends after `idle` without one, or when stopped).
    pub fn alive(&self) -> bool {
        self.slot.alive.load(Ordering::SeqCst)
    }

    /// Hands over a picture, in place of the one that hadn't been taken yet. False when the thread has ended.
    pub fn put(&self, frame: Frame) -> bool {
        let mut latest = self.slot.latest.lock().unwrap_or_else(|e| e.into_inner());
        if !self.alive() {
            return false;
        }
        *latest = Some(frame);
        self.slot.ready.notify_all();
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.slot.stop.store(true, Ordering::SeqCst);
        self.slot.ready.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The next picture, or None after `idle` without one or when stopped.
fn next(slot: &Slot, idle: Duration) -> Option<Frame> {
    let latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    let (mut latest, _) = slot.ready.wait_timeout_while(latest, idle, |l| l.is_none() && !slot.stop.load(Ordering::SeqCst)).unwrap_or_else(|e| e.into_inner());
    if slot.stop.load(Ordering::SeqCst) {
        return None;
    }
    latest.take()
}

/// Waits `time`, ending early when stopped.
fn pause(slot: &Slot, time: Duration) {
    let latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    let _ = slot.ready.wait_timeout_while(latest, time, |_| !slot.stop.load(Ordering::SeqCst));
}

fn run(name: &str, backend: &dyn Backend, tell: &(dyn Fn(State) + Send + Sync), slot: &Slot, idle: Duration, retry: Duration) {
    let mut open: Option<(u32, u32, Box<dyn Sender>)> = None;
    let mut bytes = Vec::new();
    while let Some(frame) = next(slot, idle) {
        let size = (frame.width, frame.height);
        if size.0 == 0 || size.1 == 0 {
            continue;
        }
        if open.as_ref().is_some_and(|(w, h, _)| (*w, *h) != size) {
            open = None;
        }
        if open.is_none() {
            match backend.open(name, size.0, size.1) {
                Ok(sender) => open = Some((size.0, size.1, sender)),
                Err(why) => {
                    log::warn!("couldn't start the sender {name}: {why}");
                    tell(State::Failed(why));
                    pause(slot, retry);
                    continue;
                }
            }
        }
        if !pack::bgra(&frame, None, &mut bytes) {
            tell(State::Failed("The picture is in a format that can't be shared".into()));
            continue;
        }
        if let Some((_, _, sender)) = open.as_mut() {
            match sender.send(&bytes) {
                Ok(()) => tell(State::Sending(size.0, size.1)),
                Err(why) => {
                    log::warn!("the sender {name} stopped: {why}");
                    open = None;
                    tell(State::Failed(why));
                    pause(slot, retry);
                }
            }
        }
    }
    drop(open);
    slot.alive.store(false, Ordering::SeqCst);
    tell(State::Idle);
}

#[cfg(test)]
pub(crate) mod tests;
