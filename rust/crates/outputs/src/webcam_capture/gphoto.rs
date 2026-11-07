//! A DSLR as a webcam: `gphoto2 --capture-movie --stdout` sends its live view as a run of JPEGs, which this cuts apart and
//! hands out like any other MJPEG webcam. The camera ends live view every few minutes and gphoto2 exits then, so the
//! capture starts it again itself and the feed carries on (about a second of frames goes missing).

use super::Capture;
use crate::webcams::{gphoto_port, Choice, Device};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What gphoto2 says when something else (the file manager, usually) has mounted the camera.
pub const CLAIM_ERROR: &str = "Could not claim the USB device";
/// The biggest JPEG taken as a frame; anything longer without an end is garbage.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

pub fn is_claim_error(why: &str) -> bool {
    why.contains(CLAIM_ERROR)
}

/// Cuts a byte stream into JPEGs at their start (FF D8 FF) and end (FF D9) markers, whatever sizes it arrives in. Bytes
/// outside a JPEG are dropped, and so is a JPEG that runs past max without ending.
pub struct JpegSplitter {
    buf: Vec<u8>,
    max: usize,
}

impl JpegSplitter {
    pub fn new(max: usize) -> JpegSplitter {
        JpegSplitter { buf: Vec::new(), max }
    }

    /// Takes the next bytes and gives every JPEG they finished.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            let Some(start) = self.buf.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) else {
                // No start yet: only the tail could still become one
                let keep = self.buf.len().saturating_sub(2);
                self.buf.drain(..keep);
                break;
            };
            self.buf.drain(..start);
            match self.buf[2..].windows(2).position(|w| w == [0xFF, 0xD9]) {
                Some(end) => frames.push(self.buf.drain(..end + 4).collect()),
                None if self.buf.len() > self.max => {
                    // Too long to be a frame: look for the next start after this one
                    self.buf.drain(..1);
                }
                None => break,
            }
        }
        frames
    }
}

/// How the capture waits and gives up.
#[derive(Clone, Copy)]
pub struct Timing {
    /// Before starting gphoto2 again after it ended; doubles while it sends nothing.
    pub restart: Duration,
    pub restart_max: Duration,
    /// Runs in a row that sent nothing before it counts as failed.
    pub give_up_runs: u32,
    /// Without a frame for this long a read fails.
    pub silent: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing { restart: Duration::from_millis(200), restart_max: Duration::from_secs(4), give_up_runs: 5, silent: Duration::from_secs(15) }
    }
}

enum Msg {
    Frame(Vec<u8>),
    Failed(String),
}

/// Ends the capture from another thread: the flag, and the running gphoto2 killed.
struct Stop {
    flag: AtomicBool,
    pid: Mutex<Option<u32>>,
}

