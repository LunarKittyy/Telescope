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

enum Parse {
    /// A whole JPEG of this many bytes.
    Done(usize),
    /// Fine so far, the rest hasn't arrived.
    Need,
    /// Not a JPEG after all.
    Bad,
}

/// Walks a JPEG that starts at the beginning of b: segments by their lengths (so an EXIF thumbnail or any FF D9 inside one is
/// skipped), then the picture data after a scan header up to the next real marker (FF 00 and restart markers belong to the
/// data), until the end marker.
fn parse(b: &[u8]) -> Parse {
    let mut i = 2;
    loop {
        if i + 1 >= b.len() {
            return Parse::Need;
        }
        if b[i] != 0xFF {
            return Parse::Bad;
        }
        let marker = b[i + 1];
        match marker {
            0xFF => {
                i += 1; // fill
                continue;
            }
            0x00 => return Parse::Bad,
            0xD9 => return Parse::Done(i + 2),
            0x01 | 0xD0..=0xD8 => {
                i += 2; // no length
                continue;
            }
            _ => {}
        }
        if i + 3 >= b.len() {
            return Parse::Need;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            return Parse::Bad;
        }
        let end = i + 2 + len;
        if end > b.len() {
            return Parse::Need;
        }
        i = end;
        if marker != 0xDA {
            continue;
        }
        // Picture data: on to the next marker that isn't part of it
        loop {
            let Some(p) = b[i..].iter().position(|&c| c == 0xFF).map(|p| p + i) else { return Parse::Need };
            let Some(&next) = b.get(p + 1) else { return Parse::Need };
            match next {
                0xFF => i = p + 1,
                0x00 | 0xD0..=0xD7 => i = p + 2,
                _ => {
                    i = p;
                    break;
                }
            }
        }
    }
}

/// Cuts a byte stream into JPEGs, whatever sizes it arrives in. Bytes outside a JPEG are dropped, and so is one that isn't
/// well formed or runs past max without ending.
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
            match parse(&self.buf) {
                Parse::Done(len) => frames.push(self.buf.drain(..len).collect()),
                Parse::Need if self.buf.len() <= self.max => break,
                // Garbage or too long to be a frame: look for the next start after this one
                Parse::Need | Parse::Bad => {
                    self.buf.drain(..1);
                }
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
    /// Without a frame for this long a read fails (and gphoto2 is killed, so the next try can claim the camera).
    pub silent: Duration,
    /// How long gphoto2 gets to turn live view off after an interrupt before it is killed.
    pub grace: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing { restart: Duration::from_millis(200), restart_max: Duration::from_secs(4), give_up_runs: 5, silent: Duration::from_secs(15), grace: Duration::from_secs(3) }
    }
}

enum Msg {
    Frame(Vec<u8>),
    Failed(String),
}

/// Ends the capture from another thread: the flag, and the running gphoto2 interrupted (it turns the camera's live view off
/// on SIGINT, a kill would leave the mirror up) and killed if it hasn't gone after the grace time.
struct Stop {
    flag: AtomicBool,
    pid: Mutex<Option<u32>>,
    /// Whether the running gphoto2 was interrupted already (a second interrupt makes it give up without turning live view off).
    interrupted: AtomicBool,
    grace: Duration,
}

impl Stop {
    fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn signal(&self, sig: libc::c_int) -> Option<u32> {
        let pid = *self.pid.lock().unwrap();
        if let Some(pid) = pid {
            // SAFETY: the pid is cleared under this lock before the child is reaped, so it can't be someone else's by now
            unsafe { libc::kill(pid as libc::pid_t, sig) };
        }
        pid
    }

    fn kill(self: &Arc<Self>) {
        if self.interrupted.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(pid) = self.signal(libc::SIGINT) else { return };
        let (stop, grace) = (self.clone(), self.grace);
        std::thread::spawn(move || {
            std::thread::sleep(grace);
            if *stop.pid.lock().unwrap() == Some(pid) {
                stop.signal(libc::SIGKILL);
            }
        });
    }

    fn stop(self: &Arc<Self>) {
        self.flag.store(true, Ordering::SeqCst);
        self.kill();
    }
}

struct Stopper(Arc<Stop>);

impl telescope_core::media::FeedStopper for Stopper {
    fn stop(&self) {
        self.0.stop();
    }
}

