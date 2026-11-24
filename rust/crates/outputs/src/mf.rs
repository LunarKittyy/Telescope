//! The Windows 11 camera (rust/windows/virtualcamera; how it works is in docs/development/architecture.md): each frame goes to the shared
//! memory the camera reads, as NV12 at the size the app reading it picked. While no app reads it there's nowhere to send,
//! so frames are dropped. The layout is virtualcamera/shared_frame.h's.

use crate::convert::{fit_frame, rgb_to_i420, write_nv12};
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::output::VideoOutput;

pub const SHARED_NAME: &str = "Global\\TelescopeCamera";
/// The camera's event for a new frame, which a camera of an earlier version doesn't make.
pub const FRAME_EVENT_NAME: &str = "Global\\TelescopeCameraFrame";
pub const MAGIC: u32 = 0x4D43_5354; // "TSCM"
pub const PROTOCOL: u32 = 1;
pub const NV12: u32 = 1;
pub const HEADER_SIZE: usize = 64;
const COUNTER_AT: usize = 40;
const FRAME_AT: usize = 48;
/// A reader counts as gone once it hasn't asked for a frame this long.
pub const WATCH_LINGER: Duration = Duration::from_secs(3);

/// A camera's shared memory, mapped; let go of when dropped.
pub trait Mapping: Send {
    /// Runs f on the mapped bytes.
    fn with(&mut self, f: &mut dyn FnMut(&mut [u8]));
    /// Tells the camera a frame has landed, so it needn't poll for one.
    fn signal(&mut self) {}
}

/// The header's fields: magic, version, data_offset, data_capacity, consumer_state, consumer_width, consumer_height,
/// consumer_fps, then consumer_heartbeat, frame_counter (u64s), then frame_width, frame_height, frame_format, reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub magic: u32,
    pub version: u32,
    pub data_offset: u32,
    pub data_capacity: u32,
    pub consumer_state: u32,
    pub consumer_width: u32,
    pub consumer_height: u32,
    pub consumer_fps: u32,
    pub heartbeat: u64,
    pub counter: u64,
    pub frame_width: u32,
    pub frame_height: u32,
    pub frame_format: u32,
}

