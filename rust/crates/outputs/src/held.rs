//! Jobs that hold a virtual camera from a thread of their own (vcam._Held), and letting go of the cameras for a driver
//! reload (vcam.device_released).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
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
                for h in &holders {
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
        self.inner.join(&mut st);
    }

    pub fn running(&self) -> bool {
        alive(&self.inner.state.lock().unwrap())
    }

    pub fn wanted(&self) -> bool {
        self.inner.state.lock().unwrap().wanted
    }

    /// Stops the thread, runs change while nothing runs, then starts again if wanted: a restart with new settings.
    pub fn restart_with(&self, change: impl FnOnce()) {
        let mut st = self.inner.state.lock().unwrap();
        self.inner.join(&mut st);
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

fn alive(st: &HeldState) -> bool {
    // A thread that ended by itself (a failed prepare, a crash) counts as not running, so asking again restarts it
    st.thread.as_ref().is_some_and(|(_, _, done)| !done.load(Ordering::SeqCst))
}

impl<J: Job> Inner<J> {
    fn spawn(&self, st: &mut HeldState) {
        if alive(st) || self.gate.is_released() {
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

    fn join(&self, st: &mut HeldState) {
        if let Some((handle, stop, done)) = st.thread.take() {
            stop.set();
            self.job.wake();
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while !done.load(Ordering::SeqCst) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if done.load(Ordering::SeqCst) {
                let _ = handle.join();
            } else {
                log::warn!("A virtual camera job did not stop within 3s");
            }
        }
    }
}

impl<J: Job> Suspend for Inner<J> {
    fn suspend(&self) {
        let mut st = self.state.lock().unwrap();
        self.join(&mut st);
        drop(st);
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
        if Arc::strong_count(&self.inner) == 1 {
            let mut st = self.inner.state.lock().unwrap();
            self.inner.join(&mut st);
        }
    }
}
