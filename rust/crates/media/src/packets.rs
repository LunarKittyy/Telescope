//! Plugins changing a stream's H.264 before it's decoded (HostPackets): each stream's tap, and what's safe to hand back.
//! A decoder only asks its Steps for a step, which is None while nobody changes that stream's packets, so it costs nothing then.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, Weak};
use telescope_core::host::{PacketStep, PacketStepFactory};
use telescope_core::media::PacketTap;

/// A step that fails this many access units in a row is left out until it's put on again.
pub const FAILS_BEFORE_SKIP: u32 = 30;

type Makers = Vec<(String, Arc<dyn PacketStepFactory>)>;

/// The packet steps of one stream. Set from the window's thread; each decoder feeds its own Steps, since a step's state
/// belongs to the decoder it fed (a hand-over decodes a second route next to the first).
#[derive(Default)]
pub struct Tap {
    inner: Mutex<TapInner>,
}

#[derive(Default)]
struct TapInner {
    makers: Makers,
    open: Vec<Weak<Mutex<StepsInner>>>,
}

impl Tap {
    pub fn new() -> Arc<Tap> {
        Arc::new(Tap::default())
    }

    /// Fresh steps from each owner's factory, for a new decoder, kept up to date by set().
    pub fn open(&self) -> Steps {
        let steps = Steps::default();
        let mut inner = self.inner.lock().unwrap();
        steps.update(&inner.makers);
        inner.open.retain(|w| w.strong_count() > 0);
        inner.open.push(Arc::downgrade(&steps.inner));
        steps
    }
}

impl PacketTap for Tap {
    fn set(&self, makers: Makers) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let names = |m: &Makers| m.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        let (mut old, mut new) =(names(&inner.makers), names(&makers));
        old.sort();
        new.sort();
        let changed = old != new;
        inner.makers = makers;
        inner.open.retain(|w| w.strong_count() > 0);
        for steps in inner.open.iter().filter_map(Weak::upgrade) {
            Steps { inner: steps }.update(&inner.makers);
        }
        changed
    }

    fn on(&self) -> bool {
        !self.inner.lock().unwrap().makers.is_empty()
    }
}

/// One decoder's packet steps from a Tap.
#[derive(Default)]
pub struct Steps {
    inner: Arc<Mutex<StepsInner>>,
}

#[derive(Default)]
struct StepsInner {
    steps: Vec<(String, Guarded)>,
    /// Goes up whenever the steps change, so the decoder can tell a new step came on.
    generation: u64,
    /// How many units went through step().
    #[cfg(test)]
    asked: u32,
}

impl Steps {
    fn update(&self, makers: &Makers) {
        let mut inner = self.inner.lock().unwrap();
        let mut old = std::mem::take(&mut inner.steps);
        inner.steps = makers.iter().map(|(owner, make)| {
            match old.iter().position(|(o, _)| o == owner) {
                Some(i) => old.swap_remove(i),
                None => (owner.clone(), Guarded::new(owner, make.make())),
            }
        }).collect();
        inner.generation += 1;
    }

    /// Whether any step is on.
    pub fn on(&self) -> bool {
        !self.inner.lock().unwrap().steps.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn asked(&self) -> u32 {
        self.inner.lock().unwrap().asked
    }

    /// Changes whenever a step comes on or off.
    pub fn generation(&self) -> u64 {
        self.inner.lock().unwrap().generation
    }

    /// The units to decode in place of unit: each step one after the other, each on every unit the one before handed back.
    pub fn step(&self, unit: &[u8]) -> Vec<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        #[cfg(test)]
        {
            inner.asked += 1;
        }
        let mut units = vec![unit.to_vec()];
        for (_, step) in inner.steps.iter_mut() {
            units = units.iter().flat_map(|u| step.run(u)).collect();
        }
        units
    }
}

/// A plugin's packet step that can't take the stream down: an access unit it fails on goes through unchanged.
pub struct Guarded {
    name: String,
    step: Box<dyn PacketStep>,
    fails: u32,
}

impl Guarded {
    pub fn new(name: &str, step: Box<dyn PacketStep>) -> Guarded {
        Guarded { name: name.into(), step, fails: 0 }
    }