impl Header {
    fn reader_size(&self) -> Option<(u32, u32)> {
        let (w, h) = (self.consumer_width, self.consumer_height);
        if self.consumer_state != 1 || w == 0 || h == 0 || w % 2 == 1 || h % 2 == 1
            || w as u64 * h as u64 * 3 / 2 > self.data_capacity as u64 {
            return None;
        }
        Some((w, h))
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub struct SharedView {
    map: Box<dyn Mapping>,
}

impl SharedView {
    pub fn new(map: Box<dyn Mapping>) -> SharedView {
        SharedView { map }
    }

    fn bytes<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.map.with(&mut |b| out = f.take().map(|f| f(b)));
        out.expect("the mapping ran nothing")
    }

    pub fn header(&mut self) -> Header {
        self.bytes(|b| Header {
            magic: u32_at(b, 0), version: u32_at(b, 4), data_offset: u32_at(b, 8), data_capacity: u32_at(b, 12),
            consumer_state: u32_at(b, 16), consumer_width: u32_at(b, 20), consumer_height: u32_at(b, 24),
            consumer_fps: u32_at(b, 28), heartbeat: u64_at(b, 32), counter: u64_at(b, COUNTER_AT),
            frame_width: u32_at(b, FRAME_AT), frame_height: u32_at(b, FRAME_AT + 4), frame_format: u32_at(b, FRAME_AT + 8),
        })
    }

    /// The camera's own memory, as this version of the camera lays it out (not some other program's).
    pub fn valid(&mut self) -> bool {
        let len = self.bytes(|b| b.len());
        if len < HEADER_SIZE {
            return false;
        }
        let h = self.header();
        h.magic == MAGIC && h.version == PROTOCOL && h.data_offset as usize >= HEADER_SIZE
            && h.data_offset as usize + h.data_capacity as usize <= len
    }

    /// The size the app reading the camera picked, if one is reading and it's a size frames fit in.
    pub fn reader_size(&mut self) -> Option<(u32, u32)> {
        self.header().reader_size()
    }

    /// Writes an I420 frame of exactly the reader's size as NV12, with the counter odd while it's half written.
    fn write(&mut self, frame: &Frame) {
        let h = self.header();
        let counter = h.counter + (h.counter & 1); // odd: a write was cut off half way
        self.bytes(|b| {
            b[COUNTER_AT..COUNTER_AT + 8].copy_from_slice(&(counter + 1).to_le_bytes());
            let size = (frame.width * frame.height * 3 / 2) as usize;
            let at = h.data_offset as usize;
            write_nv12(frame, &mut b[at..at + size]);
            b[FRAME_AT..FRAME_AT + 4].copy_from_slice(&frame.width.to_le_bytes());
            b[FRAME_AT + 4..FRAME_AT + 8].copy_from_slice(&frame.height.to_le_bytes());
            b[FRAME_AT + 8..FRAME_AT + 12].copy_from_slice(&NV12.to_le_bytes());
            b[COUNTER_AT..COUNTER_AT + 8].copy_from_slice(&(counter + 2).to_le_bytes());
        });
        self.map.signal();
    }
}

/// Slot's shared memory while an app reads the camera, else None.
pub type Opener = Box<dyn Fn() -> Option<SharedView> + Send>;

pub struct MfCamera {
    pub width: u32,
    pub height: u32,
    pub device: String,
    open: Opener,
    view: Option<SharedView>,
    beat: Option<u64>,
    beat_at: Instant,
    /// The heartbeat a reader stopped on when it was let go: a mapping that still shows it is the stalled reader, not a new request.
    stalled: Option<u64>,
    linger: Duration,
    last_write: Option<(Instant, u32, u32)>,
    /// Whether the slot still goes to the Windows 11 camera.
    still_mf: Box<dyn Fn() -> bool + Send>,
}

impl MfCamera {
    pub fn new(width: u32, height: u32, device: String, open: Opener, still_mf: Box<dyn Fn() -> bool + Send>) -> MfCamera {
        MfCamera { width, height, device, open, view: None, beat: None, beat_at: Instant::now(), stalled: None, linger: WATCH_LINGER, last_write: None, still_mf }
    }

    /// The shared memory while an app reads the camera. Let go of once it stops (or stops asking for frames), so the
    /// camera can take it away and the camera watch sees it gone.
    fn reader(&mut self) -> Option<&mut SharedView> {
        let now = Instant::now();
        if let Some(view) = self.view.as_mut() {
            let beat = view.header().heartbeat;
            if Some(beat) != self.beat {
                self.beat = Some(beat);
                self.beat_at = now;
            }
            if view.reader_size().is_none() {
                self.stalled = None;
                self.view = None;
            } else if now - self.beat_at > self.linger {
                self.stalled = self.beat;
                self.view = None;
            }
        }
        if self.view.is_none() {
            self.last_write = None;
            self.view = (self.open)();
            match self.view.as_mut().map(|view| view.header().heartbeat) {
                // The same mapping, still not beating (a paused or hung reader): nothing is sent until it beats again
                Some(beat) if self.stalled == Some(beat) => self.view = None,
                Some(beat) => {
                    self.stalled = None;
                    self.beat = Some(beat);
                    self.beat_at = now;
                }
                None => self.stalled = None,
            }
        }
        self.view.as_mut()
    }

    pub fn is_open(&self) -> bool {
        self.view.is_some()
    }

    #[cfg(test)]
    fn send_at(&mut self, frame: &Frame, now: Instant) -> Result<(), String> {
        self.write_at(frame, now, false)
    }

