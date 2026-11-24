//! The thread that shares one stream's pictures: it takes the newest picture the frame step left (older ones are dropped, the stream never waits for it), packs it and hands it
//! to the backend's sender, which it opens when the first picture comes and again when the size changes, and closes when the pictures stop.

use super::pack;
use super::{Backend, Resized, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::Frame;

/// Without a new picture for this long the sender is taken down, so a stream that stopped doesn't leave one behind.
pub const IDLE: Duration = Duration::from_secs(3);
/// After a sender couldn't be opened or written, the time before the next try.
pub const RETRY: Duration = Duration::from_secs(2);
/// After a sender that is busy with a reader couldn't change size, the time before the next try.
const RESIZE_RETRY: Duration = Duration::from_millis(100);

/// Why a sender isn't working, in words the card has texts for (the backend's own error goes to the log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The sender couldn't be made.
    Open,
    /// The sender stopped taking pictures.
    Send,
    /// The picture can't be shared in the format it is in.
    Format,
}

/// What one stream's sender is doing, for the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Sending pictures of this size.
    Sending(u32, u32),
    /// Couldn't open or write the sender.
    Failed(Problem),
    /// Nothing to send right now.
    Idle,
}

struct Slot {
    /// The picture, and once it is being retried, when it was first tried (a fresh one has no time, however long the thread took to get to it).
    latest: Mutex<Option<(Frame, Option<Instant>)>>,
    ready: Condvar,
    stop: AtomicBool,
    alive: AtomicBool,
}

/// How long dropping a worker waits for its thread before leaving it to end by itself (a backend call that never returns must not hold the caller up).
const JOIN_WAIT: Duration = Duration::from_millis(500);

/// Marks the thread as ended however it ends, a panic included.
struct Ending(Arc<Slot>);

impl Drop for Ending {
    fn drop(&mut self) {
        let _latest = self.0.latest.lock().unwrap_or_else(|e| e.into_inner());
        self.0.alive.store(false, Ordering::SeqCst);
    }
}

/// One running thread's handle: put pictures in, and it ends by itself when they stop.
pub struct Worker {
    slot: Arc<Slot>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// `keep`: the sender stays up through a pause in the pictures (the card says idle), instead of going after `idle`.
    pub fn spawn(name: String, backend: Arc<dyn Backend>, tell: Arc<dyn Fn(State) + Send + Sync>, idle: Duration, retry: Duration, keep: bool) -> Option<Worker> {
        let slot = Arc::new(Slot { latest: Mutex::new(None), ready: Condvar::new(), stop: AtomicBool::new(false), alive: AtomicBool::new(true) });
        let held = slot.clone();
        let thread = std::thread::Builder::new().name("spout-send".into()).spawn(move || run(&name, &*backend, &*tell, &held, idle, retry, keep)).ok()?;
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
        *latest = Some((frame, None));
        self.slot.ready.notify_all();
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.slot.stop.store(true, Ordering::SeqCst);
        self.slot.ready.notify_all();
        let Some(thread) = self.thread.take() else { return };
        let until = std::time::Instant::now() + JOIN_WAIT;
        while !thread.is_finished() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            log::warn!("a sender thread didn't end in time and is left to end by itself");
        }
    }
}

enum Next {
    Picture(Box<Frame>, Option<Instant>),
    /// No picture for the idle time.
    Quiet,
    Stopped,
}

/// The next picture, `Quiet` after `idle` without one, or `Stopped`.
fn next(slot: &Slot, idle: Duration) -> Next {
    let latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    let (mut latest, _) = slot.ready.wait_timeout_while(latest, idle, |l| l.is_none() && !slot.stop.load(Ordering::SeqCst)).unwrap_or_else(|e| e.into_inner());
    if slot.stop.load(Ordering::SeqCst) {
        return Next::Stopped;
    }
    latest.take().map_or(Next::Quiet, |(frame, retried)| Next::Picture(Box::new(frame), retried))
}

/// Puts a picture back to be taken again, unless a newer one has come meanwhile (which is then the one that goes out).
fn requeue(slot: &Slot, frame: Frame, first_try: Instant) {
    let mut latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    if latest.is_none() {
        *latest = Some((frame, Some(first_try)));
    }
}