pub struct GphotoCapture {
    rx: Option<Receiver<Msg>>,
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
    let stop = Arc::new(Stop { flag: AtomicBool::new(false), pid: Mutex::new(None), interrupted: AtomicBool::new(false), grace: timing.grace });
    let (tx, rx) = sync_channel(4);
    let (program, port, s) = (program.to_string(), gphoto_port(location).to_string(), stop.clone());
    let worker = std::thread::Builder::new().name("gphoto2".into()).spawn(move || run(&program, &port, timing, &s, &tx))
        .map_err(|e| (false, e.to_string()))?;
    Ok(Box::new(GphotoCapture { rx: Some(rx), stop, worker: Some(worker), last_frame: Instant::now(), silent: timing.silent }))
}

/// The gphoto2 command, speaking English whatever the desktop's language is, since the claim error is told by its text.
pub fn command(program: &str, port: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("LC_ALL", "C").env("LANG", "C").env_remove("LANGUAGE");
    if !port.is_empty() {
        cmd.args(["--port", port]);
    }
    cmd.args(["--capture-movie", "--stdout"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// Hands over a last message once there's room, without holding up a stop.
fn deliver(tx: &SyncSender<Msg>, stop: &Stop, msg: Msg) {
    let mut msg = msg;
    loop {
        match tx.try_send(msg) {
            Err(TrySendError::Full(back)) if !stop.stopped() => {
                msg = back;
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => return,
        }
    }
}

/// Keeps live view running until stopped: starts gphoto2, passes on its frames, starts it again when it exits.
fn run(program: &str, port: &str, timing: Timing, stop: &Arc<Stop>, tx: &SyncSender<Msg>) {
    let mut delay = timing.restart;
    let mut empty_runs = 0;
    loop {
        if stop.stopped() {
            return;
        }
        let mut child: Child = match command(program, port).spawn() {
            Ok(c) => c,
            Err(e) => {
                let why = if e.kind() == std::io::ErrorKind::NotFound { "gphoto2 isn't installed".to_string() } else { format!("couldn't start gphoto2: {e}") };
                deliver(tx, stop, Msg::Failed(why));
                return;
            }
        };
        stop.interrupted.store(false, Ordering::SeqCst);
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
        // Kept open until gphoto2 is gone: with the pipe closed its goodbye to the camera dies of SIGPIPE
        let mut out = child.stdout.take();
        if let Some(out) = out.as_mut() {
            let mut split = JpegSplitter::new(MAX_FRAME);
            let mut buf = vec![0u8; 64 * 1024];
            let mut ending = false;
            while let Ok(n @ 1..) = out.read(&mut buf) {
                if ending {
                    continue; // only draining until it has gone
                }
                for frame in split.push(&buf[..n]) {
                    got += 1;
                    match tx.try_send(Msg::Frame(frame)) {
                        Ok(()) | Err(TrySendError::Full(_)) => {} // a reader that falls behind skips frames rather than lagging
                        Err(TrySendError::Disconnected(_)) => {
                            ending = true;
                            stop.kill();
                            break;
                        }
                    }
                }
            }
        }
        // It ended (or is being stopped): let it turn live view off if it's still running, then reap it
        end(&mut child, stop);
        drop(out);
        if let Some(t) = err_reader {
            let _ = t.join();
        }
        if stop.stopped() {
            return;
        }
        let said = errors.lock().unwrap().clone();
        if is_claim_error(&said) {
            deliver(tx, stop, Msg::Failed(CLAIM_ERROR.into()));
            return;
        }
        if got > 0 {
            empty_runs = 0;
            delay = timing.restart;
        } else {
            empty_runs += 1;
            if empty_runs >= timing.give_up_runs {
                let line = said.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("it sent nothing");
                deliver(tx, stop, Msg::Failed(format!("gphoto2 stopped: {line}")));
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

/// Interrupts the child if it's still running, kills it when it takes longer than the grace time, and reaps it.
fn end(child: &mut Child, stop: &Arc<Stop>) {
    let until = Instant::now() + stop.grace;
    if !stop.interrupted.swap(true, Ordering::SeqCst) {
        stop.signal(libc::SIGINT);
    }
    loop {
        // Reaping under the lock, so nobody signals the pid after it's free
        let mut pid = stop.pid.lock().unwrap();
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            *pid = None;
            return;
        }
        drop(pid);
        if Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut pid = stop.pid.lock().unwrap();
    let _ = child.kill();
    let _ = child.wait();
    *pid = None;
}

impl GphotoCapture {
    /// Ends it for good: gphoto2 killed and reaped, so nothing keeps the camera claimed afterwards.
    fn shutdown(&mut self) {
        // Dropping the receiver first means a worker waiting to hand something over gives up at once
        self.rx = None;
        self.stop.stop();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Capture for GphotoCapture {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        loop {
            if self.stop.stopped() {
                return Err("stopped".into());
            }
            let Some(rx) = &self.rx else { return Err("stopped".into()) };
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Frame(f)) => {
                    self.last_frame = Instant::now();
                    return Ok(f);
                }
                Ok(Msg::Failed(why)) => return Err(why),
                Err(RecvTimeoutError::Timeout) if self.last_frame.elapsed() > self.silent => {
                    // A gphoto2 that hung still has the camera claimed, so it goes before anything else tries
                    self.shutdown();
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
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// SOI, an APP0 segment, a scan header, picture data (tag bytes with a stuffed FF 00 and a restart marker) and EOI.
    fn jpeg(tag: u8, len: usize) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x06, b'J', b'F', b'I', b'F'];
        v.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0]);
        v.extend(std::iter::repeat_n(tag, len));
        v.extend([0xFF, 0x00, 0xFF, 0xD0, tag, 0xFF, 0xD9]);
        v
    }

    /// A JPEG whose EXIF segment holds a whole thumbnail JPEG and a stray FF D9, and a comment with one too.
    fn jpeg_with_metadata() -> Vec<u8> {
        let thumb = jpeg(9, 6);
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend([0xFF, 0xD9, 0x01]);
        exif.extend(&thumb);
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
        v.extend(((exif.len() + 2) as u16).to_be_bytes());
        v.extend(&exif);
        v.extend([0xFF, 0xFE, 0x00, 0x06, b'a', 0xFF, 0xD9, b'b']);
        v.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0, 5, 6, 7, 0xFF, 0xD9]);
        v
    }

    /// Two scans with a table between them, the way progressive JPEGs come.
    fn jpeg_progressive() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x03, 0];
        v.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0, 1, 2, 0xFF, 0x00, 3]);
        v.extend([0xFF, 0xC4, 0x00, 0x04, 0xFF, 0xD9]);
        v.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0, 4, 5, 0xFF, 0xD9]);
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
        for cut in 1..all.len() {
            assert_eq!(split_all(&[&all[..cut], &all[cut..]]), [a.clone(), b.clone()], "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = all.chunks(1).collect();
        assert_eq!(split_all(&bytes), [a, b]);
    }

    #[test]
    fn metadata_with_an_end_marker_in_it_doesnt_cut_a_frame_short() {
        let frames = [jpeg_with_metadata(), jpeg_progressive(), jpeg(4, 3)];
        let all: Vec<u8> = frames.concat();
        assert_eq!(split_all(&[&all]), frames);
        for cut in 1..all.len() {
            assert_eq!(split_all(&[&all[..cut], &all[cut..]]), frames, "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = all.chunks(1).collect();
        assert_eq!(split_all(&bytes), frames);
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
        // A start that turns out not to be a JPEG (a segment length of 1) doesn't swallow the frame after it
        let mut input = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 9, 9];
        input.extend(&a);
        assert_eq!(split_all(&[&input]), [a.clone()]);
        // A half-sent frame is waited for
        let mut s = JpegSplitter::new(1024);
        assert!(s.push(&a[..9]).is_empty());
        assert_eq!(s.push(&a[9..]), [a]);
    }

    #[test]
    fn the_splitter_drops_a_frame_that_never_ends_and_keeps_memory_bounded() {
        let mut s = JpegSplitter::new(64);
        let mut junk = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x7F, 0xFF];
        junk.extend(std::iter::repeat_n(0x11, 500));
        assert!(s.push(&junk).is_empty());
        assert!(s.buf.len() <= 2, "{}", s.buf.len());
        let a = jpeg(3, 10);
        assert_eq!(s.push(&a), [a]);
        let mut s = JpegSplitter::new(64);
        for _ in 0..100 {
            s.push(&[0x42; 1000]);
        }
        assert!(s.buf.len() <= 2);
    }

    /// A folder with a fake gphoto2 in it, the frames it plays and its state files.
    struct Fake {
        dir: std::path::PathBuf,
    }

    impl Fake {
        fn new(name: &str, script: &str) -> Fake {
            let dir = std::env::temp_dir().join(format!("telescope-gphoto-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, frame) in [("a", jpeg(b'A', 4)), ("b", jpeg(b'B', 4)), ("c", jpeg(b'C', 4)), ("z", jpeg(b'Z', 1))] {
                std::fs::write(dir.join(file), frame).unwrap();
            }
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
        Timing { restart: Duration::from_millis(20), restart_max: Duration::from_millis(100), give_up_runs: 3, silent: Duration::from_secs(10), grace: Duration::from_millis(1500) }
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
  cat "$D/a"
  head -c 20 "$D/b"
  sleep 0.1
  tail -c +21 "$D/b"
  exit 0
fi
cat "$D/c"
exec sleep 30
"#;

    #[test]
    fn frames_arrive_live_view_restarts_and_cancel_kills_the_child() {
        let fake = Fake::new("live", LIVE_VIEW);
        let mut cap = open_with(&fake.program(), "gphoto2:usb:001,004", quick()).unwrap();
        let stopper = cap.stopper();
        assert_eq!(cap.read().unwrap(), jpeg(b'A', 4));
        assert_eq!(cap.read().unwrap(), jpeg(b'B', 4));
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
        let fake = Fake::new("drop", "echo $$ > \"$D/pid\"\ncat \"$D/z\"\nexec sleep 30\n");
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

    // A gphoto2 that answers in the desktop's language unless it's told to speak English
    const LOCALISED: &str = r#"
if [ "$LC_ALL" = C ] && [ "$LANG" = C ] && [ -z "$LANGUAGE" ]; then
  echo 'Could not claim the USB device' >&2
else
  echo 'Das USB-Gerät konnte nicht belegt werden' >&2
fi
exit 1
"#;

    #[test]
    fn the_claim_error_is_found_whatever_language_the_desktop_speaks() {
        let cmd = command("gphoto2", "usb:001,004");
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().to_string(), v.map(|v| v.to_string_lossy().to_string()))).collect();
        assert_eq!(envs["LC_ALL"].as_deref(), Some("C"));
        assert_eq!(envs["LANG"].as_deref(), Some("C"));
        assert_eq!(envs["LANGUAGE"], None);
        let fake = Fake::new("locale", LOCALISED);
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert!(is_claim_error(&cap.read().unwrap_err()));
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

    // The first run hangs without a frame, holding the camera; a run after it gets frames
    const HANGS_ONCE: &str = r#"
echo $$ >> "$D/pids"
if [ "$(wc -l < "$D/pids")" = 1 ]; then exec sleep 30; fi
cat "$D/z"
exec sleep 30
"#;

    #[test]
    fn a_silent_gphoto2_is_killed_before_the_error_so_the_next_try_can_have_the_camera() {
        let fake = Fake::new("hung", HANGS_ONCE);
        let timing = Timing { silent: Duration::from_millis(300), ..quick() };
        let mut cap = open_with(&fake.program(), "", timing).unwrap();
        assert_eq!(cap.read().unwrap_err(), "no frame within 0.3 s");
        let hung = fake.file("pids").lines().next().unwrap().to_string();
        assert!(!alive(&hung), "the hung gphoto2 was left running");
        assert_eq!(cap.read().unwrap_err(), "stopped");
        // Another capture (what the reopened feed makes) gets frames
        let mut next = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(next.read().unwrap(), jpeg(b'Z', 1));
    }

    // Plays far more frames than the queue holds and then fails, so the worker has something to hand over with the queue full
    const FLOOD_THEN_CLAIM: &str = r#"
echo $$ > "$D/pid"
for i in $(seq 1 40); do cat "$D/z"; done
sleep 0.2
echo 'Could not claim the USB device' >&2
exit 1
"#;

    fn dropped_within(cap: Box<dyn Capture>, limit: Duration) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(cap);
            let _ = tx.send(());
        });
        rx.recv_timeout(limit).is_ok()
    }

    #[test]
    fn cancelling_with_a_full_queue_doesnt_hang() {
        let fake = Fake::new("full", FLOOD_THEN_CLAIM);
        let cap = open_with(&fake.program(), "", quick()).unwrap();
        // Nothing reads, so the queue fills and the last message waits for room
        std::thread::sleep(Duration::from_millis(700));
        assert!(dropped_within(cap, Duration::from_secs(3)), "dropping the capture hung on a full queue");
        assert!(!alive(&fake.file("pid")));
        // Same when it's stopped rather than dropped, and still hands the error over once there's room
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        std::thread::sleep(Duration::from_millis(700));
        let mut last = Ok(Vec::new());
        for _ in 0..10 {
            last = cap.read();
            if last.is_err() {
                break;
            }
        }
        assert!(is_claim_error(&last.unwrap_err()));
    }

    // Sends a frame, then waits like gphoto2 does and turns live view off (here: writes a file) when interrupted
    const TIDY: &str = r#"
echo $$ > "$D/pid"
trap 'echo off > "$D/off"; kill $S; exit 0' INT
cat "$D/z"
sleep 30 &
S=$!
wait
"#;

    #[test]
    fn stopping_interrupts_gphoto2_so_it_can_turn_live_view_off() {
        let fake = Fake::new("tidy", TIDY);
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(cap.read().unwrap(), jpeg(b'Z', 1));
        drop(cap);
        assert_eq!(fake.file("off").trim(), "off", "gphoto2 was killed before it could end live view");
        assert!(!alive(&fake.file("pid")));
        let fake = Fake::new("tidy-stop", TIDY);
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(cap.read().unwrap(), jpeg(b'Z', 1));
        cap.stopper().stop();
        assert_eq!(cap.read().unwrap_err(), "stopped");
        drop(cap);
        assert_eq!(fake.file("off").trim(), "off");
    }

    // Ignores the interrupt, so only a kill ends it
    const DEAF: &str = r#"
echo $$ > "$D/pid"
trap '' INT
cat "$D/z"
while :; do sleep 0.05; done
"#;

    #[test]
    fn a_gphoto2_that_ignores_the_interrupt_is_killed_after_the_grace_time() {
        let fake = Fake::new("deaf", DEAF);
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(cap.read().unwrap(), jpeg(b'Z', 1));
        let stopper = cap.stopper();
        stopper.stop();
        let started = Instant::now();
        assert!(dropped_within(cap, Duration::from_secs(3)), "stop hung on a gphoto2 that ignores the interrupt");
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(!alive(&fake.file("pid")));
    }

    /// The size a JPEG's start-of-frame segment declares.
    fn jpeg_size(b: &[u8]) -> Option<(u16, u16)> {
        let mut i = 2;
        while i + 9 < b.len() {
            let m = b[i + 1];
            if (0xC0..=0xC3).contains(&m) {
                return Some((u16::from_be_bytes([b[i + 7], b[i + 8]]), u16::from_be_bytes([b[i + 5], b[i + 6]])));
            }
            i += 2 + u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        }
        None
    }

    /// With a camera plugged in: TELESCOPE_DSLR_SECS=60 TELESCOPE_DSLR_OUT=/some/folder cargo nextest run --run-ignored all real_camera
    #[test]
    #[ignore]
    fn real_camera_live_view() {
        let secs: u64 = std::env::var("TELESCOPE_DSLR_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
        let out = std::env::var("TELESCOPE_DSLR_OUT").ok();
        let mut scanner = crate::webcams::GphotoScanner::system();
        let mut found = Vec::new();
        for _ in 0..20 {
            found = scanner.scan();
            if !found.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let device = found.first().expect("no camera listed");
        eprintln!("listed: {} at {} modes {}", device.name, device.location, device.modes.len());
        let started = Instant::now();
        let mut cap = open(&"gphoto2", device).unwrap_or_else(|e| panic!("{e:?}"));
        let mut times = Vec::new();
        let mut size = None;
        while started.elapsed() < Duration::from_secs(secs) {
            let frame = cap.read().expect("read");
            times.push(started.elapsed().as_secs_f64());
            size = size.or(jpeg_size(&frame));
            if let (Some(dir), true) = (&out, times.len() % 100 == 1 && times.len() < 400) {
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(format!("{dir}/frame{:04}.jpg", times.len()), &frame).unwrap();
            }
        }
        let gaps: Vec<(f64, f64)> = times.windows(2).filter(|w| w[1] - w[0] > 0.3).map(|w| (w[0], w[1] - w[0])).collect();
        eprintln!("first frame {:.2} s, {} frames in {secs} s = {:.1} fps, size {:?}, gaps over 0.3 s: {:?}", times[0], times.len(), times.len() as f64 / (secs as f64 - times[0]), size, gaps);
        drop(cap);
    }

    // Keeps sending frames, and says goodbye to the camera (over its output, as gphoto2 does) a moment after being interrupted
    const CHATTY: &str = r#"
echo $$ > "$D/pid"
trap 'echo i >> "$D/ints"; sleep 0.2; cat "$D/z"; echo off > "$D/off"; exit 0' INT
while :; do cat "$D/z"; sleep 0.01; done
"#;

    #[test]
    fn the_output_stays_open_until_gphoto2_has_said_goodbye() {
        let fake = Fake::new("chatty", CHATTY);
        let mut cap = open_with(&fake.program(), "", quick()).unwrap();
        assert_eq!(cap.read().unwrap(), jpeg(b'Z', 1));
        drop(cap);
        assert_eq!(fake.file("off").trim(), "off", "gphoto2 lost its output before it finished");
        assert_eq!(fake.file("ints").lines().count(), 1, "a second interrupt makes gphoto2 quit without turning live view off");
    }
}
