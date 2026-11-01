//! A few threads that stay around for the pixel work of every frame, so a frame doesn't start and end threads of its own. They sleep
//! until there's work, the caller works too, and a part nobody has picked up yet goes to whoever gets there first, so a helper the
//! system hasn't run yet (a game on every core) holds nothing up.

use std::any::Any;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::thread::{self, Thread};

/// One call's work. `work` is only read for a part claimed below `count`, and the call stays until all of those are finished.
struct Job {
    work: *const (dyn Fn(usize) + Sync),
    count: usize,
    next: AtomicUsize,
    finished: AtomicUsize,
    panic: Mutex<Option<Box<dyn Any + Send>>>,
    caller: Thread,
}

// SAFETY: work points at something Sync, and only while its call waits for the parts handed out here
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// Takes parts until none are left.
    fn run(&self) {
        loop {
            let part = self.next.fetch_add(1, Ordering::Relaxed);
            if part >= self.count {
                return;
            }
            // SAFETY: part is below count and not finished yet, so the call that owns work is still waiting
            let work = unsafe { &*self.work };
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| work(part))) {
                *self.panic.lock().unwrap_or_else(|e| e.into_inner()) = Some(panic);
            }
            if self.finished.fetch_add(1, Ordering::AcqRel) + 1 == self.count {
                self.caller.unpark();
            }
        }
    }
}

#[derive(Default)]
struct Slot {
    job: Option<Arc<Job>>,
    /// Goes up with each job, so a helper that's done with one sleeps until the next.
    round: u64,
}

struct Workers {
    slot: Mutex<Slot>,
    wake: Condvar,
    helpers: usize,
}

/// How many threads help: a quarter of what this process may run on, at most 3, as more only take cores from a game.
fn helpers_for(cpus: usize) -> usize {
    (cpus / 4).min(3)
}

static WORKERS: LazyLock<Workers> = LazyLock::new(|| {
    let cpus = thread::available_parallelism().map_or(1, usize::from);
    Workers { slot: Mutex::default(), wake: Condvar::new(), helpers: helpers_for(cpus) }
});

fn help() {
    let workers = &*WORKERS;
    let mut round = 0;
    loop {
        let job = {
            let slot = workers.wake.wait_while(workers.slot.lock().unwrap(), |s| s.job.is_none() || s.round == round).unwrap();
            round = slot.round;
            slot.job.clone()
        };
        if let Some(job) = job {
            job.run();
        }
    }
}

fn start_helpers() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        for i in 0..WORKERS.helpers {
            let _ = thread::Builder::new().name(format!("telescope-rows-{i}")).spawn(help);
        }
    });
}

/// Runs work(0) to work(count - 1), each once, on any mix of this thread and the helpers, and returns when all are done. A panic in
/// a part comes out here once the others have finished. Work that's already on a helper, or arrives while another stream's frame is
/// using them, runs on its own thread.
pub fn spread(count: usize, work: &(dyn Fn(usize) + Sync)) {
    if count <= 1 || WORKERS.helpers == 0 {
        (0..count).for_each(work);
        return;
    }
    start_helpers();
    // SAFETY: only the lifetime goes, and nothing reads work once finished reaches count, which is what this call waits for
    let erased = unsafe { std::mem::transmute::<&(dyn Fn(usize) + Sync), *const (dyn Fn(usize) + Sync)>(work) };
    let job = Arc::new(Job { work: erased, count, next: AtomicUsize::new(0), finished: AtomicUsize::new(0), panic: Mutex::new(None), caller: thread::current() });
    let workers = &*WORKERS;
    {
        let mut slot = workers.slot.lock().unwrap();
        if slot.job.is_some() {
            drop(slot);
            (0..count).for_each(work);
            return;
        }
        slot.job = Some(job.clone());
        slot.round += 1;
    }
    workers.wake.notify_all();
    job.run();
    // A helper in the middle of its last part is about done, so a short look saves going to sleep for it
    let mut looks = 0;
    while job.finished.load(Ordering::Acquire) < count {
        looks += 1;
        if looks < 200 {
            std::hint::spin_loop();
        } else {
            thread::park();
        }
    }
    workers.slot.lock().unwrap().job = None;
    let panic = job.panic.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(panic) = panic {
        resume_unwind(panic);
    }
}

/// work(index, part) for every part, spread over the helpers.
pub fn each_part<T: Send>(parts: &mut [T], work: impl Fn(usize, &mut T) + Sync) {
    struct Parts<T>(*mut T);
    // SAFETY: the pointer is only used through at below, with an index spread hands out once
    unsafe impl<T: Send> Sync for Parts<T> {}
    impl<T> Parts<T> {
        /// SAFETY: i is inside the slice and no one else has it.
        unsafe fn at(&self, i: usize) -> &mut T {
            &mut *self.0.add(i)
        }
    }
    let shared = Parts(parts.as_mut_ptr());
    spread(parts.len(), &|i| {
        // SAFETY: spread hands out each index below parts.len() once
        work(i, unsafe { shared.at(i) });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn helpers_leave_most_of_the_machine_alone() {
        assert_eq!([1, 2, 3, 4, 6, 8, 12, 16, 64].map(helpers_for), [0, 0, 0, 1, 1, 2, 3, 3, 3]);
    }

    #[test]
    fn every_part_runs_once_with_its_own_index() {
        for count in [0, 1, 2, 8, 100] {
            let mut parts = vec![0u32; count];
            each_part(&mut parts, |i, p| *p += i as u32 + 1);
            assert_eq!(parts, (1..=count as u32).collect::<Vec<_>>());
        }
    }

    #[test]
    fn many_threads_can_ask_at_once() {
        let total = AtomicU32::new(0);
        thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..200 {
                        spread(8, &|_| {
                            total.fetch_add(1, Ordering::Relaxed);
                        });
                    }
                });
            }
        });
        assert_eq!(total.into_inner(), 8 * 200 * 8);
    }

    #[test]
    fn work_inside_work_finishes() {
        let total = AtomicU32::new(0);
        spread(4, &|_| {
            spread(4, &|_| {
                total.fetch_add(1, Ordering::Relaxed);
            });
        });
        assert_eq!(total.into_inner(), 16);
    }

    #[test]
    fn a_panicking_part_comes_out_of_the_call_and_the_helpers_go_on() {
        let done = AtomicU32::new(0);
        let caught = catch_unwind(AssertUnwindSafe(|| {
            spread(8, &|i| {
                assert_ne!(i, 3, "broken");
                done.fetch_add(1, Ordering::Relaxed);
            });
        }));
        assert!(caught.is_err());
        assert_eq!(done.load(Ordering::Relaxed), 7, "the other parts still ran");
        spread(8, &|_| {
            done.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(done.into_inner(), 15);
    }
}