    pub fn run(&mut self, unit: &[u8]) -> Vec<Vec<u8>> {
        if self.fails >= FAILS_BEFORE_SKIP {
            return vec![unit.to_vec()];
        }
        match catch_unwind(AssertUnwindSafe(|| self.step.step(unit))) {
            Ok(out) => {
                self.fails = 0;
                out
            }
            Err(_) => {
                self.fails += 1;
                if self.fails == 1 {
                    log::error!("Plugin {} failed on a packet; passing it through unchanged", self.name);
                } else if self.fails == FAILS_BEFORE_SKIP {
                    log::error!("Plugin {} failed on {} packets in a row; leaving it out", self.name, self.fails);
                }
                vec![unit.to_vec()]
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A factory from a closure that makes steps from closures.
    pub(crate) struct Make<F>(pub F);

    pub(crate) struct FnStep<G>(pub G);

    impl<G: FnMut(&[u8]) -> Vec<Vec<u8>> + Send> PacketStep for FnStep<G> {
        fn step(&mut self, unit: &[u8]) -> Vec<Vec<u8>> {
            (self.0)(unit)
        }
    }

    impl<F: Fn() -> Box<dyn PacketStep> + Send + Sync> PacketStepFactory for Make<F> {
        fn make(&self) -> Box<dyn PacketStep> {
            (self.0)()
        }
    }

    pub(crate) fn maker<F: Fn() -> Box<dyn PacketStep> + Send + Sync + 'static>(f: F) -> Arc<dyn PacketStepFactory> {
        Arc::new(Make(f))
    }

    fn counting(made: &Arc<AtomicUsize>) -> Arc<dyn PacketStepFactory> {
        let made = made.clone();
        maker(move || {
            made.fetch_add(1, Ordering::SeqCst);
            Box::new(FnStep(|u: &[u8]| vec![u.to_vec()]))
        })
    }

    struct Logged;

    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    impl log::Log for Logged {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Error
        }
        fn log(&self, record: &log::Record) {
            LOGGED.lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }

    #[test]
    fn a_failing_step_passes_units_through_and_is_left_out_after_a_while() {
        let _ = log::set_logger(&Logged);
        log::set_max_level(log::LevelFilter::Error);
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let mut run = Guarded::new("fun", Box::new(FnStep(move |_u: &[u8]| -> Vec<Vec<u8>> {
            c.fetch_add(1, Ordering::SeqCst);
            panic!("broken")
        })));
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        for _ in 0..FAILS_BEFORE_SKIP + 10 {
            assert_eq!(run.run(b"abc"), vec![b"abc".to_vec()]);
        }
        std::panic::set_hook(hook);
        assert_eq!(calls.load(Ordering::SeqCst), FAILS_BEFORE_SKIP as usize);
        assert!(LOGGED.lock().unwrap().iter().any(|line| line.contains("fun") && line.contains("failed on a packet")));
    }

    #[test]
    fn steps_run_one_after_the_other() {
        let tap = Tap::default();
        tap.set(vec![
            ("a".into(), maker(|| Box::new(FnStep(|u: &[u8]| vec![u.to_vec(), u.to_vec()])))),
            ("b".into(), maker(|| Box::new(FnStep(|u: &[u8]| vec![[u, b"!"].concat()])))),
        ]);
        assert_eq!(tap.open().step(b"x"), vec![b"x!".to_vec(), b"x!".to_vec()]);
    }

    #[test]
    fn each_stream_gets_a_fresh_step_and_a_step_coming_on_or_off_says_so() {
        let made = Arc::new(AtomicUsize::new(0));
        let (one, two) = (Tap::default(), Tap::default());
        let steps = one.open();
        assert!(one.set(vec![("fun".into(), counting(&made))]));
        // Already on: it keeps its step
        assert!(!one.set(vec![("fun".into(), counting(&made))]));
        assert!(two.set(vec![("fun".into(), counting(&made))]));
        let _two_steps = two.open();
        assert_eq!(made.load(Ordering::SeqCst), 2);
        assert!(one.set(vec![]) && !steps.on());
        assert!(!one.set(vec![]));
    }

    #[test]
    fn each_open_gives_every_owner_a_fresh_step() {
        let made = Arc::new(AtomicUsize::new(0));
        let tap = Tap::default();
        assert!(!tap.open().on() && made.load(Ordering::SeqCst) == 0);
        tap.set(vec![("fun".into(), counting(&made))]);
        let (one, two) = (tap.open(), tap.open());
        assert_eq!(made.load(Ordering::SeqCst), 2);
        assert!(one.on() && two.on() && !Arc::ptr_eq(&one.inner, &two.inner));
        tap.set(vec![]);
        assert!(!one.on() && !two.on() && made.load(Ordering::SeqCst) == 2);
    }
}