    /// `urgent` skips the rate limit: the frame has to reach the reader, whatever came just before it.
    fn write_at(&mut self, frame: &Frame, now: Instant, urgent: bool) -> Result<(), String> {
        if !matches!(frame.format, PixelFormat::I420 | PixelFormat::Rgba) {
            return Err(format!("The Windows 11 camera takes I420 or RGBA frames, not {:?}", frame.format));
        }
        // No converting for a picture no app is there to see
        let Some(header) = self.reader().map(SharedView::header) else { return Ok(()) };
        let Some((w, h)) = header.reader_size() else { return Ok(()) };
        if let Some((at, width, height)) = self.last_write.filter(|_| !urgent) {
            if (w, h) == (width, height) && header.consumer_fps != 0 {
                // A frame that comes a little early still counts, so jitter never drops a frame at the consumer's own rate.
                let period = Duration::from_secs_f64(0.75 / header.consumer_fps.clamp(1, 240) as f64);
                if now - at < period {
                    return Ok(());
                }
            }
        }
        let frame = match frame.format {
            PixelFormat::Rgba => rgb_to_i420(&frame.data, frame.width, frame.height, 4, false),
            _ => frame.clone(),
        };
        self.view.as_mut().unwrap().write(&fit_frame(&frame, w, h));
        self.last_write = Some((now, w, h));
        Ok(())
    }
}

impl VideoOutput for MfCamera {
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.write_at(frame, Instant::now(), false)
    }

    fn send_now(&mut self, frame: &Frame) -> Result<(), String> {
        self.write_at(frame, Instant::now(), true)
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn fit_size(&mut self) -> Option<(u32, u32)> {
        self.reader()?.reader_size()
    }

    fn device(&self) -> String {
        self.device.clone()
    }

    fn outdated(&self) -> bool {
        !(self.still_mf)()
    }
}

/// Whether an app reads the camera now, for the watch: (reading, heartbeat). The view is let go of straight away.
pub fn peek(open: &dyn Fn() -> Option<SharedView>) -> Option<(bool, u64)> {
    let mut view = open()?;
    Some((view.reader_size().is_some(), view.header().heartbeat))
}

/// The real thing: Global\TelescopeCamera<slot> mapped while it exists.
#[cfg(windows)]
pub fn open_shared(slot: u32) -> Option<SharedView> {
    struct Win {
        view: crate::win::View,
        slot: u32,
        event: Option<crate::win::Owned>,
        /// When the event was last looked for, so a camera without one is asked again about once a second.
        tried: Option<std::time::Instant>,
    }
    impl Mapping for Win {
        fn with(&mut self, f: &mut dyn FnMut(&mut [u8])) {
            f(self.view.buf())
        }
        fn signal(&mut self) {
            let slot = self.slot;
            if self.event.is_none() && self.tried.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
                self.tried = Some(std::time::Instant::now());
                self.event = crate::win::open_event(&format!("{FRAME_EVENT_NAME}{slot}"), crate::win::EVENT_MODIFY_STATE);
            }
            if let Some(event) = &self.event {
                crate::win::set_event(event);
            }
        }
    }
    let access = crate::win::FILE_MAP_READ | crate::win::FILE_MAP_WRITE;
    let view = crate::win::View::open(&format!("{SHARED_NAME}{slot}"), access)?;
    let mut shared = SharedView::new(Box::new(Win { view, slot, event: None, tried: None }));
    shared.valid().then_some(shared)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::convert::grey;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A camera's shared memory as its DLL leaves it while an app reads it at w x h.
    pub fn shared(w: u32, h: u32, state: u32, beat: u64, counter: u64, capacity: Option<u32>, magic: u32) -> Vec<u8> {
        let capacity = capacity.unwrap_or(w * h * 3 / 2);
        let mut b = vec![0u8; HEADER_SIZE + capacity as usize];
        for (i, v) in [magic, PROTOCOL, 64, capacity, state, w, h, 30].iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[32..40].copy_from_slice(&beat.to_le_bytes());
        b[40..48].copy_from_slice(&counter.to_le_bytes());
        b
    }