impl Stop {
    fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn kill(&self) {
        if let Some(pid) = *self.pid.lock().unwrap() {
            // SAFETY: the pid is cleared under this lock before the child is reaped, so it can't be someone else's by now
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

struct Stopper(Arc<Stop>);

impl telescope_core::media::FeedStopper for Stopper {
    fn stop(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        self.0.kill();
    }
}

pub struct GphotoCapture {
    rx: Receiver<Msg>,
    stop: Arc<Stop>,
    worker: Option<std::thread::JoinHandle<()>>,
    last_frame: Instant,
    silent: Duration,
}

/// Starts live view on the camera at device's port: Err((opened at all, why)). The first frame comes from read().
pub fn open(program: &str, device: &Device) -> Result<Box<dyn Capture>, (bool, String)> {
    open_with(program, &device.location, Timing::default())
}

pub fn open_with(program: &str, location: &str, timing: Timing) -> Result<Box<dyn Capture>, (bool, String)> {
    let stop = Arc::new(Stop { flag: AtomicBool::new(false), pid: Mutex::new(None) });
    let (tx, rx) = sync_channel(4);
    let (program, port, s) = (program.to_string(), gphoto_port(location).to_string(), stop.clone());
    let worker = std::thread::Builder::new().name("gphoto2".into()).spawn(move || run(&program, &port, timing, &s, &tx))
        .map_err(|e| (false, e.to_string()))?;
    Ok(Box::new(GphotoCapture { rx, stop, worker: Some(worker), last_frame: Instant::now(), silent: timing.silent }))
}

fn spawn(program: &str, port: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new(program);
    if !port.is_empty() {
        cmd.args(["--port", port]);
    }
    cmd.args(["--capture-movie", "--stdout"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
}

/// Keeps live view running until stopped: starts gphoto2, passes on its frames, starts it again when it exits.
fn run(program: &str, port: &str, timing: Timing, stop: &Arc<Stop>, tx: &SyncSender<Msg>) {
    let mut delay = timing.restart;
    let mut empty_runs = 0;
    loop {
        if stop.stopped() {
            return;
        }
        let mut child = match spawn(program, port) {
            Ok(c) => c,
            Err(e) => {
                let why = if e.kind() == std::io::ErrorKind::NotFound { "gphoto2 isn't installed".to_string() } else { format!("couldn't start gphoto2: {e}") };
                let _ = tx.send(Msg::Failed(why));
                return;
            }
        };
        *stop.pid.lock().unwrap() = Some(child.id());
        if stop.stopped() {
            stop.kill();
        }
        let errors = Arc::new(Mutex::new(String::new()));
        let err_reader = child.stderr.take().map(|mut e| {
            let errors = errors.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n @ 1..) = e.read(&mut buf) {
                    let mut text = errors.lock().unwrap();
                    if text.len() < 4096 {
                        text.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            })
        });
        let mut got = 0;
        if let Some(mut out) = child.stdout.take() {
            let mut split = JpegSplitter::new(MAX_FRAME);
            let mut buf = vec![0u8; 64 * 1024];
            'read: while let Ok(n @ 1..) = out.read(&mut buf) {
                for frame in split.push(&buf[..n]) {
                    got += 1;
                    match tx.try_send(Msg::Frame(frame)) {
                        Ok(()) | Err(TrySendError::Full(_)) => {} // a reader that falls behind skips frames rather than lagging
                        Err(TrySendError::Disconnected(_)) => break 'read,
                    }
                }
            }
        }
        // It ended (or is being stopped): make sure, then reap it
        *stop.pid.lock().unwrap() = None;
        let _ = child.kill();
        let _ = child.wait();
        if let Some(t) = err_reader {
            let _ = t.join();
        }
        if stop.stopped() {
            return;
        }
        let said = errors.lock().unwrap().clone();
        if is_claim_error(&said) {
            let _ = tx.send(Msg::Failed(CLAIM_ERROR.into()));
            return;
        }
        if got > 0 {
            empty_runs = 0;
            delay = timing.restart;
        } else {
            empty_runs += 1;
            if empty_runs >= timing.give_up_runs {
                let line = said.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("it sent nothing");
                let _ = tx.send(Msg::Failed(format!("gphoto2 stopped: {line}")));
                return;
            }
        }
        log::info!("gphoto2 ended after {got} frames, starting it again");
        let until = Instant::now() + delay;
        while Instant::now() < until && !stop.stopped() {
            std::thread::sleep(Duration::from_millis(20));
        }
        if got == 0 {
            delay = (delay * 2).min(timing.restart_max);
        }
    }
}

impl Capture for GphotoCapture {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        loop {
            if self.stop.stopped() {
                return Err("stopped".into());
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Frame(f)) => {
                    self.last_frame = Instant::now();
                    return Ok(f);
                }
                Ok(Msg::Failed(why)) => return Err(why),
                Err(RecvTimeoutError::Timeout) if self.last_frame.elapsed() > self.silent => {
                    return Err(format!("no frame within {:.1} s", self.silent.as_secs_f64()));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("gphoto2 ended".into()),
            }
        }
    }

    fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
        Box::new(Stopper(self.stop.clone()))
    }

    fn negotiated(&self) -> Option<Choice> {
        None
    }
}

impl Drop for GphotoCapture {
    fn drop(&mut self) {
        self.stop.flag.store(true, Ordering::SeqCst);
        self.stop.kill();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn jpeg(tag: u8, len: usize) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
        v.extend(std::iter::repeat_n(tag, len));
        v.extend([0xFF, 0xD9]);
        v
    }

