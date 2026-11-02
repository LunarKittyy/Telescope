//! Plays the phone's microphone into the virtual microphone (audio.py's AudioWorker). A reader fills the jitter
//! buffer from the phone; a writer drains it into the output at the output's own pace.
//!
//! Each connection is read on a thread of its own, so a hand-over can switch to the new route at once even while a
//! read on the old one is stuck: the old thread lets go as soon as its read comes back.

use telescope_core::i18n::Fields;
use crate::jitter::{target_for, JitterBuffer, CHUNK};
use crate::shape::{Controls, MakeDenoiser, Measure, Shaper};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use telescope_core::audio::{AudioFeed, AudioPacket, MicLevel, MicSettings, MicStream};
use telescope_core::i18n::tr;
use telescope_core::media::{FeedEnd, FeedStopper};

pub const RETRY: Duration = Duration::from_secs(3);

/// Opens the phone's audio on one route; the error is what the card shows.
pub type Opener = Arc<dyn Fn() -> Result<Box<dyn AudioFeed>, String> + Send + Sync>;

/// Where the virtual microphone takes the audio (FifoSink, the Windows output, a test's stand-in).
pub trait MicSink: Send {
    /// Blocks at the output's pace.
    fn write(&mut self, samples: &[i16]) -> std::io::Result<()>;
}

pub type OpenSink = Arc<dyn Fn() -> Result<Box<dyn MicSink>, String> + Send + Sync>;

/// on_status(kind, text) from the worker's threads: "ok" once audio flows, "err" with the reason.
pub type OnStatus = Arc<dyn Fn(&str, &str) + Send + Sync>;

struct Route {
    url: String,
    open: Opener,
}

enum Wake {
    /// The connection with this number ended, with what to say (None: nothing, a hand-over or a stop).
    Ended(u64, Option<String>),
    /// A hand-over's connection is ready: its feed, its first audio and its URL.
    Ready(Box<dyn AudioFeed>, AudioPacket, String),
}

struct Shared {
    stop: AtomicBool,
    controls: Controls,
    buffer: Mutex<JitterBuffer>,
    measure: Mutex<Measure>,
    on_status: OnStatus,
    route: Mutex<Route>,
    /// The connection whose audio goes into the buffer; others let go.
    current: AtomicU64,
    /// Ends a read stuck on the connection going into the buffer.
    reading: Mutex<Option<Box<dyn FeedStopper>>>,
    wakes: Mutex<Vec<Wake>>,
    signal: Condvar,
    retry: Duration,
}

