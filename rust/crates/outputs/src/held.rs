//! Jobs that hold a virtual camera from a thread of their own (vcam._Held), and letting go of the cameras for a driver
//! reload (vcam.device_released).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;
use telescope_core::output::Released;

/// A stop flag a job's thread waits on (threading.Event).
#[derive(Default)]
pub struct Stop {
    set: Mutex<bool>,
    cond: Condvar,
}

impl Stop {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cond.notify_all();
    }
    pub fn is_set(&self) -> bool {
        *self.set.lock().unwrap()
    }
    /// Waits until set or timeout; whether it's set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.cond.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

/// A wake-up a job's thread waits on between sends (threading.Event that gets cleared).
#[derive(Default)]
pub struct Nudge {
    set: Mutex<bool>,
    cond: Condvar,
}

impl Nudge {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cond.notify_all();
    }
    pub fn clear(&self) {
        *self.set.lock().unwrap() = false;
    }
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.cond.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

/// What a held job does on its thread until stop is set.
pub trait Job: Send + Sync + 'static {
    fn run(&self, stop: &Stop);
    /// Wake the thread so it sees the stop sooner.
    fn wake(&self) {}
    /// Told when the job lets go for a driver reload.
    fn suspended(&self) {}
}

/// Everything holding the cameras, so a reload can ask them all to let go.
#[derive(Default)]
pub struct Gate {
    released: AtomicUsize,
    holders: Mutex<Vec<Weak<dyn Suspend>>>,
}

pub trait Suspend: Send + Sync {
    fn suspend(&self);
    fn resume(&self);
}

impl Gate {
    pub fn is_released(&self) -> bool {
        self.released.load(Ordering::SeqCst) > 0
    }

    fn register(&self, holder: Weak<dyn Suspend>) {
        let mut holders = self.holders.lock().unwrap();
        holders.retain(|h| h.strong_count() > 0);
        holders.push(holder);
    }

    /// Everything lets go until the Released is dropped.
    pub fn release(self: &Arc<Self>) -> Released {
        let holders: Vec<Arc<dyn Suspend>> = {
            let list = self.holders.lock().unwrap();
            self.released.fetch_add(1, Ordering::SeqCst);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        for h in &holders {
            h.suspend();
        }
        let gate = self.clone();
        Released::new(move || {
            let resume = gate.released.fetch_sub(1, Ordering::SeqCst) == 1;
            if resume {
                // Whatever is registered now, not only what was when this began: one made during the reload starts too
                let now: Vec<Arc<dyn Suspend>> = gate.holders.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
                for h in &now {
                    h.resume();
                }
            }
        })
    }
}

struct Inner<J: Job> {
    job: Arc<J>,
    gate: Arc<Gate>,
    state: Mutex<HeldState>,
}

#[derive(Default)]
struct HeldState {
    wanted: bool,
    thread: Option<(std::thread::JoinHandle<()>, Arc<Stop>, Arc<AtomicBool>)>,
}

/// A job on a thread of its own: start()/stop() from any thread, paused while the gate is released.
pub struct Held<J: Job> {
    inner: Arc<Inner<J>>,
}

impl<J: Job> Held<J> {
    pub fn new(job: J, gate: &Arc<Gate>) -> Held<J> {
        let inner = Arc::new(Inner { job: Arc::new(job), gate: gate.clone(), state: Mutex::new(HeldState::default()) });
        let weak: Weak<dyn Suspend> = Arc::downgrade(&(inner.clone() as Arc<dyn Suspend>));
        gate.register(weak);
        Held { inner }
    }

    pub fn job(&self) -> &Arc<J> {
        &self.inner.job
    }

    pub fn start(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.wanted = true;
        self.inner.spawn(&mut st);
    }

    pub fn stop(&self) {
        let mut st = self.inner.state.lock().unwrap();
        st.wanted = false;
        let mut st = self.inner.join(st);
        // A start() that came in while the lock was free is honoured now that the thread is gone
        if st.wanted {
            self.inner.spawn(&mut st);
        }
    }

    pub fn running(&self) -> bool {
        alive(&self.inner.state.lock().unwrap())
    }

    pub fn wanted(&self) -> bool {
        self.inner.state.lock().unwrap().wanted
    }

    /// Stops the thread, runs change while nothing runs, then starts again if wanted: a restart with new settings.
    pub fn restart_with(&self, change: impl FnOnce()) {
        let st = self.inner.state.lock().unwrap();
        let mut st = self.inner.join(st);
        change();
        st.wanted = true;
        self.inner.spawn(&mut st);
    }