    pub fn reading(w: u32, h: u32) -> Vec<u8> {
        shared(w, h, 1, 0, 0, None, MAGIC)
    }

    /// Hands out views of buf (None: no app reads the camera) and counts how many were opened and closed.
    #[derive(Clone, Default)]
    pub struct Opener {
        pub buf: Arc<Mutex<Option<Vec<u8>>>>,
        pub opened: Arc<AtomicUsize>,
        pub closed: Arc<AtomicUsize>,
        pub signals: Arc<AtomicUsize>,
    }

    /// A view of the shared buffer; once the camera takes its memory away (None) it keeps the copy it last saw.
    struct View(Arc<Mutex<Option<Vec<u8>>>>, Arc<AtomicUsize>, Arc<AtomicUsize>);

    impl Mapping for View {
        fn with(&mut self, f: &mut dyn FnMut(&mut [u8])) {
            let mut b = self.0.lock().unwrap();
            match b.as_mut() {
                Some(b) => f(b),
                None => f(&mut [0u8; 64]),
            }
        }
        fn signal(&mut self) {
            self.2.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Opener {
        pub fn new(buf: Option<Vec<u8>>) -> Opener {
            Opener { buf: Arc::new(Mutex::new(buf)), ..Opener::default() }
        }
        pub fn opener(&self) -> super::Opener {
            let me = self.clone();
            Box::new(move || {
                me.buf.lock().unwrap().as_ref()?;
                me.opened.fetch_add(1, Ordering::SeqCst);
                Some(SharedView::new(Box::new(View(me.buf.clone(), me.closed.clone(), me.signals.clone()))))
            })
        }
        pub fn header(&self) -> Header {
            let b = self.buf.lock().unwrap().clone().unwrap();
            SharedView::new(Box::new(View(Arc::new(Mutex::new(Some(b))), Arc::default(), Arc::default()))).header()
        }
        pub fn set(&self, at: usize, value: u8) {
            if let Some(b) = self.buf.lock().unwrap().as_mut() {
                b[at] = value;
            }
        }
        fn set_fps(&self, fps: u32) {
            self.buf.lock().unwrap().as_mut().unwrap()[28..32].copy_from_slice(&fps.to_le_bytes());
        }
    }

    fn cam(o: &Opener, w: u32, h: u32) -> MfCamera {
        MfCamera::new(w, h, "Telescope".into(), o.opener(), Box::new(|| true))
    }

    fn send(c: &mut MfCamera, _o: &Opener, frame: &Frame) {
        c.send(frame).unwrap();
    }

    // test_vcam.py::test_the_windows_11_camera_gets_nv12_at_the_readers_size
    #[test]
    fn the_windows_11_camera_gets_nv12_at_the_readers_size() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        let data: Vec<u8> = (0..48u8).collect();
        let frame = Frame::new(8, 4, PixelFormat::I420, data.clone());
        let now = Instant::now();
        c.send_at(&frame, now).unwrap();
        let buf = o.buf.lock().unwrap().clone().unwrap();
        assert_eq!(&buf[64..96], &data[..32]);
        let (u, v): (Vec<u8>, Vec<u8>) = (buf[96..].iter().step_by(2).copied().collect(), buf[97..].iter().step_by(2).copied().collect());
        assert_eq!((u.as_slice(), v.as_slice()), (&data[32..40], &data[40..48]));
        let h = o.header();
        assert_eq!((h.counter, h.frame_width, h.frame_height, h.frame_format), (2, 8, 4, NV12));
        c.send_at(&frame, now + Duration::from_millis(34)).unwrap();
        assert_eq!(o.header().counter, 4);
    }