impl Shared {
    fn wake(&self, w: Wake) {
        self.wakes.lock().unwrap_or_else(|e| e.into_inner()).push(w);
        self.signal.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// feed is what's read from now; the connection before it lets go of a read it's stuck in.
    fn read_from(&self, feed: &dyn AudioFeed) {
        let last = std::mem::replace(&mut *self.reading.lock().unwrap_or_else(|e| e.into_inner()), feed.stopper());
        if let Some(last) = last {
            last.stop();
        }
    }

    /// Waits up to d for a stop or a wake; true once stopped.
    fn wait(&self, d: Duration) -> bool {
        let until = Instant::now() + d;
        let mut wakes = self.wakes.lock().unwrap_or_else(|e| e.into_inner());
        while !self.stopped() && wakes.is_empty() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            wakes = self.signal.wait_timeout(wakes, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        self.stopped()
    }
}

pub struct AudioWorker {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    make_denoiser: Option<MakeDenoiser>,
    open_sink: OpenSink,
}

impl AudioWorker {
    pub fn new(url: &str, open: Opener, open_sink: OpenSink, on_status: OnStatus) -> AudioWorker {
        AudioWorker::with(url, open, open_sink, on_status, RETRY, Box::new(crate::denoise::make))
    }

    pub fn with(url: &str, open: Opener, open_sink: OpenSink, on_status: OnStatus, retry: Duration,
                make_denoiser: MakeDenoiser) -> AudioWorker {
        AudioWorker {
            shared: Arc::new(Shared {
                stop: AtomicBool::new(false),
                controls: Controls::default(),
                buffer: Mutex::new(JitterBuffer::with_target(target_for(url))),
                measure: Mutex::new(Measure::default()),
                on_status,
                route: Mutex::new(Route { url: url.into(), open }),
                current: AtomicU64::new(0),
                reading: Mutex::new(None),
                wakes: Mutex::new(Vec::new()),
                signal: Condvar::new(),
                retry,
            }),
            threads: Vec::new(),
            make_denoiser: Some(make_denoiser),
            open_sink,
        }
    }

    pub fn controls(&self) -> &Controls {
        &self.shared.controls
    }

    /// (peak, rms, limited) since the last call.
    pub fn take_level(&self) -> (f32, f32, bool) {
        self.shared.controls.take_level()
    }

    pub fn url(&self) -> String {
        self.shared.route.lock().unwrap_or_else(|e| e.into_inner()).url.clone()
    }

    pub fn start(&mut self) {
        let s = self.shared.clone();
        self.threads.push(std::thread::Builder::new().name("mic-read".into()).spawn(move || read_loop(&s)).expect("thread"));
        let s = self.shared.clone();
        let open_sink = self.open_sink.clone();
        let make = self.make_denoiser.take().unwrap_or_else(|| Box::new(|| Ok(None)));
        self.threads.push(std::thread::Builder::new().name("mic-write".into())
            .spawn(move || write_loop(&s, &open_sink, Shaper::new(make))).expect("thread"));
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.current.store(u64::MAX, Ordering::SeqCst);
        self.shared.signal.notify_all();
        let reading = self.shared.reading.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(reading) = reading {
            reading.stop();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Moves to url without a gap: opens it next to the current connection and switches once audio comes through. If
    /// it doesn't, the current one keeps going and the next reconnect goes to url.
    pub fn hand_over(&self, url: &str, open: Opener) {
        *self.shared.route.lock().unwrap_or_else(|e| e.into_inner()) = Route { url: url.into(), open: open.clone() };
        let s = self.shared.clone();
        let url = url.to_string();
        let _ = std::thread::Builder::new().name("mic-handover".into()).spawn(move || {
            let mut feed = match open() {
                Ok(f) => f,
                Err(_) => {
                    log::info!("Couldn't open the microphone's new connection; staying on the old one");
                    return;
                }
            };
            let Ok(first) = feed.read() else { return };
            if first.data.is_empty() || s.stopped() || s.route.lock().unwrap_or_else(|e| e.into_inner()).url != url {
                return;
            }
            s.wake(Wake::Ready(feed, first, url));
        });
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.stop();
        }
    }
}

impl AudioWorker {
    pub fn set_settings(&mut self, settings: &MicSettings) {
        MicStream::set_settings(self, settings)
    }
}

impl MicStream for AudioWorker {
    fn set_settings(&mut self, settings: &MicSettings) {
        let c = self.controls();
        c.set_muted(settings.muted);
        c.set_gain(10f32.powf(settings.gain_db / 20.0));
        c.set_limit(settings.limiter);
        c.set_noise_reduction(settings.noise_reduction);
        c.set_auto_level(settings.auto_level);
    }

    fn level(&self) -> MicLevel {
        let (peak, rms, _) = self.take_level();
        MicLevel { peak, rms }
    }

    fn stop(mut self: Box<Self>) {
        AudioWorker::stop(&mut self);
    }
}

/// Reads one connection into the buffer until it ends or another one takes over.
fn pump(s: Arc<Shared>, id: u64, mut feed: Box<dyn AudioFeed>, first: Option<AudioPacket>) {
    let mut told = false;
    let mut next = first;
    let reason = loop {
        if s.stopped() || s.current.load(Ordering::SeqCst) != id {
            break None;
        }
        let packet = match next.take().map(Ok).unwrap_or_else(|| feed.read()) {
            Ok(p) if !p.data.is_empty() => p,
            Err(FeedEnd::Stopped) => break None,
            Ok(_) | Err(_) => break Some(tr("The phone's microphone stopped.")),
        };
        if s.stopped() || s.current.load(Ordering::SeqCst) != id {
            break None;
        }
        s.buffer.lock().unwrap_or_else(|e| e.into_inner()).push(&packet.data);
        s.measure.lock().unwrap_or_else(|e| e.into_inner()).measure(&s.controls, &packet.data);
        if !told {
            (s.on_status)("ok", "");
            told = true;
        }
    };
    drop(feed);
    s.wake(Wake::Ended(id, reason));
}

fn read_loop(s: &Arc<Shared>) {
    let mut next_id = 0u64;
    let mut running: Option<u64> = None;
    while !s.stopped() {
        let wakes: Vec<Wake> = std::mem::take(&mut *s.wakes.lock().unwrap_or_else(|e| e.into_inner()));
        let mut problem: Option<String> = None;
        let mut ended = false;
        for w in wakes {
            match w {
                // The route moved on since this one connected (a newer hand-over, say): it lets go
                Wake::Ready(_, _, url) if s.route.lock().unwrap_or_else(|e| e.into_inner()).url != url => {}
                Wake::Ready(feed, first, url) => {
                    next_id += 1;
                    s.current.store(next_id, Ordering::SeqCst);
                    {
                        let mut b = s.buffer.lock().unwrap_or_else(|e| e.into_inner());
                        b.splice();
                        b.set_target_ms(target_for(&url));
                    }
                    s.measure.lock().unwrap_or_else(|e| e.into_inner()).reset();
                    running = Some(next_id);
                    s.read_from(&*feed);
                    let (s2, id) = (s.clone(), next_id);
                    let _ = std::thread::Builder::new().name("mic-read".into()).spawn(move || pump(s2, id, feed, Some(first)));
                }
                Wake::Ended(id, reason) if Some(id) == running => {
                    running = None;
                    ended = true;
                    problem = reason;
                }
                Wake::Ended(..) => {}
            }
        }
        if s.stopped() {
            break;
        }
        if running.is_none() {
            if ended {
                if let Some(p) = &problem {
                    (s.on_status)("err", p);
                }
                // a hand-over meanwhile goes ahead at once
                if s.wait(s.retry) {
                    break;
                }
                if !s.wakes.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                    continue;
                }
            }
            let (url, open) = {
                let r = s.route.lock().unwrap_or_else(|e| e.into_inner());
                (r.url.clone(), r.open.clone())
            };
            match open() {
                Ok(feed) => {
                    next_id += 1;
                    s.current.store(next_id, Ordering::SeqCst);
                    {
                        let mut b = s.buffer.lock().unwrap_or_else(|e| e.into_inner());
                        b.restart_clock();
                        b.set_target_ms(target_for(&url));
                    }
                    s.measure.lock().unwrap_or_else(|e| e.into_inner()).reset();
                    running = Some(next_id);
                    s.read_from(&*feed);
                    let (s2, id) = (s.clone(), next_id);
                    let _ = std::thread::Builder::new().name("mic-read".into()).spawn(move || pump(s2, id, feed, None));
                }
                Err(text) => {
                    if s.stopped() {
                        break;
                    }
                    s.wake(Wake::Ended(0, Some(text)));
                    running = Some(0);
                }
            }
            continue;
        }
        s.wait(Duration::from_secs(1));
    }
}

fn write_loop(s: &Shared, open_sink: &OpenSink, mut shaper: Shaper) {
    // The output is reopened after a failure (a restarted sound server, a reader that went away)
    let mut failed = false;
    while !s.stopped() {
        let Some(problem) = write_once(s, open_sink, &mut shaper, failed) else { break };
        if s.stopped() {
            break;
        }
        failed = true;
        (s.on_status)("err", &problem);
        let until = Instant::now() + s.retry;
        while !s.stopped() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10).min(s.retry));
        }
    }
}

