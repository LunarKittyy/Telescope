//! The event bus: any thread emits, the host delivers on its own thread in the order things were emitted.

use crate::event::Event;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    queue: VecDeque<Event>,
    waker: Option<Waker>,
}

/// The queue every event goes through. The host owns it, drains it on its thread and hands each event to the plugins.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<Inner>>,
}

/// A handle to emit from anywhere (stream threads, the pairing server); cheap to clone.
#[derive(Clone)]
pub struct BusSender {
    inner: Arc<Mutex<Inner>>,
}

impl EventBus {
    pub fn new() -> EventBus {
        EventBus::default()
    }

    pub fn sender(&self) -> BusSender {
        BusSender { inner: self.inner.clone() }
    }

    /// Called after something lands in an empty queue, so the host can schedule a drain on its own thread.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).waker = Some(Arc::new(waker));
    }

    /// Takes the oldest event; events emitted while handling one queue up behind it rather than nesting.
    pub fn next(&self) -> Option<Event> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).queue.pop_front()
    }

    pub fn emit(&self, event: Event) {
        self.sender().emit(event);
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).queue.is_empty()
    }
}

impl BusSender {
    pub fn emit(&self, event: Event) {
        let waker = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let was_empty = inner.queue.is_empty();
            inner.queue.push_back(event);
            if was_empty { inner.waker.clone() } else { None }
        };
        // Outside the lock: a waker that drains straight away mustn't deadlock
        if let Some(w) = waker {
            w();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn delivers_in_order_and_wakes_once_per_batch() {
        let bus = EventBus::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        bus.set_waker(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let tx = bus.sender();
        std::thread::spawn(move || {
            tx.emit(Event::StreamStarted { url: "a".into() });
            tx.emit(Event::StreamStopped);
        })
        .join()
        .unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(bus.next(), Some(Event::StreamStarted { url: "a".into() }));
        assert_eq!(bus.next(), Some(Event::StreamStopped));
        assert_eq!(bus.next(), None);
    }
}