    fn split_all(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = JpegSplitter::new(1024);
        chunks.iter().flat_map(|c| s.push(c)).collect()
    }

    #[test]
    fn the_splitter_cuts_frames_apart() {
        let (a, b) = (jpeg(1, 10), jpeg(2, 5));
        let both: Vec<u8> = [a.clone(), b.clone()].concat();
        assert_eq!(split_all(&[&both]), [a, b]);
    }

    #[test]
    fn the_splitter_joins_a_frame_split_across_reads() {
        let (a, b) = (jpeg(1, 20), jpeg(2, 20));
        let all: Vec<u8> = [a.clone(), b.clone()].concat();
        // Cut inside the start marker, inside the end marker and in the middle
        for cut in 1..all.len() {
            assert_eq!(split_all(&[&all[..cut], &all[cut..]]), [a.clone(), b.clone()], "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = all.chunks(1).collect();
        assert_eq!(split_all(&bytes), [a, b]);
    }

    #[test]
    fn the_splitter_skips_garbage() {
        let a = jpeg(7, 8);
        let mut input = vec![0x00, 0xFF, 0xD9, 0x12, 0xFF, 0xD8, 0x00];
        input.extend(&a);
        input.extend([0xAA, 0xFF]);
        assert_eq!(split_all(&[&input]), [a.clone()]);
        assert!(split_all(&[&[1, 2, 3, 0xFF, 0xD9, 0xFF, 0xD8, 0x00]]).is_empty());
        assert!(split_all(&[&[]]).is_empty());
        // A start with no end is waited for, then the next frame still comes through
        let mut s = JpegSplitter::new(1024);
        assert!(s.push(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2]).is_empty());
        assert_eq!(s.push(&a), [vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 0xFF, 0xD8, 0xFF, 0xE0, 7, 7, 7, 7, 7, 7, 7, 7, 0xFF, 0xD9]]);
    }

    #[test]
    fn the_splitter_drops_a_frame_that_never_ends_and_keeps_memory_bounded() {
        let mut s = JpegSplitter::new(64);
        let mut junk = vec![0xFF, 0xD8, 0xFF, 0xE0];
        junk.extend(std::iter::repeat_n(0x11, 500));
        assert!(s.push(&junk).is_empty());
        assert!(s.buf.len() <= 64 + 3, "{}", s.buf.len());
        let a = jpeg(3, 10);
        assert_eq!(s.push(&a), [a]);
        // Endless bytes with no start never pile up
        let mut s = JpegSplitter::new(64);
        for _ in 0..100 {
            s.push(&[0x42; 1000]);
        }
        assert!(s.buf.len() <= 2);
    }

    /// A folder with a fake gphoto2 in it, and its state files.
    struct Fake {
        dir: std::path::PathBuf,
    }