fn write_once(s: &Shared, open_sink: &OpenSink, shaper: &mut Shaper, recovering: bool) -> Option<String> {
    let mut sink = match open_sink() {
        Ok(sink) => sink,
        Err(e) => {
            if !recovering {
                log::error!("Couldn't open the microphone output: {e}");
            }
            return Some(tr("Couldn't open the virtual microphone: {error}").field("error", &e));
        }
    };
    if recovering {
        (s.on_status)("ok", "");
    }
    let mut problem = None;
    while !s.stopped() {
        let (raw, has_audio, quiet) = {
            let mut b = s.buffer.lock().unwrap_or_else(|e| e.into_inner());
            let raw = b.pop(CHUNK);
            (raw, b.has_audio, b.quiet_limit())
        };
        let data = shaper.shape(&s.controls, &raw, has_audio, quiet);
        let data = shaper.mute(&s.controls, data);
        if let Err(e) = sink.write(&data) {
            if !s.stopped() {
                log::error!("Microphone output failed: {e}");
                problem = Some(tr("The virtual microphone stopped taking audio."));
            }
            break;
        }
    }
    drop(sink);
    shaper.drop_denoiser();
    problem
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shape::tests::{bytes, chunk};
    use std::collections::VecDeque;

    /// A phone connection that hands over what it has in 480-byte reads, then ends.
    pub struct Resp(VecDeque<u8>);

    impl AudioFeed for Resp {
        fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
            if self.0.is_empty() {
                return Err(FeedEnd::Closed);
            }
            let n = self.0.len().min(480);
            Ok(AudioPacket { data: self.0.drain(..n).collect(), time_ns: 0 })
        }
    }

    #[derive(Clone, Default)]
    pub struct Sink {
        pub data: Arc<Mutex<Vec<i16>>>,
        pub closed: Arc<AtomicBool>,
    }

    struct SinkHandle(Sink, bool);

    impl MicSink for SinkHandle {
        fn write(&mut self, samples: &[i16]) -> std::io::Result<()> {
            if self.1 {
                return Err(std::io::Error::other("reader gone"));
            }
            self.0.data.lock().unwrap().extend_from_slice(samples);
            std::thread::sleep(Duration::from_millis(2));
            Ok(())
        }
    }

    impl Drop for SinkHandle {
        fn drop(&mut self) {
            self.0.closed.store(true, Ordering::SeqCst);
        }
    }

    pub fn sink_opener(sink: &Sink) -> OpenSink {
        let sink = sink.clone();
        Arc::new(move || Ok(Box::new(SinkHandle(sink.clone(), false)) as Box<dyn MicSink>))
    }

    type Statuses = Arc<Mutex<Vec<(String, String)>>>;

    fn status(statuses: &Statuses) -> OnStatus {
        let st = statuses.clone();
        Arc::new(move |k, t| st.lock().unwrap().push((k.into(), t.into())))
    }

    fn worker(url: &str, open: Opener, sink: OpenSink) -> (AudioWorker, Statuses) {
        let statuses = Statuses::default();
        let w = AudioWorker::with(url, open, sink, status(&statuses), Duration::from_millis(50), Box::new(|| Ok(None)));
        (w, statuses)
    }

    pub fn until(cond: impl Fn() -> bool, secs: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while !cond() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    fn contains(data: &[i16], v: i16, run: usize) -> bool {
        data.windows(run).any(|w| w.iter().all(|&x| x == v))
    }

    #[test]
    fn worker_plays_the_phone_audio_into_the_sink() {
        let pcm = bytes(&vec![16i16; 200 * 48]);
        let sink = Sink::default();
        let (mut w, statuses) = worker("https://phone/v1/audio", Arc::new(move || Ok(Box::new(Resp(pcm.clone().into())) as Box<dyn AudioFeed>)), sink_opener(&sink));
        w.start();
        assert!(until(|| contains(&sink.data.lock().unwrap(), 16, 8), 3.0));
        w.stop();
        assert!(statuses.lock().unwrap().contains(&("ok".into(), "".into())));
        assert!(sink.closed.load(Ordering::SeqCst));
    }

    #[test]
    fn worker_reports_the_phones_reason_and_keeps_trying() {
        let calls = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let (mut w, statuses) = worker("https://phone/v1/audio", Arc::new(move || {
            *c2.lock().unwrap() += 1;
            Err("Allow the microphone in Telescope on the phone".into())
        }), sink_opener(&Sink::default()));
        w.start();
        assert!(until(|| *calls.lock().unwrap() >= 2, 3.0));
        w.stop();
        assert_eq!(statuses.lock().unwrap()[0], ("err".into(), "Allow the microphone in Telescope on the phone".into()));
    }

    #[test]
    fn worker_reports_an_output_that_wont_open() {
        let (mut w, statuses) = worker("https://p/v1/audio", Arc::new(|| Ok(Box::new(Resp(VecDeque::new())) as Box<dyn AudioFeed>)),
                                       Arc::new(|| Err("no reader".into())));
        w.start();
        assert!(until(|| statuses.lock().unwrap().iter().any(|(k, t)| k == "err" && t.contains("no reader")), 3.0));
        w.stop();
    }

    #[test]
    fn worker_reopens_an_output_that_stopped_taking_audio() {
        let (dying, good) = (Sink::default(), Sink::default());
        let opened = Arc::new(Mutex::new(0));
        let (d2, g2, o2) = (dying.clone(), good.clone(), opened.clone());
        let open_sink: OpenSink = Arc::new(move || {
            let mut n = o2.lock().unwrap();
            *n += 1;
            Ok(Box::new(if *n == 1 { SinkHandle(d2.clone(), true) } else { SinkHandle(g2.clone(), false) }) as Box<dyn MicSink>)
        });
        let pcm = bytes(&vec![16i16; 4000]);
        let (mut w, statuses) = worker("https://p/v1/audio", Arc::new(move || Ok(Box::new(Resp(pcm.clone().into())) as Box<dyn AudioFeed>)), open_sink);
        w.start();
        assert!(until(|| !good.data.lock().unwrap().is_empty(), 3.0));
        w.stop();
        assert!(dying.closed.load(Ordering::SeqCst));
        assert!(statuses.lock().unwrap().contains(&("err".into(), "The virtual microphone stopped taking audio.".into())));
    }

    #[test]
    fn meter_keeps_moving_while_nothing_reads_the_mic() {
        struct Stuck(Arc<AtomicBool>);
        impl MicSink for Stuck {
            fn write(&mut self, _s: &[i16]) -> std::io::Result<()> {
                // PipeWire suspended the mic and stopped reading the pipe
                let end = Instant::now() + Duration::from_secs(10);
                while !self.0.load(Ordering::SeqCst) && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            }
        }
        let resumed = Arc::new(AtomicBool::new(false));
        let r2 = resumed.clone();
        let pcm = bytes(&vec![16384i16; 48_000]);
        let (mut w, _) = worker("https://p/v1/audio", Arc::new(move || Ok(Box::new(Resp(pcm.clone().into())) as Box<dyn AudioFeed>)),
                               Arc::new(move || Ok(Box::new(Stuck(r2.clone())) as Box<dyn MicSink>)));
        w.start();
        let seen = Mutex::new(0f32);
        let moved = until(|| {
            let mut s = seen.lock().unwrap();
            *s = s.max(w.take_level().0); // each look resets the level
            *s > 0.4
        }, 3.0);
        resumed.store(true, Ordering::SeqCst);
        w.stop();
        assert!(moved);
    }

    /// A phone connection that keeps sending one value until it's dropped.
    struct Endless(i16, Arc<AtomicBool>);

    impl AudioFeed for Endless {
        fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
            std::thread::sleep(Duration::from_millis(5));
            Ok(AudioPacket { data: bytes(&vec![self.0; 240]), time_ns: 0 })
        }
    }

    impl Drop for Endless {
        fn drop(&mut self) {
            self.1.store(true, Ordering::SeqCst);
        }
    }

    fn endless(v: i16, closed: &Arc<AtomicBool>) -> Opener {
        let closed = closed.clone();
        Arc::new(move || Ok(Box::new(Endless(v, closed.clone())) as Box<dyn AudioFeed>))
    }

    #[test]
    fn worker_hands_over_to_another_route_without_restarting() {
        let (wifi_closed, usb_closed) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let sink = Sink::default();
        let (mut w, _) = worker("https://192.168.1.5:8443/v1/audio", endless(100, &wifi_closed), sink_opener(&sink));
        w.start();
        assert!(until(|| contains(&sink.data.lock().unwrap(), 100, 8), 3.0));
        w.hand_over("https://127.0.0.1:41000/v1/audio", endless(200, &usb_closed));
        assert!(until(|| wifi_closed.load(Ordering::SeqCst), 3.0));
        assert!(until(|| contains(&sink.data.lock().unwrap(), 200, 8), 3.0));
        w.stop();
        assert!(until(|| usb_closed.load(Ordering::SeqCst), 3.0));
    }

    #[test]
    fn a_hand_over_overtaken_by_a_newer_route_is_dropped() {
        let (wifi_closed, stale_closed) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let sink = Sink::default();
        let (mut w, _) = worker("https://wifi/v1/audio", endless(100, &wifi_closed), sink_opener(&sink));
        w.start();
        assert!(until(|| contains(&sink.data.lock().unwrap(), 100, 8), 3.0));
        // The route moved on to usb, whose connection never comes up, while an older hand-over was already on its way
        w.hand_over("https://usb/v1/audio", Arc::new(|| Err("refused".into())));
        let first = AudioPacket { data: bytes(&chunk(300)), time_ns: 0 };
        w.shared.wake(Wake::Ready(Box::new(Endless(300, stale_closed.clone())), first, "https://old/v1/audio".into()));
        assert!(until(|| stale_closed.load(Ordering::SeqCst), 3.0));
        assert!(!wifi_closed.load(Ordering::SeqCst)); // the connection in use stayed
        assert!(!contains(&sink.data.lock().unwrap(), 300, 8));
        w.stop();
    }

    #[test]
    fn a_hand_over_that_cant_connect_keeps_the_old_route() {
        let closed = Arc::new(AtomicBool::new(false));
        let sink = Sink::default();
        let (mut w, statuses) = worker("https://wifi/v1/audio", endless(100, &closed), sink_opener(&sink));
        w.start();
        assert!(until(|| contains(&sink.data.lock().unwrap(), 100, 8), 3.0));
        w.hand_over("https://usb/v1/audio", Arc::new(|| Err("refused".into())));
        std::thread::sleep(Duration::from_millis(200));
        let before = sink.data.lock().unwrap().len();
        assert!(until(|| sink.data.lock().unwrap().len() > before + 2000, 3.0)); // still playing
        assert!(!statuses.lock().unwrap().iter().any(|(k, _)| k == "err"));
        w.stop();
    }

    /// A connection that stays open and silent until something stops it.
    #[derive(Clone, Default)]
    struct Silent {
        stopped: Arc<AtomicBool>,
        reading: Arc<AtomicBool>,
    }

    struct StopSilent(Arc<AtomicBool>);

    impl FeedStopper for StopSilent {
        fn stop(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl AudioFeed for Silent {
        fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
            self.reading.store(true, Ordering::SeqCst);
            while !self.stopped.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(FeedEnd::Stopped)
        }

        fn stopper(&self) -> Option<Box<dyn FeedStopper>> {
            Some(Box::new(StopSilent(self.stopped.clone())))
        }
    }

    fn silent(feed: &Silent) -> Opener {
        let feed = feed.clone();
        Arc::new(move || Ok(Box::new(feed.clone()) as Box<dyn AudioFeed>))
    }

    #[test]
    fn stopping_ends_a_read_on_a_silent_connection() {
        let feed = Silent::default();
        let (mut w, _) = worker("browser:/audio", silent(&feed), sink_opener(&Sink::default()));
        w.start();
        assert!(until(|| feed.reading.load(Ordering::SeqCst), 3.0));
        w.stop();
        assert!(until(|| feed.stopped.load(Ordering::SeqCst), 2.0));
    }

    #[test]
    fn a_hand_over_ends_the_old_connections_silent_read() {
        let feed = Silent::default();
        let (mut w, _) = worker("https://wifi/v1/audio", silent(&feed), sink_opener(&Sink::default()));
        w.start();
        assert!(until(|| feed.reading.load(Ordering::SeqCst), 3.0));
        w.hand_over("https://usb/v1/audio", endless(200, &Arc::new(AtomicBool::new(false))));
        assert!(until(|| feed.stopped.load(Ordering::SeqCst), 2.0));
        w.stop();
    }

    #[test]
    fn handover_interrupts_a_stalled_read_instead_of_waiting_for_its_timeout() {
        /// The old route sends one chunk, then nothing, without closing.
        struct Stalls(bool, Arc<AtomicBool>);
        impl AudioFeed for Stalls {
            fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
                if std::mem::replace(&mut self.0, false) {
                    return Ok(AudioPacket { data: bytes(&chunk(100)), time_ns: 0 });
                }
                self.1.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(10));
                Err(FeedEnd::Closed)
            }
        }
        let stalled = Arc::new(AtomicBool::new(false));
        let st = stalled.clone();
        let sink = Sink::default();
        let (mut w, _) = worker("https://phone/old", Arc::new(move || Ok(Box::new(Stalls(true, st.clone())) as Box<dyn AudioFeed>)),
                               sink_opener(&sink));
        w.start();
        assert!(until(|| stalled.load(Ordering::SeqCst), 10.0));
        let started = Instant::now();
        w.hand_over("https://phone/new", endless(200, &Arc::new(AtomicBool::new(false))));
        assert!(until(|| contains(&sink.data.lock().unwrap(), 200, 8), 2.0)); // well before a five-second timeout
        assert!(started.elapsed() < Duration::from_secs(2));
        w.stop();
    }
}