/// Ends the thread unless a picture landed meanwhile; the check and the end are one step under the lock, so a picture handed over after it sees the thread gone.
fn retire(slot: &Slot) -> bool {
    let latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    if latest.is_some() {
        return false;
    }
    slot.alive.store(false, Ordering::SeqCst);
    true
}

fn stopped(slot: &Slot) -> bool {
    slot.stop.load(Ordering::SeqCst)
}

/// Waits `time`, ending early when stopped.
fn pause(slot: &Slot, time: Duration) {
    let latest = slot.latest.lock().unwrap_or_else(|e| e.into_inner());
    let _ = slot.ready.wait_timeout_while(latest, time, |_| !slot.stop.load(Ordering::SeqCst));
}

fn run(name: &str, backend: &dyn Backend, tell: &(dyn Fn(State) + Send + Sync), slot: &Arc<Slot>, idle: Duration, retry: Duration, keep: bool) {
    let _ending = Ending(slot.clone());
    let mut open: Option<(u32, u32, Box<dyn Sender>)> = None;
    let mut bytes = Vec::new();
    loop {
        let (frame, retried) = match next(slot, idle) {
            // A picture retried past the idle time counts as no picture, so a stream that stopped doesn't send its last one late
            Next::Picture(_, Some(first_try)) if first_try.elapsed() >= idle => {
                if keep || !retire(slot) {
                    if keep {
                        tell(State::Idle);
                    }
                    continue;
                }
                break;
            }
            Next::Picture(frame, retried) => (*frame, retried),
            Next::Quiet if keep => {
                tell(State::Idle);
                continue;
            }
            Next::Quiet if !retire(slot) => continue,
            Next::Quiet | Next::Stopped => break,
        };
        let size = (frame.width, frame.height);
        if size.0 == 0 || size.1 == 0 {
            continue;
        }
        // A sender that can change size where it is keeps its place, so whoever reads it isn't cut off
        if let Some((w, h, sender)) = open.as_mut().filter(|(w, h, _)| (*w, *h) != size) {
            match sender.resize(size.0, size.1) {
                Ok(Resized::Yes) => (*w, *h) = size,
                Ok(Resized::Remake) => open = None,
                // A reader holds the picture just now: the sender stays where it is (a reader that is busy is not a sender that is gone) and this picture is tried again
                Ok(Resized::Busy) => {
                    requeue(slot, frame, retried.unwrap_or_else(Instant::now));
                    pause(slot, RESIZE_RETRY);
                    continue;
                }
                Err(why) => {
                    log::warn!("the sender {name} couldn't change size: {why}");
                    open = None;
                }
            }
        }
        // Opening and resizing can take a while, and Stop doesn't wait for them: a picture taken before it is not sent after
        if stopped(slot) {
            break;
        }
        if open.is_none() {
            match backend.open(name, size.0, size.1) {
                Ok(sender) => open = Some((size.0, size.1, sender)),
                Err(why) => {
                    log::warn!("couldn't start the sender {name}: {why}");
                    tell(State::Failed(Problem::Open));
                    pause(slot, retry);
                    continue;
                }
            }
            if stopped(slot) {
                break;
            }
        }
        if open.as_ref().is_some_and(|(_, _, sender)| !sender.wants_frame()) {
            tell(State::Idle);
            continue;
        }
        // The cut-out the frame carries (the step that tapped it left it only for a stream that asked for transparency); colours that are already multiplied by it stay opaque
        let cut_out = frame.alpha().filter(|a| !a.premultiplied);
        if !pack::bgra(&frame, cut_out.map(|a| &a.data[..]), &mut bytes) {
            tell(State::Failed(Problem::Format));
            continue;
        }
        // The last look before the picture leaves
        if stopped(slot) {
            break;
        }
        if let Some((_, _, sender)) = open.as_mut() {
            match sender.send_buffer(&mut bytes) {
                Ok(()) => tell(State::Sending(size.0, size.1)),
                Err(why) => {
                    log::warn!("the sender {name} stopped: {why}");
                    open = None;
                    tell(State::Failed(Problem::Send));
                    pause(slot, retry);
                }
            }
        }
    }
    drop(open);
    tell(State::Idle);
}

#[cfg(test)]
pub(crate) mod tests;