    impl Fake {
        fn new(name: &str, script: &str) -> Fake {
            let dir = std::env::temp_dir().join(format!("telescope-gphoto-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("gphoto2");
            std::fs::write(&path, format!("#!/bin/bash\nD='{}'\n{script}", dir.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake { dir }
        }
        fn program(&self) -> String {
            self.dir.join("gphoto2").to_string_lossy().to_string()
        }
        fn file(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn quick() -> Timing {
        Timing { restart: Duration::from_millis(20), restart_max: Duration::from_millis(100), give_up_runs: 3, silent: Duration::from_secs(10) }
    }

    fn alive(pid: &str) -> bool {
        // SAFETY: signal 0 only checks that the process exists
        unsafe { libc::kill(pid.trim().parse().unwrap(), 0) == 0 }
    }

    // First run: two frames (one sent in two pieces) then it exits like a camera ending live view. Second run: one frame, then it hangs.
    const LIVE_VIEW: &str = r#"
echo "$@" >> "$D/args"
echo $$ >> "$D/pids"
n=$(wc -l < "$D/pids")
if [ "$n" = 1 ]; then
  printf '\xff\xd8\xff\xe0AAAA\xff\xd9'
  printf '\xff\xd8\xff\xe0BB'
  sleep 0.1
  printf 'BB\xff\xd9'
  exit 0
fi
printf '\xff\xd8\xff\xe0CCCC\xff\xd9'
exec sleep 30
"#;

    #[test]
    fn frames_arrive_live_view_restarts_and_cancel_kills_the_child() {
        let fake = Fake::new("live", LIVE_VIEW);
        let mut cap = open_with(&fake.program(), "gphoto2:usb:001,004", quick()).unwrap();
        let stopper = cap.stopper();
        assert_eq!(cap.read().unwrap(), jpeg(b'A', 4));
        assert_eq!(cap.read().unwrap(), [vec![0xFF, 0xD8, 0xFF, 0xE0, b'B', b'B', b'B', b'B', 0xFF, 0xD9]].concat());
        // The first run ended; a frame from the second means it started again
        assert_eq!(cap.read().unwrap(), jpeg(b'C', 4));
        assert_eq!(fake.file("pids").lines().count(), 2);
        assert!(fake.file("args").lines().all(|l| l == "--port usb:001,004 --capture-movie --stdout"), "{}", fake.file("args"));
        let hanging = fake.file("pids").lines().last().unwrap().to_string();
        assert!(alive(&hanging));
        stopper.stop();
        assert_eq!(cap.read().unwrap_err(), "stopped");
        drop(cap);
        // Killed and reaped: a reaped process is gone, not a zombie
        assert!(!alive(&hanging));
    }

    #[test]
    fn dropping_the_capture_kills_the_child_too() {
        let fake = Fake::new("drop", "echo $$ > \"$D/pid\"\nprintf '\\xff\\xd8\\xff\\xe0Z\\xff\\xd9'\nexec sleep 30\n");
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(cap.read().unwrap(), jpeg(b'Z', 1));
        let pid = fake.file("pid");
        assert!(alive(&pid));
        drop(cap);
        assert!(!alive(&pid));
    }

    #[test]
    fn a_camera_the_file_manager_has_mounted_gives_the_claim_error_at_once() {
        let fake = Fake::new("claim", "echo $$ >> \"$D/pids\"\necho '*** Error ***' >&2\necho 'Could not claim the USB device' >&2\nexit 1\n");
        let mut cap = open_with(&fake.program(), "gphoto2:usb:001,004", quick()).unwrap();
        let why = cap.read().unwrap_err();
        assert!(is_claim_error(&why), "{why}");
        assert_eq!(fake.file("pids").lines().count(), 1, "no point starting it again");
    }

    #[test]
    fn a_gphoto2_that_keeps_failing_ends_the_feed_with_what_it_said() {
        let fake = Fake::new("fail", "echo $$ >> \"$D/pids\"\necho 'Unsupported operation' >&2\nexit 1\n");
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        let why = cap.read().unwrap_err();
        assert!(why.contains("Unsupported operation"), "{why}");
        assert_eq!(fake.file("pids").lines().count(), 3);
    }

    #[test]
    fn a_missing_gphoto2_is_said_plainly() {
        let mut cap = open_with("/nonexistent/gphoto2", "", quick()).unwrap();
        assert_eq!(cap.read().unwrap_err(), "gphoto2 isn't installed");
    }

    #[test]
    fn a_hung_gphoto2_with_no_frames_is_a_silent_camera() {
        let fake = Fake::new("hung", "echo $$ > \"$D/pid\"\nexec sleep 30\n");
        let timing = Timing { silent: Duration::from_millis(300), ..quick() };
        let mut cap = open_with(&fake.program(), "", timing).unwrap();
        assert_eq!(cap.read().unwrap_err(), "no frame within 0.3 s");
        let pid = fake.file("pid");
        drop(cap);
        assert!(!alive(&pid));
    }
}
