//! UnityCapture, the camera older Windows gets: frames go to the shared image an app's filter makes when it opens the
//! camera, as RGBA bottom row first, the way pyvirtualcam's unitycapture backend sends them (UnityCapture's shared.inl).

use crate::convert::{fit_frame, i420_to_rgba_flipped_into, rgb_to_i420};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::output::VideoOutput;
use std::time::Instant;

/// The biggest picture the filter makes room for: 4K at four 16-bit channels. Must match the filter's own limit, or it resets the picture it finds in the primed image.
pub const MAX_SHARED_IMAGE_SIZE: usize = 3840 * 2160 * 4 * 2;
const HEADER: usize = 32;
const FORMAT_UINT8: i32 = 0;
const RESIZEMODE_LINEAR: i32 = 1;
const MIRRORMODE_DISABLED: i32 = 0;
/// Keeps the last frame showing after the sender stops while an app still reads (pyvirtualcam's choice).
const TIMEOUT: i32 = i32::MAX - 200;

/// Fills the shared image's header and pixels with one RGBA frame (stride = width). False if it's too big for it.
pub fn write_frame(buf: &mut [u8], rgba: &[u8], width: u32, height: u32) -> bool {
    let max = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    if rgba.len() > max || HEADER + rgba.len() > buf.len() {
        return false;
    }
    for (i, v) in [width as i32, height as i32, width as i32, FORMAT_UINT8, RESIZEMODE_LINEAR, MIRRORMODE_DISABLED, TIMEOUT].iter().enumerate() {
        buf[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    buf[HEADER..HEADER + rgba.len()].copy_from_slice(rgba);
    true
}

/// The pixels a frame becomes on UnityCapture: I420 or RGBA in, RGBA bottom row first out, at the camera's size.
pub fn picture(frame: &Frame, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    picture_into(frame, width, height, &mut out)?;
    Ok(out)
}

/// picture into out, which keeps its allocation from one call to the next.
pub fn picture_into(frame: &Frame, width: u32, height: u32, out: &mut Vec<u8>) -> Result<(), String> {
    let i420 = match frame.format {
        PixelFormat::I420 => frame.clone(),
        PixelFormat::Rgba => rgb_to_i420(&frame.data, frame.width, frame.height, 4, false),
        other => return Err(format!("UnityCapture takes I420 or RGBA frames, not {other:?}")),
    };
    i420_to_rgba_flipped_into(&fit_frame(&i420, width, height), out);
    Ok(())
}

/// Where frames go: the filter's objects, opened once an app has the camera open.
pub trait Target: Send {
    /// Sends the RGBA picture if an app's filter is there to take it; Ok either way.
    fn send(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<(), String>;
    /// Whether an app's filter is there to take pictures now, so one that nobody would see isn't made.
    fn ready(&mut self) -> bool {
        true
    }
    /// Whether a picture left for the filter to find when an app opens the camera would be used (prime).
    fn primes(&self) -> bool {
        false
    }
    /// Leaves the picture where the filter finds it the moment an app opens the camera, so that app's first frames aren't black. False if it wasn't written (the filter held the image, say), so it must be tried again.
    fn prime(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> bool {
        true
    }
}

/// When each camera's filter last asked for a frame, as noted by whoever took the signal. The Want event resets itself when a waiter takes it, so
/// the sender and the watcher each take some of the signals: both note what they took and read what the other noted, so neither mistakes the other's for silence.
static ASKS: std::sync::Mutex<Vec<(u32, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());

/// Notes that the filter of camera num just asked for a frame.
pub fn note_ask(num: u32) {
    let mut asks = ASKS.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    match asks.iter_mut().find(|(n, _)| *n == num) {
        Some(entry) => entry.1 = now,
        None => asks.push((num, now)),
    }
}

/// When the filter of camera num last asked for a frame, by anyone's note.
pub fn last_ask(num: u32) -> Option<std::time::Instant> {
    ASKS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(n, _)| *n == num).map(|(_, at)| *at)
}

/// Whether the filter asked for a frame: this waiter took a signal (`took`, which it notes), or the other waiter took one within `within`.
pub fn asked(num: u32, took: bool, within: std::time::Duration) -> bool {
    if took {
        note_ask(num);
        return true;
    }
    last_ask(num).is_some_and(|at| at.elapsed() < within)
}

/// How often the picture waiting for an app to open the camera is refreshed.
const PRIME_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// How soon a picture that couldn't be written is tried again.
const PRIME_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// The picture last made, and what it was made from: the same pixels again (a wait screen showing one picture) need no new conversion.
struct Made {
    /// Weak, so keeping it doesn't make a step that changes the pixels copy them (and its allocation can't be reused while it's here).
    source: std::sync::Weak<Vec<u8>>,
    format: PixelFormat,
    size: (u32, u32),
}

impl Made {
    fn of(frame: &Frame) -> Made {
        Made { source: std::sync::Arc::downgrade(&frame.data), format: frame.format, size: (frame.width, frame.height) }
    }

    fn is_of(&self, frame: &Frame) -> bool {
        std::ptr::eq(self.source.as_ptr(), std::sync::Arc::as_ptr(&frame.data)) && self.format == frame.format && self.size == (frame.width, frame.height)
    }
}

pub struct UcCamera {
    pub width: u32,
    pub height: u32,
    pub device: String,
    target: Box<dyn Target>,
    /// Whether the slot moved to the Windows 11 camera since.
    moved: Box<dyn Fn() -> bool + Send>,
    /// The RGBA picture made last, kept for its allocation and for the same frame again.
    rgba: Vec<u8>,
    made: Option<Made>,
    /// When the picture waiting for an app to open the camera was last refreshed.
    primed: Option<std::time::Instant>,
    /// What that picture was made from: the same pixels again (a wait screen showing one picture) aren't converted and written again.
    primed_from: Option<Made>,
    prime_every: std::time::Duration,
}

impl UcCamera {
    pub fn new(width: u32, height: u32, device: String, target: Box<dyn Target>, moved: Box<dyn Fn() -> bool + Send>) -> UcCamera {
        UcCamera { width, height, device, target, moved, rgba: Vec::new(), made: None, primed: None, primed_from: None, prime_every: PRIME_EVERY }
    }
}

impl VideoOutput for UcCamera {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Rgba) {
            return Err(format!("UnityCapture takes I420 or RGBA frames, not {:?}", frame.format));
        }
        if !self.target.ready() {
            // No app has the camera open: the picture is made again when one does
            self.rgba = Vec::new();
            self.made = None;
            if self.target.primes() && self.primed.is_none_or(|at| at.elapsed() >= self.prime_every) {
                self.primed = Some(Instant::now());
                if !self.primed_from.as_ref().is_some_and(|m| m.is_of(frame)) {
                    let mut picture = Vec::new();
                    if picture_into(frame, self.width, self.height, &mut picture).is_ok() {
                        // Only a picture that was written counts as primed: a skipped write is tried again soon, not left to the next refresh
                        if self.target.prime(&picture, self.width, self.height) {
                            self.primed_from = Some(Made::of(frame));
                        } else {
                            self.primed = Instant::now().checked_sub(self.prime_every.saturating_sub(PRIME_RETRY));
                        }
                    }
                }
            }
            return Ok(());
        }
        self.primed_from = None;
        if !self.made.as_ref().is_some_and(|m| m.is_of(frame)) {
            self.made = None;
            picture_into(frame, self.width, self.height, &mut self.rgba)?;
            self.made = Some(Made::of(frame));
        }
        self.target.send(&self.rgba, self.width, self.height)
    }
    fn send_now(&mut self, frame: &Frame) -> Result<(), String> {
        // After a hiding the picture waiting for an app to open the camera must be this one, not the clear one from up to a second ago
        self.primed = None;
        self.primed_from = None;
        self.send(frame)
    }
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn device(&self) -> String {
        self.device.clone()
    }
    fn outdated(&self) -> bool {
        (self.moved)()
    }
}

/// One go at opening the filter's objects while holding its mutex. The Want event is made once and kept in want
/// across goes: dropping it while the filter's Sent event isn't there yet would take away what the filter opens to answer.
pub fn open_objects<W, S, V>(want: &mut Option<W>, make_want: impl FnOnce() -> Option<W>, open_sent: impl FnOnce() -> Option<S>,
                             open_view: impl FnOnce() -> Option<V>) -> Option<(S, V)> {
    if want.is_none() {
        *want = Some(make_want()?);
    }
    let sent = open_sent()?;
    Some((sent, open_view()?))
}

/// The filter's mutex, events and shared image for camera number num, as the sending side of shared.inl opens them.
#[cfg(windows)]
pub struct Shared {
    num: u32,
    /// Made on the first try and kept while the filter has yet to answer it.
    want: Option<crate::win::Owned>,
    /// The shared image made ahead of the filter, holding the picture its first frames show.
    primed: Option<crate::win::View>,
    /// When making the shared image found someone else's, so it isn't tried again every second.
    create_failed: Option<std::time::Instant>,
    open: Option<(crate::win::Owned, crate::win::Owned, crate::win::Owned, crate::win::View)>,
    /// When the filter last asked for a frame (its Want event), or when it was opened. An app that closed the camera stops asking.
    asked: std::time::Instant,
}

/// How long the filter may go without asking for a frame before it counts as closed: frames stop being made and only the waiting picture is kept fresh.
#[cfg(windows)]
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How long to leave an image that someone else made alone before trying to make it again.
#[cfg(windows)]
const CREATE_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Room the primed image has beyond the biggest picture and the header, in case the filter counts a little more than we do.
#[cfg(windows)]
const IMAGE_SLACK: usize = 4096;

/// How long the first go at the filter's objects waits for its mutex.
#[cfg(windows)]
const OPEN_WAIT_MS: u32 = 30;

/// How long a frame waits for the filter's mutex before it's dropped: the filter holds it only while it copies a frame, and a
/// stuck one mustn't hold up the next picture for most of a frame's time.
#[cfg(windows)]
const LOCK_WAIT_MS: u32 = 2;

#[cfg(windows)]
impl Shared {
    pub fn new(num: u32) -> Shared {
        Shared { num, want: None, primed: None, create_failed: None, open: None, asked: std::time::Instant::now() }
    }

    /// Makes the Want event and the shared image before any app opens the camera, with the picture in it. The filter shows what it
    /// finds there at once, where a new image that nobody has written to yet makes its first frames black. After an app closed the
    /// camera it replaces what the image shows, so the next one to open it doesn't start on an old picture.
    fn prime_image(&mut self, rgba: &[u8], width: u32, height: u32) -> bool {
        use crate::wincam::uc_object_name as name;
        if let Some((mutex, _, _, view)) = self.open.as_mut() {
            if !crate::win::wait(mutex, LOCK_WAIT_MS) {
                return false;
            }
            let written = write_frame(view.buf(), rgba, width, height);
            crate::win::release_mutex(mutex);
            return written;
        }
        if self.want.is_none() {
            self.want = crate::win::create_event(&name("Want", self.num));
        }
        if self.primed.is_none() {
            if self.create_failed.is_some_and(|at| at.elapsed() < CREATE_RETRY) {
                return false;
            }
            match crate::win::View::create(&name("Data", self.num), HEADER + MAX_SHARED_IMAGE_SIZE + IMAGE_SLACK) {
                // Someone else made it, so a filter is already running: the frames go the usual way once it's opened
                Some((_, false)) | None => {
                    self.create_failed = Some(std::time::Instant::now());
                    return false;
                }
                Some((mut view, true)) => {
                    view.buf()[0..4].copy_from_slice(&(MAX_SHARED_IMAGE_SIZE as u32).to_le_bytes());
                    self.primed = Some(view);
                }
            }
        }
        // The filter makes its mutex when an app opens the camera and holds it while it copies the picture: a write now would tear it
        if crate::win::mutex_exists(&name("Mutx", self.num)) {
            return false;
        }
        self.primed.as_mut().is_some_and(|view| write_frame(view.buf(), rgba, width, height))
    }

    fn try_open(&mut self) -> bool {
        use crate::wincam::uc_object_name as name;
        if self.open.is_some() {
            return true;
        }
        let Some(mutex) = crate::win::open_mutex(&name("Mutx", self.num)) else { return false };
        // A filter stuck holding the mutex must not freeze the sender: try again on the next frame
        if !crate::win::wait(&mutex, OPEN_WAIT_MS) {
            return false;
        }
        let num = self.num;
        let opened = open_objects(
            &mut self.want,
            || crate::win::create_event(&name("Want", num)),
            || crate::win::open_event(&name("Sent", num), crate::win::EVENT_MODIFY_STATE),
            || crate::win::View::open(&name("Data", num), crate::win::FILE_MAP_WRITE),
        );
        crate::win::release_mutex(&mutex);
        match opened {
            Some((sent, view)) => {
                self.primed = None;
                let want = self.want.take().unwrap();
                self.open = Some((mutex, want, sent, view));
                self.asked = std::time::Instant::now();
                true
            }
            None => false,
        }
    }
}

#[cfg(windows)]
impl Target for Shared {
    fn ready(&mut self) -> bool {
        if !self.try_open() {
            return false;
        }
        // The filter signals Want for every frame it takes. Nothing for a while means the app closed the camera, and pictures
        // made meanwhile would be thrown away: the next sign of an app is the moment to start making them again.
        // The watcher takes some of the signals too (they reset on being taken), and notes the ones it took
        let (_, want, _, _) = self.open.as_ref().unwrap();
        if asked(self.num, crate::win::wait(want, 0), ASK_TIMEOUT) {
            self.asked = last_ask(self.num).unwrap_or_else(std::time::Instant::now);
        }
        self.asked.elapsed() < ASK_TIMEOUT
    }

    fn primes(&self) -> bool {
        self.open.is_none() || self.asked.elapsed() >= ASK_TIMEOUT
    }

    fn prime(&mut self, rgba: &[u8], width: u32, height: u32) -> bool {
        self.prime_image(rgba, width, height)
    }

    fn send(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
        // Until an app opens the camera there's nowhere to send: that's normal, not an error
        if !self.try_open() {
            return Ok(());
        }
        let (mutex, _want, sent, view) = self.open.as_mut().unwrap();
        // The filter holds the mutex while it reads a frame; if it's stuck, drop this frame instead of waiting
        if !crate::win::wait(mutex, LOCK_WAIT_MS) {
            return Ok(());
        }
        let fits = write_frame(view.buf(), rgba, width, height);
        crate::win::release_mutex(mutex);
        if !fits {
            return Err(format!("{width}x{height} is too big for UnityCapture"));
        }
        crate::win::set_event(sent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::grey;

    fn buf(max: u32) -> Vec<u8> {
        let mut b = vec![0u8; HEADER + max as usize];
        b[0..4].copy_from_slice(&max.to_le_bytes());
        b
    }

    #[test]
    fn want_is_made_once_and_kept_while_sent_is_still_missing() {
        let mut want: Option<u32> = None;
        let mut made = 0;
        for _ in 0..3 {
            let got = open_objects(&mut want, || { made += 1; Some(7) }, || None::<u8>, || Some(()));
            assert!(got.is_none());
            assert_eq!(want, Some(7), "dropped between tries");
        }
        assert_eq!(made, 1);
        assert_eq!(open_objects(&mut want, || unreachable!(), || Some(1u8), || Some(())), Some((1, ())));
    }

    #[test]
    fn a_frame_fills_the_header_and_pixels() {
        let mut b = buf(64);
        assert!(write_frame(&mut b, &[7u8; 32], 4, 2));
        let i = |at: usize| i32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        assert_eq!((i(4), i(8), i(12), i(16), i(20), i(24), i(28)), (4, 2, 4, 0, 1, 0, i32::MAX - 200));
        assert_eq!(&b[32..64], &[7u8; 32]);
    }

    #[test]
    fn a_frame_too_big_for_the_shared_image_is_never_written() {
        let mut b = buf(16);
        assert!(!write_frame(&mut b, &[7u8; 32], 4, 2));
        assert_eq!(&b[4..8], &[0; 4]);
    }

    #[test]
    fn frames_go_upside_down_at_the_cameras_size() {
        let mut f = grey(2, 2, 16);
        f.data_mut()[2] = 235; // bottom left white
        let rgba = picture(&f, 2, 2).unwrap();
        assert!(rgba[0] > 200 && rgba[3] == 255, "the bottom row comes first: {rgba:?}");
        assert_eq!(picture(&grey(4, 4, 16), 2, 2).unwrap().len(), 16);
    }

    struct Sink {
        ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl Target for Sink {
        fn send(&mut self, rgba: &[u8], _width: u32, _height: u32) -> Result<(), String> {
            self.sent.lock().unwrap().push(rgba.to_vec());
            Ok(())
        }
        fn ready(&mut self) -> bool {
            self.ready.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// A target nobody has opened that keeps the pictures it is primed with.
    struct Priming(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    impl Target for Priming {
        fn send(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> Result<(), String> {
            Ok(())
        }
        fn ready(&mut self) -> bool {
            false
        }
        fn primes(&self) -> bool {
            true
        }
        fn prime(&mut self, rgba: &[u8], _width: u32, _height: u32) -> bool {
            self.0.lock().unwrap().push(rgba.to_vec());
            true
        }
    }

    #[test]
    fn a_picture_waits_for_the_app_that_opens_the_camera_and_is_refreshed_now_and_then() {
        let primed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut cam = UcCamera::new(4, 4, "cam".into(), Box::new(Priming(primed.clone())), Box::new(|| false));
        cam.send(&grey(4, 4, 100)).unwrap();
        cam.send(&grey(4, 4, 200)).unwrap();
        assert_eq!(primed.lock().unwrap().len(), 1, "once a second at most");
        let first = primed.lock().unwrap()[0].clone();
        assert_eq!(first.len(), 4 * 4 * 4);
        assert!(first[0] < 150, "the picture the camera had then: {first:?}");
        cam.prime_every = std::time::Duration::ZERO;
        cam.send(&grey(4, 4, 200)).unwrap();
        let primed = primed.lock().unwrap();
        assert_eq!(primed.len(), 2);
        assert!(primed[1][0] > 150, "the newer picture");
        assert_eq!(cam.rgba.capacity(), 0, "nothing kept for a camera nobody reads");
    }

    #[test]
    fn a_hidden_picture_replaces_the_waiting_one_at_once() {
        let primed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut cam = UcCamera::new(4, 4, "cam".into(), Box::new(Priming(primed.clone())), Box::new(|| false));
        cam.prime_every = std::time::Duration::from_secs(3600);
        cam.send(&grey(4, 4, 200)).unwrap();
        cam.send(&grey(4, 4, 100)).unwrap();
        assert_eq!(primed.lock().unwrap().len(), 1);
        cam.send_now(&grey(4, 4, 100)).unwrap();
        let primed = primed.lock().unwrap();
        assert_eq!(primed.len(), 2, "not left to the next refresh");
        assert!(primed[1][0] < 150, "the hidden picture, not the clear one");
    }

    /// A target nobody has opened whose writes are skipped (the filter holds its image) until the test lets them through.
    struct Skipping(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>, std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Target for Skipping {
        fn send(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> Result<(), String> {
            Ok(())
        }
        fn ready(&mut self) -> bool {
            false
        }
        fn primes(&self) -> bool {
            true
        }
        fn prime(&mut self, rgba: &[u8], _width: u32, _height: u32) -> bool {
            let written = self.1.load(std::sync::atomic::Ordering::SeqCst);
            if written {
                self.0.lock().unwrap().push(rgba.to_vec());
            }
            written
        }
    }

    #[test]
    fn a_hidden_picture_whose_write_was_skipped_is_written_again_with_the_same_pixels() {
        let primed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut cam = UcCamera::new(4, 4, "cam".into(), Box::new(Skipping(primed.clone(), open.clone())), Box::new(|| false));
        let hidden = grey(4, 4, 100);
        cam.send_now(&hidden).unwrap();
        assert!(primed.lock().unwrap().is_empty());
        assert!(cam.primed_from.is_none(), "a skipped write was taken for a primed picture");
        open.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(PRIME_RETRY + std::time::Duration::from_millis(30));
        cam.send(&hidden).unwrap();
        assert_eq!(primed.lock().unwrap().len(), 1, "the same pixels weren't tried again");
        assert!(primed.lock().unwrap()[0][0] < 150);
        cam.send(&hidden).unwrap();
        assert_eq!(primed.lock().unwrap().len(), 1, "written once it went through");
    }

    #[test]
    fn a_signal_one_waiter_of_the_want_event_took_counts_for_the_other() {
        let second = std::time::Duration::from_secs(1);
        assert!(!asked(4242, false, second), "nothing was asked yet");
        assert!(asked(4242, true, second), "taken here");
        assert!(asked(4242, false, second), "taken by the other waiter a moment ago");
        assert!(!asked(4242, false, std::time::Duration::ZERO), "too long ago for the wait it is asked for");
        assert!(!asked(4243, false, second), "another camera's filter");
    }

    #[test]
    fn a_picture_that_has_not_changed_is_not_made_and_written_again() {
        let primed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut cam = UcCamera::new(4, 4, "cam".into(), Box::new(Priming(primed.clone())), Box::new(|| false));
        cam.prime_every = std::time::Duration::ZERO;
        let still = grey(4, 4, 100);
        for _ in 0..3 {
            cam.send(&still).unwrap();
        }
        assert_eq!(primed.lock().unwrap().len(), 1);
        cam.send(&grey(4, 4, 200)).unwrap();
        assert_eq!(primed.lock().unwrap().len(), 2, "a new picture is written");
    }

    #[test]
    fn nothing_is_made_for_a_camera_that_takes_no_primed_picture() {
        let (mut cam, _sent, _open) = switchable_camera(false);
        cam.send(&grey(4, 4, 100)).unwrap();
        assert!(cam.primed.is_none());
    }

    fn camera(ready: bool) -> (UcCamera, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        let (cam, sent, _) = switchable_camera(ready);
        (cam, sent)
    }

    /// A camera and the switch that tells it whether an app has the camera open.
    fn switchable_camera(ready: bool) -> (UcCamera, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let switch = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(ready));
        (UcCamera::new(4, 4, "cam".into(), Box::new(Sink { ready: switch.clone(), sent: sent.clone() }), Box::new(|| false)), sent, switch)
    }

    #[test]
    fn the_picture_is_let_go_while_no_app_has_the_camera_open_and_made_again_after() {
        let (mut cam, sent, open) = switchable_camera(true);
        let f = grey(4, 4, 100);
        cam.send(&f).unwrap();
        assert!(cam.rgba.capacity() > 0);
        open.store(false, std::sync::atomic::Ordering::SeqCst);
        cam.send(&f).unwrap();
        assert_eq!(cam.rgba.capacity(), 0, "the RGBA buffer is not kept for a camera nobody reads");
        open.store(true, std::sync::atomic::Ordering::SeqCst);
        cam.send(&f).unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], picture(&f, 4, 4).unwrap(), "the same frame is made again, not sent from an empty buffer");
    }

    #[test]
    fn nothing_is_sent_while_no_app_has_the_camera_open() {
        let (mut cam, sent) = camera(false);
        cam.send(&grey(4, 4, 100)).unwrap();
        assert!(sent.lock().unwrap().is_empty());
        assert!(cam.send(&Frame::new(1, 1, PixelFormat::Bgra, vec![0; 4])).is_err());
    }

    #[test]
    fn the_same_frame_again_sends_the_same_picture_and_a_new_one_a_new_picture() {
        let (mut cam, sent) = camera(true);
        let (a, b) = (grey(4, 4, 100), grey(4, 4, 200));
        for f in [&a, &a, &b, &a] {
            cam.send(f).unwrap();
        }
        let sent = sent.lock().unwrap();
        let (pa, pb) = (picture(&a, 4, 4).unwrap(), picture(&b, 4, 4).unwrap());
        assert_ne!(pa, pb);
        assert_eq!(*sent, [pa.clone(), pa.clone(), pb, pa]);
    }

    #[test]
    fn a_frame_changed_in_place_is_not_taken_for_the_one_before() {
        let (mut cam, sent) = camera(true);
        let mut f = grey(4, 4, 100);
        cam.send(&f).unwrap();
        f.data_mut()[0] = 235;
        cam.send(&f).unwrap();
        let sent = sent.lock().unwrap();
        assert_ne!(sent[0], sent[1]);
        assert_eq!(sent[1], picture(&f, 4, 4).unwrap());
    }
}