    #[test]
    fn the_camera_is_told_after_each_frame_that_is_written_and_not_for_one_that_is_thinned() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        let frame = grey(8, 4, 235);
        let now = Instant::now();
        c.send_at(&frame, now).unwrap();
        assert_eq!(o.signals.load(Ordering::SeqCst), 1);
        c.send_at(&frame, now + Duration::from_millis(5)).unwrap();
        assert_eq!(o.signals.load(Ordering::SeqCst), 1, "thinned");
        c.send_at(&frame, now + Duration::from_millis(40)).unwrap();
        assert_eq!(o.signals.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn sixty_fps_into_thirty_writes_every_second_frame_despite_jitter() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        let frame = grey(8, 4, 235);
        let now = Instant::now();
        for i in 0..60 {
            let elapsed = Duration::from_micros(i * 16_667 - (i % 3) * 200);
            c.send_at(&frame, now + elapsed).unwrap();
            assert_eq!(o.header().counter, 2 * (i / 2 + 1), "frame {i}");
        }
    }

    #[test]
    fn an_urgent_frame_is_written_though_the_last_one_just_went() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        let now = Instant::now();
        c.write_at(&grey(8, 4, 235), now, false).unwrap();
        c.write_at(&grey(8, 4, 16), now + Duration::from_millis(5), false).unwrap();
        assert_eq!(o.header().counter, 2, "thinned as usual");
        c.write_at(&grey(8, 4, 16), now + Duration::from_millis(5), true).unwrap();
        assert_eq!(o.header().counter, 4, "the hidden frame reaches the reader");
        assert_eq!(o.buf.lock().unwrap().clone().unwrap()[64], 16);
        c.write_at(&grey(8, 4, 235), now + Duration::from_millis(6), false).unwrap();
        assert_eq!(o.header().counter, 4, "the limit counts from the urgent one");
    }

    #[test]
    fn a_zero_consumer_rate_writes_every_frame() {
        let o = Opener::new(Some(reading(8, 4)));
        o.set_fps(0);
        let mut c = cam(&o, 8, 4);
        let now = Instant::now();
        for i in 1..=60 {
            c.send_at(&grey(8, 4, 235), now).unwrap();
            assert_eq!(o.header().counter, i * 2);
        }
    }