    /// Whether it's wanted and its thread is running.
    pub fn alive_and_wanted(&self) -> bool {
        let st = self.inner.state.lock().unwrap();
        st.wanted && alive(&st)
    }
}

/// A thread that hasn't ended yet, even one that was told to stop and hasn't: only one may ever write to the camera.
fn occupied(st: &HeldState) -> bool {
    // A thread that ended by itself (a failed prepare, a crash) counts as not running, so asking again restarts it
    st.thread.as_ref().is_some_and(|(_, _, done)| !done.load(Ordering::SeqCst))
}

fn alive(st: &HeldState) -> bool {
    st.thread.as_ref().is_some_and(|(_, stop, done)| !done.load(Ordering::SeqCst) && !stop.is_set())
}

#[cfg(not(test))]
const JOIN_WAIT: Duration = Duration::from_secs(3);
#[cfg(test)]
const JOIN_WAIT: Duration = Duration::from_millis(150);

impl<J: Job> Inner<J> {
    fn spawn(&self, st: &mut HeldState) {
        if occupied(st) || self.gate.is_released() {
            return;
        }
        if let Some((handle, _, _)) = st.thread.take() {
            let _ = handle.join();
        }
        let stop = Arc::new(Stop::default());
        let done = Arc::new(AtomicBool::new(false));
        let (job, s, d) = (self.job.clone(), stop.clone(), done.clone());
        let handle = std::thread::Builder::new().name("vcam".into()).spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run(&s)));
            if caught.is_err() {
                log::error!("A virtual camera job crashed");
            }
            d.store(true, Ordering::SeqCst);
        });
        match handle {
            Ok(handle) => st.thread = Some((handle, stop, done)),
            Err(e) => log::error!("Couldn't start a virtual camera thread: {e}"),
        }
    }

    /// Asks the thread to stop and waits without holding the lock. One that doesn't stop in time stays in the state, so
    /// nothing else is spawned until it ends.
    fn join<'a>(&'a self, st: MutexGuard<'a, HeldState>) -> MutexGuard<'a, HeldState> {
        let Some((_, stop, done)) = st.thread.as_ref() else { return st };
        let (stop, done) = (stop.clone(), done.clone());
        stop.set();
        self.job.wake();
        drop(st);
        let until = std::time::Instant::now() + JOIN_WAIT;
        while !done.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut st = self.state.lock().unwrap();
        if done.load(Ordering::SeqCst) {
            // Only the thread waited for: another caller may have joined it and started a new one meanwhile
            if st.thread.as_ref().is_some_and(|(_, _, d)| Arc::ptr_eq(d, &done)) {
                let _ = st.thread.take().unwrap().0.join();
            }
        } else {
            log::warn!("A virtual camera job did not stop within {}s", JOIN_WAIT.as_secs_f32());
        }
        st
    }
}

impl<J: Job> Suspend for Inner<J> {
    fn suspend(&self) {
        let st = self.state.lock().unwrap();
        drop(self.join(st));
        self.job.suspended();
    }

    fn resume(&self) {
        let mut st = self.state.lock().unwrap();
        if st.wanted {
            self.spawn(&mut st);
        }
    }
}

impl<J: Job> Drop for Held<J> {
    fn drop(&mut self) {
        // The thread holds its own Arc of the job; stopping it here lets the camera go with the holder
        let mut st = self.inner.state.lock().unwrap();
        st.wanted = false;
        drop(self.inner.join(st));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job that runs until stopped.
    struct Idle;

    impl Job for Idle {
        fn run(&self, stop: &Stop) {
            stop.wait(Duration::from_secs(30));
        }
    }

    #[test]
    fn a_holder_made_during_a_reload_starts_when_it_ends() {
        let gate = Arc::new(Gate::default());
        let before = Held::new(Idle, &gate);
        before.start();
        assert!(before.running());
        let released = gate.release();
        assert!(!before.running());
        let during = Held::new(Idle, &gate);
        during.start();
        assert!(!during.running()); // the cameras are let go
        drop(released);
        assert!(before.running());
        assert!(during.running());
    }

    /// A job that ignores the stop until told to leave.
    struct Stubborn {
        leave: AtomicBool,
        runs: AtomicUsize,
    }

    impl Job for Stubborn {
        fn run(&self, _stop: &Stop) {
            self.runs.fetch_add(1, Ordering::SeqCst);
            while !self.leave.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn a_job_that_will_not_stop_is_never_joined_by_a_second_writer() {
        let gate = Arc::new(Gate::default());
        let held = Held::new(Stubborn { leave: AtomicBool::new(false), runs: AtomicUsize::new(0) }, &gate);
        held.start();
        held.stop();
        held.start();
        held.restart_with(|| {});
        assert_eq!(held.job().runs.load(Ordering::SeqCst), 1);
        assert!(!held.running());
        held.job().leave.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        held.start();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(held.job().runs.load(Ordering::SeqCst), 2);
    }
}
