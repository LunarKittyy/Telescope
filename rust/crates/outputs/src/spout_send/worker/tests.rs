use super::*;
use std::sync::atomic::AtomicUsize;
use telescope_core::frame::PixelFormat;

#[derive(Default)]
pub struct Log {
    pub opened: Mutex<Vec<(String, u32, u32)>>,
    pub sent: Mutex<Vec<Vec<u8>>>,
    pub closed: AtomicUsize,
    pub fail_open: AtomicBool,
    pub fail_send: AtomicBool,
    pub resizable: AtomicBool,
    pub resized: Mutex<Vec<(u32, u32)>>,
    pub panic_open: AtomicBool,
    pub no_reader: AtomicBool,
    pub resize_busy: AtomicBool,
    /// While set, opening a sender waits.
    pub hold_open: AtomicBool,
    /// How many times a sender was asked for.
    pub open_asked: AtomicUsize,
}

pub struct Mock(pub Arc<Log>);

struct MockSender(Arc<Log>);

impl Backend for Mock {
    fn open(&self, name: &str, width: u32, height: u32) -> Result<Box<dyn Sender>, String> {
        if self.0.panic_open.load(Ordering::SeqCst) {
            panic!("a backend that broke");
        }
        if self.0.fail_open.load(Ordering::SeqCst) {
            return Err("no room".into());
        }
        self.0.open_asked.fetch_add(1, Ordering::SeqCst);
        while self.0.hold_open.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.0.opened.lock().unwrap().push((name.to_string(), width, height));
        Ok(Box::new(MockSender(self.0.clone())))
    }
}