    #[test]
    fn consumer_rate_changes_apply_to_the_next_frame() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        let frame = grey(8, 4, 235);
        let now = Instant::now();
        c.send_at(&frame, now).unwrap();
        c.send_at(&frame, now + Duration::from_millis(16)).unwrap();
        assert_eq!(o.header().counter, 2);
        o.set_fps(60);
        c.send_at(&frame, now + Duration::from_millis(16)).unwrap();
        assert_eq!(o.header().counter, 4);
        o.set_fps(15);
        c.send_at(&frame, now + Duration::from_millis(56)).unwrap();
        assert_eq!(o.header().counter, 4);
        c.send_at(&frame, now + Duration::from_millis(76)).unwrap();
        assert_eq!(o.header().counter, 6);
        o.set_fps(0);
        c.send_at(&frame, now + Duration::from_millis(76)).unwrap();
        assert_eq!(o.header().counter, 8);
    }

    #[test]
    fn the_first_frame_after_open_or_resize_is_always_written() {
        let o = Opener::new(None);
        let mut c = cam(&o, 8, 4);
        let frame = grey(8, 4, 235);
        let now = Instant::now();
        c.send_at(&frame, now).unwrap();
        *o.buf.lock().unwrap() = Some(reading(8, 4));
        c.send_at(&frame, now).unwrap();
        assert_eq!(o.header().counter, 2);
        o.set(20, 4);
        c.send_at(&frame, now).unwrap();
        assert_eq!((o.header().counter, o.header().frame_width), (4, 4));
        *o.buf.lock().unwrap() = None;
        c.send_at(&frame, now).unwrap();
        assert!(!c.is_open());
        *o.buf.lock().unwrap() = Some(reading(4, 4));
        c.send_at(&frame, now).unwrap();
        assert_eq!(o.header().counter, 2);
    }

    #[test]
    fn consumer_rates_above_240_are_clamped() {
        let o = Opener::new(Some(reading(8, 4)));
        o.set_fps(u32::MAX);
        let mut c = cam(&o, 8, 4);
        let frame = grey(8, 4, 235);
        let now = Instant::now();
        c.send_at(&frame, now).unwrap();
        c.send_at(&frame, now + Duration::from_micros(3_124)).unwrap();
        assert_eq!(o.header().counter, 2);
        c.send_at(&frame, now + Duration::from_micros(3_125)).unwrap();
        assert_eq!(o.header().counter, 4);
    }

    // test_vcam.py::test_a_frame_of_another_size_is_fitted_to_what_the_app_picked
    #[test]
    fn a_frame_of_another_size_is_fitted_to_what_the_app_picked() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 16, 16);
        send(&mut c, &o, &grey(16, 16, 235));
        let buf = o.buf.lock().unwrap().clone().unwrap();
        assert_eq!((o.header().frame_width, o.header().frame_height), (8, 4));
        for row in buf[64..96].chunks(8) {
            assert!(row[3] > 200 && row[4] > 200 && row[0] < 30, "white in the middle, black bars at the sides: {row:?}");
        }
    }

    #[test]
    fn the_size_to_fit_to_follows_the_reader() {
        let o = Opener::new(None);
        let mut c = cam(&o, 16, 16);
        assert_eq!(c.fit_size(), None);
        *o.buf.lock().unwrap() = Some(reading(8, 4));
        assert_eq!(c.fit_size(), Some((8, 4)));
        *o.buf.lock().unwrap() = Some(reading(4, 2));
        assert_eq!(c.fit_size(), Some((4, 2)));
    }

    // test_vcam.py::test_frames_are_dropped_until_an_app_opens_the_camera
    #[test]
    fn frames_are_dropped_until_an_app_opens_the_camera() {
        let o = Opener::new(None);
        let mut c = cam(&o, 8, 4);
        send(&mut c, &o, &grey(8, 4, 0));
        *o.buf.lock().unwrap() = Some(reading(8, 4));
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.header().counter, 2);
    }

    // test_vcam.py::test_a_write_cut_off_half_way_is_finished_with_an_even_count
    #[test]
    fn a_write_cut_off_half_way_is_finished_with_an_even_count() {
        let o = Opener::new(Some(shared(8, 4, 1, 0, 5, None, MAGIC)));
        let mut c = cam(&o, 8, 4);
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.header().counter, 8);
    }

    // test_vcam.py::test_the_camera_is_let_go_once_the_app_stops_reading
    #[test]
    fn the_camera_is_let_go_once_the_app_stops_reading() {
        let o = Opener::new(Some(reading(8, 4)));
        let mut c = cam(&o, 8, 4);
        send(&mut c, &o, &grey(8, 4, 0));
        o.set(16, 0);
        *o.buf.lock().unwrap() = None;
        c.send(&grey(8, 4, 0)).unwrap();
        assert_eq!(o.closed.load(Ordering::SeqCst), 1);
        assert!(!c.is_open());
    }

    // test_vcam.py::test_an_app_that_stopped_asking_for_frames_counts_as_gone
    #[test]
    fn an_app_that_stopped_asking_for_frames_counts_as_gone() {
        let o = Opener::new(Some(shared(8, 4, 1, 7, 0, None, MAGIC)));
        let mut c = cam(&o, 8, 4);
        c.linger = Duration::from_millis(20);
        send(&mut c, &o, &grey(8, 4, 0));
        std::thread::sleep(Duration::from_millis(40));
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.closed.load(Ordering::SeqCst), 2, "let go, looked for again, and let go once more: it still isn't beating");
        assert_eq!(o.opened.load(Ordering::SeqCst), 2);
        assert_eq!(o.header().counter, 2, "nothing was written for the stalled reader");
        assert!(!c.is_open());
    }

    #[test]
    fn a_stalled_reader_gets_frames_again_once_it_beats() {
        let o = Opener::new(Some(shared(8, 4, 1, 7, 0, None, MAGIC)));
        let mut c = cam(&o, 8, 4);
        c.linger = Duration::from_millis(20);
        send(&mut c, &o, &grey(8, 4, 0));
        std::thread::sleep(Duration::from_millis(40));
        send(&mut c, &o, &grey(8, 4, 0));
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.header().counter, 2);
        // The reader wakes up and asks for a frame
        o.set(32, 8);
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.header().counter, 4);
        assert!(c.is_open());
    }

    // test_vcam.py::test_a_size_too_big_for_the_shared_memory_is_never_written
    #[test]
    fn a_size_too_big_for_the_shared_memory_is_never_written() {
        let o = Opener::new(Some(shared(8, 4, 1, 0, 0, Some(10), MAGIC)));
        assert_eq!(o.opener()().unwrap().reader_size(), None);
        let mut c = cam(&o, 8, 4);
        send(&mut c, &o, &grey(8, 4, 0));
        assert_eq!(o.header().counter, 0);
    }

    // test_vcam.py::test_memory_from_another_program_is_refused
    #[test]
    fn memory_from_another_program_is_refused() {
        assert!(!Opener::new(Some(shared(8, 4, 1, 0, 0, None, 0x1234))).opener()().unwrap().valid());
        assert!(Opener::new(Some(reading(8, 4))).opener()().unwrap().valid());
    }

    // test_vcam.py::test_the_windows_11_camera_only_takes_rgb_or_bgr (here: I420 or RGBA)
    #[test]
    fn the_windows_11_camera_only_takes_i420_or_rgba() {
        let o = Opener::new(None);
        let mut c = cam(&o, 8, 4);
        let nv12 = Frame::new(8, 4, PixelFormat::Nv12, vec![0; 48]);
        assert!(c.send(&nv12).is_err());
        assert!(c.send(&Frame::new(8, 4, PixelFormat::Rgba, vec![0; 128])).is_ok());
    }

    // test_vcam.py::test_camera_changed_compares_the_kind_with_the_slots_camera
    #[test]
    fn a_camera_whose_slot_moved_off_the_windows_11_camera_is_outdated() {
        let o = Opener::new(None);
        let still = Arc::new(Mutex::new(true));
        let s = still.clone();
        let c = MfCamera::new(8, 4, "Telescope".into(), o.opener(), Box::new(move || *s.lock().unwrap()));
        assert!(!c.outdated());
        *still.lock().unwrap() = false;
        assert!(c.outdated());
    }

    // test_vcam.py::test_the_python_side_matches_the_dlls_shared_memory_layout
    #[test]
    fn the_layout_matches_the_dlls_shared_memory() {
        let header = include_str!("../../../windows/virtualcamera/shared_frame.h");
        assert!(header.contains(&format!("kMagic = 0x{MAGIC:08X}")));
        assert!(header.contains(&format!("kVersion = {PROTOCOL};")) && header.contains(&format!("kFormatNV12 = {NV12};")));
        assert!(header.contains("kDataOffset = 64;") && HEADER_SIZE == 64);
        assert!(header.contains(&SHARED_NAME.replace('\\', "\\\\")));
        let start = header.find("struct SharedHeader").unwrap();
        let fields: Vec<&str> = header[start..].lines().skip(1).filter(|l| l.trim().starts_with("uint"))
            .map(|l| l.split_whitespace().nth(1).unwrap().trim_end_matches(';')).collect();
        assert_eq!((fields[8], fields[9], fields[10]), ("consumer_heartbeat", "frame_counter", "frame_width"));
    }
}