impl Sender for MockSender {
    fn wants_frame(&self) -> bool {
        !self.0.no_reader.load(Ordering::SeqCst)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<Resized, String> {
        if self.0.resize_busy.load(Ordering::SeqCst) {
            return Ok(Resized::Busy);
        }
        if self.0.resizable.load(Ordering::SeqCst) {
            self.0.resized.lock().unwrap().push((width, height));
        }
        Ok(if self.0.resizable.load(Ordering::SeqCst) { Resized::Yes } else { Resized::Remake })
    }

    fn send(&mut self, bgra: &[u8]) -> Result<(), String> {
        if self.0.fail_send.load(Ordering::SeqCst) {
            return Err("gone".into());
        }
        self.0.sent.lock().unwrap().push(bgra.to_vec());
        Ok(())
    }
}

impl Drop for MockSender {
    fn drop(&mut self) {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn picture(w: u32, h: u32, byte: u8) -> Frame {
    Frame::new(w, h, PixelFormat::Bgra, vec![byte; (w * h * 4) as usize])
}

pub fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(std::time::Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn spawn(log: &Arc<Log>, idle: Duration) -> (Worker, Arc<Mutex<Vec<State>>>) {
    spawn_keeping(log, idle, false)
}

fn spawn_keeping(log: &Arc<Log>, idle: Duration, keep: bool) -> (Worker, Arc<Mutex<Vec<State>>>) {
    let told = Arc::new(Mutex::new(Vec::new()));
    let t = told.clone();
    let worker = Worker::spawn("Telescope - Pixel".into(), Arc::new(Mock(log.clone())), Arc::new(move |s| t.lock().unwrap().push(s)), idle, Duration::from_millis(20), keep).unwrap();
    (worker, told)
}

#[test]
fn the_first_picture_opens_a_sender_at_its_size_and_goes_out_opaque() {
    let log = Arc::new(Log::default());
    let (worker, told) = spawn(&log, Duration::from_secs(5));
    assert!(worker.put(picture(2, 2, 7)));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    assert_eq!(*log.opened.lock().unwrap(), [("Telescope - Pixel".to_string(), 2, 2)]);
    assert_eq!(log.sent.lock().unwrap()[0], [7, 7, 7, 255].repeat(4));
    wait_for("the state", || told.lock().unwrap().contains(&State::Sending(2, 2)));
}

#[test]
fn another_size_opens_a_new_sender_and_closes_the_old_one() {
    let log = Arc::new(Log::default());
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    worker.put(picture(4, 2, 2));
    wait_for("the second", || log.sent.lock().unwrap().len() == 2);
    assert_eq!(log.opened.lock().unwrap().iter().map(|(_, w, h)| (*w, *h)).collect::<Vec<_>>(), [(2, 2), (4, 2)]);
    assert_eq!(log.closed.load(Ordering::SeqCst), 1);
}

#[test]
fn pictures_that_stop_take_the_sender_down_and_end_the_thread() {
    let log = Arc::new(Log::default());
    let (worker, told) = spawn(&log, Duration::from_millis(50));
    worker.put(picture(2, 2, 1));
    wait_for("the sender to go", || log.closed.load(Ordering::SeqCst) == 1 && !worker.alive());
    assert_eq!(told.lock().unwrap().last(), Some(&State::Idle));
    assert!(!worker.put(picture(2, 2, 1)), "a thread that ended takes nothing");
}

#[test]
fn a_sender_that_wont_open_says_why_and_tries_again_with_the_next_picture() {
    let log = Arc::new(Log::default());
    log.fail_open.store(true, Ordering::SeqCst);
    let (worker, told) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the failure", || told.lock().unwrap().contains(&State::Failed(Problem::Open)));
    log.fail_open.store(false, Ordering::SeqCst);
    wait_for("a retry", || {
        worker.put(picture(2, 2, 1));
        !log.sent.lock().unwrap().is_empty()
    });
}

#[test]
fn a_send_that_fails_closes_the_sender_and_a_later_picture_opens_another() {
    let log = Arc::new(Log::default());
    let (worker, told) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    log.fail_send.store(true, Ordering::SeqCst);
    worker.put(picture(2, 2, 2));
    wait_for("the failure", || told.lock().unwrap().contains(&State::Failed(Problem::Send)));
    log.fail_send.store(false, Ordering::SeqCst);
    wait_for("a new sender", || {
        worker.put(picture(2, 2, 3));
        log.opened.lock().unwrap().len() == 2 && log.sent.lock().unwrap().len() >= 2
    });
}

#[test]
fn dropping_the_worker_closes_its_sender() {
    let log = Arc::new(Log::default());
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    drop(worker);
    assert_eq!(log.closed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_sender_that_can_change_size_keeps_its_place() {
    let log = Arc::new(Log::default());
    log.resizable.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    worker.put(picture(4, 2, 2));
    wait_for("the second", || log.sent.lock().unwrap().len() == 2);
    assert_eq!(*log.resized.lock().unwrap(), [(4, 2)]);
    assert_eq!(log.opened.lock().unwrap().len(), 1, "no new sender");
    assert_eq!(log.closed.load(Ordering::SeqCst), 0);
    assert_eq!(log.sent.lock().unwrap()[1].len(), 4 * 2 * 4);
}

#[test]
fn a_kept_sender_stays_through_a_pause_and_says_idle() {
    let log = Arc::new(Log::default());
    let (worker, told) = spawn_keeping(&log, Duration::from_millis(40), true);
    worker.put(picture(2, 2, 1));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    wait_for("idle", || told.lock().unwrap().last() == Some(&State::Idle));
    assert!(worker.alive());
    assert_eq!(log.closed.load(Ordering::SeqCst), 0);
    worker.put(picture(2, 2, 2));
    wait_for("the next picture", || log.sent.lock().unwrap().len() == 2);
    assert_eq!(log.opened.lock().unwrap().len(), 1);
}

#[test]
fn a_thread_that_panics_is_no_longer_alive() {
    let log = Arc::new(Log::default());
    log.panic_open.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the thread to end", || !worker.alive());
}

#[test]
fn a_picture_taken_before_sharing_was_switched_off_is_not_sent_when_the_open_finishes_late() {
    let log = Arc::new(Log::default());
    log.hold_open.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the sender to be asked for", || log.open_asked.load(Ordering::SeqCst) == 1);
    // Stop doesn't wait for a sender that is still opening for longer than this
    drop(worker);
    log.hold_open.store(false, Ordering::SeqCst);
    wait_for("the late sender to be let go", || log.opened.lock().unwrap().len() == 1 && log.closed.load(Ordering::SeqCst) == 1);
    assert!(log.sent.lock().unwrap().is_empty(), "a picture went out after Stop");
}

#[test]
fn a_reader_that_holds_the_sender_while_it_changes_size_does_not_cost_the_sender() {
    let log = Arc::new(Log::default());
    log.resizable.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    log.resize_busy.store(true, Ordering::SeqCst);
    worker.put(picture(4, 2, 2));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(log.closed.load(Ordering::SeqCst), 0, "the sender was taken down for a busy reader");
    assert_eq!(log.opened.lock().unwrap().len(), 1);
    assert_eq!(log.sent.lock().unwrap().len(), 1, "nothing of the wrong size goes out");
    log.resize_busy.store(false, Ordering::SeqCst);
    wait_for("the picture after the reader let go", || log.sent.lock().unwrap().len() == 2);
    assert_eq!(*log.resized.lock().unwrap(), [(4, 2)]);
    assert_eq!(log.opened.lock().unwrap().len(), 1, "no new sender");
    assert_eq!(log.sent.lock().unwrap()[1].len(), 4 * 2 * 4);
}

#[test]
fn a_picture_that_waits_for_a_busy_reader_gives_way_to_a_newer_one() {
    let log = Arc::new(Log::default());
    log.resizable.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_secs(5));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    log.resize_busy.store(true, Ordering::SeqCst);
    worker.put(picture(4, 2, 2));
    std::thread::sleep(Duration::from_millis(50));
    worker.put(picture(4, 2, 9));
    log.resize_busy.store(false, Ordering::SeqCst);
    wait_for("the newest", || log.sent.lock().unwrap().len() >= 2);
    assert_eq!(log.sent.lock().unwrap()[1][0], 9, "the picture that was waiting went out over a newer one");
}

#[test]
fn a_picture_that_waits_for_a_busy_reader_past_the_idle_time_is_dropped_and_the_sender_goes() {
    let log = Arc::new(Log::default());
    log.resizable.store(true, Ordering::SeqCst);
    let (worker, _) = spawn(&log, Duration::from_millis(300));
    worker.put(picture(2, 2, 1));
    wait_for("the first", || log.sent.lock().unwrap().len() == 1);
    log.resize_busy.store(true, Ordering::SeqCst);
    worker.put(picture(4, 2, 2));
    wait_for("the thread to end", || !worker.alive());
    log.resize_busy.store(false, Ordering::SeqCst);
    assert_eq!(log.sent.lock().unwrap().len(), 1, "a stale picture went out after the stream stopped");
    wait_for("the sender to close", || log.closed.load(Ordering::SeqCst) == 1);
}
